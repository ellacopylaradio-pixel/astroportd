//! ISLAND-1 « Ğ1 en différé » : la station a perdu l'accès à Duniter, la chaîne continue sans elle.
//!
//! Règles (détail et validateur pur : [`astro_economy::island`]) :
//! - **aucune création de monnaie** : un MULTIPASS hébergé promet au plus son dernier solde Ğ1 dépensable connu, plus les DU
//!   que la chaîne lui verse pendant la coupure (s'il est membre de la toile de confiance), 30 jours au plus ;
//! - les promesses sont des kinds 30852 `t=island`, signées par la station avec la clé du membre (elle la détient déjà) ;
//! - **au retour de la chaîne**, l'île se ferme et chaque promesse acceptée est **réglée en vraie Ğ1** du portefeuille du
//!   payeur vers celui du destinataire, avec le mémo `UPLANET:ISLAND:<id>` ;
//! - garde-fous du règlement (comme [`crate::settlement`]) : chaque virement est **noté avant l'envoi** ; une issue inconnue
//!   n'est **jamais** rejouée (le Capitaine est prévenu) ; un solde devenu insuffisant (le membre a dépensé ailleurs
//!   pendant la coupure) laisse la promesse « à découvert », retentée au passage suivant, et prévient payeur et Capitaine.
//!
//! L'île s'ouvre à la main ([`Island::open`]) ou seule après `auto_after` sans chaîne (désactivé par défaut) ; elle se
//! ferme seule dès que la chaîne répond. L'instantané des soldes est rafraîchi toutes les 6 h tant que la chaîne répond.

use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use astro_core::{ActivityBus, Audience};
use astro_crypto::KeySource;
use astro_db::{Db, DbError, Statement};
use astro_duniter::{Account, Amount, Chain, ChainError, Ed25519Signer, TransferMode};
use astro_economy::island::{self as rules, Credit, Opening, Verdict};
use astro_lifecycle::dunikey::Dunikey;
use astro_nostr_proto::{Event, UnsignedEvent, kind};
use rusqlite::OptionalExtension;
use rusqlite::types::Value;
use serde_json::{Value as Json, json};
use tokio::sync::{Mutex, watch};

use crate::register::PublishFn;
use crate::station::RESERVE_G1_CENTS;

/// Espace `kv` de l'île : `current` (île ouverte), `down_since` (première panne constatée), `snapshot_at`.
const SCOPE: &str = "island";
/// Intervalle entre deux passages.
const TICK: Duration = Duration::from_secs(5 * 60);
/// Fréquence de l'instantané des soldes tant que la chaîne répond.
const SNAPSHOT_EVERY: u64 = 6 * 3600;

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn int(v: u64) -> Value {
    Value::Integer(i64::try_from(v).unwrap_or(i64::MAX))
}

fn txt(s: &str) -> Value {
    Value::Text(s.to_owned())
}

/// Pourquoi une promesse n'est pas faite.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum IslandFail {
    /// Aucune île ouverte : la chaîne répond, on paie directement en Ğ1.
    #[error("aucune île ouverte : payez directement en Ğ1")]
    Closed,
    /// Le membre n'a pas de crédit dans l'île (pas d'instantané de son portefeuille).
    #[error("pas de crédit d'île pour ce compte")]
    NoCredit,
    /// Fourche constatée : le compte est gelé dans cette île.
    #[error("compte gelé dans cette île (double dépense constatée)")]
    Frozen,
    /// Au-delà du disponible.
    #[error("disponible insuffisant : {have} Ğ1")]
    Short {
        /// Disponible.
        have: Amount,
    },
    /// Entrée invalide.
    #[error("{0}")]
    Invalid(String),
    /// Erreur interne.
    #[error("{0}")]
    Internal(String),
}

impl From<DbError> for IslandFail {
    fn from(e: DbError) -> Self {
        Self::Internal(e.to_string())
    }
}

/// Île ouverte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Current {
    /// Identifiant.
    pub id: String,
    /// Ouverture (secondes Unix).
    pub opened_at: u64,
}

/// Bilan d'un règlement.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Settled {
    /// Promesses réglées en Ğ1.
    pub paid: u32,
    /// Solde insuffisant : retentées plus tard.
    pub short: u32,
    /// Issue inconnue : jamais rejouées.
    pub unknown: u32,
    /// Chaîne injoignable : au passage suivant.
    pub retry: u32,
}

/// Futur rendu par les points d'entrée de [`Deferred`].
pub type BoxFut<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

/// L'île vue du circuit d'envoi habituel (« Envoyer des Ẑen ») : quand elle est ouverte, l'envoi à un proche devient une
/// promesse, sans autre geste du membre ; l'écran l'en avertit.
pub trait Deferred: Send + Sync {
    /// `None` : pas d'île ouverte, le paiement passe normalement par la chaîne. Sinon `{ island, available_g1_cents, frozen, credit }`.
    fn quote<'a>(&'a self, payer: &'a str) -> BoxFut<'a, Result<Option<Json>, IslandFail>>;
    /// Promet `g1_cents` à `to` (portefeuille `to_g1`) ; `req` rend l'appel rejouable.
    fn promise<'a>(&'a self, payer: &'a str, to: &'a str, to_g1: &'a str, g1_cents: u64, req: &'a str) -> BoxFut<'a, Result<Json, IslandFail>>;
}

impl<C: Chain> Deferred for Island<C> {
    fn quote<'a>(&'a self, payer: &'a str) -> BoxFut<'a, Result<Option<Json>, IslandFail>> {
        Box::pin(async move {
            let Some(cur) = self.current().await else { return Ok(None) };
            let at = now();
            let state = self.revalidate(&cur.id, payer, at).await?;
            let (available, frozen) = state.as_ref().map_or((Amount(0), false), |(c, v)| (v.available(c, at), v.frozen));
            Ok(Some(json!({ "island": cur.id, "opened_at": cur.opened_at, "available_g1_cents": available.0, "frozen": frozen, "credit": state.is_some() })))
        })
    }

    fn promise<'a>(&'a self, payer: &'a str, to: &'a str, to_g1: &'a str, g1_cents: u64, req: &'a str) -> BoxFut<'a, Result<Json, IslandFail>> {
        Box::pin(async move { self.pay_once(payer, to, Some(to_g1), Amount(g1_cents), Some(req), now()).await })
    }
}

/// Service d'île de la station.
pub struct Island<C: Chain> {
    db: Arc<Db>,
    chain: Arc<C>,
    accounts_dir: PathBuf,
    keys: Arc<dyn KeySource>,
    node_hex: String,
    node_secret: [u8; 32],
    publish: PublishFn,
    activity: ActivityBus,
    auto_after: Option<u64>,
    /// Une promesse à la fois : deux promesses simultanées du même membre liraient le même `prev` et feraient une fourche.
    pay_lock: Mutex<()>,
}

