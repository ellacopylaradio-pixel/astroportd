//! Mesure de la réplication : **où**, et **par qui**, les données d'un membre existent en plus de sa station.
//!
//! La synchronisation N¹/N² ([`crate::Crawler`]) tire les événements des amis ; ici on vérifie l'inverse, pour chaque
//! membre hébergé : parmi ses derniers événements, combien se trouvent aussi sur le relai de chaque autre station de la
//! constellation (un `REQ` par ids), et parmi ses derniers fichiers uCloud, quelles stations les fournissent (kubo).
//!
//! Seuls des kinds **lisibles sans authentification** sont échantillonnés : une absence signifie alors « pas répliqué »,
//! jamais « lecture refusée ». Un relai qui ne répond pas est marqué `error`, pas « 0 copie ».

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use astro_db::Db;
use astro_nostr_proto::Filter;
use serde_json::{Value, json};

use crate::client::RelayClient;
use crate::peers::Peer;
use crate::SyncError;

/// Scope des bilans dans `kv` (une clé par membre).
pub const SCOPE: &str = "replication";
/// Kinds échantillonnés : lisibles sans authentification (profil, notes, contacts, relais, articles, DID).
pub const SAMPLE_KINDS: [u32; 6] = [0, 1, 3, 10_002, 30_023, 30_800];

/// Qui fournit un fichier : PeerID des stations qui en ont une copie.
pub type ProviderFn = Arc<dyn Fn(String) -> Pin<Box<dyn Future<Output = Result<Vec<String>, String>> + Send>> + Send + Sync>;
/// Qui détient **plusieurs** fichiers à la fois (sonde groupée du maillage) : `CID → PeerID des stations qui l'ont`. Un
/// CID absent de la réponse n'a pas pu être cherché ainsi (il passe alors par [`ProviderFn`]).
pub type HoldersFn = Arc<dyn Fn(Vec<String>) -> Pin<Box<dyn Future<Output = HashMap<String, Vec<String>>> + Send>> + Send + Sync>;
/// Les stations de la constellation, relues à chaque passage.
pub type PeersFn = Arc<dyn Fn() -> Vec<Peer> + Send + Sync>;

/// Réglages.
#[derive(Clone)]
pub struct ReplicationConfig {
    /// Derniers événements vérifiés par membre.
    pub events_per_owner: usize,
    /// Derniers fichiers vérifiés par membre.
    pub files_per_owner: usize,
    /// Identifiants de la station elle-même (jamais comptée comme copie).
    pub own_node_id: Option<String>,
    /// Délai par relai.
    pub timeout: Duration,
}

impl Default for ReplicationConfig {
    fn default() -> Self {
        Self { events_per_owner: 30, files_per_owner: 8, own_node_id: None, timeout: Duration::from_secs(30) }
    }
}

/// Mesure de réplication.
pub struct Replicator<C: RelayClient> {
    db: Arc<Db>,
    client: C,
    peers: PeersFn,
    providers: Option<ProviderFn>,
    holders: Option<HoldersFn>,
    config: ReplicationConfig,
}

impl<C: RelayClient> Replicator<C> {
    /// `providers` : recherche des stations qui détiennent un CID (absent : les médias ne sont pas mesurés).
    pub fn new(db: Arc<Db>, client: C, peers: PeersFn, providers: Option<ProviderFn>, config: ReplicationConfig) -> Self {
        Self { db, client, peers, providers, holders: None, config }
    }

    /// Recherche groupée des fichiers (une sonde par station pour tout l'échantillon) avant la recherche fichier par fichier.
    pub fn with_holders(mut self, holders: HoldersFn) -> Self {
        self.holders = Some(holders);
        self
    }

    async fn owners(&self) -> Result<Vec<String>, SyncError> {
        Ok(self
            .db
            .readers()
            .read(|c| {
                let mut stmt = c.prepare("SELECT hex FROM identities WHERE hosted = 1 AND role IN ('multipass','captain') ORDER BY hex")?;
                Ok(stmt.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?)
            })
            .await?)
    }