impl<C: Chain> Island<C> {
    /// Service. `auto_after` : secondes sans chaîne avant l'ouverture automatique (`None` : jamais seule).
    #[allow(clippy::too_many_arguments)]
    pub fn new(db: Arc<Db>, chain: Arc<C>, accounts_dir: PathBuf, keys: Arc<dyn KeySource>, node_hex: String, node_secret: [u8; 32], publish: PublishFn, activity: ActivityBus, auto_after: Option<u64>) -> Self {
        Self { db, chain, accounts_dir, keys, node_hex, node_secret, publish, activity, auto_after, pay_lock: Mutex::new(()) }
    }

    fn alert(&self, text: String) {
        self.activity.publish_to(Audience::Captain, "alert", "island", text, None);
    }

    fn tell(&self, owner: &str, text: String) {
        self.activity.publish_to(Audience::Owner(owner.to_owned()), "economy", "island", text, None);
    }

    async fn kv(&self, key: &'static str) -> Option<String> {
        self.db.readers().read(move |c| Ok(c.query_row("SELECT value FROM kv WHERE scope = ?1 AND key = ?2", [SCOPE, key], |r| r.get::<_, String>(0)).optional()?)).await.ok().flatten()
    }

    async fn set_kv(&self, key: &str, value: Option<String>) -> Result<(), DbError> {
        let stmt = match value {
            Some(v) => Statement::new("INSERT OR REPLACE INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4)", vec![txt(SCOPE), txt(key), Value::Text(v), int(now())]),
            None => Statement::new("DELETE FROM kv WHERE scope = ?1 AND key = ?2", vec![txt(SCOPE), txt(key)]),
        };
        self.db.writer().batch(vec![stmt]).await.map(|_| ())
    }

    /// Délai d'ouverture automatique en vigueur : le réglage du Capitaine (écran Île) s'il existe, sinon l'option de lancement.
    pub async fn auto_after(&self) -> Option<u64> {
        match self.kv("auto_after").await.as_deref() {
            Some("off") => None,
            Some(v) => v.parse().ok(),
            None => self.auto_after,
        }
    }

    /// Règle l'ouverture automatique : `Some(minutes)` (1 à 1440), `None` = jamais seule.
    pub async fn set_auto(&self, minutes: Option<u64>) -> Result<Option<u64>, IslandFail> {
        if minutes.is_some_and(|m| !(1..=1440).contains(&m)) {
            return Err(IslandFail::Invalid("délai : de 1 à 1440 minutes".into()));
        }
        let secs = minutes.map(|m| m * 60);
        self.set_kv("auto_after", Some(secs.map_or_else(|| "off".to_owned(), |s| s.to_string()))).await?;
        self.alert(match minutes {
            Some(m) => format!("Île : ouverture automatique après {m} min sans Duniter"),
            None => "Île : ouverture automatique coupée (seulement à la main)".to_owned(),
        });
        Ok(secs)
    }

    /// Île ouverte, s'il y en a une.
    pub async fn current(&self) -> Option<Current> {
        let v: Json = serde_json::from_str(&self.kv("current").await?).ok()?;
        Some(Current { id: v["id"].as_str()?.to_owned(), opened_at: v["opened_at"].as_u64()? })
    }

    /// MULTIPASS hébergés : (hex, portefeuille Ğ1, email).
    async fn members(&self) -> Result<Vec<(String, String, String)>, DbError> {
        self.db
            .readers()
            .read(|c| {
                let mut s = c.prepare("SELECT hex, g1pub_nostr, email FROM identities WHERE hosted = 1 AND role = 'multipass' AND g1pub_nostr IS NOT NULL AND email IS NOT NULL ORDER BY hex")?;
                Ok(s.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<Result<Vec<_>, _>>()?)
            })
            .await
    }

    /// Instantané Ğ1 de chaque MULTIPASS hébergé : dépensable (réserve déduite), DU s'il est membre, fin d'adhésion.
    /// Une erreur de connexion arrête tout (la chaîne ne répond pas) ; rien n'est écrit pour les comptes non lus.
    pub async fn snapshot(&self, at: u64) -> Result<usize, ChainError> {
        let mut rows = Vec::new();
        for (hex, wallet, _) in self.members().await.map_err(|e| ChainError::Invalid(e.to_string()))? {
            let Ok(account) = Account::from_str(&wallet) else { continue };
            let spendable = self.chain.balance(&account).await?.spendable(Amount(RESERVE_G1_CENTS));
            let (ud, period, until) = match self.chain.ud_status(&account).await {
                Ok(u) if u.member && u.period_secs > 0 => (u.ud, u.period_secs, at.saturating_add(u.membership_left_secs.unwrap_or(0))),
                Ok(_) | Err(ChainError::Invalid(_)) => (Amount(0), 0, at),
                Err(e) => return Err(e),
            };
            rows.push(Statement::new(
                "INSERT OR REPLACE INTO g1_snapshot(hex, wallet, spendable, ud, ud_period, ud_until, at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                vec![txt(&hex), Value::Text(account.ss58()), int(spendable.0), int(ud.0), int(period), int(until), int(at)],
            ));
        }
        let n = rows.len();
        rows.push(Statement::new("INSERT OR REPLACE INTO kv(scope, key, value, updated_at) VALUES (?1, 'snapshot_at', ?2, ?2)", vec![txt(SCOPE), int(at)]));
        self.db.writer().batch(rows).await.map_err(|e| ChainError::Invalid(e.to_string()))?;
        Ok(n)
    }