    async fn sample_events(&self, owner: &str) -> Result<Vec<(String, u64)>, SyncError> {
        let (owner, n) = (owner.to_owned(), i64::try_from(self.config.events_per_owner).unwrap_or(30));
        let kinds = SAMPLE_KINDS.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
        Ok(self
            .db
            .readers()
            .read(move |c| {
                let mut stmt = c.prepare(&format!("SELECT id, created_at FROM nostr_events WHERE pubkey = ?1 AND kind IN ({kinds}) ORDER BY created_at DESC LIMIT ?2"))?;
                Ok(stmt.query_map(rusqlite::params![owner, n], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64)))?.collect::<Result<Vec<_>, _>>()?)
            })
            .await?)
    }

    async fn sample_files(&self, owner: &str) -> Result<Vec<String>, SyncError> {
        let (owner, n) = (owner.to_owned(), i64::try_from(self.config.files_per_owner).unwrap_or(8));
        Ok(self
            .db
            .readers()
            .read(move |c| {
                let mut stmt = c.prepare("SELECT DISTINCT cid FROM ucloud_entries WHERE owner_hex = ?1 AND type = 'file' AND cid IS NOT NULL ORDER BY mtime DESC LIMIT ?2")?;
                Ok(stmt.query_map(rusqlite::params![owner, n], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?)
            })
            .await?)
    }

    /// Niveau social (1 = ami, 2 = ami d'ami) du Capitaine de `peer` pour `owner`.
    async fn level_of(&self, owner: &str, peer: &Peer) -> Option<i64> {
        let (owner, captain) = (owner.to_owned(), peer.captain_hex.clone()?);
        self.db.readers().read(move |c| Ok(rusqlite::OptionalExtension::optional(c.query_row("SELECT level FROM social_graph WHERE owner_hex = ?1 AND pubkey = ?2", [&owner, &captain], |r| r.get::<_, i64>(0)))?)).await.ok().flatten()
    }

    /// Un passage : bilan de chaque membre hébergé, enregistré dans `kv` (scope `replication`, clé = clé du membre).
    pub async fn run_once(&self) -> Result<usize, SyncError> {
        let peers: Vec<Peer> = (self.peers)().into_iter().filter(|p| Some(&p.node_id) != self.config.own_node_id.as_ref()).collect();
        let owners = self.owners().await?;
        // Échantillon de tous les membres, puis un seul aller-retour par relai.
        let mut samples: HashMap<String, Vec<String>> = HashMap::new();
        let mut dated: HashMap<String, u64> = HashMap::new();
        for o in &owners {
            let sample = self.sample_events(o).await?;
            dated.extend(sample.iter().cloned());
            samples.insert(o.clone(), sample.into_iter().map(|(id, _)| id).collect());
        }
        let all_ids: Vec<String> = samples.values().flatten().cloned().collect::<HashSet<_>>().into_iter().collect();
        // Par relai : `Ok(ids présents)` ou la raison de l'échec.
        let mut held: HashMap<String, Result<HashSet<String>, String>> = HashMap::new();
        for peer in &peers {
            let mut present = HashSet::new();
            let mut failure = None;
            for chunk in all_ids.chunks(astro_nostr_proto::filter::MAX_LIST_LEN) {
                let filter = Filter { ids: Some(chunk.to_vec()), ..Filter::default() };
                // NIP-77 d'abord : des empreintes de plages au lieu des événements entiers, pour savoir seulement s'ils y sont.
                let local: Vec<(u64, String)> = chunk.iter().map(|id| (dated.get(id).copied().unwrap_or(0), id.clone())).collect();
                match tokio::time::timeout(self.config.timeout, self.client.sync(&peer.target(), &filter, &local)).await {
                    Ok(Ok(r)) => {
                        let gone: HashSet<&str> = r.gone.iter().map(String::as_str).collect();
                        present.extend(chunk.iter().filter(|i| !gone.contains(i.as_str())).cloned());
                        continue;
                    }
                    Ok(Err(crate::client::ClientError::Unsupported(_))) => {}
                    Ok(Err(e)) => {
                        failure = Some(e.to_string());
                        break;
                    }
                    Err(_) => {
                        failure = Some("délai dépassé".to_owned());
                        break;
                    }
                }
                match tokio::time::timeout(self.config.timeout, self.client.fetch(&peer.target(), &filter)).await {
                    Ok(Ok(events)) => present.extend(events.into_iter().map(|e| e.id)),
                    Ok(Err(e)) => {
                        failure = Some(e.to_string());
                        break;
                    }
                    Err(_) => {
                        failure = Some("délai dépassé".to_owned());
                        break;
                    }
                }
            }
            held.insert(peer.node_id.clone(), failure.map_or(Ok(present), Err));
        }
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        for owner in &owners {
            let ids = &samples[owner];
            let mut per_peer = Vec::new();
            let mut copies: HashMap<&str, usize> = ids.iter().map(|i| (i.as_str(), 0)).collect();
            for peer in &peers {
                let level = self.level_of(owner, peer).await;
                let (have, error) = match &held[&peer.node_id] {
                    Ok(present) => {
                        let have = ids.iter().filter(|i| present.contains(*i)).inspect(|i| *copies.entry(i.as_str()).or_insert(0) += 1).count();
                        (Some(have), None)
                    }
                    Err(why) => (None, Some(why.clone())),
                };
                per_peer.push(json!({
                    "node_id": peer.node_id, "name": peer.label(), "captain": peer.captain_email, "level": level, "have": have, "error": error,
                }));
            }
            let replicated = copies.values().filter(|n| **n > 0).count();
            let media = self.media_of(owner, &peers).await;
            let doc = json!({
                "at": now, "events": ids.len(), "replicated": replicated, "peers": per_peer, "media": media,
                "unreachable": per_peer.iter().filter(|p| !p["error"].is_null()).count(),
            });
            self.db
                .writer()
                .execute(
                    "INSERT INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                    vec![SCOPE.to_owned().into(), owner.clone().into(), doc.to_string().into(), i64::try_from(now).unwrap_or(i64::MAX).into()],
                )
                .await?;
        }
        Ok(owners.len())
    }

    /// Fichiers du membre fournis par d'autres stations (`null` quand les médias ne sont pas mesurables).
    async fn media_of(&self, owner: &str, peers: &[Peer]) -> Value {
        if self.providers.is_none() && self.holders.is_none() {
            return Value::Null;
        }
        let Ok(cids) = self.sample_files(owner).await else { return Value::Null };
        let ids: HashSet<&str> = peers.iter().map(|p| p.node_id.as_str()).collect();
        let mut per_peer: HashMap<String, usize> = HashMap::new();
        let (mut replicated, mut failed) = (0usize, 0usize);
        // D'abord une sonde groupée par station pour tout l'échantillon ; le reste, fichier par fichier.
        let grouped = match &self.holders {
            Some(h) => tokio::time::timeout(self.config.timeout, h(cids.clone())).await.unwrap_or_default(),
            None => HashMap::new(),
        };
        for cid in &cids {
            let found = match (grouped.get(cid), &self.providers) {
                (Some(found), _) => Ok(Ok(found.clone())),
                (None, Some(lookup)) => tokio::time::timeout(self.config.timeout, lookup(cid.clone())).await,
                (None, None) => Ok(Err("non cherché".to_owned())),
            };
            match found {
                Ok(Ok(found)) => {
                    let here: HashSet<&str> = found.iter().map(String::as_str).filter(|p| ids.contains(p)).collect();
                    replicated += usize::from(!here.is_empty());
                    for p in here {
                        *per_peer.entry(p.to_owned()).or_insert(0) += 1;
                    }
                }
                _ => failed += 1,
            }
        }
        let mut list: Vec<Value> = peers.iter().filter_map(|p| per_peer.get(&p.node_id).map(|n| json!({ "node_id": p.node_id, "name": p.label(), "have": n }))).collect();
        list.sort_by(|a, b| b["have"].as_u64().cmp(&a["have"].as_u64()));
        json!({ "files": cids.len(), "replicated": replicated, "lookup_failed": failed, "peers": list })
    }

    /// Boucle : un passage toutes les `every`, après `first`. `on_pass` reçoit le nombre de membres mesurés ou l'erreur :
    /// elle n'est jamais avalée.
    pub async fn run_forever(self: Arc<Self>, first: Duration, every: Duration, on_pass: Arc<dyn Fn(Result<usize, String>) + Send + Sync>, mut stop: tokio::sync::watch::Receiver<bool>) {
        let mut wait = first;
        loop {
            tokio::select! {
                () = tokio::time::sleep(wait) => {}
                changed = stop.changed() => {
                    if changed.is_err() || *stop.borrow() {
                        return;
                    }
                    continue;
                }
            }
            on_pass(self.run_once().await.map_err(|e| e.to_string()));
            wait = every;
        }
    }
}