    /// Ouvre l'île (idempotent) : publie, signée par le NODE, l'ouverture de crédit de chaque MULTIPASS qui a un instantané.
    pub async fn open(&self, at: u64) -> Result<Current, IslandFail> {
        if let Some(c) = self.current().await {
            return Ok(c);
        }
        let id = rules::island_id(&self.node_hex, at);
        let snaps: Vec<(String, String, u64, u64, u64, u64, u64)> = self
            .db
            .readers()
            .read(|c| {
                let mut s = c.prepare("SELECT hex, wallet, spendable, ud, ud_period, ud_until, at FROM g1_snapshot ORDER BY hex")?;
                Ok(s.query_map([], |r| {
                    let u = |i: usize| r.get::<_, i64>(i).map(|v| u64::try_from(v).unwrap_or(0));
                    Ok((r.get(0)?, r.get(1)?, u(2)?, u(3)?, u(4)?, u(5)?, u(6)?))
                })?
                .collect::<Result<Vec<_>, _>>()?)
            })
            .await?;
        if snaps.is_empty() {
            return Err(IslandFail::Invalid("aucun instantané des soldes : l'île ne peut rien promettre".into()));
        }
        let mut opened = 0;
        for (member, g1, spendable, ud, ud_period, ud_until, snap_at) in snaps {
            let credit = Credit { opening: Amount(spendable), ud: Amount(ud), ud_period, from: snap_at, ud_until };
            let o = Opening { island: id.clone(), node: self.node_hex.clone(), member, g1, credit };
            let e = UnsignedEvent { created_at: at, kind: kind::ISLAND_CREDIT, tags: o.tags(), content: String::new() }.sign(&self.node_secret).map_err(|e| IslandFail::Internal(e.to_string()))?;
            self.store_and_publish(e).await?;
            opened += 1;
        }
        let cur = Current { id: id.clone(), opened_at: at };
        self.set_kv("current", Some(json!({ "id": id, "opened_at": at }).to_string())).await?;
        self.alert(format!("Île ouverte ({id}) : Duniter injoignable ; {opened} MULTIPASS peuvent promettre des Ğ1, réglées au retour de la chaîne"));
        Ok(cur)
    }

    /// Ferme l'île : plus de nouvelles promesses ; celles acceptées seront réglées dès que la chaîne répond.
    pub async fn close(&self) -> Result<Option<Current>, IslandFail> {
        let Some(c) = self.current().await else { return Ok(None) };
        self.set_kv("current", None).await?;
        self.alert(format!("Île fermée ({}) : les promesses acceptées vont être réglées en Ğ1", c.id));
        Ok(Some(c))
    }

    /// Stocke localement (projection comprise) puis publie : la promesse suivante voit toujours la précédente.
    async fn store_and_publish(&self, e: Event) -> Result<(), IslandFail> {
        astro_sync::ingest_event(self.db.writer(), e.clone(), "island").await.map_err(|err| IslandFail::Internal(err.to_string()))?;
        if let Err(err) = (self.publish)(e).await {
            self.alert(format!("île : publication vers les relais impossible ({err}) ; l'événement reste ici"));
        }
        Ok(())
    }

    async fn credit_of(&self, island: &str, member: &str) -> Result<Option<Credit>, DbError> {
        let (i, m) = (island.to_owned(), member.to_owned());
        self.db
            .readers()
            .read(move |c| {
                Ok(c.query_row("SELECT opening, ud, ud_period, ud_until, at FROM island_credits WHERE island = ?1 AND member_hex = ?2", [i, m], |r| {
                    let u = |n: usize| r.get::<_, i64>(n).map(|v| u64::try_from(v).unwrap_or(0));
                    Ok(Credit { opening: Amount(u(0)?), ud: Amount(u(1)?), ud_period: u(2)?, ud_until: u(3)?, from: u(4)? })
                })
                .optional()?)
            })
            .await
    }

    async fn promises_of(&self, island: &str, author: &str) -> Result<Vec<Event>, DbError> {
        let (i, a) = (island.to_owned(), author.to_owned());
        let events = self
            .db
            .readers()
            .read(move |c| {
                let mut s = c.prepare(
                    "SELECT e.id, e.pubkey, e.kind, e.created_at, e.content, e.sig, e.tags FROM nostr_events e \
                     WHERE e.kind = ?1 AND e.pubkey = ?2 AND EXISTS (SELECT 1 FROM nostr_tags t WHERE t.event_id = e.seq AND t.name = 't' AND t.value = ?3)",
                )?;
                let mut rows = s.query(rusqlite::params![kind::LEDGER_N2, a, rules::TAG])?;
                let mut out = Vec::new();
                while let Some(r) = rows.next()? {
                    out.push(astro_db::events::row_to_event(r)?);
                }
                Ok(out)
            })
            .await?;
        Ok(events.into_iter().filter(|e| rules::island_of(e) == Some(i.as_str())).collect())
    }

    /// Recalcule l'état de toutes les promesses de `author` dans `island`, depuis leur ensemble.
    pub async fn revalidate(&self, island: &str, author: &str, at: u64) -> Result<Option<(Credit, Verdict)>, DbError> {
        let events = self.promises_of(island, author).await?;
        let Some(credit) = self.credit_of(island, author).await? else {
            let stmts = events.iter().map(|e| Statement::new("UPDATE island_promises SET state = 'refused', reason = ?2 WHERE event_id = ?1", vec![txt(&e.id), txt("pas de crédit ouvert pour ce payeur")])).collect::<Vec<_>>();
            if !stmts.is_empty() {
                self.db.writer().batch(stmts).await?;
            }
            return Ok(None);
        };
        let v = rules::validate(author, island, &credit, &events, at);
        let mut stmts: Vec<Statement> = v.accepted.iter().map(|a| Statement::new("UPDATE island_promises SET state = 'accepted', reason = NULL WHERE event_id = ?1", vec![txt(&a.tx.id)])).collect();
        stmts.extend(v.rejected.iter().map(|(id, why)| Statement::new("UPDATE island_promises SET state = 'refused', reason = ?2 WHERE event_id = ?1", vec![txt(id), Value::Text(why.to_string())])));
        if !stmts.is_empty() {
            self.db.writer().batch(stmts).await?;
        }
        Ok(Some((credit, v)))
    }

    /// Recalcule toutes les promesses connues (après un replay, ou à chaque passage).
    pub async fn revalidate_all(&self, at: u64) -> Result<usize, DbError> {
        let pairs: Vec<(String, String)> = self.db.readers().read(|c| Ok(c.prepare("SELECT DISTINCT island, author FROM island_promises")?.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<Vec<_>, _>>()?)).await?;
        for (island, author) in &pairs {
            self.revalidate(island, author, at).await?;
        }
        Ok(pairs.len())
    }

    /// Ce que `member` peut encore promettre (île ouverte ici), et ses promesses envoyées et reçues, **île ouverte ou non** :
    /// un membre d'une station restée connectée voit aussi ce qu'on lui a promis ailleurs. Chaque promesse dit quelle station
    /// la garantit (`node`) et si cette station est connue ici (`known_station`) : une clé inconnue peut s'inventer une île.
    pub async fn account(&self, member: &str, at: u64) -> Result<Json, IslandFail> {
        let cur = self.current().await;
        let state = match &cur {
            Some(c) => self.revalidate(&c.id, member, at).await?,
            None => None,
        };
        let m = member.to_owned();
        let list = self
            .db
            .readers()
            .read(move |c| {
                let mut s = c.prepare(
                    "SELECT p.event_id, p.author, p.dest, p.amount, p.created_at, p.state, p.reason, s.state, k.node_hex, \
                       EXISTS (SELECT 1 FROM identities n WHERE n.hex = k.node_hex AND n.role = 'node') \
                     FROM island_promises p LEFT JOIN island_settle s ON s.event_id = p.event_id \
                     LEFT JOIN island_credits k ON k.island = p.island AND k.member_hex = p.author \
                     WHERE p.author = ?1 OR p.dest = ?1 ORDER BY p.created_at DESC LIMIT 200",
                )?;
                Ok(s.query_map([&m], |r| {
                    let author: String = r.get(1)?;
                    Ok(json!({ "id": r.get::<_, String>(0)?, "sent": author == m, "author": author, "dest": r.get::<_, String>(2)?, "amount": Amount(u64::try_from(r.get::<_, i64>(3)?).unwrap_or(0)).to_string(), "amount_zen": Amount(u64::try_from(r.get::<_, i64>(3)?).unwrap_or(0).saturating_mul(crate::station::ZEN_PER_G1)).to_string(),
                        "at": r.get::<_, i64>(4)?, "state": r.get::<_, String>(5)?, "reason": r.get::<_, Option<String>>(6)?, "settled": r.get::<_, Option<String>>(7)?,
                        "node": r.get::<_, Option<String>>(8)?, "known_station": r.get::<_, bool>(9)? }))
                })?
                .collect::<Result<Vec<_>, _>>()?)
            })
            .await?;
        let (available, frozen) = state.as_ref().map_or((Amount(0), false), |(c, v)| (v.available(c, at), v.frozen));
        Ok(json!({ "open": cur.is_some(), "island": cur.as_ref().map(|c| c.id.clone()), "opened_at": cur.map(|c| c.opened_at), "credit": state.is_some(), "available": available.to_string(), "available_zen": Amount(available.0.saturating_mul(crate::station::ZEN_PER_G1)).to_string(), "frozen": frozen, "promises": list }))
    }

    /// Promet `amount` Ğ1 de `payer` (MULTIPASS hébergé ici) à `dest` (clé Nostr), réglés plus tard sur `dest_g1`.
    /// Portefeuille Ğ1 connu d'un MULTIPASS (hébergé ici, ou ailleurs par son identité DID).
    async fn wallet_of(&self, hex: &str) -> Result<Option<String>, DbError> {
        let h = hex.to_owned();
        self.db
            .readers()
            .read(move |c| Ok(c.query_row("SELECT g1pub_nostr FROM identities WHERE hex = ?1 AND role IN ('multipass', 'captain') AND g1pub_nostr IS NOT NULL", [h], |r| r.get::<_, String>(0)).optional()?))
            .await
    }

    /// Ouverture de crédit (kind 30854) du membre dans l'île, telle que signée par le NODE.
    async fn opening_event(&self, island: &str, member: &str) -> Result<Option<Event>, DbError> {
        let d = format!("{island}:{member}");
        let node = self.node_hex.clone();
        self.db
            .readers()
            .read(move |c| {
                let mut s = c.prepare("SELECT id, pubkey, kind, created_at, content, sig, tags FROM nostr_events WHERE kind = ?1 AND pubkey = ?2 AND d_tag = ?3 ORDER BY created_at DESC LIMIT 1")?;
                let mut rows = s.query(rusqlite::params![kind::ISLAND_CREDIT, node, d])?;
                match rows.next()? {
                    Some(r) => Ok(Some(astro_db::events::row_to_event(r)?)),
                    None => Ok(None),
                }
            })
            .await
    }

    /// Promet `amount` Ğ1 de `payer` (MULTIPASS hébergé ici) à `dest` (clé Nostr). `dest_g1` : portefeuille où arrivera le
    /// règlement ; absent, celui que la constellation connaît pour `dest`. La promesse **embarque l'ouverture de crédit**
    /// signée par le NODE : la station du destinataire, même coupée de celle-ci, vérifie seule le crédit du payeur.
    pub async fn pay(&self, payer: &str, dest: &str, dest_g1: Option<&str>, amount: Amount, at: u64) -> Result<Json, IslandFail> {
        self.pay_once(payer, dest, dest_g1, amount, None, at).await
    }

    /// Comme [`Self::pay`], **rejouable** : la même demande `req` (identifiant de versement de l'écran) rend la même promesse,
    /// sans jamais en faire une seconde.
    pub async fn pay_once(&self, payer: &str, dest: &str, dest_g1: Option<&str>, amount: Amount, req: Option<&str>, at: u64) -> Result<Json, IslandFail> {
        let cur = self.current().await.ok_or(IslandFail::Closed)?;
        if !astro_nostr_proto::is_hex64(dest) || dest == payer {
            return Err(IslandFail::Invalid("destinataire invalide".into()));
        }
        let known = match dest_g1 {
            Some(g) => g.to_owned(),
            None => self.wallet_of(dest).await?.ok_or_else(|| IslandFail::Invalid("portefeuille Ğ1 du destinataire inconnu de cette station".into()))?,
        };
        let dest_g1 = Account::from_str(&known).map_err(|e| IslandFail::Invalid(e.to_string()))?.ss58();
        if amount == Amount(0) {
            return Err(IslandFail::Invalid("montant nul".into()));
        }
        let _one_at_a_time = self.pay_lock.lock().await;
        let req_key = req.map(|r| format!("req:{payer}:{}", r.chars().take(64).collect::<String>()));
        if let Some(k) = &req_key {
            let k = k.clone();
            let done: Option<String> = self.db.readers().read(move |c| Ok(c.query_row("SELECT value FROM kv WHERE scope = ?1 AND key = ?2", [SCOPE, k.as_str()], |r| r.get(0)).optional()?)).await?;
            if let Some(v) = done.and_then(|d| serde_json::from_str::<Json>(&d).ok()) {
                return Ok(v);
            }
        }
        let (credit, v) = self.revalidate(&cur.id, payer, at).await?.ok_or(IslandFail::NoCredit)?;
        if v.frozen {
            return Err(IslandFail::Frozen);
        }
        let have = v.available(&credit, at);
        if amount > have {
            return Err(IslandFail::Short { have });
        }
        let secret = self.keys.secret(payer).ok_or_else(|| IslandFail::Internal("clé du compte introuvable sur cette station".into()))?;
        let d = format!("{}-{}", cur.id, v.accepted.len() + 1);
        let tags = rules::promise_tags(&cur.id, &d, dest, &dest_g1, amount, &v.next_prev());
        let content = self.opening_event(&cur.id, payer).await?.map(|o| serde_json::to_string(&o).unwrap_or_default()).unwrap_or_default();
        let e = UnsignedEvent { created_at: at.max(credit.from), kind: kind::LEDGER_N2, tags, content }.sign(&secret).map_err(|e| IslandFail::Internal(e.to_string()))?;
        let id = e.id.clone();
        self.store_and_publish(e).await?;
        let (_, after) = self.revalidate(&cur.id, payer, at).await?.ok_or(IslandFail::NoCredit)?;
        if !after.accepted.iter().any(|a| a.tx.id == id) {
            return Err(IslandFail::Internal("promesse refusée à la relecture".into()));
        }
        self.tell(payer, format!("Promesse de {amount} Ğ1 enregistrée : elle sera réglée en Ğ1 au retour de la chaîne"));
        let out = json!({ "id": id, "island": cur.id, "amount": amount.to_string(), "available": after.available(&credit, at).to_string() });
        if let Some(k) = req_key {
            self.set_kv(&k, Some(out.to_string())).await?;
        }
        Ok(out)
    }

    fn signer_of(&self, email: &str, wallet: &str) -> Option<Ed25519Signer> {
        let dk = std::fs::read_to_string(self.accounts_dir.join(email).join(".secret.dunikey")).ok().and_then(|t| Dunikey::parse(&t).ok())?;
        let signer = Ed25519Signer::from_seed(*dk.ed25519_seed());
        (Account::from_str(wallet).ok() == Some(signer.account())).then_some(signer)
    }

    /// Règle en vraie Ğ1 les promesses acceptées des MULTIPASS hébergés ici, hors île ouverte, dans l'ordre de chaque chaîne.
    pub async fn settle(&self, at: u64) -> Result<Settled, DbError> {
        let open = self.current().await.map(|c| c.id).unwrap_or_default();
        let todo: Vec<(String, String, String, String, u64, String, String)> = self
            .db
            .readers()
            .read(move |c| {
                let mut s = c.prepare(
                    "SELECT p.event_id, p.island, p.author, p.dest_g1, p.amount, i.email, k.g1 FROM island_promises p \
                     JOIN identities i ON i.hex = p.author AND i.hosted = 1 AND i.role = 'multipass' AND i.email IS NOT NULL \
                     JOIN island_credits k ON k.island = p.island AND k.member_hex = p.author \
                     LEFT JOIN island_settle s ON s.event_id = p.event_id \
                     WHERE p.state = 'accepted' AND p.island <> ?1 AND (s.state IS NULL OR s.state = 'short') \
                     ORDER BY p.island, p.author, p.created_at, p.event_id",
                )?;
                Ok(s.query_map([&open], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, u64::try_from(r.get::<_, i64>(4)?).unwrap_or(0), r.get(5)?, r.get(6)?)))?.collect::<Result<Vec<_>, _>>()?)
            })
            .await?;
        let mut report = Settled::default();
        // Un envoi resté « en cours » (arrêt pendant l'envoi) ou d'issue inconnue bloque son payeur : jamais rejoué, jamais dépassé.
        let mut stuck: std::collections::HashSet<String> = self
            .db
            .readers()
            .read(|c| {
                let mut s = c.prepare("SELECT DISTINCT p.author FROM island_settle s JOIN island_promises p ON p.event_id = s.event_id WHERE s.state IN ('sending', 'unknown')")?;
                Ok(s.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<_, _>>()?)
            })
            .await?;
        for (id, island, author, dest_g1, cents, email, wallet) in todo {
            if stuck.contains(&author) {
                continue; // l'ordre de la chaîne : une promesse non réglée bloque les suivantes du même payeur
            }
            let who = email.split('@').next().unwrap_or(&email).to_owned();
            let (Some(signer), Ok(dest)) = (self.signer_of(&email, &wallet), Account::from_str(&dest_g1)) else {
                self.alert(format!("île {island} : promesse de {who} non réglable ici (clé Ğ1 illisible ou destinataire invalide)"));
                stuck.insert(author);
                continue;
            };
            let amount = Amount(cents);
            match self.chain.balance(&signer.account()).await {
                Ok(b) if b.spendable(Amount(RESERVE_G1_CENTS)) >= amount => {}
                Ok(b) => {
                    self.mark(&id, "short", at, &format!("dépensable {}", b.spendable(Amount(RESERVE_G1_CENTS)))).await?;
                    self.tell(&author, format!("Une promesse de {amount} Ğ1 faite pendant la coupure ne peut pas encore être réglée : approvisionnez votre portefeuille"));
                    self.alert(format!("île {island} : promesse de {who} ({amount} Ğ1) à découvert, retentée plus tard"));
                    report.short += 1;
                    stuck.insert(author);
                    continue;
                }
                Err(_) => {
                    report.retry += 1;
                    return Ok(report);
                }
            }
            // Noté AVANT l'envoi : un redémarrage pendant l'envoi ne rejoue jamais ce virement.
            self.mark(&id, "sending", at, "").await?;
            let memo = format!("UPLANET:ISLAND:{}", &id[..16]);
            match self.chain.transfer(&signer, &dest, amount, TransferMode::KeepAlive, Some(&memo)).await {
                Ok(out) => {
                    self.mark(&id, "paid", at, &hex::encode(out.block_hash)).await?;
                    self.tell(&author, format!("Promesse de {amount} Ğ1 réglée sur la Ğ1"));
                    report.paid += 1;
                }
                Err(ChainError::Connection(_)) => {
                    self.unmark(&id).await?; // rien n'est parti : rejouable
                    report.retry += 1;
                    return Ok(report);
                }
                Err(ChainError::Rejected(why) | ChainError::Invalid(why)) => {
                    self.mark(&id, "short", at, &why).await?;
                    self.alert(format!("île {island} : règlement de {who} refusé par la chaîne ({why}), retenté plus tard"));
                    report.short += 1;
                    stuck.insert(author);
                }
                Err(ChainError::UnknownOutcome(why)) => {
                    self.mark(&id, "unknown", at, &why).await?;
                    self.alert(format!("île {island} : règlement de {who} ({amount} Ğ1) d'issue inconnue ({why}) : vérifiez le portefeuille avant toute reprise, il ne sera pas rejoué"));
                    report.unknown += 1;
                    stuck.insert(author);
                }
            }
        }
        Ok(report)
    }

    async fn mark(&self, id: &str, state: &str, at: u64, detail: &str) -> Result<(), DbError> {
        self.db.writer().batch(vec![Statement::new("INSERT OR REPLACE INTO island_settle(event_id, state, at, detail) VALUES (?1, ?2, ?3, ?4)", vec![txt(id), txt(state), int(at), txt(detail)])]).await.map(|_| ())
    }

    async fn unmark(&self, id: &str) -> Result<(), DbError> {
        self.db.writer().batch(vec![Statement::new("DELETE FROM island_settle WHERE event_id = ?1 AND state = 'sending'", vec![txt(id)])]).await.map(|_| ())
    }

    /// Un passage : sonde la chaîne ; si elle répond, instantané (toutes les 6 h), fermeture de l'île et règlement ;
    /// sinon, ouverture automatique après `auto_after`. Les promesses sont recalculées à chaque passage.
    pub async fn tick(&self, at: u64) {
        let members = self.members().await.unwrap_or_default();
        let probe = match members.iter().find_map(|(_, w, _)| Account::from_str(w).ok()) {
            Some(a) => self.chain.balance(&a).await.map(|_| ()),
            None => Err(ChainError::Invalid("aucun portefeuille à sonder".into())),
        };
        match probe {
            Ok(()) => {
                let _ = self.set_kv("down_since", None).await;
                let last: u64 = self.kv("snapshot_at").await.and_then(|v| v.parse().ok()).unwrap_or(0);
                if at.saturating_sub(last) >= SNAPSHOT_EVERY {
                    if let Err(e) = self.snapshot(at).await {
                        self.alert(format!("île : instantané des soldes Ğ1 impossible ({e})"));
                    }
                }
                if let Err(e) = self.close().await {
                    self.alert(format!("île : fermeture impossible ({e})"));
                }
                if let Err(e) = self.revalidate_all(at).await {
                    self.alert(format!("île : recalcul des promesses impossible ({e})"));
                }
                match self.settle(at).await {
                    Ok(r) if r.paid > 0 => {
                        self.activity.publish("economy", "island", format!("île : {} promesse(s) réglée(s) en Ğ1", r.paid), None);
                    }
                    Ok(_) => {}
                    Err(e) => self.alert(format!("île : règlement impossible ({e})")),
                };
            }
            Err(ChainError::Connection(_)) => {
                let since: u64 = match self.kv("down_since").await.and_then(|v| v.parse().ok()) {
                    Some(s) => s,
                    None => {
                        let _ = self.set_kv("down_since", Some(at.to_string())).await;
                        at
                    }
                };
                if let Some(after) = self.auto_after().await {
                    if at.saturating_sub(since) >= after && self.current().await.is_none() {
                        if let Err(e) = self.open(at).await {
                            self.alert(format!("île : ouverture automatique impossible ({e})"));
                        }
                    }
                }
                let _ = self.revalidate_all(at).await;
            }
            Err(_) => {
                let _ = self.revalidate_all(at).await;
            }
        }
    }

    /// État de l'île pour le Capitaine : ouverte ou non, fraîcheur de l'instantané, panne en cours, promesses et règlements.
    pub async fn status(&self, at: u64) -> Result<Json, IslandFail> {
        let cur = self.current().await;
        let num = |v: Option<String>| v.and_then(|s| s.parse::<u64>().ok());
        let (snapshot_at, down_since) = (num(self.kv("snapshot_at").await), num(self.kv("down_since").await));
        let node = self.node_hex.clone();
        let counts = self
            .db
            .readers()
            .read(move |c| {
                let group = |sql: &str| -> Result<serde_json::Map<String, Json>, DbError> {
                    let mut s = c.prepare(sql)?;
                    Ok(s.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?.collect::<Result<Vec<_>, _>>()?.into_iter().map(|(k, n)| (k, json!(n))).collect())
                };
                let members: i64 = c.query_row("SELECT count(*) FROM g1_snapshot", [], |r| r.get(0))?;
                // Autres stations vues en île (la coupure en touche souvent plusieurs) : leurs crédits arrivent avec leurs promesses.
                let mut s = c.prepare(
                    "SELECT k.island, k.node_hex, count(*), max(k.at), EXISTS (SELECT 1 FROM identities n WHERE n.hex = k.node_hex AND n.role = 'node'), \
                       (SELECT count(*) FROM island_promises p WHERE p.island = k.island) \
                     FROM island_credits k WHERE k.node_hex <> ?1 GROUP BY k.island ORDER BY max(k.at) DESC LIMIT 50",
                )?;
                let neighbors: Vec<Json> = s
                    .query_map([&node], |r| Ok(json!({ "island": r.get::<_, String>(0)?, "node": r.get::<_, String>(1)?, "members": r.get::<_, i64>(2)?, "at": r.get::<_, i64>(3)?, "known_station": r.get::<_, bool>(4)?, "promises": r.get::<_, i64>(5)? })))?
                    .collect::<Result<_, _>>()?;
                Ok((members, group("SELECT state, count(*) FROM island_promises GROUP BY state")?, group("SELECT state, count(*) FROM island_settle GROUP BY state")?, neighbors))
            })
            .await?;
        Ok(json!({
            "open": cur.is_some(), "island": cur.as_ref().map(|c| c.id.clone()), "opened_at": cur.map(|c| c.opened_at),
            "snapshot_at": snapshot_at, "snapshot_members": counts.0, "down_since": down_since,
            "auto_after_secs": self.auto_after().await, "promises": counts.1, "settlements": counts.2, "neighbors": counts.3, "at": at,
        }))
    }

    /// Point d'entrée de l'API : `me` et `pay` pour le membre `hex` ; `status`, `open`, `close` pour le Capitaine (le contrôle
    /// d'accès est fait par la route). `pay` : `{ to: <hex>, g1: <adresse Ğ1 du destinataire>, amount: "12.34" }`.
    pub async fn handle(&self, op: &str, hex: &str, body: &Json) -> Result<Json, IslandFail> {
        let at = now();
        let field = |k: &str| body[k].as_str().map(str::trim).filter(|v| !v.is_empty()).ok_or_else(|| IslandFail::Invalid(format!("« {k} » manquant")));
        match op {
            "me" => self.account(hex, at).await,
            "pay" => {
                let amount: Amount = field("amount")?.parse().map_err(|_| IslandFail::Invalid("montant : nombre de Ğ1, deux décimales au plus".into()))?;
                self.pay(hex, field("to")?, field("g1").ok(), amount, at).await
            }
            "status" => self.status(at).await,
            "open" => self.open(at).await.map(|c| json!({ "open": true, "island": c.id, "opened_at": c.opened_at })),
            "close" => self.close().await.map(|c| json!({ "open": false, "closed": c.map(|c| c.id) })),
            "auto" => {
                let minutes = match &body["minutes"] {
                    Json::Null => None,
                    v => Some(v.as_u64().ok_or_else(|| IslandFail::Invalid("minutes : entier, ou null pour couper".into()))?),
                };
                self.set_auto(minutes).await.map(|s| json!({ "auto_after_secs": s }))
            }
            _ => Err(IslandFail::Invalid("opération inconnue".into())),
        }
    }

    /// Boucle de fond.
    pub async fn run(self: Arc<Self>, mut stop: watch::Receiver<bool>) {
        loop {
            self.tick(now()).await;
            tokio::select! {
                () = tokio::time::sleep(TICK) => {}
                _ = stop.changed() => return,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    use astro_crypto::StaticKeySource;
    use astro_duniter::{Balance, TransferEvent, TxOutcome, UdStatus};
    use astro_nostr_proto::pubkey_of;
    use tokio::sync::mpsc;

    use super::*;

    const T0: u64 = 1_800_000_000;
    const DAY: u64 = 86_400;

    /// Fausse chaîne : soldes en mémoire, peut tomber en panne, enregistre les virements.
    #[derive(Default)]
    struct Fake {
        balances: StdMutex<HashMap<Account, u64>>,
        down: AtomicBool,
        unknown: AtomicBool,
        sent: StdMutex<Vec<(Account, Account, u64, String)>>,
    }

    impl Chain for Fake {
        async fn balance(&self, who: &Account) -> Result<Balance, ChainError> {
            if self.down.load(Ordering::SeqCst) {
                return Err(ChainError::Connection("coupé".into()));
            }
            let b = Amount(self.balances.lock().unwrap().get(who).copied().unwrap_or(0));
            Ok(Balance { total: b, transferable: b, unclaimed_uds: Amount(0) })
        }

        async fn transfer(&self, from: &Ed25519Signer, to: &Account, amount: Amount, _: TransferMode, memo: Option<&str>) -> Result<TxOutcome, ChainError> {
            if self.down.load(Ordering::SeqCst) {
                return Err(ChainError::Connection("coupé".into()));
            }
            if self.unknown.load(Ordering::SeqCst) {
                return Err(ChainError::UnknownOutcome("délai".into()));
            }
            let mut b = self.balances.lock().unwrap();
            let have = b.get(&from.account()).copied().unwrap_or(0);
            *b.entry(from.account()).or_default() = have - amount.0;
            *b.entry(*to).or_default() += amount.0;
            self.sent.lock().unwrap().push((from.account(), *to, amount.0, memo.unwrap_or_default().to_owned()));
            Ok(TxOutcome { block_hash: [1; 32], transferred: amount })
        }

        async fn watch_transfers(&self, _: mpsc::Sender<TransferEvent>) -> Result<(), ChainError> {
            Ok(())
        }

        fn existential_deposit(&self) -> Amount {
            Amount(100)
        }

        async fn ud_status(&self, _: &Account) -> Result<UdStatus, ChainError> {
            if self.down.load(Ordering::SeqCst) {
                return Err(ChainError::Connection("coupé".into()));
            }
            Ok(UdStatus { member: true, ud: Amount(1_100), period_secs: DAY, membership_left_secs: Some(10 * DAY) })
        }
    }

    struct World {
        dir: PathBuf,
        island: Arc<Island<Fake>>,
        chain: Arc<Fake>,
        alice: String,
        alice_g1: Account,
        bob: String,
        bob_g1: Account,
    }

    const ALICE_NOSTR: [u8; 32] = [2; 32];
    const ALICE_G1_SEED: [u8; 32] = [0x2a; 32];

    async fn world(tag: &str) -> World {
        let dir = std::env::temp_dir().join(format!("astroportd-island-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("accounts/alice@example.org")).unwrap();
        let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
        let (node_secret, alice, bob) = ([1u8; 32], pubkey_of(&ALICE_NOSTR).unwrap(), pubkey_of(&[3; 32]).unwrap());
        let alice_g1 = Ed25519Signer::from_seed(ALICE_G1_SEED).account();
        let bob_g1 = Ed25519Signer::from_seed([0x3b; 32]).account();
        std::fs::write(dir.join("accounts/alice@example.org/.secret.dunikey"), format!("Type: PubSec\nVersion: 1\npub: {}\nsec: {}\n", alice_g1.v1(), Account(ALICE_G1_SEED).v1() /* la graine seule, en base58 */)).unwrap();
        db.writer()
            .execute("INSERT INTO identities(hex, role, email, g1pub_nostr, hosted) VALUES (?1, 'multipass', 'alice@example.org', ?2, 1)", vec![txt(&alice), Value::Text(alice_g1.ss58())])
            .await
            .unwrap();
        let chain = Arc::new(Fake::default());
        chain.balances.lock().unwrap().insert(alice_g1, 2_100); // 21 Ğ1, dont 1 Ğ1 de réserve : 20 Ğ1 dépensables
        let keys = StaticKeySource::new().with(ALICE_NOSTR);
        let publish: PublishFn = Arc::new(|_| Box::pin(async { Ok(()) }));
        let island = Arc::new(Island::new(db, chain.clone(), dir.join("accounts"), Arc::new(keys), pubkey_of(&node_secret).unwrap(), node_secret, publish, ActivityBus::new(), Some(3600)));
        World { dir, island, chain, alice, alice_g1, bob, bob_g1 }
    }

    #[tokio::test]
    async fn a_cut_opens_the_island_promises_stay_within_balance_plus_ud_and_are_settled_on_return() {
        let w = world("cycle").await;
        let isl = &w.island;
        // La chaîne répond : instantané pris, aucune île.
        isl.tick(T0).await;
        assert_eq!(isl.current().await, None);
        assert_eq!(isl.pay(&w.alice, &w.bob, Some(&w.bob_g1.ss58()), Amount(100), T0).await, Err(IslandFail::Closed));
        // Coupure : pas d'île avant le délai, puis ouverture automatique.
        w.chain.down.store(true, Ordering::SeqCst);
        isl.tick(T0 + 60).await;
        assert_eq!(isl.current().await, None, "pas avant une heure de coupure");
        isl.tick(T0 + 60 + 3600).await;
        let cur = isl.current().await.unwrap(); // île ouverte
        let t = cur.opened_at;
        // 20 Ğ1 dépensables : 15 passent, 6 de plus non.
        isl.pay(&w.alice, &w.bob, Some(&w.bob_g1.ss58()), Amount(1_500), t + 10).await.unwrap();
        assert_eq!(isl.pay(&w.alice, &w.bob, Some(&w.bob_g1.ss58()), Amount(600), t + 20).await, Err(IslandFail::Short { have: Amount(500) }));
        // Le lendemain de l'instantané, le DU du jour (11 Ğ1) s'ajoute.
        isl.pay(&w.alice, &w.bob, Some(&w.bob_g1.ss58()), Amount(1_600), T0 + DAY + 10).await.unwrap();
        let acc = isl.account(&w.alice, T0 + DAY + 20).await.unwrap();
        assert_eq!(acc["available"], "0.00");
        assert_eq!(acc["promises"].as_array().unwrap().len(), 2);
        let seen_by_bob = isl.account(&w.bob, T0 + DAY + 20).await.unwrap();
        assert!(seen_by_bob["promises"].as_array().unwrap().iter().all(|p| p["sent"] == false && p["state"] == "accepted"));
        // Rien n'est réglé pendant la coupure.
        assert!(w.chain.sent.lock().unwrap().is_empty());
        // Retour de la chaîne : l'île se ferme ; la chaîne a versé le DU (ici on le crédite à la main) ; tout est réglé.
        w.chain.down.store(false, Ordering::SeqCst);
        *w.chain.balances.lock().unwrap().get_mut(&w.alice_g1).unwrap() += 1_100;
        isl.tick(T0 + DAY + 100).await;
        assert_eq!(isl.current().await, None);
        let sent = w.chain.sent.lock().unwrap().clone();
        assert_eq!(sent.iter().map(|s| (s.0, s.1, s.2)).collect::<Vec<_>>(), [(w.alice_g1, w.bob_g1, 1_500), (w.alice_g1, w.bob_g1, 1_600)]);
        assert!(sent.iter().all(|s| s.3.starts_with("UPLANET:ISLAND:")));
        // Un second passage ne règle rien deux fois.
        isl.tick(T0 + DAY + 200).await;
        assert_eq!(w.chain.sent.lock().unwrap().len(), 2);
        let _ = std::fs::remove_dir_all(&w.dir);
    }

    #[tokio::test]
    async fn a_member_who_spent_elsewhere_is_short_and_an_unknown_outcome_is_never_replayed() {
        let w = world("short").await;
        let isl = &w.island;
        isl.snapshot(T0).await.unwrap();
        let cur = isl.open(T0 + 10).await.unwrap();
        isl.pay(&w.alice, &w.bob, Some(&w.bob_g1.ss58()), Amount(1_000), cur.opened_at + 1).await.unwrap();
        isl.pay(&w.alice, &w.bob, Some(&w.bob_g1.ss58()), Amount(500), cur.opened_at + 2).await.unwrap();
        isl.close().await.unwrap();
        // Pendant la coupure, Alice a vidé son portefeuille ailleurs : rien n'est réglé, la promesse est « à découvert ».
        w.chain.balances.lock().unwrap().insert(w.alice_g1, 150);
        let r = isl.settle(T0 + 100).await.unwrap();
        assert_eq!((r.paid, r.short), (0, 1), "la seconde attend la première (ordre de la chaîne)");
        // Réapprovisionnée, mais la chaîne perd la réponse du premier virement : jamais rejoué.
        w.chain.balances.lock().unwrap().insert(w.alice_g1, 5_000);
        w.chain.unknown.store(true, Ordering::SeqCst);
        let r = isl.settle(T0 + 200).await.unwrap();
        assert_eq!((r.paid, r.unknown), (0, 1));
        w.chain.unknown.store(false, Ordering::SeqCst);
        let r = isl.settle(T0 + 300).await.unwrap();
        assert_eq!(r, Settled::default(), "issue inconnue : ni rejouée, ni dépassée par la suivante");
        assert!(w.chain.sent.lock().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&w.dir);
    }

    #[tokio::test]
    async fn a_promise_carries_its_credit_to_another_station_cut_off_from_the_first() {
        let a = world("carry-a").await;
        a.island.snapshot(T0).await.unwrap();
        let cur = a.island.open(T0 + 10).await.unwrap();
        let paid = a.island.pay_once(&a.alice, &a.bob, Some(&a.bob_g1.ss58()), Amount(700), Some("req-1-abcdef"), cur.opened_at + 1).await.unwrap();
        // Rejouer la même demande (panne réseau pendant l'envoi) rend la même promesse, sans en faire une seconde.
        assert_eq!(a.island.pay_once(&a.alice, &a.bob, Some(&a.bob_g1.ss58()), Amount(700), Some("req-1-abcdef"), cur.opened_at + 2).await.unwrap(), paid);
        let promise = a.island.promises_of(&cur.id, &a.alice).await.unwrap();
        assert_eq!(promise.len(), 1);
        // Station B (celle de Bob) : elle ne reçoit QUE la promesse, jamais l'ouverture publiée par le NODE de A.
        let b = world("carry-b").await;
        astro_sync::ingest_event(b.island.db.writer(), promise[0].clone(), "crawl").await.unwrap();
        b.island.revalidate_all(cur.opened_at + 5).await.unwrap();
        let seen = b.island.account(&b.bob, cur.opened_at + 5).await.unwrap();
        let p = &seen["promises"][0];
        assert_eq!((p["state"].as_str(), p["amount"].as_str(), p["sent"].as_bool()), (Some("accepted"), Some("7.00"), Some(false)), "{seen}");
        assert_eq!(p["node"].as_str(), Some(a.island.node_hex.as_str()), "la station garante est dite");
        assert_eq!(p["known_station"].as_bool(), Some(false), "station inconnue de B : le membre en est averti");
        // Une promesse qui embarque le crédit d'un AUTRE membre n'ouvre rien.
        let mut forged = promise[0].clone();
        forged.content = String::new();
        let stranger = UnsignedEvent { created_at: forged.created_at, kind: forged.kind, tags: forged.tags.clone(), content: promise[0].content.clone() }.sign(&[9; 32]).unwrap();
        let c = world("carry-c").await;
        astro_sync::ingest_event(c.island.db.writer(), stranger, "crawl").await.unwrap();
        c.island.revalidate_all(cur.opened_at + 5).await.unwrap();
        let none: i64 = c.island.db.readers().read(|x| Ok(x.query_row("SELECT count(*) FROM island_credits", [], |r| r.get(0))?)).await.unwrap();
        assert_eq!(none, 0);
        for w in [a, b, c] {
            let _ = std::fs::remove_dir_all(&w.dir);
        }
    }

    #[tokio::test]
    async fn hostile_concurrent_payments_never_fork_the_payer() {
        let w = world("race").await;
        let isl = w.island.clone();
        isl.snapshot(T0).await.unwrap();
        let cur = isl.open(T0 + 10).await.unwrap();
        let at = cur.opened_at + 5;
        let mut tasks = Vec::new();
        for _ in 0..8 {
            let (isl, a, b, g) = (isl.clone(), w.alice.clone(), w.bob.clone(), w.bob_g1.ss58());
            tasks.push(tokio::spawn(async move { isl.pay(&a, &b, Some(&g), Amount(300), at).await }));
        }
        let mut ok = 0;
        for t in tasks {
            if t.await.unwrap().is_ok() {
                ok += 1;
            }
        }
        assert_eq!(ok, 6, "20 Ğ1 / 3 Ğ1 : six promesses, les autres refusées pour solde");
        let acc = isl.account(&w.alice, at).await.unwrap();
        assert_eq!((acc["frozen"].as_bool(), acc["available"].as_str()), (Some(false), Some("2.00")));
        let _ = std::fs::remove_dir_all(&w.dir);
    }
}
