//! Sauvegarde **privée** des fichiers non partagés d'un MULTIPASS chez un proche réciproque.
//!
//! - le propriétaire choisit un ami et active l'option ; **l'ami doit accepter**, et fixe le volume qu'il met à disposition ;
//! - ne partent que les fichiers « privés » : ni sous `/Partages/`, ni jamais envoyés (`ucloud_sharing`), ni dans une
//!   invitation, ni partagés par FaceCloud ; les contacts (`/_/Contacts`) en font partie ;
//! - l'ami reçoit le blob **chiffré**, sans la clé : il le range sous `/Sauvegardes/De_<nom>/<cid>`, où il compte dans son
//!   quota, et ne peut rien lire. Les clés restent dans le trousseau du propriétaire (déjà sauvegardé par son manifeste) ;
//! - sur la même station tout se fait en base ; sur une autre, par messages NIP-59 portant l'étiquette `astro-backup`
//!   (`op` : `request`, `accept`, `decline`, `revoke`, `item`), blob rapatrié par le maillage comme pour un partage.
//!
//! L'envoi se fait **une fois par nuit**, au 20h12 solaire de la station ; la réception, elle, est continue. Les fichiers
//! supprimés ou remplacés chez le propriétaire sont retirés de chez l'ami au bout de 7 jours (nettoyage de la même nuit),
//! pour qu'une suppression accidentelle reste rattrapable.
//!
//! **Restauration** (station détruite, ou déménagement) : le membre rétablit son compte sur une autre station depuis sa carte
//! secrète ([`astro_lifecycle::multipass::SecretCard`]), et tout se passe ensuite **sans aucune action de ses amis** :
//!
//! - l'envoi nocturne range chez l'ami, avec les fichiers privés, le **blob du manifeste** (VFS + trousseau, chiffré) : sans lui
//!   l'événement 30078 retrouvé sur les relais ne désignerait rien ;
//! - la nouvelle station annonce sa **station d'accueil** en republiant le DID du membre (`home_station`, répliqué par le crawl)
//!   et envoie à chaque proche un message `restore` ; la station de l'ami met à jour la station d'accueil, puis renvoie d'elle-même
//!   ce qu'elle détient : le manifeste d'abord, puis les fichiers sauvegardés et ceux que le membre lui avait partagés (`restored`) ;
//! - le manifeste rétabli, le VFS et le trousseau reviennent ; les fichiers qui manquent encore (reçus d'autres amis) sont redemandés
//!   par leur CID, qu'un ami ne rend qu'à qui le connaît et seulement s'il l'a partagé ;
//! - tant que la restauration est en cours, aucun manifeste vide n'est publié ([`astro_dav::manifest::restoring`]) ;
//! - **déménagement** : la nouvelle station écrit à l'ancienne Home un message `moved`, signé de la clé du compte (le seul message
//!   qu'un compte s'adresse à lui-même). L'ancienne station, si elle tourne encore, cesse d'héberger ce compte : il passe à
//!   « non hébergé », pointe vers la nouvelle Home, et son dossier est mis de côté dans la corbeille `trash/moved/` (rien n'est effacé tout de suite).
//!
//! **Secours par les gardiens** (« J'appelle un ami », sans QR à lire) : sur la nouvelle station, le membre scanne sa carte
//! imprimée (part P1) et demande de l'aide. La station retrouve sa liste de contacts (crawl de sa clé, [`astro_sync::WANTED_SCOPE`])
//! et envoie à chacun, **depuis sa clé NODE**, un message `rescue`. Seule la station d'un ami qui garde sa part P3 y donne
//! suite : l'ami est prévenu (« Léa dit avoir perdu sa station »), l'appelle, puis confirme d'un geste. Sa station renvoie alors
//! la part P3 (`rescued`) à la clé NODE de la nouvelle station, qui la garde 24 h pour ce compte : la restauration assemble P1 et
//! P3 sans que personne ne lise ni ne recopie la part. La sécurité est celle de l'appel téléphonique : P3 ne sert à rien sans
//! la carte P1, et l'ami ne la rend qu'après avoir reconnu son ami. Débit borné des deux côtés (3 demandes par jour et par
//! compte, 10 comptes par heure et par station) ; un refus (« ce n'est pas lui ») alerte le Capitaine de l'ami.

use std::sync::Arc;
use std::time::Duration;

use astro_core::{ActivityBus, Audience};
use astro_crypto::KeySource;
use astro_crypto::nip59::{self, Rumor, gift_wrap};
use astro_dav::adopt::{adopt_backup_blob, sender_label};
use astro_dav::manifest::{self, Pointer};
use astro_dav::{BlobStore, ManifestPublisher};
use astro_db::Db;
use astro_db::events::row_to_event;
use astro_facecloud::Outbox;
use astro_nostr_proto::{Event, UnsignedEvent, kind};
use rusqlite::OptionalExtension;
use serde_json::{Value, json};
use tokio::sync::watch;

use crate::register::PublishFn;
use crate::share_inbox::Prefetch;
use crate::sharing::Sharing;
use crate::upload_worker::Notify;

mod compact;
mod repair;
mod rs;
pub use repair::{REPAIR_AFTER, RepairStats, Sample};
pub use rs::{BIG_BLOB, BIG_BLOB_COPIES, FileCopies, Probe, RS_TARGET_PERMILLE, choose_k};

/// Étiquette (`["t", …]`) des messages de sauvegarde.
pub const TAG: &str = "astro-backup";
const CLAIM: &str = "backup_inbox";
const TRIES: &str = "backup_inbox_tries";
/// Fichiers envoyés au plus par passage et par compte.
const PER_PASS: usize = 100;
/// Pause entre deux envois NIP-59 vers une autre station.
const SEND_PACE: std::time::Duration = std::time::Duration::from_millis(if cfg!(test) { 0 } else { 250 });
const LOOKBACK_SECS: i64 = 30 * 24 * 3600;
const BATCH: usize = 50;
const MAX_TRIES: u32 = 12;
const MAX_BYTES: u64 = astro_p2p::blob::MAX_BLOB_BYTES;
/// Une demande de restauration est renouvelée toutes les 6 h tant que le manifeste n'est pas rétabli.
const RESTORE_RETRY_SECS: i64 = 6 * 3600;
/// Sans manifeste retrouvé au bout de 3 jours, le compte repart à neuf : la publication de manifestes est débloquée.
const RESTORE_GIVE_UP_SECS: i64 = 3 * 24 * 3600;
/// Après ces 3 jours (état `late`) : une relance par jour des amis qui n'ont encore rien renvoyé…
const RESTORE_LATE_RETRY_SECS: i64 = 24 * 3600;
/// … pendant 30 jours ; un manifeste qui revient pendant ce temps est fusionné sans rien écraser.
const RESTORE_LATE_SECS: i64 = 30 * 24 * 3600;
/// Suivi « vos fichiers reviennent » montré au membre après la fin de la restauration.
const RESTORE_SHOW_SECS: i64 = 7 * 24 * 3600;
/// Événement de manifeste d'origine, gardé avant que le compte ne publie le sien (il le remplacerait) : `kv` restore_origin.
const ORIGIN: &str = "restore_origin";
/// Blobs renvoyés par chaque ami pendant la restauration : `kv` restore_from, clé `<propriétaire>:<ami>`.
const RETURNED: &str = "restore_from";
/// Contacts sans nom public au plus montrés sur la page de secours (le reste est compté).
const RESCUE_SHOW: usize = 50;
/// Fichiers renvoyés à un propriétaire par passage, et CID redemandés par message.
const RESTORE_PER_PASS: usize = 40;
const RESTORE_WANT: usize = 200;
/// Fichiers qui manquent encore après le manifeste : au plus 4 relances espacées de 6 h.
const RESTORE_MISSING_ROUNDS: u32 = 4;
/// Tâches de renvoi côté ami : `kv(scope, "<ami>:<propriétaire>")`.
/// Durée de garde d'un compte déménagé, mis de côté dans `trash/moved/` (il contient encore ses clés) : 30 jours, puis effacé.
const MOVED_KEEP_SECS: i64 = 30 * 24 * 3600;
/// Part P3 de la clé de secours : côté propriétaire, ce qui a déjà été confié (`<owner>:<host>` → empreinte de la part).
const SHARE_SENT: &str = "backup_share_sent";
/// Côté hébergeur : la part P3 qu'un ami lui a confiée (`<host>:<owner>` → texte de la part).
const SHARE_HELD: &str = "backup_share_held";
const JOB: &str = "backup_restore_job";
/// Secours, côté nouvelle station : `<hex du compte>` → [`RescueOut`] (JSON).
const RESCUE_OUT: &str = "backup_rescue_out";
/// Secours, côté gardien : `<gardien>:<compte>` → demande en attente (JSON : `from`, `station`, `at`).
const RESCUE_IN: &str = "backup_rescue_in";
/// Secours, côté gardien : `<gardien>:<compte>` → dates des dernières demandes (débit).
const RESCUE_ASKED: &str = "backup_rescue_asked";
/// Durée de vie d'une demande de secours.
pub const RESCUE_SECS: i64 = 24 * 3600;
/// Demandes de secours au plus par compte et par jour (des deux côtés).
const RESCUE_MAX_ASKS: usize = 3;
/// Comptes différents pour lesquels une station envoie des demandes, au plus par heure (anti-amplification).
const RESCUE_MAX_PER_HOUR: usize = 10;
/// Contacts sollicités au plus par demande.
const RESCUE_MAX_FRIENDS: usize = 50;

/// Demande de secours suivie par la nouvelle station (état local, 24 h).
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
struct RescueOut {
    /// Ouverture de la demande.
    at: i64,
    /// Dates des envois (débit).
    asks: Vec<i64>,
    /// Contacts déjà sollicités.
    sent: Vec<String>,
    /// Part P3 reçue d'un gardien.
    share: Option<String>,
    /// Nom du gardien qui l'a rendue.
    by: Option<String>,
    /// Gardiens qui n'ont pas reconnu la demande.
    denied: Vec<String>,
    /// Clé du gardien qui a rendu la part (pour l'état nominatif de la page de secours).
    #[serde(default)]
    by_hex: Option<String>,
    /// Clés des gardiens qui ont refusé.
    #[serde(default)]
    denied_hex: Vec<String>,
    /// Contacts dont la station n'a pas pu être jointe, avec la date du premier échec (retentés à chaque passage).
    #[serde(default)]
    failed: Vec<(String, i64)>,
}
/// Délai avant de retirer chez l'ami le blob d'un fichier disparu chez le propriétaire (suppression ou remplacement).
pub const GRACE_SECS: i64 = 7 * 24 * 3600;

/// Résumé de la sauvegarde k-sur-n de toute la station pour l'espace Capitaine (Santé) : côté propriétaires hébergés ici,
/// paquets, fragments (acquittés, en attente, perdus), réparations en cours, compactage, retraits en file, gardiens fautifs
/// (fragment perdu ou contrôle à finir) ; côté gardien, fragments tenus pour des amis.
pub fn captain_summary(c: &rusqlite::Connection) -> rusqlite::Result<Value> {
    const MINE: &str = "owner_hex IN (SELECT hex FROM identities WHERE hosted = 1)";
    let one = |sql: String| c.query_row(&sql, [], |r| r.get::<_, i64>(0));
    Ok(json!({
        "packets": one(format!("SELECT COUNT(*) FROM backup_packets WHERE {MINE} AND staged = 0"))?,
        "staged": one(format!("SELECT COUNT(*) FROM backup_packets WHERE {MINE} AND staged = 1"))?,
        "frags_acked": one(format!("SELECT COUNT(*) FROM backup_frags WHERE {MINE} AND acked_at IS NOT NULL AND lost_at IS NULL"))?,
        "frags_pending": one(format!("SELECT COUNT(*) FROM backup_frags WHERE {MINE} AND acked_at IS NULL AND lost_at IS NULL"))?,
        "frags_lost": one(format!("SELECT COUNT(*) FROM backup_frags WHERE {MINE} AND lost_at IS NOT NULL"))?,
        "repairing": one(format!("SELECT COUNT(*) FROM backup_frags f WHERE f.{MINE} AND f.acked_at IS NULL AND f.lost_at IS NULL AND EXISTS (SELECT 1 FROM backup_frags x WHERE x.owner_hex = f.owner_hex AND x.packet_id = f.packet_id AND x.lost_at IS NOT NULL)"))?,
        "drops_queued": one("SELECT COUNT(*) FROM kv WHERE scope = 'backup_rs_drop'".into())?,
        "faulty_guardians": one(format!("SELECT COUNT(DISTINCT host_hex) FROM backup_frags WHERE {MINE} AND lost_at IS NOT NULL"))?,
        "suspects": one("SELECT COUNT(*) FROM kv WHERE scope = 'backup_rs_suspect'".into())?,
        "held_for_friends": one("SELECT COUNT(*) FROM backup_frags WHERE owner_hex NOT IN (SELECT hex FROM identities WHERE hosted = 1)".into())?,
    }))
}

/// Secondes UNIX de l'horloge de la station.
pub fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

fn backoff(tries: u32) -> i64 {
    (60i64 << tries.min(9)).min(6 * 3600)
}

fn tag<'a>(tags: &'a [Vec<String>], name: &str) -> Option<&'a str> {
    tags.iter().find(|t| t.first().map(String::as_str) == Some(name)).and_then(|t| t.get(1)).map(String::as_str)
}

/// Plus aucun CID prioritaire à renvoyer dans cette tâche ?
fn rest_is_empty(job: &Value) -> bool {
    job["priority"].as_array().is_none_or(Vec::is_empty)
}

/// `<PeerID>:<NODE_HEX>` (même règle que le projecteur du DID).
fn valid_home(h: &str) -> bool {
    h.split_once(':').is_some_and(|(peer, node)| (10..=64).contains(&peer.len()) && peer.bytes().all(|b| b.is_ascii_alphanumeric()) && is_hex64(node))
}

fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Bilan d'un passage de réception.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BackupStats {
    /// Messages de contrôle traités (demande, réponse).
    pub control: usize,
    /// Blobs rangés.
    pub stored: usize,
    /// Messages écartés (hors accord, volume dépassé, mal formés).
    pub ignored: usize,
    /// Blobs pas encore joignables : nouvel essai plus tard.
    pub deferred: usize,
}

/// Sauvegarde privée : réglages, accords et envoi.
pub struct PrivateBackup<O: Outbox, B: BlobStore> {
    db: Arc<Db>,
    keys: Arc<dyn KeySource>,
    outbox: Arc<O>,
    blobs: Arc<B>,
    sharing: Arc<Sharing<O>>,
    publisher: Option<ManifestPublisher>,
    station_peer: Option<String>,
    /// `<PeerID>:<NODE_HEX>` de cette station, annoncé dans le DID d'un compte rétabli ici.
    home: Option<String>,
    publish: Option<PublishFn>,
    /// Dossier des comptes (`~/.zen/game/nostr`) : un compte déménagé est mis de côté dans la corbeille `trash/moved/` (voir [`crate::trash`]).
    accounts_dir: Option<std::path::PathBuf>,
    activity: ActivityBus,
    notify: Option<Notify>,
    prefetch: Option<Prefetch>,
    /// Rapatriement plafonné à la taille d'une pièce jointe, pour les copies de pièces jointes (sinon `prefetch`).
    attachment_prefetch: Option<Prefetch>,
    /// Clé NODE de la station (hex, secret) : expéditrice et destinataire des messages de secours.
    node: Option<(String, astro_crypto::SecretBytes)>,
    /// Nom de la station montré au gardien (« sur la station … »).
    label: Option<String>,
    /// Réveil du crawl, pour retrouver tout de suite les contacts d'un compte en secours.
    crawl_nudge: Option<Arc<tokio::sync::Notify>>,
    /// Sonde groupée de présence des fragments chez une station (`HASN`), pour la restauration k-sur-n.
    probe: Option<Probe>,
    /// Contrôle de possession des fragments (lecture d'une plage chez la station du gardien), pour la réparation nocturne.
    sample: Option<Sample>,
}

impl<O: Outbox, B: BlobStore> PrivateBackup<O, B> {
    /// `sharing` fournit la liste des proches réciproques ; `station_peer` est indiqué à l'hébergeur pour rapatrier les blobs.
    pub fn new(db: Arc<Db>, keys: Arc<dyn KeySource>, outbox: Arc<O>, blobs: Arc<B>, sharing: Arc<Sharing<O>>, activity: ActivityBus) -> Self {
        Self { db, keys, outbox, blobs, sharing, publisher: None, station_peer: None, home: None, publish: None, accounts_dir: None, activity, notify: None, prefetch: None, attachment_prefetch: None, node: None, label: None, crawl_nudge: None, probe: None, sample: None }
    }

    /// Republie le manifeste de l'hébergeur après chaque rangement, et indique la station aux amis.
    pub fn with_station(mut self, publisher: Option<ManifestPublisher>, station_peer: Option<String>) -> Self {
        self.publisher = publisher;
        self.station_peer = station_peer;
        self
    }

    /// Station d'accueil annoncée (`<PeerID>:<NODE_HEX>`) et publication des événements signés, pour la restauration d'un compte.
    pub fn with_home(mut self, home: Option<String>, publish: Option<PublishFn>) -> Self {
        self.home = home;
        self.publish = publish;
        self
    }

    /// Dossier des comptes, pour mettre de côté celui d'un membre qui a déménagé.
    pub fn with_accounts_dir(mut self, dir: Option<std::path::PathBuf>) -> Self {
        self.accounts_dir = dir;
        self
    }

    /// Secours par les gardiens : clé NODE de la station, son nom montré aux gardiens, et le réveil du crawl.
    pub fn with_rescue(mut self, node_secret: Option<[u8; 32]>, label: Option<String>, crawl_nudge: Option<Arc<tokio::sync::Notify>>) -> Self {
        self.node = node_secret.and_then(|s| astro_nostr_proto::pubkey_of(&s).ok().map(|h| (h, zeroize::Zeroizing::new(s))));
        self.label = label;
        self.crawl_nudge = crawl_nudge;
        self
    }

    /// Prévient le titulaire (message privé de BRO).
    pub fn with_notify(mut self, notify: Option<Notify>) -> Self {
        self.notify = notify;
        self
    }

    /// Rapatriement plafonné (taille d'une pièce jointe) des copies de pièces jointes.
    pub fn with_attachment_prefetch(mut self, f: Prefetch) -> Self {
        self.attachment_prefetch = Some(f);
        self
    }

    /// Rapatriement d'un blob depuis la station du propriétaire.
    pub fn with_prefetch(mut self, f: Prefetch) -> Self {
        self.prefetch = Some(f);
        self
    }

    async fn tell(&self, who: &str, text: String) {
        self.activity.publish_to(Audience::Owner(who.to_owned()), "ucloud", "backup", text.clone(), None);
        if let Some(n) = &self.notify {
            n(who.to_owned(), text).await;
        }
    }

    async fn label_of(&self, hex: &str) -> String {
        let h = hex.to_owned();
        self.db
            .readers()
            .read(move |c| {
                let email = c.query_row("SELECT email FROM identities WHERE hex = ?1", [&h], |r| r.get::<_, Option<String>>(0)).optional()?.flatten();
                let name = c.query_row("SELECT json_extract(content, '$.name') FROM nostr_events WHERE pubkey = ?1 AND kind = 0 ORDER BY created_at DESC LIMIT 1", [&h], |r| r.get::<_, Option<String>>(0)).optional()?.flatten();
                Ok(sender_label(email.as_deref().or(name.as_deref().map(str::trim).filter(|n| !n.is_empty())), &h))
            })
            .await
            .unwrap_or_else(|_| hex.chars().take(8).collect())
    }

    /// Confie la part P3 de la clé de secours de chaque compte hébergé ici aux amis qui ont accepté d'héberger sa sauvegarde
    /// (un seul message par ami, renvoyé si la part change). Une part seule ne révèle rien à l'ami qui la garde.
    pub async fn send_shares(&self, at: i64) -> usize {
        let Some(dir) = self.accounts_dir.clone() else { return 0 };
        let pacts: Vec<(String, String, String)> = self
            .db
            .readers()
            .read(|c| {
                let mut stmt = c.prepare("SELECT p.owner_hex, p.host_hex, i.email FROM backup_pacts p JOIN identities i ON i.hex = p.owner_hex AND i.hosted = 1 WHERE p.status = 'accepted' AND i.email IS NOT NULL")?;
                Ok(stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .unwrap_or_default();
        let mut sent = 0;
        for (owner, host, email) in pacts {
            let Some(share) = astro_lifecycle::share::read_share(&dir.join(&email), 3) else { continue };
            let (text, key) = (share.to_text(), format!("{owner}:{host}"));
            let fingerprint = hex::encode(&<sha2::Sha256 as sha2::Digest>::digest(text.as_bytes())[..8]);
            if self.kv_get(SHARE_SENT, &key).await.is_some_and(|(v, _)| v == fingerprint) {
                continue;
            }
            let delivered = if self.hosted_here(&host).await {
                self.kv_put(SHARE_HELD, &format!("{host}:{owner}"), &text, at).await;
                Ok(())
            } else {
                self.send_to(&owner, &host, kind::CHAT, vec![vec!["op".into(), "share".into()]], &text).await
            };
            match delivered {
                Ok(()) => {
                    self.kv_put(SHARE_SENT, &key, &fingerprint, at).await;
                    sent += 1;
                }
                Err(e) => {
                    self.activity.publish_to(Audience::Captain, "alert", "backup", format!("clé de secours : part non confiée à …{} ({e})", &host[..8.min(host.len())]), None);
                }
            }
        }
        sent
    }

    /// Proches réciproques à qui demander d'être gardien, **le conseillé d'abord** : un gardien doit surtout être sur un **autre
    /// Astroport** (sur cette station, il tomberait avec elle), puis le plus proche cercle (`proximity`, Famille d'abord), puis
    /// rencontré en personne (preuve QR, `meetings.verified`). Chacun porte `same_station`, `level`, `met_verified`.
    async fn guardian_candidates(&self, owner: &str) -> Result<Vec<Value>, String> {
        let friends = self.sharing.friends(owner).await?;
        // Cercles illisibles (clé absente) : on garde le niveau par défaut plutôt que de priver le membre de ses gardiens.
        let ranks = crate::proximity::load(&self.db, self.keys.as_ref(), owner).await.map(|r| r.0).unwrap_or_default();
        let o = owner.to_owned();
        let met: std::collections::HashSet<String> = self
            .db
            .readers()
            .read(move |c| {
                let mut s = c.prepare("SELECT target_hex FROM meetings WHERE owner_hex = ?1 AND verified = 1")?;
                Ok(s.query_map([o], |r| r.get::<_, String>(0))?.collect::<Result<_, _>>()?)
            })
            .await
            .map_err(|e| e.to_string())?;
        let mut list: Vec<(bool, u8, bool, String, String)> = friends
            .into_iter()
            .map(|f| (f.hosted, ranks.get(&f.hex).copied().unwrap_or(crate::proximity::default_level(true)), met.contains(&f.hex), f.label, f.hex))
            .collect();
        list.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)).then(b.2.cmp(&a.2)).then_with(|| a.3.to_lowercase().cmp(&b.3.to_lowercase())));
        Ok(list.into_iter().map(|(same, level, met, label, hex)| json!({ "hex": hex, "label": label, "same_station": same, "level": level, "met_verified": met })).collect())
    }

    async fn hosted_here(&self, hex: &str) -> bool {
        let h = hex.to_owned();
        self.db.readers().read(move |c| Ok(c.query_row("SELECT 1 FROM identities WHERE hex = ?1 AND hosted = 1", [h], |r| r.get::<_, i64>(0)).optional()?.is_some())).await.unwrap_or(false)
    }

    async fn mutual(&self, owner: &str, other: &str) -> bool {
        let (o, s) = (owner.to_owned(), other.to_owned());
        self.db
            .readers()
            .read(move |c| {
                let follows = c.query_row("SELECT 1 FROM social_graph WHERE owner_hex = ?1 AND pubkey = ?2 AND level = 1", [&o, &s], |r| r.get::<_, i64>(0)).optional()?.is_some();
                let back = c.query_row("SELECT 1 FROM nostr_events e JOIN nostr_tags t ON t.event_id = e.seq WHERE e.kind = 3 AND e.pubkey = ?1 AND t.name = 'p' AND t.value = ?2 LIMIT 1", [&s, &o], |r| r.get::<_, i64>(0)).optional()?.is_some();
                Ok(follows && back)
            })
            .await
            .unwrap_or(false)
    }

    /// Message de contrôle (kind 14) ou blob (kind 15) emballé pour `to`.
    async fn send_to(&self, from: &str, to: &str, k: u32, tags: Vec<Vec<String>>, content: &str) -> Result<(), String> {
        let secret = self.keys.secret(from).ok_or("clé du compte indisponible")?;
        self.send_with(&secret, from, to, k, tags, content).await
    }

    /// Comme [`Self::send_to`], avec une clé donnée (la clé NODE pour les messages de secours).
    async fn send_with(&self, secret: &[u8; 32], from: &str, to: &str, k: u32, mut tags: Vec<Vec<String>>, content: &str) -> Result<(), String> {
        tags.insert(0, vec!["t".into(), TAG.into()]);
        tags.insert(0, vec!["p".into(), to.to_owned()]);
        let at = u64::try_from(now()).unwrap_or(0);
        let rumor = Rumor { pubkey: from.to_owned(), created_at: at, kind: k, tags, content: content.to_owned() };
        let wrap = gift_wrap(secret, to, &rumor, at).map_err(|_| "emballage NIP-59 impossible".to_owned())?;
        self.outbox.send(to, wrap).await.map_err(|e| e.to_string())
    }

    async fn control(&self, from: &str, to: &str, op: &str, extra: Vec<Vec<String>>) -> Result<(), String> {
        let mut tags = vec![vec!["op".to_owned(), op.to_owned()]];
        tags.extend(extra);
        self.send_to(from, to, kind::CHAT, tags, "sauvegarde privée").await
    }

    /// État pour l'écran : mon réglage, et les demandes reçues comme hébergeur.
    pub async fn status(&self, owner: &str) -> Result<Value, String> {
        let o = owner.to_owned();
        let (mine, hosting) = self
            .db
            .readers()
            .read(move |c| {
                let mine = c
                    .query_row(
                        "SELECT host_hex, status, quota_bytes, enabled, \
                                COALESCE((SELECT SUM(size) FROM backup_items b WHERE b.owner_hex = p.owner_hex AND b.host_hex = p.host_hex), 0), \
                                (SELECT COUNT(*) FROM backup_items b WHERE b.owner_hex = p.owner_hex AND b.host_hex = p.host_hex) \
                         FROM backup_pacts p WHERE owner_hex = ?1 ORDER BY enabled DESC, asked_at DESC LIMIT 1",
                        [&o],
                        |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?, r.get::<_, i64>(4)?, r.get::<_, i64>(5)?)),
                    )
                    .optional()?;
                let mut stmt = c.prepare(
                    "SELECT owner_hex, status, quota_bytes, asked_at, \
                            COALESCE((SELECT SUM(size) FROM backup_items b WHERE b.owner_hex = p.owner_hex AND b.host_hex = p.host_hex), 0) \
                     FROM backup_pacts p WHERE host_hex = ?1 AND status IN ('pending','accepted') ORDER BY asked_at DESC",
                )?;
                let hosting = stmt.query_map([&o], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?, r.get::<_, i64>(4)?)))?.collect::<Result<Vec<_>, _>>()?;
                Ok((mine, hosting))
            })
            .await
            .map_err(|e| e.to_string())?;
        let mine = match mine.filter(|m| m.3 == 1 || m.1 == "accepted") {
            Some((host, status, quota, enabled, used, files)) => json!({ "host": host, "label": self.label_of(&host).await, "status": status, "quota_bytes": quota, "enabled": enabled == 1, "backed_up_bytes": used, "files": files }),
            None => Value::Null,
        };
        let mut hosting_json = Vec::new();
        for (who, status, quota, at, used) in hosting {
            let mut entry = json!({ "owner": who, "label": self.label_of(&who).await, "status": status, "quota_bytes": quota, "used_bytes": used, "asked_at": at });
            // « J'appelle un ami » : l'ami hébergeur montre la part P3 (texte et QR) à celui qui rétablit son compte.
            if status == "accepted" {
                if let Some((share, _)) = self.kv_get(SHARE_HELD, &format!("{owner}:{who}")).await {
                    entry["share_svg"] = astro_ui::qr::svg(&share).into();
                    entry["share"] = share.into();
                }
                // Une demande de secours attend la confirmation de l'ami (24 h).
                if let Some(req) = self.rescue_request(owner, &who).await {
                    entry["rescue"] = json!({ "station": req["station"], "at": req["at"] });
                }
            }
            hosting_json.push(entry);
        }
        let o = owner.to_owned();
        let rescue = self.db.readers().read(move |c| Ok(c.query_row("SELECT COUNT(*) FROM kv WHERE scope = ?1 AND key LIKE ?2", rusqlite::params![SHARE_SENT, format!("{o}:%")], |r| r.get::<_, i64>(0))?)).await.unwrap_or(0);
        let friends = self.guardian_candidates(owner).await?;
        let at = now();
        let guardians = self.guardians(owner, at).await;
        let mut gjson = Vec::new();
        for (hex, enabled, same, p) in &guardians {
            gjson.push(json!({ "label": self.label_of(hex).await, "files": enabled, "same_station": same, "availability": p }));
        }
        // Clés : la carte imprimée + un gardien joignable ; fichiers : un gardien qui reçoit la copie complète.
        let remote: Vec<Option<u32>> = guardians.iter().filter(|g| !g.2).map(|g| g.3).collect();
        let files: Vec<Option<u32>> = guardians.iter().filter(|g| !g.2 && g.1).map(|g| g.3).collect();
        let (permille, next) = crate::guardians::estimate(&remote);
        let safety = json!({ "permille": permille, "next_permille": next, "files_permille": crate::guardians::estimate(&files).0 });
        Ok(json!({ "mine": mine, "hosting": hosting_json, "friends": friends, "rescue_friends": rescue, "guardians": gjson, "safety": safety, "coded": self.coded_view(owner, at).await, "restore": self.restore_view(owner, at).await }))
    }

    /// Active la sauvegarde chez `host` (un proche réciproque : la demande lui est envoyée) ou la suspend (`host` absent).
    pub async fn set(&self, owner: &str, enabled: bool, host: Option<&str>) -> Result<Value, String> {
        if !enabled {
            self.db.writer().execute("UPDATE backup_pacts SET enabled = 0 WHERE owner_hex = ?1", vec![owner.to_owned().into()]).await.map_err(|e| e.to_string())?;
            return self.status(owner).await;
        }
        let host = host.filter(|h| is_hex64(h)).ok_or("choisissez un proche pour héberger la sauvegarde")?.to_ascii_lowercase();
        if self.sharing.friends(owner).await?.iter().all(|f| f.hex != host) {
            return Err("cet ami n'est pas un proche réciproque (vous vous suivez mutuellement ?)".into());
        }
        let (o, h, at) = (owner.to_owned(), host.clone(), now());
        let must_ask = self
            .db
            .writer()
            .transaction(move |tx| {
                // Un gardien de plus ne remplace pas les autres : chacun reçoit sa part de chaque nouveau paquet.
                let status: Option<String> = tx.query_row("SELECT status FROM backup_pacts WHERE owner_hex = ?1 AND host_hex = ?2", [&o, &h], |r| r.get(0)).optional()?;
                let ask = !matches!(status.as_deref(), Some("accepted" | "pending"));
                if status.is_none() {
                    tx.execute("INSERT INTO backup_pacts(owner_hex, host_hex, status, enabled, asked_at) VALUES (?1, ?2, 'pending', 1, ?3)", rusqlite::params![o, h, at])?;
                } else if ask {
                    tx.execute("UPDATE backup_pacts SET status = 'pending', quota_bytes = 0, enabled = 1, asked_at = ?3, decided_at = NULL WHERE owner_hex = ?1 AND host_hex = ?2", rusqlite::params![o, h, at])?;
                } else {
                    tx.execute("UPDATE backup_pacts SET enabled = 1 WHERE owner_hex = ?1 AND host_hex = ?2", [&o, &h])?;
                }
                Ok(ask)
            })
            .await
            .map_err(|e| e.to_string())?;
        if must_ask {
            let label = self.label_of(owner).await;
            if self.hosted_here(&host).await {
                self.tell(&host, format!("🗄️ {label} vous demande d'héberger sa sauvegarde privée (chiffrée, illisible pour vous). Acceptez ou refusez dans Moi › Sauvegarde.")).await;
            } else {
                self.control(owner, &host, "request", vec![]).await?;
            }
        }
        self.status(owner).await
    }

    /// L'hébergeur `host` répond à `owner` : accepte avec un volume (`quota_bytes`, plafonné à `max_bytes`), refuse, ou
    /// révoque un accord (ses blobs sont alors supprimés et son quota libéré).
    pub async fn respond(&self, host: &str, owner: &str, accept: bool, quota_bytes: u64, max_bytes: u64) -> Result<Value, String> {
        let (h, o) = (host.to_owned(), owner.to_owned());
        let current: Option<String> = self.db.readers().read(move |c| Ok(c.query_row("SELECT status FROM backup_pacts WHERE owner_hex = ?1 AND host_hex = ?2", [&o, &h], |r| r.get(0)).optional()?)).await.map_err(|e| e.to_string())?;
        let current = current.ok_or("aucune demande de cet ami")?;
        let (status, quota) = if accept {
            if quota_bytes == 0 {
                return Err("indiquez le volume que vous mettez à disposition".into());
            }
            ("accepted", quota_bytes.min(max_bytes))
        } else if current == "accepted" {
            ("revoked", 0)
        } else {
            ("declined", 0)
        };
        let (h, o, at) = (host.to_owned(), owner.to_owned(), now());
        self.db
            .writer()
            .transaction(move |tx| {
                tx.execute("UPDATE backup_pacts SET status = ?3, quota_bytes = ?4, decided_at = ?5 WHERE owner_hex = ?1 AND host_hex = ?2", rusqlite::params![o, h, status, i64::try_from(quota).unwrap_or(i64::MAX), at])?;
                Ok(())
            })
            .await
            .map_err(|e| e.to_string())?;
        if status == "revoked" || status == "declined" {
            self.forget_hosted(host, owner).await;
        }
        let label = self.label_of(host).await;
        if self.hosted_here(owner).await {
            let text = if accept { format!("✅ {label} accepte d'héberger votre sauvegarde privée.") } else { format!("⛔ {label} n'héberge plus (ou refuse d'héberger) votre sauvegarde privée.") };
            self.tell(owner, text).await;
        } else {
            let op = match status {
                "accepted" => "accept",
                "revoked" => "revoke",
                _ => "decline",
            };
            // `["rs","1"]` : cette station sait garder des fragments (sauvegarde k-sur-n) ; sans lui, copie complète.
            self.control(host, owner, op, vec![vec!["quota".into(), quota.to_string()], vec!["rs".into(), "1".into()]]).await?;
        }
        self.status(host).await
    }

    /// Supprime chez `host` les blobs sauvegardés par `owner` (quota libéré) et oublie l'inventaire de ce couple.
    async fn forget_hosted(&self, host: &str, owner: &str) {
        self.drop_hosted(host, owner, None).await;
        // La part P3 de la clé de secours suit le sort de la sauvegarde : plus d'accord, plus de part.
        let key = format!("{host}:{owner}");
        let _ = self.db.writer().execute("DELETE FROM kv WHERE scope = ?1 AND key = ?2", vec![SHARE_HELD.to_owned().into(), key.into()]).await;
    }

    /// Supprime chez `host` les blobs de `owner` (tous, ou seulement `only`), puis les blobs que plus aucune entrée ne référence.
    async fn drop_hosted(&self, host: &str, owner: &str, only: Option<&[String]>) -> usize {
        let (h, o, only) = (host.to_owned(), owner.to_owned(), only.map(<[String]>::to_vec));
        let cids: Vec<String> = self
            .db
            .writer()
            .transaction(move |tx| {
                let mut stmt = tx.prepare("SELECT cid FROM ucloud_entries WHERE owner_hex = ?1 AND path LIKE '/Sauvegardes/%' AND json_extract(data, '$.backup_of') = ?2")?;
                let mut cids = stmt.query_map([&h, &o], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
                drop(stmt);
                if let Some(only) = &only {
                    cids.retain(|c| only.contains(c));
                }
                for cid in &cids {
                    tx.execute("DELETE FROM ucloud_entries WHERE owner_hex = ?1 AND path LIKE '/Sauvegardes/%' AND json_extract(data, '$.backup_of') = ?2 AND cid = ?3", rusqlite::params![h, o, cid])?;
                }
                match &only {
                    None => {
                        tx.execute("DELETE FROM backup_frags WHERE owner_hex = ?2 AND host_hex = ?1", [&h, &o])?;
                        tx.execute("DELETE FROM backup_items WHERE owner_hex = ?2 AND host_hex = ?1", [&h, &o])?
                    }
                    Some(only) => {
                        let mut n = 0;
                        for cid in only {
                            n += tx.execute("DELETE FROM backup_items WHERE owner_hex = ?1 AND host_hex = ?2 AND cid = ?3", rusqlite::params![o, h, cid])?;
                            n += tx.execute("DELETE FROM backup_frags WHERE owner_hex = ?1 AND host_hex = ?2 AND cid = ?3", rusqlite::params![o, h, cid])?;
                        }
                        n
                    }
                };
                Ok(cids)
            })
            .await
            .unwrap_or_default();
        for cid in &cids {
            let c = cid.clone();
            let used = self.db.readers().read(move |conn| astro_dav::vfs::cid_in_use(conn, &c).map_err(|_| astro_db::DbError::Aborted)).await.unwrap_or(true);
            if !used {
                let _ = self.blobs.remove(cid).await;
            }
        }
        if !cids.is_empty() {
            if let Some(p) = &self.publisher {
                p.notify(host);
            }
        }
        cids.len()
    }

    /// Nettoyage de nuit côté propriétaire : un blob dont le fichier n'existe plus chez lui (supprimé ou remplacé) est noté
    /// « disparu » ; s'il le reste [`GRACE_SECS`], il est retiré de chez l'ami. Un fichier revenu efface la marque.
    /// Rend le nombre de blobs retirés.
    pub async fn gc_night(&self, at: i64) -> usize {
        // Le manifeste courant n'est pas une entrée du VFS : il ne doit jamais être pris pour un fichier disparu.
        const GONE: &str = "(NOT EXISTS (SELECT 1 FROM ucloud_entries e WHERE e.owner_hex = backup_items.owner_hex AND e.cid = backup_items.cid AND e.path NOT LIKE '/Sauvegardes/%') \
                            AND backup_items.cid <> COALESCE((SELECT k.value FROM kv k WHERE k.scope = 'ucloud_manifest_cid' AND k.key = backup_items.owner_hex), '') \
                            AND backup_items.cid NOT IN (SELECT x.index_cid FROM backup_index x WHERE x.owner_hex = backup_items.owner_hex))";
        let pacts: Vec<(String, String)> = self
            .db
            .readers()
            .read(|c| {
                let mut stmt = c.prepare("SELECT p.owner_hex, p.host_hex FROM backup_pacts p JOIN identities i ON i.hex = p.owner_hex AND i.hosted = 1 WHERE p.status = 'accepted'")?;
                Ok(stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .unwrap_or_default();
        let mut dropped = 0;
        for (owner, host) in pacts {
            let (o, h) = (owner.clone(), host.clone());
            let due: Vec<String> = self
                .db
                .writer()
                .transaction(move |tx| {
                    tx.execute(&format!("UPDATE backup_items SET missing_since = NULL WHERE owner_hex = ?1 AND host_hex = ?2 AND missing_since IS NOT NULL AND NOT {GONE}"), [&o, &h])?;
                    tx.execute(&format!("UPDATE backup_items SET missing_since = ?3 WHERE owner_hex = ?1 AND host_hex = ?2 AND missing_since IS NULL AND {GONE}"), rusqlite::params![o, h, at])?;
                    let mut stmt = tx.prepare("SELECT cid FROM backup_items WHERE owner_hex = ?1 AND host_hex = ?2 AND missing_since IS NOT NULL AND missing_since <= ?3")?;
                    Ok(stmt.query_map(rusqlite::params![o, h, at - GRACE_SECS], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?)
                })
                .await
                .unwrap_or_default();
            if due.is_empty() {
                continue;
            }
            if self.hosted_here(&host).await {
                self.drop_hosted(&host, &owner, Some(&due)).await;
                dropped += due.len();
            } else {
                for chunk in due.chunks(100) {
                    let tags = chunk.iter().map(|c| vec!["cid".to_owned(), c.clone()]).collect();
                    if self.control(&owner, &host, "drop", tags).await.is_err() {
                        break;
                    }
                    let (o, h, ids) = (owner.clone(), host.clone(), chunk.to_vec());
                    let _ = self
                        .db
                        .writer()
                        .transaction(move |tx| {
                            for cid in &ids {
                                tx.execute("DELETE FROM backup_items WHERE owner_hex = ?1 AND host_hex = ?2 AND cid = ?3", rusqlite::params![o, h, cid])?;
                            }
                            Ok(())
                        })
                        .await;
                    dropped += chunk.len();
                }
            }
        }
        dropped
    }

    /// Fichiers privés de `owner` pas encore sauvegardés chez `host`. `unpacked_only` (gardien k-sur-n) : seulement ceux
    /// que le flux de paquets ne prend pas (CID d'une autre forme que `bafkrei…`), qui partent donc en copie complète.
    /// Fichiers pas encore chez `host`, les plus anciens d'abord, **qui tiennent dans `room` octets** : un gros fichier qui ne
    /// rentre plus dans le volume restant ne bouche pas la sélection (sinon les 100 plus anciens, trop gros, la tiendraient à
    /// jamais). Une erreur de lecture est dite au Capitaine.
    async fn pending_files(&self, owner: &str, host: &str, limit: usize, unpacked_only: bool, room: u64) -> Vec<(String, u64)> {
        let (o, h) = (owner.to_owned(), host.to_owned());
        self.db
            .readers()
            .read(move |c| {
                let mut stmt = c.prepare(
                    "SELECT DISTINCT e.cid, COALESCE(e.size, 0) FROM ucloud_entries e \
                     WHERE e.owner_hex = ?1 AND e.type = 'file' AND e.cid IS NOT NULL \
                       AND e.path NOT LIKE '/Partages/%' AND e.path NOT LIKE '/Sauvegardes/%' \
                       AND NOT EXISTS (SELECT 1 FROM ucloud_sharing s WHERE s.owner_hex = e.owner_hex AND s.cid = e.cid) \
                       AND NOT EXISTS (SELECT 1 FROM face_shares f WHERE f.owner_hex = e.owner_hex AND f.cid = e.cid) \
                       AND NOT EXISTS (SELECT 1 FROM invites i WHERE i.owner_hex = e.owner_hex AND i.revoked_at IS NULL AND instr(i.items, e.cid) > 0) \
                       AND NOT EXISTS (SELECT 1 FROM backup_items b WHERE b.owner_hex = e.owner_hex AND b.host_hex = ?2 AND b.cid = e.cid) \
                       AND (?4 = 0 OR e.cid NOT LIKE 'bafkrei%') \
                       AND COALESCE(e.size, 0) <= ?5 \
                     ORDER BY COALESCE(e.mtime, 0) LIMIT ?3",
                )?;
                let rows = stmt.query_map(rusqlite::params![o, h, i64::try_from(limit).unwrap_or(i64::MAX), i64::from(unpacked_only), i64::try_from(room).unwrap_or(i64::MAX)], |r| Ok((r.get::<_, String>(0)?, u64::try_from(r.get::<_, i64>(1)?).unwrap_or(0))))?;
                Ok(rows.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .unwrap_or_else(|e| {
                self.activity.publish_to(Audience::Captain, "alert", "backup", format!("sauvegarde privée : fichiers à envoyer illisibles ({e})"), None);
                Vec::new()
            })
    }

    /// Le blob du manifeste uCloud de `owner` (son VFS et son trousseau, chiffrés), s'il n'est pas encore chez `host`.
    /// Sans lui, un compte dont la station a brûlé retrouverait l'événement 30078 mais pas son contenu.
    async fn manifest_item(&self, owner: &str, host: &str) -> Option<(String, u64)> {
        let (o, h) = (owner.to_owned(), host.to_owned());
        let cid: String = self
            .db
            .readers()
            .read(move |c| {
                Ok(c.query_row(
                    "SELECT k.value FROM kv k WHERE k.scope = 'ucloud_manifest_cid' AND k.key = ?1 \
                       AND NOT EXISTS (SELECT 1 FROM backup_items b WHERE b.owner_hex = ?1 AND b.host_hex = ?2 AND b.cid = k.value)",
                    [o, h],
                    |r| r.get::<_, String>(0),
                )
                .optional()?)
            })
            .await
            .ok()
            .flatten()?;
        let (_, size) = self.blobs.open(&cid).await.ok()?;
        Some((cid, size))
    }

    async fn record_item(&self, owner: &str, host: &str, cid: &str, size: u64) -> Result<(), String> {
        self.db
            .writer()
            .execute("INSERT OR IGNORE INTO backup_items(owner_hex, host_hex, cid, size, at) VALUES (?1, ?2, ?3, ?4, ?5)", vec![owner.to_owned().into(), host.to_owned().into(), cid.to_owned().into(), i64::try_from(size).unwrap_or(0).into(), now().into()])
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    async fn used(&self, owner: &str, host: &str) -> u64 {
        let (o, h) = (owner.to_owned(), host.to_owned());
        // Copies complètes et fragments comptent ensemble dans le volume accordé.
        self.db
            .readers()
            .read(move |c| {
                Ok(c.query_row(
                    "SELECT COALESCE((SELECT SUM(size) FROM backup_items WHERE owner_hex = ?1 AND host_hex = ?2), 0) + COALESCE((SELECT SUM(size) FROM backup_frags WHERE owner_hex = ?1 AND host_hex = ?2), 0)",
                    [o, h],
                    |r| r.get::<_, i64>(0),
                )?)
            })
            .await
            .map_or(0, |n| u64::try_from(n).unwrap_or(0))
    }

    /// Envoie ce qui reste à sauvegarder pour chaque propriétaire hébergé ici dont l'ami a accepté : fragments k-sur-n et index
    /// aux gardiens qui savent les garder ([`rs`]), copie complète aux autres (et à ceux de cette station), manifeste à tous.
    /// Rend le nombre de blobs (fichiers, fragments, index) envoyés.
    pub async fn run_once(&self) -> usize {
        let packed = self.rs_once(now()).await;
        let pacts: Vec<(String, String, u64)> = self
            .db
            .readers()
            .read(|c| {
                let mut stmt = c.prepare("SELECT p.owner_hex, p.host_hex, p.quota_bytes FROM backup_pacts p JOIN identities i ON i.hex = p.owner_hex AND i.hosted = 1 WHERE p.enabled = 1 AND p.status = 'accepted'")?;
                Ok(stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, u64::try_from(r.get::<_, i64>(2)?).unwrap_or(0))))?.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .unwrap_or_default();
        let mut sent = packed;
        for (owner, host, quota) in pacts {
            let local = self.hosted_here(&host).await;
            let coded = !local && self.kv_get(rs::RS, &format!("{owner}:{host}")).await.is_some();
            let label = self.label_of(&owner).await;
            let mut used = self.used(&owner, &host).await;
            let mut stored_here = false;
            let mut todo: Vec<(String, u64)> = self.manifest_item(&owner, &host).await.into_iter().collect();
            let room = quota.saturating_sub(used).saturating_sub(todo.iter().map(|(_, n)| *n).sum());
            todo.extend(self.pending_files(&owner, &host, PER_PASS, coded, room).await);
            for (cid, size) in todo {
                if used.saturating_add(size) > quota {
                    continue; // trop gros pour le volume restant : les plus petits peuvent encore passer
                }
                let result = if local {
                    adopt_backup_blob(&self.db, &host, &owner, &label, &cid, size).await.map(|_| ()).map_err(|e| e.to_string())
                } else {
                    // Compromis assumé : la déduplication zéro-copie (CID = SHA-256 du blob chiffré) prévaut sur l'anonymisation
                    // de la taille, qui reste visible de l'hébergeur.
                    let mut tags = vec![vec!["cid".to_owned(), cid.clone()], vec!["size".into(), size.to_string()], vec!["op".into(), "item".into()]];
                    if let Some(peer) = &self.station_peer {
                        tags.push(vec!["station".into(), peer.clone()]);
                    }
                    let sent = self.send_to(&owner, &host, kind::FILE, tags, &format!("ipfs://{cid}")).await;
                    // Pas de rafale de gift wraps : un premier envoi de milliers de fichiers ne doit pas saturer la box ni les relais.
                    tokio::time::sleep(SEND_PACE).await;
                    sent
                };
                match result {
                    Ok(()) => {
                        if self.record_item(&owner, &host, &cid, size).await.is_ok() {
                            used += size;
                            sent += 1;
                            stored_here |= local;
                        }
                    }
                    Err(e) => {
                        self.activity.publish_to(Audience::Captain, "alert", "backup", format!("sauvegarde privée non envoyée : {e}"), None);
                        break;
                    }
                }
            }
            if stored_here {
                if let Some(p) = &self.publisher {
                    p.notify(&host);
                }
            }
        }
        sent
    }

    /// Un passage de réception sur les comptes hébergés ici.
    pub async fn poll_once(&self, at: i64) -> BackupStats {
        let mut stats = BackupStats::default();
        let owners: Vec<String> = self
            .db
            .readers()
            .read(|c| {
                let mut stmt = c.prepare("SELECT hex FROM identities WHERE hosted = 1 AND role IN ('multipass', 'captain')")?;
                Ok(stmt.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .unwrap_or_default();
        // La clé NODE reçoit les réponses des gardiens (secours) ; un compte, tout le reste.
        let mut inboxes: Vec<(String, astro_crypto::SecretBytes)> = owners.into_iter().filter_map(|me| self.keys.secret(&me).map(|s| (me, s))).collect();
        if let Some((hex, secret)) = &self.node {
            inboxes.push((hex.clone(), secret.clone()));
        }
        self.rescue_step(at).await;
        for (me, secret) in inboxes {
            let (m, since) = (me.clone(), at - LOOKBACK_SECS);
            let wraps: Vec<Event> = self
                .db
                .readers()
                .read(move |c| {
                    let mut stmt = c.prepare_cached(
                        "SELECT e.id, e.pubkey, e.kind, e.created_at, e.content, e.sig, e.tags FROM nostr_events e \
                         JOIN nostr_tags t ON t.event_id = e.seq AND t.name = 'p' AND t.value = ?1 \
                         WHERE e.kind = 1059 AND e.created_at >= ?2 \
                           AND NOT EXISTS (SELECT 1 FROM claimed c WHERE c.owner = 'backup_inbox' AND c.event_id = e.id) \
                           AND NOT EXISTS (SELECT 1 FROM kv k WHERE k.scope = 'backup_inbox_tries' AND k.key = e.id AND CAST(substr(k.value, instr(k.value, ':') + 1) AS INTEGER) > ?3) \
                         ORDER BY e.created_at LIMIT ?4",
                    )?;
                    let mut rows = stmt.query(rusqlite::params![m, since, at, BATCH as i64])?;
                    let mut out = Vec::new();
                    while let Some(r) = rows.next()? {
                        out.push(row_to_event(r)?);
                    }
                    Ok(out)
                })
                .await
                .unwrap_or_default();
            for wrap in wraps {
                self.handle(&me, &secret, wrap, at, &mut stats).await;
            }
        }
        stats
    }

    async fn claim(&self, id: &str, at: i64) {
        let _ = self.db.writer().execute("INSERT OR IGNORE INTO claimed(owner, event_id, at) VALUES (?1, ?2, ?3)", vec![CLAIM.to_owned().into(), id.to_owned().into(), at.into()]).await;
    }

    async fn handle(&self, me: &str, secret: &[u8; 32], wrap: Event, at: i64, stats: &mut BackupStats) {
        let Ok(opened) = nip59::unwrap(secret, &wrap) else {
            self.claim(&wrap.id, at).await;
            return;
        };
        let (sender, rumor) = (opened.sender, opened.rumor);
        // Le seul message qu'un compte s'adresse à lui-même : « j'habite ailleurs ». La signature est la sienne, donc c'est lui.
        if sender == me && tag(&rumor.tags, "t") == Some(TAG) && tag(&rumor.tags, "op") == Some("moved") {
            self.moved_away(me, &rumor, at).await;
            self.claim(&wrap.id, at).await;
            stats.control += 1;
            return;
        }
        if sender == me || tag(&rumor.tags, "t") != Some(TAG) {
            self.claim(&wrap.id, at).await; // pas une sauvegarde : le partage entre amis s'en occupe
            return;
        }
        self.seen(&sender, at).await;
        // Secours : l'expéditeur n'est pas (encore) un ami connu ici ; chaque cas porte son propre contrôle.
        let to_node = self.node.as_ref().is_some_and(|(h, _)| h == me);
        match tag(&rumor.tags, "op") {
            Some(op @ ("rescue" | "rescue-closed")) if !to_node => {
                self.rescue_in(me, &sender, op, &rumor, at).await;
                self.claim(&wrap.id, at).await;
                stats.control += 1;
                return;
            }
            Some(op @ ("rescued" | "rescue-denied")) if to_node => {
                self.rescue_reply(&sender, op, &rumor).await;
                self.claim(&wrap.id, at).await;
                stats.control += 1;
                return;
            }
            _ => {}
        }
        if to_node || !self.mutual(me, &sender).await {
            self.claim(&wrap.id, at).await;
            stats.ignored += 1;
            return;
        }
        let label = self.label_of(&sender).await;
        match tag(&rumor.tags, "op") {
            // Un ami me demande de l'héberger.
            Some("request") => {
                let (m, s, t) = (me.to_owned(), sender.clone(), at);
                let _ = self
                    .db
                    .writer()
                    .execute(
                        "INSERT INTO backup_pacts(owner_hex, host_hex, status, enabled, asked_at) VALUES (?1, ?2, 'pending', 0, ?3) \
                         ON CONFLICT(owner_hex, host_hex) DO UPDATE SET status = CASE WHEN status = 'accepted' THEN status ELSE 'pending' END, asked_at = ?3",
                        vec![s.into(), m.into(), t.into()],
                    )
                    .await;
                self.claim(&wrap.id, at).await;
                self.tell(me, format!("🗄️ {label} vous demande d'héberger sa sauvegarde privée (chiffrée, illisible pour vous). Acceptez ou refusez dans Moi › Sauvegarde.")).await;
                stats.control += 1;
            }
            // Mon hébergeur répond.
            Some(op @ ("accept" | "decline" | "revoke")) => {
                let quota: i64 = tag(&rumor.tags, "quota").and_then(|q| q.parse().ok()).unwrap_or(0);
                let status = match op {
                    "accept" => "accepted",
                    "revoke" => "revoked",
                    _ => "declined",
                };
                let (m, s) = (me.to_owned(), sender.clone());
                let _ = self
                    .db
                    .writer()
                    .transaction(move |tx| {
                        tx.execute("UPDATE backup_pacts SET status = ?3, quota_bytes = ?4, decided_at = ?5 WHERE owner_hex = ?1 AND host_hex = ?2", rusqlite::params![m, s, status, quota.max(0), at])?;
                        if status != "accepted" {
                            tx.execute("DELETE FROM backup_items WHERE owner_hex = ?1 AND host_hex = ?2", [&m, &s])?;
                            tx.execute("DELETE FROM backup_frags WHERE owner_hex = ?1 AND host_hex = ?2", [&m, &s])?;
                        }
                        Ok(())
                    })
                    .await;
                // Négociation : seul un gardien qui annonce `rs` reçoit des fragments ; les autres gardent la copie complète.
                let key = format!("{me}:{sender}");
                if op == "accept" && tag(&rumor.tags, "rs") == Some("1") {
                    self.kv_put(rs::RS, &key, "1", at).await;
                } else {
                    self.kv_del(rs::RS, &key).await;
                }
                self.claim(&wrap.id, at).await;
                let text = if op == "accept" { format!("✅ {label} accepte d'héberger votre sauvegarde privée.") } else { format!("⛔ {label} n'héberge plus (ou refuse d'héberger) votre sauvegarde privée.") };
                self.tell(me, text).await;
                stats.control += 1;
            }
            // Mon ami retire des blobs dont les fichiers ont disparu chez lui depuis plus d'une semaine.
            Some("drop") => {
                let cids: Vec<String> = rumor.tags.iter().filter(|t| t.first().map(String::as_str) == Some("cid")).filter_map(|t| t.get(1).cloned()).take(100).collect();
                let (m, s) = (me.to_owned(), sender.clone());
                let accepted = self.db.readers().read(move |c| Ok(c.query_row("SELECT 1 FROM backup_pacts WHERE owner_hex = ?1 AND host_hex = ?2 AND status = 'accepted'", [s, m], |r| r.get::<_, i64>(0)).optional()?.is_some())).await.unwrap_or(false);
                if accepted {
                    self.drop_hosted(me, &sender, Some(&cids)).await;
                }
                self.claim(&wrap.id, at).await;
                stats.control += 1;
            }
            // La part P3 de la clé de secours de mon ami : je la garde, sans pouvoir rien en faire seul.
            Some("share") => {
                let accepted = {
                    let (m, s) = (me.to_owned(), sender.clone());
                    self.db.readers().read(move |c| Ok(c.query_row("SELECT 1 FROM backup_pacts WHERE owner_hex = ?1 AND host_hex = ?2 AND status = 'accepted'", [s, m], |r| r.get::<_, i64>(0)).optional()?.is_some())).await.unwrap_or(false)
                };
                // La part doit être celle de CET ami (clé publique du message) et porter le numéro 3.
                match astro_lifecycle::share::SecretShare::parse(&rumor.content) {
                    Ok(share) if accepted && share.index == 3 && share.hex == sender => {
                        self.kv_put(SHARE_HELD, &format!("{me}:{sender}"), &share.to_text(), at).await;
                        self.tell(me, format!("🔑 {label} vous a confié une part de sa clé de secours. Seule, elle ne sert à rien : s'il perd sa station, il vous la demandera : vous confirmerez d'un geste après lui avoir parlé (Moi › Sauvegarde).")).await;
                    }
                    _ => stats.ignored += 1,
                }
                self.claim(&wrap.id, at).await;
                stats.control += 1;
            }
            // Un fragment de paquet (ou l'index des paquets) à ranger, puis à acquitter.
            Some("frag") if rumor.kind == kind::FILE => self.store_frag(me, &sender, &wrap.id, &rumor, at, stats).await,
            // Mon gardien a rangé mes fragments.
            Some("frag-ack") => {
                self.frag_ack(me, &sender, &rumor, at).await;
                self.claim(&wrap.id, at).await;
                stats.control += 1;
            }
            // Mon ami retire des fragments.
            Some("frag-drop") => {
                self.frag_drop(me, &sender, &rumor).await;
                self.claim(&wrap.id, at).await;
                stats.control += 1;
            }
            // Un blob chiffré à ranger.
            Some("item") if rumor.kind == kind::FILE => self.store_item(me, &sender, &wrap.id, &rumor, at, stats).await,
            // La copie d'une pièce jointe de message, servie ici jusqu'à son échéance.
            Some("attachment") if rumor.kind == kind::FILE => self.store_replica(me, &sender, &wrap.id, &rumor, at, stats).await,
            // Mon ami, rétabli sur une autre station, demande ce que je détiens pour lui.
            Some("restore") => {
                self.accept_restore(me, &sender, &rumor, at).await;
                self.claim(&wrap.id, at).await;
                stats.control += 1;
            }
            // Mon ami me renvoie ce que je lui avais confié (ou partagé).
            Some("restored") if rumor.kind == kind::FILE => self.store_restored(me, &sender, &wrap.id, &rumor, at, stats).await,
            _ => {
                self.claim(&wrap.id, at).await;
                stats.ignored += 1;
            }
        }
    }

    /// Le blob `cid` est-il disponible ici ? Sinon le rapatrie depuis la station indiquée par l'expéditeur ; en cas d'échec,
    /// remet le message à plus tard (reculade exponentielle), ou l'écarte après [`MAX_TRIES`] essais.
    async fn ensure_blob(&self, cid: &str, rumor: &Rumor, wrap_id: &str, at: i64, stats: &mut BackupStats) -> bool {
        self.ensure_blob_with(self.prefetch.as_ref(), cid, rumor, wrap_id, at, stats).await
    }

    async fn ensure_blob_with(&self, prefetch: Option<&Prefetch>, cid: &str, rumor: &Rumor, wrap_id: &str, at: i64, stats: &mut BackupStats) -> bool {
        if self.blobs.open(cid).await.is_err() {
            if let (Some(peer), Some(prefetch)) = (tag(&rumor.tags, "station"), prefetch) {
                let _ = prefetch(cid.to_owned(), peer.to_owned()).await;
            }
        }
        if self.blobs.open(cid).await.is_err() {
            let id = wrap_id.to_owned();
            let tries = self.db.readers().read(move |c| Ok(c.query_row("SELECT value FROM kv WHERE scope = ?1 AND key = ?2", [TRIES, id.as_str()], |r| r.get::<_, String>(0)).optional()?)).await.ok().flatten().and_then(|v| v.split(':').next().and_then(|n| n.parse::<u32>().ok())).unwrap_or(0) + 1;
            if tries >= MAX_TRIES {
                self.claim(wrap_id, at).await;
                stats.ignored += 1;
            } else {
                let _ = self
                    .db
                    .writer()
                    .execute("INSERT OR REPLACE INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4)", vec![TRIES.to_owned().into(), wrap_id.to_owned().into(), format!("{tries}:{}", at + backoff(tries)).into(), at.into()])
                    .await;
                stats.deferred += 1;
            }
            return false;
        }
        true
    }

    async fn store_item(&self, me: &str, sender: &str, wrap_id: &str, rumor: &Rumor, at: i64, stats: &mut BackupStats) {
        let (Some(cid), Some(size)) = (tag(&rumor.tags, "cid"), tag(&rumor.tags, "size").and_then(|s| s.parse::<u64>().ok())) else {
            self.claim(wrap_id, at).await;
            stats.ignored += 1;
            return;
        };
        let (m, s) = (me.to_owned(), sender.to_owned());
        let pact: Option<(String, i64)> = self.db.readers().read(move |c| Ok(c.query_row("SELECT status, quota_bytes FROM backup_pacts WHERE owner_hex = ?1 AND host_hex = ?2", [&s, &m], |r| Ok((r.get(0)?, r.get(1)?))).optional()?)).await.ok().flatten();
        let quota = match pact {
            Some((status, q)) if status == "accepted" => u64::try_from(q).unwrap_or(0),
            _ => 0,
        };
        if !astro_dav::blob::is_cid(cid) || size > MAX_BYTES || self.used(sender, me).await.saturating_add(size) > quota {
            self.claim(wrap_id, at).await; // hors accord ou volume dépassé : refusé, sans bruit
            stats.ignored += 1;
            return;
        }
        if !self.ensure_blob(cid, rumor, wrap_id, at, stats).await {
            return;
        }
        let label = self.label_of(sender).await;
        match adopt_backup_blob(&self.db, me, sender, &label, cid, size).await {
            Ok(_) => {
                let _ = self.record_item(sender, me, cid, size).await;
                if let Some(p) = &self.publisher {
                    p.notify(me);
                }
                self.claim(wrap_id, at).await;
                stats.stored += 1;
            }
            Err(e) => {
                self.activity.publish_to(Audience::Captain, "alert", "backup", format!("sauvegarde d'un ami non rangée : {e}"), None);
                stats.deferred += 1;
            }
        }
    }

    /// Volume que `host` met à disposition de `owner` s'il a accepté (octets), sinon 0.
    async fn pact_quota(&self, owner: &str, host: &str) -> u64 {
        let (o, h) = (owner.to_owned(), host.to_owned());
        let pact: Option<(String, i64)> = self.db.readers().read(move |c| Ok(c.query_row("SELECT status, quota_bytes FROM backup_pacts WHERE owner_hex = ?1 AND host_hex = ?2", [&o, &h], |r| Ok((r.get(0)?, r.get(1)?))).optional()?)).await.ok().flatten();
        match pact {
            Some((status, q)) if status == "accepted" => u64::try_from(q).unwrap_or(0),
            _ => 0,
        }
    }

    /// Côté hébergeur : une copie de pièce jointe de `sender`. Mêmes garde-fous que [`Self::store_item`] (accord accepté, volume
    /// du pacte, blob vérifié par son CID), plus : octets chiffrés (UENC v2, jamais de clair), taille et durée bornées. Elle compte
    /// dans le volume accordé et disparaît d'elle-même à l'échéance (purge des liens), sans la semaine de grâce des sauvegardes.
    async fn store_replica(&self, me: &str, sender: &str, wrap_id: &str, rumor: &Rumor, at: i64, stats: &mut BackupStats) {
        let expires = tag(&rumor.tags, "expires").and_then(|e| e.parse::<i64>().ok());
        let (Some(cid), Some(expires)) = (tag(&rumor.tags, "cid"), expires) else {
            self.claim(wrap_id, at).await;
            stats.ignored += 1;
            return;
        };
        let quota = self.pact_quota(sender, me).await;
        // Avant tout rapatriement : accord accepté, durée raisonnable, et la taille annoncée tient dans le volume restant.
        let announced = tag(&rumor.tags, "size").and_then(|n| n.parse::<u64>().ok()).unwrap_or(u64::MAX);
        let used_before = self.used(sender, me).await.saturating_add(astro_dav::share::replica_bytes(&self.db, sender, at).await);
        if !astro_dav::blob::is_cid(cid) || expires <= at || expires > at + astro_dav::share::ATTACHMENT_MAX_TTL || announced > astro_dav::share::ATTACHMENT_MAX as u64 || used_before.saturating_add(announced) > quota {
            self.claim(wrap_id, at).await;
            stats.ignored += 1;
            return;
        }
        let existed = self.blobs.open(cid).await.is_ok();
        if !self.ensure_blob_with(self.attachment_prefetch.as_ref().or(self.prefetch.as_ref()), cid, rumor, wrap_id, at, stats).await {
            return;
        }
        // La taille et la forme sont celles du blob reçu, jamais celles que l'expéditeur annonce.
        let verdict = async {
            use tokio::io::AsyncReadExt as _;
            let (mut reader, size) = self.blobs.open(cid).await.map_err(|e| e.to_string())?;
            let mut head = [0u8; 6];
            reader.read_exact(&mut head).await.map_err(|e| e.to_string())?;
            if !astro_dav::share::is_uenc_v2(&head) {
                return Err("pas une pièce jointe chiffrée".to_owned());
            }
            if size > astro_dav::share::ATTACHMENT_MAX as u64 {
                return Err("trop lourde".to_owned());
            }
            Ok(size)
        }
        .await;
        let refuse = |why: String| {
            self.activity.publish_to(Audience::Captain, "alert", "backup", format!("copie de pièce jointe refusée : {why}"), None);
        };
        let size = match verdict {
            Ok(size) => size,
            Err(why) => {
                if !existed {
                    let _ = self.blobs.remove(cid).await; // seulement ce que ce message vient d'apporter
                }
                refuse(why);
                self.claim(wrap_id, at).await;
                stats.ignored += 1;
                return;
            }
        };
        if used_before.saturating_add(size) > quota {
            if !existed {
                let _ = self.blobs.remove(cid).await;
            }
            self.claim(wrap_id, at).await; // hors accord ou volume dépassé : refusé, sans bruit
            stats.ignored += 1;
            return;
        }
        match astro_dav::share::record_attachment(&self.db, sender, "replica", cid, size, expires, at).await {
            Ok(_) => {
                self.claim(wrap_id, at).await;
                stats.stored += 1;
            }
            Err(e) => {
                self.activity.publish_to(Audience::Captain, "alert", "backup", format!("copie de pièce jointe non rangée : {e}"), None);
                stats.deferred += 1;
            }
        }
    }

    /// Côté propriétaire : copie tout de suite la pièce jointe `cid` (déjà conservée ici jusqu'à `expires_at`) chez les amis qui
    /// hébergent sa sauvegarde (ceux qui gardent sa part P3), avec l'échéance. Rend le nombre de copies parties ou rangées.
    /// Un hébergeur sans volume ou hors ligne ne retarde rien : la pièce jointe reste servie par cette station.
    pub async fn replicate_attachment(&self, owner: &str, cid: &str, size: u64, expires_at: i64) -> usize {
        // Trois amis au plus : le but est la disponibilité, pas de répandre le fichier.
        let o = owner.to_owned();
        let hosts: Vec<String> = self
            .db
            .readers()
            .read(move |c| {
                let mut stmt = c.prepare("SELECT host_hex FROM backup_pacts WHERE owner_hex = ?1 AND status = 'accepted' ORDER BY decided_at LIMIT 3")?;
                Ok(stmt.query_map([&o], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .unwrap_or_default();
        let mut done = 0;
        for host in hosts {
            if self.hosted_here(&host).await {
                continue; // même station : le blob est déjà servi ici, une copie n'ajouterait aucune disponibilité
            }
            let mut tags = vec![vec!["cid".to_owned(), cid.to_owned()], vec!["size".into(), size.to_string()], vec!["expires".into(), expires_at.to_string()], vec!["op".into(), "attachment".into()]];
            if let Some(peer) = &self.station_peer {
                tags.push(vec!["station".into(), peer.clone()]);
            }
            if self.send_to(owner, &host, kind::FILE, tags, &format!("ipfs://{cid}")).await.is_ok() {
                done += 1;
            }
        }
        done
    }

    // ───────────────────────────── Restauration ─────────────────────────────

    async fn kv_get(&self, scope: &'static str, key: &str) -> Option<(String, i64)> {
        let key = key.to_owned();
        self.db
            .readers()
            .read(move |c| Ok(c.query_row("SELECT value, updated_at FROM kv WHERE scope = ?1 AND key = ?2", rusqlite::params![scope, key], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))).optional()?))
            .await
            .ok()
            .flatten()
    }

    async fn kv_put(&self, scope: &'static str, key: &str, value: &str, at: i64) {
        let stored = self
            .db
            .writer()
            .execute(
                "INSERT INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                vec![scope.to_owned().into(), key.to_owned().into(), value.to_owned().into(), at.into()],
            )
            .await;
        if let Err(e) = stored {
            self.activity.publish_to(Audience::Captain, "alert", "backup", format!("restauration : état « {scope} » non enregistré ({e})"), None);
        }
    }

    /// Dernier événement de `owner` de ce kind (et de ce tag `d`), tel que le crawl l'a rapatrié.
    async fn latest_event(&self, owner: &str, k: u32, d: Option<&str>) -> Option<Event> {
        let (o, d) = (owner.to_owned(), d.map(str::to_owned));
        self.db
            .readers()
            .read(move |c| {
                let mut stmt = c.prepare_cached("SELECT e.id, e.pubkey, e.kind, e.created_at, e.content, e.sig, e.tags FROM nostr_events e WHERE e.pubkey = ?1 AND e.kind = ?2 AND (?3 IS NULL OR e.d_tag = ?3) ORDER BY e.created_at DESC LIMIT 1")?;
                let mut rows = stmt.query(rusqlite::params![o, k, d])?;
                Ok(match rows.next()? {
                    Some(r) => Some(row_to_event(r)?),
                    None => None,
                })
            })
            .await
            .ok()
            .flatten()
    }

    /// Le compte `hex` s'est montré aujourd'hui (message de sauvegarde reçu de lui).
    async fn seen(&self, hex: &str, at: i64) {
        if let Err(e) = self.db.writer().execute("INSERT OR IGNORE INTO guardian_presence(hex, day) VALUES (?1, ?2)", vec![hex.to_owned().into(), (at / 86_400).into()]).await {
            self.activity.publish_to(Audience::Captain, "alert", "backup", format!("présence d'un gardien non notée ({e})"), None);
        }
    }

    /// Relève, au plus une fois par heure, les jours où chaque gardien des comptes d'ici s'est montré : un événement signé de lui,
    /// ou une balise de santé (30850) de sa station d'accueil, rapatriés par le crawl (les deux derniers jours). La décision est
    /// prise en SQL ; les jours de plus de 60 jours sont oubliés. Rend le nombre de jours ajoutés.
    pub async fn observe_presence(&self, at: i64) -> usize {
        if self.kv_get("guardian_presence_at", "").await.is_some_and(|(_, t)| at - t < 3600 && at >= t) {
            return 0;
        }
        self.kv_put("guardian_presence_at", "", "", at).await;
        let (since, horizon) = (at - 2 * 86_400, (at / 86_400 - 60));
        let added = self
            .db
            .writer()
            .transaction(move |tx| {
                let n = tx.execute(
                    "INSERT OR IGNORE INTO guardian_presence(hex, day) \
                     SELECT g.host_hex, e.created_at / 86400 FROM (SELECT DISTINCT host_hex FROM backup_pacts WHERE status = 'accepted') g \
                       JOIN nostr_events e ON e.pubkey = g.host_hex WHERE e.created_at BETWEEN ?1 AND ?2 \
                     UNION SELECT g.host_hex, e.created_at / 86400 FROM (SELECT DISTINCT host_hex FROM backup_pacts WHERE status = 'accepted') g \
                       JOIN identities i ON i.hex = g.host_hex AND instr(i.home_station, ':') > 0 \
                       JOIN nostr_events e ON e.kind = 30850 AND e.pubkey = substr(i.home_station, instr(i.home_station, ':') + 1) \
                     WHERE e.created_at BETWEEN ?1 AND ?2",
                    rusqlite::params![since, at],
                )?;
                tx.execute("DELETE FROM guardian_presence WHERE day < ?1", [horizon])?;
                Ok(n)
            })
            .await;
        match added {
            Ok(n) => n,
            Err(e) => {
                self.activity.publish_to(Audience::Captain, "alert", "backup", format!("présence des gardiens non relevée ({e})"), None);
                0
            }
        }
    }

    /// Gardiens de `owner` : amis qui hébergent sa sauvegarde ET à qui sa part de clé a été confiée, avec leur disponibilité (‰,
    /// `None` sans assez de recul). Un gardien hébergé sur cette même station ne protège pas d'une panne de la station : `same`.
    async fn guardians(&self, owner: &str, at: i64) -> Vec<(String, bool, bool, Option<u32>)> {
        let (o, today) = (owner.to_owned(), at / 86_400);
        let rows: Vec<(String, i64, i64, i64, i64)> = self
            .db
            .readers()
            .read(move |c| {
                let mut stmt = c.prepare(
                    "SELECT p.host_hex, p.enabled, COALESCE(p.decided_at, p.asked_at), \
                            EXISTS (SELECT 1 FROM identities i WHERE i.hex = p.host_hex AND i.hosted = 1), \
                            (SELECT COUNT(*) FROM guardian_presence g WHERE g.hex = p.host_hex AND g.day > ?2 - ?3 AND g.day <= ?2) \
                     FROM backup_pacts p JOIN kv k ON k.scope = ?4 AND k.key = p.owner_hex || ':' || p.host_hex \
                     WHERE p.owner_hex = ?1 AND p.status = 'accepted' ORDER BY p.decided_at",
                )?;
                Ok(stmt
                    .query_map(rusqlite::params![o, today, crate::guardians::WINDOW_DAYS, SHARE_SENT], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
                    .collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .unwrap_or_default();
        rows.into_iter()
            .map(|(hex, enabled, since, same, seen)| {
                let p = if same == 1 { Some(0) } else { crate::guardians::availability(seen, today - since / 86_400 + 1) };
                (hex, enabled == 1, same == 1, p)
            })
            .collect()
    }

    /// Suivi de la restauration pour le membre : état, amis qui ont renvoyé, amis attendus (dernier jour vu).
    async fn restore_view(&self, owner: &str, at: i64) -> Value {
        let Some((state, since)) = self.kv_get("restore", owner).await else { return Value::Null };
        if !(state == "pending" || state == "late" || (state == "done" && at - since < RESTORE_SHOW_SECS)) {
            return Value::Null;
        }
        let (mut returned, mut waiting) = (Vec::new(), Vec::new());
        for f in self.remote_friends(owner).await {
            if self.kv_get(RETURNED, &format!("{owner}:{f}")).await.is_some() {
                returned.push(json!(self.label_of(&f).await));
            } else if state != "done" && waiting.len() < 8 {
                let h = f.clone();
                let last: Option<i64> = self.db.readers().read(move |c| Ok(c.query_row("SELECT MAX(day) FROM guardian_presence WHERE hex = ?1", [h], |r| r.get(0))?)).await.ok().flatten();
                waiting.push(json!({ "label": self.label_of(&f).await, "seen_days": last.map(|d| (at / 86_400 - d).max(0)) }));
            }
        }
        json!({ "state": state, "since": since, "returned": returned, "waiting": waiting })
    }

    /// Comptes hébergés ici dont la restauration est dans l'état `state` (`pending`, `done`).
    async fn restore_owners(&self, state: &'static str) -> Vec<String> {
        self.db
            .readers()
            .read(move |c| {
                let mut stmt = c.prepare("SELECT k.key FROM kv k JOIN identities i ON i.hex = k.key AND i.hosted = 1 WHERE k.scope = 'restore' AND k.value = ?1")?;
                Ok(stmt.query_map([state], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .unwrap_or_default()
    }

    /// Proches réciproques de `owner` hébergés ailleurs : ceux à qui écrire.
    async fn remote_friends(&self, owner: &str) -> Vec<String> {
        self.sharing.friends(owner).await.map(|f| f.into_iter().filter(|f| !f.hosted).map(|f| f.hex).collect()).unwrap_or_default()
    }

    /// Un passage côté propriétaire : pour chaque compte rétabli ici dont les sauvegardes ne sont pas encore revenues, annonce la
    /// station d'accueil, demande à chaque proche ce qu'il détient (renouvelé toutes les 6 h) et applique le manifeste dès que son
    /// blob est là ; puis redemande les fichiers qui manquent encore. Rend le nombre de messages envoyés.
    pub async fn restore_step(&self, at: i64) -> usize {
        let mut sent = 0;
        for owner in self.restore_owners("pending").await.into_iter().chain(self.restore_owners("late").await) {
            sent += self.restore_owner(&owner, at).await;
            // Les paquets se tirent dès que l'index est là, sans attendre le manifeste.
            self.restore_packets(&owner, at).await;
        }
        for owner in self.restore_owners("done").await {
            self.restore_packets(&owner, at).await;
            // Le manifeste a pu revenir avant que le DID soit rapatrié : la station d'accueil s'annonce quand même.
            self.announce_home(&owner, at).await;
            self.announce_moved(&owner, at).await;
            sent += self.restore_missing(&owner, at).await;
        }
        sent
    }

    async fn restore_owner(&self, owner: &str, at: i64) -> usize {
        self.announce_home(owner, at).await;
        self.announce_moved(owner, at).await;
        self.keep_origin(owner, at).await;
        if self.try_restore_manifest(owner, at).await {
            return 0;
        }
        let Some((state, since)) = self.kv_get("restore", owner).await else { return 0 };
        let late = state == "late";
        if !late && at - since > RESTORE_GIVE_UP_SECS {
            // Le compte repart (ses manifestes se publient de nouveau) ; l'événement d'origine, s'il a été vu, est déjà gardé.
            self.kv_put("restore", owner, "late", at).await;
            self.activity.publish_to(Audience::Captain, "alert", "backup", format!("restauration de …{} : aucun manifeste en 3 jours, compte débloqué, relance quotidienne des amis pendant 30 jours", &owner[..8.min(owner.len())]), None);
            self.tell(owner, "⏳ Vos fichiers ne sont pas encore revenus : vos amis qui les gardent sont sans doute hors ligne. Votre espace est de nouveau utilisable. La station leur redemande chaque jour pendant 30 jours ; ce qui reviendra sera ajouté à votre uCloud sans rien écraser.".to_owned()).await;
            return 0;
        }
        if late && at - since > RESTORE_LATE_SECS {
            self.kv_put("restore", owner, "abandoned", at).await;
            self.activity.publish_to(Audience::Captain, "alert", "backup", format!("restauration abandonnée pour …{} : aucun manifeste en 33 jours", &owner[..8.min(owner.len())]), None);
            self.tell(owner, "⚠️ Après un mois de relances, vos anciens fichiers n'ont pas été retrouvés chez vos amis. Votre compte, vos amis et vos contacts restent les vôtres.".to_owned()).await;
            return 0;
        }
        let last = self.kv_get("restore_asked", owner).await.map_or(0, |(_, t)| t);
        if last > 0 && at - last < if late { RESTORE_LATE_RETRY_SECS } else { RESTORE_RETRY_SECS } {
            return 0;
        }
        let mut friends = self.remote_friends(owner).await;
        if friends.is_empty() {
            return 0; // la liste d'amis n'est pas encore rapatriée par le crawl : on réessaiera au prochain passage
        }
        if late {
            // Seuls les absents sont relancés (ceux qui ont déjà renvoyé quelque chose ont tout envoyé) ; à défaut, tous.
            let mut absent = Vec::new();
            for f in &friends {
                if self.kv_get(RETURNED, &format!("{owner}:{f}")).await.is_none() {
                    absent.push(f.clone());
                }
            }
            if !absent.is_empty() {
                friends = absent;
            }
        }
        let mut tags = Vec::new();
        if let Some(home) = &self.home {
            tags.push(vec!["home".to_owned(), home.clone()]);
        }
        if let Some(pointer) = self.origin_event(owner).await.and_then(|e| manifest::open_pointer(&e, &*self.keys).ok()) {
            tags.push(vec!["cid".to_owned(), pointer.manifest_cid]);
        }
        tags.push(vec!["all".to_owned(), "1".to_owned()]);
        let mut sent = 0;
        for friend in &friends {
            match self.control(owner, friend, "restore", tags.clone()).await {
                Ok(()) => sent += 1,
                Err(e) => {
                    self.activity.publish_to(Audience::Captain, "alert", "backup", format!("restauration : demande non envoyée à …{} ({e})", &friend[..8.min(friend.len())]), None);
                }
            }
        }
        if sent > 0 {
            self.kv_put("restore_asked", owner, &sent.to_string(), at).await;
        }
        sent
    }

    /// Garde l'événement de manifeste d'origine dès que le crawl l'a rapatrié et tant que rien n'a été publié ici : quand le compte
    /// publiera le sien (après 3 jours), l'événement remplaçable d'origine disparaîtrait, et avec lui la clé du manifeste qu'un ami
    /// revenu tard renverra. Il reste chiffré vers le compte, comme sur le réseau.
    async fn keep_origin(&self, owner: &str, at: i64) {
        if self.kv_get("restore", owner).await.is_none_or(|(s, _)| s != "pending") || self.kv_get(ORIGIN, owner).await.is_some() {
            return;
        }
        if let Some(event) = self.latest_event(owner, kind::APP_DATA, Some(manifest::D_TAG)).await {
            match serde_json::to_string(&event) {
                Ok(text) => self.kv_put(ORIGIN, owner, &text, at).await,
                Err(e) => {
                    self.activity.publish_to(Audience::Captain, "alert", "backup", format!("restauration : manifeste d'origine non gardé ({e})"), None);
                }
            }
        }
    }

    /// Manifeste d'origine du compte : celui gardé par [`Self::keep_origin`], sinon le dernier vu tant que rien n'a été publié ici.
    async fn origin_event(&self, owner: &str) -> Option<Event> {
        if let Some(event) = self.kv_get(ORIGIN, owner).await.and_then(|(v, _)| serde_json::from_str::<Event>(&v).ok()) {
            return Some(event);
        }
        let event = self.latest_event(owner, kind::APP_DATA, Some(manifest::D_TAG)).await?;
        let mine = self.kv_get("ucloud_manifest_cid", owner).await.map(|(v, _)| v);
        // Un manifeste publié par cette station-ci n'a rien à rendre.
        match manifest::open_pointer(&event, &*self.keys) {
            Ok(p) if Some(&p.manifest_cid) != mine.as_ref() => Some(event),
            _ => None,
        }
    }

    /// Applique le manifeste d'origine dès que son blob est revenu. Rend `true` quand la restauration n'a plus rien à attendre.
    /// Un uCloud encore vide le reçoit tel quel ; sinon (le membre a déjà ajouté des fichiers, ou le manifeste arrive après les
    /// 3 jours) il est **fusionné** sans rien écraser (`manifest::merge_into`), ce qui est rejouable.
    async fn try_restore_manifest(&self, owner: &str, at: i64) -> bool {
        let Some((state, _)) = self.kv_get("restore", owner).await else { return false };
        if state != "pending" && state != "late" {
            return false;
        }
        let Some(event) = self.origin_event(owner).await else { return false };
        let Ok(pointer) = manifest::open_pointer(&event, &*self.keys) else { return false };
        if self.blobs.open(&pointer.manifest_cid).await.is_err() {
            return false;
        }
        let o = owner.to_owned();
        let files: i64 = self.db.readers().read(move |c| Ok(c.query_row("SELECT COUNT(*) FROM ucloud_entries WHERE owner_hex = ?1 AND type = 'file'", [o], |r| r.get(0))?)).await.unwrap_or(0);
        if files == 0 && state == "pending" {
            return match manifest::restore_from_event(&self.db, &*self.blobs, &*self.keys, &event).await {
                Ok(Pointer { manifest_cid, rev, files, .. }) => {
                    self.kv_put("ucloud_manifest_rev", owner, &rev.to_string(), at).await;
                    self.kv_put("ucloud_manifest_cid", owner, &manifest_cid, at).await;
                    self.finish_restore(owner, at).await;
                    self.tell(owner, format!("✅ Votre compte est rétabli : {files} fichier(s) retrouvé(s) dans votre uCloud. Les fichiers eux-mêmes reviennent de chez vos proches au fil des heures.")).await;
                    true
                }
                Err(e) => {
                    self.activity.publish_to(Audience::Captain, "alert", "backup", format!("restauration de …{} : manifeste illisible ({e})", &owner[..8.min(owner.len())]), None);
                    false
                }
            };
        }
        match manifest::merge_from_event(&self.db, &*self.blobs, &*self.keys, &event).await {
            Ok((pointer, merged)) => {
                // La prochaine révision publiée ici doit dépasser celle d'origine.
                let rev = self.kv_get("ucloud_manifest_rev", owner).await.and_then(|(v, _)| v.parse::<u64>().ok()).unwrap_or(0).max(pointer.rev);
                self.kv_put("ucloud_manifest_rev", owner, &rev.to_string(), at).await;
                self.finish_restore(owner, at).await;
                let renamed = if merged.renamed > 0 { format!(" ; {} élément(s) dont la place était prise ont été gardés sous le nom « … (ancien) »", merged.renamed) } else { String::new() };
                self.tell(owner, format!("✅ Vos anciens fichiers sont revenus : {} fichier(s) ajouté(s) à votre uCloud, sans rien écraser{renamed}. Ils se remplissent au fil des heures.", merged.added + merged.renamed)).await;
                true
            }
            Err(e) => {
                self.activity.publish_to(Audience::Captain, "alert", "backup", format!("restauration de …{} : fusion du manifeste impossible ({e})", &owner[..8.min(owner.len())]), None);
                false
            }
        }
    }

    /// Manifeste appliqué : la restauration est close, la publication reprend et les fichiers manquants sont redemandés.
    async fn finish_restore(&self, owner: &str, at: i64) {
        self.kv_put("restore_missing", owner, "0", at).await;
        self.kv_put("restore", owner, "done", at).await;
        self.kv_del(ORIGIN, owner).await;
        if let Some(p) = &self.publisher {
            p.notify(owner);
        }
    }

    /// Annonce la station d'accueil en republiant le DID du compte (le crawl la propage à toutes les stations, même celles dont
    /// l'ami est hors ligne) : `home_station`, tag que le projecteur lit sur le DID.
    async fn announce_home(&self, owner: &str, at: i64) {
        let (Some(home), Some(secret)) = (self.home.as_deref().filter(|h| valid_home(h)), self.keys.secret(owner)) else { return };
        if self.kv_get("restore_home", owner).await.is_some() {
            return;
        }
        let Some(did) = self.latest_event(owner, kind::DID, Some("did")).await else { return }; // pas encore rapatrié : plus tard
        // L'ancienne station d'accueil, avant que la nouvelle ne l'écrase : c'est à elle que s'adresse le message `moved`.
        let o = owner.to_owned();
        let old_home: Option<String> = self.db.readers().read(move |c| Ok(c.query_row("SELECT home_station FROM identities WHERE hex = ?1", [o], |r| r.get::<_, Option<String>>(0)).optional()?.flatten())).await.ok().flatten();
        if let Some(old) = old_home.filter(|h| valid_home(h) && h != home) {
            self.kv_put("restore_old_home", owner, &old, at).await;
        }
        let mut doc: Value = serde_json::from_str(&did.content).unwrap_or(Value::Null);
        if let Some(meta) = doc.get_mut("metadata").and_then(Value::as_object_mut) {
            meta.insert("astroportStation".to_owned(), json!(home));
        }
        let mut tags: Vec<Vec<String>> = did.tags.iter().filter(|t| t.first().map(String::as_str) != Some("home_station")).cloned().collect();
        tags.push(vec!["home_station".to_owned(), home.to_owned()]);
        let created_at = u64::try_from(at).unwrap_or(0).max(did.created_at + 1);
        let event = match (UnsignedEvent { created_at, kind: kind::DID, tags, content: doc.to_string() }).sign(&secret) {
            Ok(e) => e,
            Err(e) => {
                self.activity.publish_to(Audience::Captain, "alert", "backup", format!("restauration : DID non signé ({e})"), None);
                return;
            }
        };
        let stored = event.clone();
        let local = self
            .db
            .writer()
            .transaction(move |tx| {
                astro_db::events::insert_event(tx, &stored)?;
                astro_db::apply_statements(tx, &astro_sync::Projector::plan(&stored).map_err(|e| astro_db::DbError::Corrupt(e.to_string()))?)?;
                Ok(())
            })
            .await;
        if let Err(e) = local {
            self.activity.publish_to(Audience::Captain, "alert", "backup", format!("restauration : nouvelle station d'accueil non enregistrée ({e})"), None);
            return;
        }
        if let Some(publish) = &self.publish {
            if let Err(e) = publish(event).await {
                self.activity.publish_to(Audience::Captain, "alert", "backup", format!("restauration : nouvelle station d'accueil non publiée ({e})"), None);
                return;
            }
        }
        self.kv_put("restore_home", owner, home, at).await;
    }

    /// Dit à l'**ancienne** station d'accueil que le compte l'a quittée : un message `moved` que le compte s'adresse à lui-même,
    /// donc signé de sa clé, acheminé vers l'ancienne Home (encore inscrite dans la base à cet instant). Si cette station est
    /// détruite, il n'y a personne à prévenir et l'échec reste visible dans l'état ; sinon elle cesse d'héberger.
    async fn announce_moved(&self, owner: &str, at: i64) {
        let (Some((old, _)), Some(home)) = (self.kv_get("restore_old_home", owner).await, self.home.clone()) else { return };
        if self.kv_get("restore_moved", owner).await.is_some_and(|(state, t)| state == "sent" || at - t < RESTORE_RETRY_SECS) {
            return;
        }
        let state = match self.control(owner, owner, "moved", vec![vec!["home".to_owned(), home]]).await {
            Ok(()) => "sent".to_owned(),
            Err(e) => format!("injoignable ({old}) : {e}"), // station en panne, ou hors ligne : nouvel essai dans 6 h
        };
        self.kv_put("restore_moved", owner, &state, at).await;
    }

    /// Côté ancienne station : le compte `me` a écrit, de sa propre clé, qu'il habite désormais ailleurs. Il cesse d'être hébergé
    /// ici (plus de connexion, de sauvegarde ni de crawl pour lui) et son dossier est mis de côté, jamais effacé.
    async fn moved_away(&self, me: &str, rumor: &Rumor, at: i64) {
        let Some(home) = tag(&rumor.tags, "home").filter(|h| valid_home(h)) else { return };
        if self.home.as_deref() == Some(home) {
            return; // c'est cette station : rien à quitter
        }
        // Rejeu d'un ancien message après un nouveau rétablissement ici : seul un message postérieur compte.
        if self.kv_get("restore", me).await.is_some_and(|(_, since)| i64::try_from(rumor.created_at).unwrap_or(0) <= since) {
            return;
        }
        let m = me.to_owned();
        let row: Option<(String, Option<String>)> = self.db.readers().read(move |c| Ok(c.query_row("SELECT role, email FROM identities WHERE hex = ?1 AND hosted = 1", [m], |r| Ok((r.get(0)?, r.get(1)?))).optional()?)).await.ok().flatten();
        let Some((role, email)) = row else { return };
        if role != "multipass" {
            // Le Capitaine tient la station : un message ne le débarque pas, le Capitaine tranche.
            self.activity.publish_to(Audience::Captain, "alert", "backup", format!("{role} …{} annonce un déménagement vers {home} : non appliqué, à traiter à la main", &me[..8.min(me.len())]), None);
            return;
        }
        let (m, h) = (me.to_owned(), home.to_owned());
        if let Err(e) = self.db.writer().execute("UPDATE identities SET hosted = 0, home_station = ?2 WHERE hex = ?1 AND hosted = 1", vec![m.into(), h.into()]).await {
            self.activity.publish_to(Audience::Captain, "alert", "backup", format!("déménagement de …{} non enregistré ({e})", &me[..8.min(me.len())]), None);
            return;
        }
        self.kv_put("moved", me, home, at).await;
        let label = email.clone().unwrap_or_else(|| me.chars().take(8).collect());
        let mut aside = String::new();
        if let (Some(dir), Some(email)) = (self.accounts_dir.clone(), email) {
            // Dossier rangé dans la corbeille de la station (`trash/moved/<email>.<date>`, effacée après 30 jours).
            let game = dir.parent().map_or_else(|| dir.clone(), std::path::Path::to_path_buf);
            let from = dir.join(&email);
            let moved = tokio::task::spawn_blocking(move || crate::trash::put(&game, "moved", &email, &from, at)).await;
            match moved {
                Ok(Ok(target)) => aside = format!(" ; dossier mis de côté dans {}", target.display()),
                Ok(Err(e)) => {
                    self.activity.publish_to(Audience::Captain, "alert", "backup", format!("déménagement de {label} : dossier du compte non déplacé ({e}), à ranger à la main"), None);
                }
                Err(e) => {
                    self.activity.publish_to(Audience::Captain, "alert", "backup", format!("déménagement de {label} : dossier du compte non déplacé ({e})"), None);
                }
            }
        }
        self.activity.publish_to(Audience::Captain, "system", "backup", format!("{label} a déménagé vers {home} : cette station n'héberge plus son compte{aside}"), None);
    }

    /// Après le manifeste : redemande par leur CID les fichiers dont le blob manque encore (ceux que des amis ont partagés avec le
    /// membre, jamais sauvegardés chez un hôte). Un ami ne rend que ce qu'il a lui-même partagé.
    async fn restore_missing(&self, owner: &str, at: i64) -> usize {
        let Some((rounds, last)) = self.kv_get("restore_missing", owner).await else { return 0 };
        let rounds: u32 = rounds.parse().unwrap_or(RESTORE_MISSING_ROUNDS);
        if rounds >= RESTORE_MISSING_ROUNDS || at - last < RESTORE_RETRY_SECS {
            return 0;
        }
        let o = owner.to_owned();
        let candidates: Vec<String> = self
            .db
            .readers()
            .read(move |c| {
                let mut stmt = c.prepare("SELECT DISTINCT cid FROM ucloud_entries WHERE owner_hex = ?1 AND type = 'file' AND cid IS NOT NULL ORDER BY cid LIMIT 2000")?;
                Ok(stmt.query_map([o], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .unwrap_or_default();
        let mut missing = Vec::new();
        for cid in candidates {
            if self.blobs.open(&cid).await.is_err() {
                missing.push(cid);
                if missing.len() >= RESTORE_WANT {
                    break;
                }
            }
        }
        if missing.is_empty() {
            self.kv_put("restore_missing", owner, &RESTORE_MISSING_ROUNDS.to_string(), at).await;
            return 0;
        }
        let tags: Vec<Vec<String>> = missing.iter().map(|c| vec!["cid".to_owned(), c.clone()]).collect();
        let mut sent = 0;
        for friend in self.remote_friends(owner).await {
            if self.control(owner, &friend, "restore", tags.clone()).await.is_ok() {
                sent += 1;
            }
        }
        self.kv_put("restore_missing", owner, &(rounds + 1).to_string(), at).await;
        sent
    }

    /// Côté ami : `owner` rétabli sur une autre station demande ce que je détiens pour lui. Aucune action de ma part : j'enregistre
    /// sa nouvelle station d'accueil (jamais pour un compte hébergé ici) et je programme le renvoi.
    async fn accept_restore(&self, me: &str, owner: &str, rumor: &Rumor, at: i64) {
        if let Some(home) = tag(&rumor.tags, "home").filter(|h| valid_home(h)) {
            let (o, h) = (owner.to_owned(), home.to_owned());
            let updated = self
                .db
                .writer()
                .execute(
                    "INSERT INTO identities(hex, role, home_station, hosted) VALUES (?1, 'multipass', ?2, 0) ON CONFLICT(hex) DO UPDATE SET home_station = excluded.home_station WHERE hosted = 0",
                    vec![o.into(), h.into()],
                )
                .await;
            if let Err(e) = updated {
                self.activity.publish_to(Audience::Captain, "alert", "backup", format!("restauration : station d'accueil de …{} non enregistrée ({e})", &owner[..8.min(owner.len())]), None);
            }
        }
        if self.hosted_here(owner).await {
            return; // même station : rien à renvoyer, tout est déjà ici
        }
        let all = tag(&rumor.tags, "all") == Some("1");
        let mut priority = Vec::new();
        for cid in rumor.tags.iter().filter(|t| t.first().map(String::as_str) == Some("cid")).filter_map(|t| t.get(1)).take(RESTORE_WANT) {
            if astro_dav::blob::is_cid(cid) && self.held(me, owner, cid).await.is_some() {
                priority.push(cid.clone());
            }
        }
        if !all && priority.is_empty() {
            return;
        }
        let key = format!("{me}:{owner}");
        let mut job = match self.kv_get(JOB, &key).await.and_then(|(v, _)| serde_json::from_str::<Value>(&v).ok()) {
            // Demande renouvelée alors qu'un renvoi est en cours : on le complète sans le recommencer.
            Some(mut old) => {
                let mut merged: Vec<String> = old["priority"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect()).unwrap_or_default();
                let fresh: Vec<String> = priority.iter().filter(|c| !merged.contains(c)).cloned().collect();
                merged.extend(fresh);
                old["priority"] = json!(merged);
                old["all"] = json!(all || old["all"].as_bool().unwrap_or(false));
                old
            }
            None => json!({ "priority": priority, "all": all, "cursor": "" }),
        };
        job["asked_at"] = json!(at);
        self.kv_put(JOB, &key, &job.to_string(), at).await;
    }

    /// Ce que je détiens pour `owner` sous ce CID, et pourquoi je peux le lui rendre : une sauvegarde qu'il m'a confiée, un fichier
    /// qu'il m'a partagé, ou un fichier que j'ai moi-même partagé (connaître son CID en est la preuve). Le blob doit être là.
    async fn held(&self, host: &str, owner: &str, cid: &str) -> Option<(u64, &'static str)> {
        let (h, o, c) = (host.to_owned(), owner.to_owned(), cid.to_owned());
        let found = self
            .db
            .readers()
            .read(move |conn| {
                let backup = conn.query_row("SELECT size FROM backup_items WHERE owner_hex = ?1 AND host_hex = ?2 AND cid = ?3", [&o, &h, &c], |r| r.get::<_, i64>(0)).optional()?;
                if let Some(size) = backup {
                    return Ok(Some((size, "backup")));
                }
                let frag = conn.query_row("SELECT size FROM backup_frags WHERE owner_hex = ?1 AND host_hex = ?2 AND cid = ?3 AND acked_at IS NOT NULL", [&o, &h, &c], |r| r.get::<_, i64>(0)).optional()?;
                if let Some(size) = frag {
                    return Ok(Some((size, "frag")));
                }
                let shared = conn
                    .query_row(
                        "SELECT COALESCE(e.size, 0) FROM ucloud_entries e WHERE e.owner_hex = ?2 AND e.cid = ?3 AND e.type = 'file' AND e.path NOT LIKE '/Sauvegardes/%' \
                           AND (json_extract(e.data, '$.shared_by') = ?1 OR EXISTS (SELECT 1 FROM ucloud_sharing s WHERE s.owner_hex = e.owner_hex AND s.cid = e.cid)) LIMIT 1",
                        [&o, &h, &c],
                        |r| r.get::<_, i64>(0),
                    )
                    .optional()?;
                Ok(shared.map(|size| (size, "share")))
            })
            .await
            .ok()
            .flatten()?;
        self.blobs.open(cid).await.ok()?;
        Some((u64::try_from(found.0).unwrap_or(0), found.1))
    }

    async fn send_restored(&self, host: &str, owner: &str, cid: &str, size: u64, quota: i64, src: &str) -> Result<(), String> {
        let mut tags = vec![vec!["cid".to_owned(), cid.to_owned()], vec!["size".into(), size.to_string()], vec!["op".into(), "restored".into()], vec!["src".into(), src.to_owned()], vec!["quota".into(), quota.to_string()]];
        if let Some(peer) = &self.station_peer {
            tags.push(vec!["station".into(), peer.clone()]);
        }
        // L'index des paquets se reconnaît à la restauration ; `rs` : cette station sait garder des fragments.
        if self.kv_get(rs::INDEX_HELD, &format!("{host}:{owner}")).await.is_some_and(|(v, _)| v == cid) {
            tags.push(vec!["kind".into(), "index".into()]);
        }
        tags.push(vec!["rs".into(), "1".into()]);
        let sent = self.send_to(host, owner, kind::FILE, tags, &format!("ipfs://{cid}")).await;
        tokio::time::sleep(SEND_PACE).await; // pas de rafale de gift wraps
        sent
    }

    /// Côté ami : avance les renvois programmés par [`Self::accept_restore`], par petits lots. Rend le nombre de blobs envoyés.
    pub async fn serve_restores(&self) -> usize {
        let jobs: Vec<(String, String)> = self
            .db
            .readers()
            .read(|c| {
                let mut stmt = c.prepare("SELECT key, value FROM kv WHERE scope = ?1")?;
                Ok(stmt.query_map([JOB], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .unwrap_or_default();
        let mut sent = 0;
        for (key, value) in jobs {
            let Some((host, owner)) = key.split_once(':') else { continue };
            let Ok(mut job) = serde_json::from_str::<Value>(&value) else {
                let _ = self.db.writer().execute("DELETE FROM kv WHERE scope = ?1 AND key = ?2", vec![JOB.to_owned().into(), key.clone().into()]).await;
                continue;
            };
            let (h, o) = (host.to_owned(), owner.to_owned());
            let quota: i64 = self.db.readers().read(move |c| Ok(c.query_row("SELECT quota_bytes FROM backup_pacts WHERE owner_hex = ?2 AND host_hex = ?1 AND status = 'accepted'", [h, o], |r| r.get(0)).optional()?)).await.ok().flatten().unwrap_or(0);
            let all = job["all"].as_bool().unwrap_or(false);
            let mut priority: Vec<String> = job["priority"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect()).unwrap_or_default();
            let later = priority.split_off(RESTORE_PER_PASS.min(priority.len()));
            // CID prioritaires déjà renvoyés : le lot « tout le reste » ne les renvoie pas une seconde fois.
            let mut skip: Vec<String> = job["skip"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect()).unwrap_or_default();
            let mut failed = false;
            for (n, cid) in priority.iter().enumerate() {
                let Some((size, src)) = self.held(host, owner, cid).await else { continue };
                if let Err(e) = self.send_restored(host, owner, cid, size, quota, src).await {
                    self.activity.publish_to(Audience::Captain, "alert", "backup", format!("restauration d'un ami : envoi impossible ({e})"), None);
                    priority.drain(..n); // reprend au CID qui a échoué
                    failed = true;
                    break;
                }
                skip.push(cid.clone());
                sent += 1;
            }
            let mut rest = if failed { priority } else { Vec::new() };
            rest.extend(later);
            job["priority"] = json!(rest);
            job["skip"] = json!(skip);
            if !failed && rest_is_empty(&job) && all {
                let cursor = job["cursor"].as_str().unwrap_or_default().to_owned();
                let (h, o) = (host.to_owned(), owner.to_owned());
                let batch: Vec<(String, i64, String)> = self
                    .db
                    .readers()
                    .read(move |c| {
                        let mut stmt = c.prepare(
                            "SELECT cid, size, src FROM (\
                               SELECT cid, size, 'backup' AS src FROM backup_items WHERE owner_hex = ?1 AND host_hex = ?2 \
                               UNION \
                               SELECT cid, COALESCE(size, 0), 'share' FROM ucloud_entries WHERE owner_hex = ?2 AND type = 'file' AND cid IS NOT NULL AND path NOT LIKE '/Sauvegardes/%' AND json_extract(data, '$.shared_by') = ?1\
                             ) WHERE cid > ?3 ORDER BY cid LIMIT ?4",
                        )?;
                        Ok(stmt.query_map(rusqlite::params![o, h, cursor, RESTORE_PER_PASS as i64], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?)))?.collect::<Result<Vec<_>, _>>()?)
                    })
                    .await
                    .unwrap_or_default();
                if batch.is_empty() {
                    let _ = self.db.writer().execute("DELETE FROM kv WHERE scope = ?1 AND key = ?2", vec![JOB.to_owned().into(), key.clone().into()]).await;
                    continue;
                }
                for (cid, size, src) in batch {
                    if !skip.contains(&cid) && self.blobs.open(&cid).await.is_ok() {
                        let src: &str = if src == "backup" { "backup" } else { "share" };
                        if let Err(e) = self.send_restored(host, owner, &cid, u64::try_from(size).unwrap_or(0), quota, src).await {
                            self.activity.publish_to(Audience::Captain, "alert", "backup", format!("restauration d'un ami : envoi impossible ({e})"), None);
                            break;
                        }
                        sent += 1;
                    }
                    job["cursor"] = json!(cid);
                }
            } else if !failed && rest_is_empty(&job) {
                let _ = self.db.writer().execute("DELETE FROM kv WHERE scope = ?1 AND key = ?2", vec![JOB.to_owned().into(), key.clone().into()]).await;
                continue;
            }
            self.kv_put(JOB, &key, &job.to_string(), now()).await;
        }
        sent
    }

    /// Côté propriétaire : un blob que mon ami me renvoie. Seul un compte en cours de restauration (ou récemment rétabli) l'accepte.
    async fn store_restored(&self, me: &str, sender: &str, wrap_id: &str, rumor: &Rumor, at: i64, stats: &mut BackupStats) {
        let (Some(cid), Some(size)) = (tag(&rumor.tags, "cid"), tag(&rumor.tags, "size").and_then(|s| s.parse::<u64>().ok())) else {
            self.claim(wrap_id, at).await;
            stats.ignored += 1;
            return;
        };
        if self.kv_get("restore", me).await.is_none() || !astro_dav::blob::is_cid(cid) || size > MAX_BYTES {
            self.claim(wrap_id, at).await; // aucune restauration demandée, ou message mal formé : écarté sans bruit
            stats.ignored += 1;
            return;
        }
        if !self.ensure_blob(cid, rumor, wrap_id, at, stats).await {
            return;
        }
        let quota: i64 = tag(&rumor.tags, "quota").and_then(|q| q.parse().ok()).unwrap_or(0);
        let src = tag(&rumor.tags, "src");
        if let Some(peer) = tag(&rumor.tags, "station").filter(|p| (10..=64).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_alphanumeric())) {
            self.kv_put(rs::PEER, sender, peer, at).await; // pour tirer ses fragments si son DID manque
        }
        let is_index = tag(&rumor.tags, "kind") == Some("index");
        if is_index {
            self.kv_put(rs::RESTORE_INDEX, &format!("{me}:{sender}"), cid, at).await;
        }
        if is_index || src == Some("frag") {
            self.rs_dirty(me, at).await;
        }
        if matches!(src, Some("backup" | "frag")) && quota > 0 {
            // L'ami héberge ma sauvegarde : l'accord est rétabli sans qu'il ait rien à faire, et ce blob n'a pas à repartir
            // (un fragment, lui, est déjà connu par l'index).
            if src == Some("backup") {
                let _ = self.record_item(me, sender, cid, size).await;
            }
            if tag(&rumor.tags, "rs") == Some("1") {
                self.kv_put(rs::RS, &format!("{me}:{sender}"), "1", at).await;
            }
            let (m, s) = (me.to_owned(), sender.to_owned());
            let pact = self
                .db
                .writer()
                .execute(
                    "INSERT INTO backup_pacts(owner_hex, host_hex, status, quota_bytes, enabled, asked_at, decided_at) \
                     VALUES (?1, ?2, 'accepted', ?3, 1, ?4, ?4) \
                     ON CONFLICT(owner_hex, host_hex) DO UPDATE SET status = 'accepted', quota_bytes = excluded.quota_bytes, decided_at = excluded.decided_at",
                    vec![m.into(), s.into(), quota.into(), at.into()],
                )
                .await;
            if let Err(e) = pact {
                self.activity.publish_to(Audience::Captain, "alert", "backup", format!("restauration : accord de sauvegarde non rétabli ({e})"), None);
            }
        }
        let key = format!("{me}:{sender}");
        let n: u64 = self.kv_get(RETURNED, &key).await.and_then(|(v, _)| v.parse().ok()).unwrap_or(0);
        self.kv_put(RETURNED, &key, &(n + 1).to_string(), at).await;
        self.claim(wrap_id, at).await;
        stats.stored += 1;
        self.try_restore_manifest(me, at).await;
    }

    /// Étape de la nuit (20h12 solaire) : envoie tout ce qui reste à sauvegarder, par lots, jusqu'à épuisement.
    pub async fn sync_night(&self) -> Result<String, String> {
        // Réparation d'abord : les fragments refaits partent avec l'envoi de la nuit (et l'index qui les cite).
        let repair = self.repair_night(now()).await;
        let mut total = 0;
        for _ in 0..200 {
            let sent = self.run_once().await;
            if sent == 0 {
                break;
            }
            total += sent;
        }
        let dropped = self.gc_night(now()).await;
        let moved = self.purge_moved(now()).await?;
        Ok(format!(
            "{total} fichier(s) privé(s) sauvegardé(s) chez des amis, {dropped} ancien(s) blob(s) retiré(s) ; réparation : {} fragment(s) contrôlé(s), {} perdu(s), {} refait(s), {} orphelin(s) ramassé(s) ; {moved} élément(s) de plus de 30 jours effacé(s) de la corbeille",
            repair.checked, repair.lost, repair.rebuilt, repair.orphans
        ))
    }

    /// Vide la corbeille de la station (comptes déménagés, comptes éjectés… : voir [`crate::trash`]) de ce qui y dort depuis
    /// plus de [`MOVED_KEEP_SECS`]. Une erreur est dite, jamais avalée.
    async fn purge_moved(&self, now: i64) -> Result<usize, String> {
        let Some(game) = self.accounts_dir.clone().and_then(|d| d.parent().map(std::path::Path::to_path_buf)) else { return Ok(0) };
        tokio::task::spawn_blocking(move || crate::trash::purge(&game, now, MOVED_KEEP_SECS)).await.map_err(|e| format!("corbeille : {e}"))?
    }

    // ───────────────── Secours par les gardiens ─────────────────

    /// Efface une entrée d'état local.
    async fn kv_del(&self, scope: &'static str, key: &str) {
        let _ = self.db.writer().execute("DELETE FROM kv WHERE scope = ?1 AND key = ?2", vec![scope.to_owned().into(), key.to_owned().into()]).await;
    }

    /// Contacts (kind 3) de `owner` tels que le crawl les a rapatriés, bornés.
    async fn follows_of(&self, owner: &str) -> Vec<String> {
        let o = owner.to_owned();
        self.db
            .readers()
            .read(move |c| {
                let mut stmt = c.prepare(
                    "SELECT DISTINCT t.value FROM nostr_tags t WHERE t.name = 'p' AND length(t.value) = 64 \
                     AND t.event_id = (SELECT seq FROM nostr_events WHERE pubkey = ?1 AND kind = 3 ORDER BY created_at DESC LIMIT 1) LIMIT ?2",
                )?;
                Ok(stmt.query_map(rusqlite::params![o, RESCUE_MAX_FRIENDS as i64], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|h| is_hex64(h) && h != owner)
            .collect()
    }

    async fn rescue_session(&self, owner: &str, at: i64) -> Option<RescueOut> {
        let (v, _) = self.kv_get(RESCUE_OUT, owner).await?;
        serde_json::from_str::<RescueOut>(&v).ok().filter(|r| at - r.at < RESCUE_SECS)
    }

    async fn save_session(&self, owner: &str, r: &RescueOut, at: i64) {
        self.kv_put(RESCUE_OUT, owner, &json!(r).to_string(), at).await;
    }

    /// Côté gardien : demande en attente pour `owner` (moins de 24 h).
    async fn rescue_request(&self, me: &str, owner: &str) -> Option<Value> {
        let (v, _) = self.kv_get(RESCUE_IN, &format!("{me}:{owner}")).await?;
        serde_json::from_str::<Value>(&v).ok().filter(|r| now() - r["at"].as_i64().unwrap_or(0) < RESCUE_SECS)
    }

    /// Nouvelle station : le membre a scanné sa carte imprimée (part P1). `send` ouvre (ou relance) la demande auprès de ses
    /// gardiens ; sinon l'état de la demande seulement. La carte sert de justificatif : sans elle, rien ne part ni ne se lit.
    pub async fn rescue_ask(&self, card: &str, send: bool, at: i64) -> Result<Value, String> {
        let share = astro_lifecycle::share::SecretShare::parse(card.trim()).map_err(|_| "carte de secours illisible : scannez le QR imprimé, ou collez sa ligne".to_owned())?;
        if share.index != 1 {
            return Err("c'est la carte imprimée (part 1) qu'il faut ici".into());
        }
        if self.node.is_none() {
            return Err("cette station ne peut pas joindre vos gardiens (clé NODE absente) : demandez la part de votre ami par téléphone".into());
        }
        if self.hosted_here(&share.hex).await {
            return Err("ce compte est déjà hébergé sur cette station : connectez-vous".into());
        }
        let mut r = self.rescue_session(&share.hex, at).await.unwrap_or(RescueOut { at, ..RescueOut::default() });
        if send {
            r.asks.retain(|t| at - t < RESCUE_SECS);
            if r.asks.len() >= RESCUE_MAX_ASKS {
                return Err("trois demandes par jour au plus : patientez, ou appelez directement un de vos gardiens".into());
            }
            if self.rescue_sessions_last_hour(at).await >= RESCUE_MAX_PER_HOUR && !r.asks.iter().any(|t| at - t < 3600) {
                return Err("trop de demandes de secours sur cette station pour l'instant : réessayez dans une heure".into());
            }
            r.asks.push(at);
            r.sent.clear(); // relance : chaque gardien est sollicité de nouveau
            r.denied.clear();
            self.save_session(&share.hex, &r, at).await;
            // Retrouver ses contacts (et, par eux, ses gardiens) : crawl de sa clé pendant 24 h, réveillé tout de suite.
            self.kv_put(astro_sync::WANTED_SCOPE, &share.hex, &(at + RESCUE_SECS).to_string(), at).await;
            if let Some(n) = &self.crawl_nudge {
                n.notify_one();
            }
            self.rescue_step(at).await;
            r = self.rescue_session(&share.hex, at).await.unwrap_or(r);
        }
        let current = self.rescue_session(&share.hex, at).await.filter(|x| !x.asks.is_empty()).or(Some(r).filter(|x| !x.asks.is_empty()));
        Ok(self.rescue_view(current.as_ref(), at).await)
    }

    /// État de la demande pour la page publique `/restore`, où l'on ne présente que la carte imprimée : rien de plus que ce que
    /// le réseau montre déjà à tous. Les contacts sollicités viennent de la liste de contacts publique du compte et sont nommés
    /// par leur nom public (profil), jamais par leur adresse ; leur état ne dit que ce que CETTE station a fait ou reçu (prévenu,
    /// station injoignable depuis…, a confirmé, n'a pas reconnu). Qui garde réellement une part n'est jamais affirmé : un ami qui
    /// n'en garde pas reste muet, exactement comme un gardien qui n'a pas encore répondu.
    async fn rescue_view(&self, r: Option<&RescueOut>, at: i64) -> Value {
        let Some(r) = r else { return json!({ "status": "none" }) };
        let mut contacts = Vec::new();
        let mut push = |name: Option<String>, state: &str, since: Option<i64>| contacts.push(json!({ "name": name, "state": state, "since": since }));
        for hex in r.by_hex.iter() {
            push(self.public_name(hex).await, "confirmed", None);
        }
        for hex in &r.denied_hex {
            push(self.public_name(hex).await, "denied", None);
        }
        for hex in r.sent.iter().filter(|h| Some(*h) != r.by_hex.as_ref() && !r.denied_hex.contains(h)).take(RESCUE_SHOW) {
            push(self.public_name(hex).await, "asked", None);
        }
        for (hex, since) in r.failed.iter().filter(|(h, _)| !r.sent.contains(h)).take(RESCUE_SHOW) {
            push(self.public_name(hex).await, "offline", Some(*since));
        }
        if r.share.is_some() {
            return json!({ "status": "ready", "by": r.by, "denied": r.denied, "contacts": contacts });
        }
        // Le crawl n'a trouvé aucun contact au bout d'un quart d'heure : personne à qui demander.
        let first = r.asks.first().copied().unwrap_or(r.at);
        let status = if !r.sent.is_empty() { "waiting" } else if r.failed.is_empty() && at - first > 15 * 60 { "nobody" } else { "searching" };
        json!({ "status": status, "sent": r.sent.len(), "denied": r.denied, "contacts": contacts, "asks_left": RESCUE_MAX_ASKS.saturating_sub(r.asks.len()) })
    }

    /// Nom public (profil) d'un contact, pour la page de secours ; jamais son adresse ni sa clé.
    async fn public_name(&self, hex: &str) -> Option<String> {
        let h = hex.to_owned();
        let name: Option<String> = self
            .db
            .readers()
            .read(move |c| Ok(c.query_row("SELECT COALESCE(NULLIF(trim(json_extract(content, '$.display_name')), ''), json_extract(content, '$.name')) FROM nostr_events WHERE pubkey = ?1 AND kind = 0 ORDER BY created_at DESC LIMIT 1", [h], |r| r.get::<_, Option<String>>(0)).optional()?.flatten()))
            .await
            .ok()
            .flatten();
        name.map(|n| n.chars().filter(|c| !c.is_control()).take(40).collect::<String>().trim().to_owned()).filter(|n| !n.is_empty() && !n.contains('@'))
    }

    async fn rescue_sessions_last_hour(&self, at: i64) -> usize {
        let since = at - 3600;
        self.db
            .readers()
            .read(move |c| {
                let mut stmt = c.prepare("SELECT value FROM kv WHERE scope = ?1 AND updated_at >= ?2")?;
                Ok(stmt.query_map(rusqlite::params![RESCUE_OUT, since], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .unwrap_or_default()
            .iter()
            .filter_map(|v| serde_json::from_str::<RescueOut>(v).ok())
            .filter(|r| r.asks.iter().any(|t| *t >= since))
            .count()
    }

    /// Nouvelle station : sollicite les contacts pas encore joints des demandes ouvertes (dès que le crawl les a trouvés).
    pub async fn rescue_step(&self, at: i64) -> usize {
        let Some((node_hex, node_secret)) = self.node.clone() else { return 0 };
        let owners: Vec<String> = self
            .db
            .readers()
            .read(|c| {
                let mut stmt = c.prepare("SELECT key FROM kv WHERE scope = ?1")?;
                Ok(stmt.query_map([RESCUE_OUT], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .unwrap_or_default();
        let mut sent = 0;
        for owner in owners {
            let Some(mut r) = self.rescue_session(&owner, at).await else {
                self.kv_del(RESCUE_OUT, &owner).await;
                continue;
            };
            if r.share.is_some() || r.asks.is_empty() {
                continue;
            }
            let mut changed = false;
            for friend in self.follows_of(&owner).await {
                if r.sent.contains(&friend) {
                    continue;
                }
                let mut tags = vec![vec!["op".to_owned(), "rescue".to_owned()], vec!["owner".into(), owner.clone()]];
                if let Some(l) = &self.label {
                    tags.push(vec!["station".into(), l.clone()]);
                }
                match self.send_with(&node_secret, &node_hex, &friend, kind::CHAT, tags, "secours").await {
                    Ok(()) => {
                        r.failed.retain(|(h, _)| h != &friend);
                        r.sent.push(friend);
                        sent += 1;
                        changed = true;
                    }
                    // Station du contact injoignable d'ici : il sera retenté au passage suivant.
                    Err(e) => {
                        self.activity.publish("ucloud", "backup", format!("secours : contact …{} injoignable pour l'instant ({e})", &friend[..8]), None);
                        if !r.failed.iter().any(|(h, _)| h == &friend) {
                            r.failed.push((friend, at));
                            changed = true;
                        }
                    }
                }
            }
            if changed {
                self.save_session(&owner, &r, at).await;
            }
        }
        sent
    }

    /// Côté gardien : une demande de secours (ou sa clôture) arrive d'une station NODE inconnue.
    async fn rescue_in(&self, me: &str, from: &str, op: &str, rumor: &Rumor, at: i64) {
        let Some(owner) = tag(&rumor.tags, "owner").filter(|h| is_hex64(h)).map(str::to_ascii_lowercase) else { return };
        let key = format!("{me}:{owner}");
        if op == "rescue-closed" {
            if self.rescue_request(me, &owner).await.is_some_and(|r| r["from"] == from) {
                self.kv_del(RESCUE_IN, &key).await;
            }
            return;
        }
        // Silence complet si je ne garde pas de part pour ce compte : un ami qui n'est pas gardien ne voit rien.
        if self.kv_get(SHARE_HELD, &key).await.is_none() {
            return;
        }
        let mut asked: Vec<i64> = self.kv_get(RESCUE_ASKED, &key).await.and_then(|(v, _)| serde_json::from_str(&v).ok()).unwrap_or_default();
        asked.retain(|t| at - t < RESCUE_SECS);
        if asked.len() >= RESCUE_MAX_ASKS {
            return;
        }
        asked.push(at);
        self.kv_put(RESCUE_ASKED, &key, &json!(asked).to_string(), at).await;
        let station: String = tag(&rumor.tags, "station").unwrap_or("une autre station").chars().filter(|c| !c.is_control()).take(80).collect();
        self.kv_put(RESCUE_IN, &key, &json!({ "from": from, "station": station, "at": at }).to_string(), at).await;
        let label = self.label_of(&owner).await;
        self.tell(me, format!("🆘 {label} dit avoir perdu sa station et veut retrouver son compte sur « {station} ». Appelez-le avant de confirmer : Moi › Sauvegarde.")).await;
    }

    /// Côté gardien : `accept` rend la part P3 à la station qui la demande ; sinon la demande est refusée et le Capitaine alerté.
    pub async fn rescue_answer(&self, me: &str, owner: &str, accept: bool) -> Result<Value, String> {
        let req = self.rescue_request(me, owner).await.ok_or("aucune demande de secours en attente pour ce compte")?;
        let from = req["from"].as_str().filter(|h| is_hex64(h)).ok_or("demande illisible")?.to_owned();
        let key = format!("{me}:{owner}");
        let tags = vec![vec!["op".to_owned(), if accept { "rescued" } else { "rescue-denied" }.to_owned()], vec!["owner".into(), owner.to_owned()]];
        if accept {
            let (share, _) = self.kv_get(SHARE_HELD, &key).await.ok_or("vous ne gardez plus de part pour ce compte")?;
            self.send_to(me, &from, kind::CHAT, tags, &share).await?;
        } else {
            // Le refus est dit à la station qui demande ; l'échec de cet envoi n'empêche pas de clore la demande.
            if let Err(e) = self.send_to(me, &from, kind::CHAT, tags, "").await {
                self.activity.publish_to(Audience::Owner(me.to_owned()), "ucloud", "backup", format!("refus non transmis ({e})"), None);
            }
            let (who, station) = (self.label_of(owner).await, req["station"].as_str().unwrap_or("?").to_owned());
            let me_label = self.label_of(me).await;
            self.activity.publish_to(Audience::Captain, "alert", "backup", format!("🆘 {me_label} n'a pas reconnu une demande de secours pour le compte de {who} (station « {station} ») : tentative d'usurpation possible."), None);
        }
        self.kv_del(RESCUE_IN, &key).await;
        self.status(me).await
    }

    /// Nouvelle station : réponse d'un gardien, reçue sur la clé NODE.
    async fn rescue_reply(&self, from: &str, op: &str, rumor: &Rumor) {
        let at = now();
        let Some(owner) = tag(&rumor.tags, "owner").filter(|h| is_hex64(h)).map(str::to_ascii_lowercase) else { return };
        let Some(mut r) = self.rescue_session(&owner, at).await else { return };
        // Seul un contact sollicité pour ce compte peut répondre.
        if r.share.is_some() || !r.sent.iter().any(|s| s == from) {
            return;
        }
        // Montré sur la page publique : le nom public seulement, jamais l'adresse.
        let label = self.public_name(from).await.unwrap_or_else(|| "un contact".to_owned());
        if op == "rescue-denied" {
            if !r.denied_hex.iter().any(|h| h == from) {
                r.denied_hex.push(from.to_owned());
                r.denied.push(label);
            }
            self.save_session(&owner, &r, at).await;
            return;
        }
        match astro_lifecycle::share::SecretShare::parse(&rumor.content) {
            Ok(share) if share.index == 3 && share.hex == owner => {
                r.share = Some(share.to_text());
                r.by = Some(label);
                r.by_hex = Some(from.to_owned());
                self.save_session(&owner, &r, at).await;
                // Les autres gardiens n'ont plus rien à faire : leur demande est close.
                if let Some((node_hex, node_secret)) = self.node.clone() {
                    for other in r.sent.iter().filter(|s| s.as_str() != from) {
                        let tags = vec![vec!["op".to_owned(), "rescue-closed".to_owned()], vec!["owner".into(), owner.clone()]];
                        let _ = self.send_with(&node_secret, &node_hex, other, kind::CHAT, tags, "").await;
                    }
                }
            }
            _ => {
                self.activity.publish_to(Audience::Captain, "alert", "backup", format!("secours : réponse illisible de …{} pour …{}", &from[..8], &owner[..8]), None);
            }
        }
    }

    /// Nouvelle station : part P3 rendue par un gardien pour `owner` (moins de 24 h), à joindre à la carte P1 à la restauration.
    pub async fn rescued_share(&self, owner: &str) -> Option<String> {
        self.rescue_session(owner, now()).await.and_then(|r| r.share)
    }

    /// Nouvelle station : le compte est rétabli, la demande et la part reçue sont effacées.
    pub async fn rescue_done(&self, owner: &str) {
        self.kv_del(RESCUE_OUT, owner).await;
        self.kv_del(astro_sync::WANTED_SCOPE, owner).await;
    }

    /// Travailleur de fond : réception des demandes, réponses et blobs jusqu'au signal d'arrêt. L'**envoi** n'a lieu
    /// qu'à 20h12 ([`Self::sync_night`]) : un lot par nuit plutôt qu'un envoi à chaque modification.
    pub async fn run(self: Arc<Self>, every: Duration, mut stop: watch::Receiver<bool>) {
        loop {
            let at = now();
            self.poll_once(at).await;
            self.restore_step(at).await;
            self.serve_restores().await;
            self.send_shares(at).await;
            self.observe_presence(at).await;
            tokio::select! {
                () = tokio::time::sleep(every) => {}
                changed = stop.changed() => {
                    if changed.is_err() || *stop.borrow() {
                        return;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use astro_crypto::StaticKeySource;
    use astro_dav::FsBlobStore;
    use astro_db::events::insert_event;
    use astro_facecloud::OutboxError;
    use astro_nostr_proto::{UnsignedEvent, pubkey_of};

    use super::*;

    const ALICE: [u8; 32] = [0xA1; 32]; // propriétaire, hébergée ici
    const BOB: [u8; 32] = [0xB2; 32]; // ami hébergé ici
    const CAROL: [u8; 32] = [0xC3; 32]; // amie hébergée ailleurs

    fn hex(s: &[u8; 32]) -> String {
        pubkey_of(s).unwrap()
    }

    /// Boîte d'envoi de test : garde ce qui part ; `.1` = envois encore permis avant une coupure du relais (`None` : sans fin).
    #[derive(Default)]
    struct Capture(Mutex<Vec<(String, Event)>>, Mutex<Option<usize>>);

    impl Outbox for Capture {
        async fn send(&self, to: &str, wrap: Event) -> Result<(), OutboxError> {
            if let Some(left) = self.1.lock().unwrap().as_mut() {
                if *left == 0 {
                    return Err(OutboxError("relais injoignable".into()));
                }
                *left -= 1;
            }
            self.0.lock().unwrap().push((to.to_owned(), wrap));
            Ok(())
        }
    }

    fn cid(n: u8) -> String {
        format!("bafkrei{}", format!("{n:02}abcdefghijklmnopqrstuvwxyz234567abcdefghijklmnopq").chars().take(52).collect::<String>())
    }

    async fn rig(tag: &str) -> (std::path::PathBuf, Arc<Db>, Arc<Capture>, PrivateBackup<Capture, FsBlobStore>) {
        let dir = std::env::temp_dir().join(format!("astroportd-backup-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
        for (s, email, hosted) in [(&ALICE, "alice@example.org", 1), (&BOB, "bob@example.org", 1), (&CAROL, "carol@example.org", 0)] {
            db.writer().execute("INSERT INTO identities(hex, role, email, hosted) VALUES (?1, 'multipass', ?2, ?3)", vec![hex(s).into(), email.to_owned().into(), i64::from(hosted).into()]).await.unwrap();
        }
        for who in [&BOB, &CAROL] {
            db.writer().execute("INSERT INTO social_graph(owner_hex, pubkey, level, updated_at) VALUES (?1, ?2, 1, 0)", vec![hex(&ALICE).into(), hex(who).into()]).await.unwrap();
            let ev = UnsignedEvent { created_at: 100, kind: 3, tags: vec![vec!["p".into(), hex(&ALICE)]], content: String::new() }.sign(who).unwrap();
            db.writer().transaction(move |tx| insert_event(tx, &ev).map(|_| ())).await.unwrap();
        }
        // Alice : trois fichiers privés de 10 octets, un déjà envoyé à un ami, un dans /Partages.
        for (n, path) in [(1u8, "/Docs/a.txt"), (2, "/Docs/b.txt"), (3, "/Docs/c.txt"), (4, "/Docs/shared.txt"), (5, "/Partages/De_bob/x.txt")] {
            db.writer()
                .execute(
                    "INSERT INTO ucloud_entries(owner_hex, path, parent, type, cid, mime, size, mtime, data) VALUES (?1, ?2, ?3, 'file', ?4, 'text/plain', 10, ?5, '{}')",
                    vec![hex(&ALICE).into(), path.to_owned().into(), path.rsplit_once('/').unwrap().0.to_owned().into(), cid(n).into(), i64::from(n).into()],
                )
                .await
                .unwrap();
        }
        astro_dav::adopt::mark_shared(&db, &hex(&ALICE), vec![cid(4)], "link").await;
        let outbox = Arc::new(Capture::default());
        let keys: Arc<dyn KeySource> = Arc::new(StaticKeySource::new().with(ALICE).with(BOB));
        let sharing = Arc::new(Sharing::new(db.clone(), keys.clone(), outbox.clone(), None, None));
        let blobs = Arc::new(FsBlobStore::open(dir.join("blobs")).unwrap());
        let backup = PrivateBackup::new(db.clone(), keys, outbox.clone(), blobs, sharing, ActivityBus::new()).with_station(None, Some("12D3KooWAlice".into()));
        (dir, db, outbox, backup)
    }

    async fn count(db: &Db, sql: &'static str, arg: String) -> i64 {
        db.readers().read(move |c| Ok(c.query_row(sql, [arg], |r| r.get(0))?)).await.unwrap()
    }

    #[tokio::test]
    async fn nothing_leaves_before_the_friend_accepts_then_only_unshared_files_within_his_quota() {
        let (dir, db, _, backup) = rig("local").await;
        let (alice, bob) = (hex(&ALICE), hex(&BOB));
        let st = backup.set(&alice, true, Some(&bob)).await.unwrap();
        assert_eq!(st["mine"]["status"], "pending");
        assert_eq!(backup.run_once().await, 0, "pas d'accord, pas d'envoi");
        assert_eq!(backup.status(&bob).await.unwrap()["hosting"][0]["status"], "pending");

        // Bob accepte 15 octets : un seul fichier de 10 octets entre, ni le partagé ni /Partages.
        backup.respond(&bob, &alice, true, 15, 100).await.unwrap();
        assert_eq!(backup.run_once().await, 1);
        assert_eq!(count(&db, "SELECT COUNT(*) FROM ucloud_entries WHERE owner_hex = ?1 AND path LIKE '/Sauvegardes/De_alice/%'", bob.clone()).await, 1);
        assert_eq!(count(&db, "SELECT COUNT(*) FROM ucloud_keyring WHERE owner_hex = ?1", bob.clone()).await, 0, "l'ami n'a jamais la clé");
        assert_eq!(backup.run_once().await, 0, "volume plein : rien de plus");

        // Il augmente le volume : le reste des fichiers privés part, jamais le fichier partagé.
        backup.respond(&bob, &alice, true, 1000, 1000).await.unwrap();
        assert_eq!(backup.run_once().await, 2);
        let stored: Vec<String> = db.readers().read(move |c| Ok(c.prepare("SELECT cid FROM backup_items ORDER BY cid")?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?)).await.unwrap();
        assert!(!stored.contains(&cid(4)) && !stored.contains(&cid(5)), "{stored:?}");

        // Il révoque : ses blobs disparaissent et son quota est libéré.
        backup.respond(&bob, &alice, false, 0, 0).await.unwrap();
        assert_eq!(count(&db, "SELECT COUNT(*) FROM ucloud_entries WHERE owner_hex = ?1 AND path LIKE '/Sauvegardes/%' AND type = 'file'", bob).await, 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn big_old_files_that_no_longer_fit_do_not_starve_the_small_ones() {
        let (dir, db, _, backup) = rig("famine").await;
        let (alice, bob) = (hex(&ALICE), hex(&BOB));
        // Plus d'une passe de très gros fichiers, plus anciens que les trois petits : aucun ne tient dans le volume accordé.
        for n in 0..(PER_PASS + 20) {
            db.writer()
                .execute(
                    "INSERT INTO ucloud_entries(owner_hex, path, parent, type, cid, mime, size, mtime, data) VALUES (?1, ?2, '/Films', 'file', ?3, 'video/mp4', 1000000, 0, '{}')",
                    vec![alice.clone().into(), format!("/Films/{n}.mp4").into(), format!("bafkreigros{n:048}").into()],
                )
                .await
                .unwrap();
        }
        backup.set(&alice, true, Some(&bob)).await.unwrap();
        backup.respond(&bob, &alice, true, 100, 100).await.unwrap();
        assert_eq!(backup.run_once().await, 3, "les petits fichiers passent malgré les gros, plus anciens");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_deleted_file_is_removed_from_the_friend_only_after_seven_days() {
        let (dir, db, _, backup) = rig("gc").await;
        let (alice, bob) = (hex(&ALICE), hex(&BOB));
        backup.set(&alice, true, Some(&bob)).await.unwrap();
        backup.respond(&bob, &alice, true, 1000, 1000).await.unwrap();
        assert_eq!(backup.run_once().await, 3);
        let hosted = |db: Arc<Db>, bob: String| async move { count(&db, "SELECT COUNT(*) FROM ucloud_entries WHERE owner_hex = ?1 AND path LIKE '/Sauvegardes/%' AND type = 'file'", bob).await };

        // Alice supprime /Docs/a.txt (cid 1) : marqué, pas encore retiré.
        db.writer().execute("DELETE FROM ucloud_entries WHERE owner_hex = ?1 AND path = '/Docs/a.txt'", vec![alice.clone().into()]).await.unwrap();
        let t0 = 1_000_000;
        assert_eq!(backup.gc_night(t0).await, 0);
        assert_eq!(backup.gc_night(t0 + GRACE_SECS - 1).await, 0, "avant 7 jours : rien");
        assert_eq!(hosted(db.clone(), bob.clone()).await, 3);

        // Elle le restaure avant la date : la marque s'efface, et le délai repart de zéro.
        db.writer()
            .execute("INSERT INTO ucloud_entries(owner_hex, path, parent, type, cid, mime, size, mtime, data) VALUES (?1, '/Docs/a.txt', '/Docs', 'file', ?2, 'text/plain', 10, 1, '{}')", vec![alice.clone().into(), cid(1).into()])
            .await
            .unwrap();
        assert_eq!(backup.gc_night(t0 + GRACE_SECS).await, 0);
        assert_eq!(count(&db, "SELECT COUNT(*) FROM backup_items WHERE owner_hex = ?1 AND missing_since IS NOT NULL", alice.clone()).await, 0);

        // Supprimé pour de bon : retiré après 7 jours, le quota de l'ami est libéré.
        db.writer().execute("DELETE FROM ucloud_entries WHERE owner_hex = ?1 AND path = '/Docs/a.txt'", vec![alice.clone().into()]).await.unwrap();
        let t1 = t0 + 2 * GRACE_SECS;
        assert_eq!(backup.gc_night(t1).await, 0);
        assert_eq!(backup.gc_night(t1 + GRACE_SECS).await, 1);
        assert_eq!(hosted(db.clone(), bob.clone()).await, 2);
        assert_eq!(count(&db, "SELECT COUNT(*) FROM backup_items WHERE owner_hex = ?1", alice).await, 2);
        let _ = std::fs::remove_dir_all(dir);
    }

    async fn count2(db: &Db, sql: &'static str, a: String, b: String) -> i64 {
        db.readers().read(move |c| Ok(c.query_row(sql, [a, b], |r| r.get(0))?)).await.unwrap()
    }

    async fn put_blob(backup: &PrivateBackup<Capture, FsBlobStore>, bytes: &[u8]) -> String {
        use astro_dav::BlobWriter as _;
        use tokio::io::AsyncWriteExt as _;
        let mut w = backup.blobs.begin().await.unwrap();
        w.write_all(bytes).await.unwrap();
        w.commit().await.unwrap().cid
    }

    async fn set_manifest(db: &Db, owner: &str, cid: &str) {
        db.writer()
            .execute("INSERT INTO kv(scope, key, value, updated_at) VALUES ('ucloud_manifest_cid', ?1, ?2, 0) ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value", vec![owner.to_owned().into(), cid.to_owned().into()])
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn the_manifest_blob_goes_to_the_friend_and_outlives_the_night_cleanup_until_replaced() {
        const HAS: &str = "SELECT COUNT(*) FROM backup_items WHERE owner_hex = ?1 AND cid = ?2";
        let (dir, db, _, backup) = rig("manifest").await;
        let (alice, bob) = (hex(&ALICE), hex(&BOB));
        let m1 = put_blob(&backup, b"manifest-v1").await;
        set_manifest(&db, &alice, &m1).await;
        backup.set(&alice, true, Some(&bob)).await.unwrap();
        backup.respond(&bob, &alice, true, 1000, 1000).await.unwrap();

        assert_eq!(backup.run_once().await, 4, "le manifeste et les trois fichiers privés");
        assert_eq!(count2(&db, HAS, alice.clone(), m1.clone()).await, 1);
        assert_eq!(backup.run_once().await, 0, "rien à renvoyer tant que le manifeste ne change pas");

        // Le manifeste n'est pas une entrée du VFS : le nettoyage de nuit ne doit pas le prendre pour un fichier disparu.
        let t0 = 1_000_000;
        assert_eq!(backup.gc_night(t0).await, 0);
        assert_eq!(backup.gc_night(t0 + 2 * GRACE_SECS).await, 0);
        assert_eq!(count2(&db, HAS, alice.clone(), m1.clone()).await, 1, "le manifeste courant reste chez l'ami");

        // Un nouveau manifeste part tout de suite ; l'ancien est retiré après 7 jours, le courant reste.
        let m2 = put_blob(&backup, b"manifest-v2-plus-long").await;
        set_manifest(&db, &alice, &m2).await;
        assert_eq!(backup.run_once().await, 1);
        assert_eq!(count2(&db, HAS, alice.clone(), m2.clone()).await, 1);
        let t1 = t0 + 4 * GRACE_SECS;
        assert_eq!(backup.gc_night(t1).await, 0);
        assert_eq!(backup.gc_night(t1 + GRACE_SECS).await, 1);
        assert_eq!(count2(&db, HAS, alice.clone(), m1).await, 0);
        assert_eq!(count2(&db, HAS, alice, m2).await, 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_remote_friend_gets_a_request_then_sealed_blobs_without_the_file_key() {
        let (dir, db, outbox, backup) = rig("remote").await;
        let (alice, carol) = (hex(&ALICE), hex(&CAROL));
        backup.set(&alice, true, Some(&carol)).await.unwrap();
        let open = |wrap: &Event| nip59::unwrap(&CAROL, wrap).unwrap().rumor;
        let first = outbox.0.lock().unwrap()[0].1.clone();
        let rumor = open(&first);
        assert_eq!((tag(&rumor.tags, "t"), tag(&rumor.tags, "op")), (Some(TAG), Some("request")));

        // Carol accepte : message `accept` scellé pour Alice, reçu par la station.
        let reply = Rumor { pubkey: carol.clone(), created_at: u64::try_from(now()).unwrap(), kind: kind::CHAT, tags: vec![vec!["t".into(), TAG.into()], vec!["op".into(), "accept".into()], vec!["quota".into(), "1000".into()]], content: String::new() };
        let wrap = gift_wrap(&CAROL, &alice, &reply, reply.created_at).unwrap();
        db.writer().transaction(move |tx| insert_event(tx, &wrap).map(|_| ())).await.unwrap();
        let stats = backup.poll_once(now()).await;
        assert_eq!(stats.control, 1);
        assert_eq!(backup.status(&alice).await.unwrap()["mine"]["status"], "accepted");

        assert_eq!(backup.run_once().await, 3);
        let sent = outbox.0.lock().unwrap().clone();
        assert_eq!(sent.len(), 4, "une demande + trois blobs");
        for (to, wrap) in &sent[1..] {
            let r = open(wrap);
            assert_eq!((to.as_str(), r.kind, tag(&r.tags, "op")), (carol.as_str(), kind::FILE, Some("item")));
            assert!(tag(&r.tags, "decryption-key").is_none() && tag(&r.tags, "cid").is_some(), "le blob part chiffré, sans sa clé");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    // ───────────────── Clé de secours : la part P3 chez les amis hébergeurs ─────────────────

    fn write_rescue_shares(dir: &std::path::Path, secret: &[u8; 32], email: &str) -> Vec<astro_lifecycle::share::SecretShare> {
        let card = astro_lifecycle::multipass::SecretCard::parse(&format!("astro-secret:v1|{email}|Sel123|Poivre456|raw||{}", hex(secret))).unwrap();
        let shares = astro_lifecycle::share::split_card(&card).unwrap();
        let account = dir.join("nostr").join(email);
        std::fs::create_dir_all(&account).unwrap();
        astro_lifecycle::share::write_shares(&account, &shares).unwrap();
        shares
    }

    async fn accept_pact(db: &Db, owner: &[u8; 32], host: &[u8; 32]) {
        db.writer().execute("INSERT OR REPLACE INTO backup_pacts(owner_hex, host_hex, status, quota_bytes, enabled, asked_at) VALUES (?1, ?2, 'accepted', 1000, 1, 1)", vec![hex(owner).into(), hex(host).into()]).await.unwrap();
    }

    #[tokio::test]
    async fn only_the_third_share_goes_to_the_friend_who_hosts_the_backup_and_he_can_show_it_until_he_stops() {
        let (alice, carol) = (hex(&ALICE), hex(&CAROL));
        let owner = node("rescue-o", &[(&ALICE, "alice@example.org")], &[(&CAROL, "carol@example.org")], "12D3KooWRescueOwner", None, None).await;
        let host = node("rescue-h", &[(&CAROL, "carol@example.org")], &[(&ALICE, "alice@example.org")], "12D3KooWRescueHost", None, None).await;
        let shares = write_rescue_shares(&owner.dir, &ALICE, "alice@example.org");
        for (n, a, b) in [(&owner, &ALICE, &CAROL), (&host, &CAROL, &ALICE)] {
            n.db.writer().execute("INSERT INTO social_graph(owner_hex, pubkey, level, updated_at) VALUES (?1, ?2, 1, 0)", vec![hex(a).into(), hex(b).into()]).await.unwrap();
        }
        give(&host.db, follows(&ALICE, &[&CAROL])).await;

        // Sans accord d'hébergement, rien ne part : l'ami n'a rien demandé.
        assert_eq!(owner.backup.send_shares(now()).await, 0);
        assert!(owner.outbox.0.lock().unwrap().is_empty());

        accept_pact(&owner.db, &ALICE, &CAROL).await;
        accept_pact(&host.db, &ALICE, &CAROL).await;
        assert_eq!(owner.backup.send_shares(now()).await, 1);
        assert_eq!(owner.backup.send_shares(now()).await, 0, "un seul envoi tant que la part ne change pas");
        let sent = owner.outbox.0.lock().unwrap().clone();
        assert_eq!(sent.len(), 1);
        let rumor = nip59::unwrap(&CAROL, &sent[0].1).unwrap().rumor;
        assert_eq!((tag(&rumor.tags, "op"), rumor.content.as_str()), (Some("share"), shares[2].to_text().as_str()), "P3, ni P1 ni P2");
        assert!(!rumor.content.contains("Sel123") && !rumor.content.contains("Poivre456"));
        assert_eq!(owner.backup.status(&alice).await.unwrap()["rescue_friends"], 1);

        // L'ami la reçoit et peut la montrer (texte et QR) à celui qui le rappelle.
        pump(&owner, &host).await;
        assert_eq!(host.backup.poll_once(now()).await.control, 1);
        let shown = host.backup.status(&carol).await.unwrap();
        assert_eq!(shown["hosting"][0]["share"], shares[2].to_text());
        assert!(shown["hosting"][0]["share_svg"].as_str().is_some_and(|s| s.contains("<svg")));

        // Une part qui n'est pas la P3 de l'expéditeur est écartée (autre numéro, ou clé publique d'un autre compte).
        let forged = |text: String| {
            let r = Rumor { pubkey: alice.clone(), created_at: u64::try_from(now()).unwrap() + 1, kind: kind::CHAT, tags: vec![vec!["t".into(), TAG.into()], vec!["op".into(), "share".into()]], content: text };
            gift_wrap(&ALICE, &carol, &r, r.created_at).unwrap()
        };
        host.db.writer().execute("DELETE FROM kv WHERE scope = ?1", vec![SHARE_HELD.to_owned().into()]).await.unwrap();
        for bad in [shares[0].to_text(), "n'importe quoi".to_owned()] {
            let wrap = forged(bad);
            host.db.writer().transaction(move |tx| insert_event(tx, &wrap).map(|_| ())).await.unwrap();
        }
        let stats = host.backup.poll_once(now()).await;
        assert_eq!(stats.ignored, 2, "P1 et le texte quelconque sont refusés");
        assert!(host.backup.status(&carol).await.unwrap()["hosting"][0].get("share").is_none());

        // L'hébergeur redonne la part puis arrête d'héberger : la part disparaît avec la sauvegarde.
        let wrap = forged(shares[2].to_text());
        host.db.writer().transaction(move |tx| insert_event(tx, &wrap).map(|_| ())).await.unwrap();
        host.backup.poll_once(now()).await;
        assert!(host.backup.status(&carol).await.unwrap()["hosting"][0].get("share").is_some());
        host.backup.respond(&carol, &alice, false, 0, 1000).await.unwrap();
        assert!(host.backup.kv_get(SHARE_HELD, &format!("{carol}:{alice}")).await.is_none());
        for n in [owner, host] {
            let _ = std::fs::remove_dir_all(n.dir);
        }
    }

    #[tokio::test]
    async fn a_friend_hosted_on_the_same_station_holds_the_share_without_any_message() {
        let (alice, bob) = (hex(&ALICE), hex(&BOB));
        let n = node("rescue-local", &[(&ALICE, "alice@example.org"), (&BOB, "bob@example.org")], &[], "12D3KooWRescueLocal", None, None).await;
        let shares = write_rescue_shares(&n.dir, &ALICE, "alice@example.org");
        accept_pact(&n.db, &ALICE, &BOB).await;
        assert_eq!(n.backup.send_shares(now()).await, 1);
        assert!(n.outbox.0.lock().unwrap().is_empty());
        assert_eq!(n.backup.status(&bob).await.unwrap()["hosting"][0]["share"], shares[2].to_text());
        // Le même compte n'a pas besoin de la part d'un autre : Alice, hébergeuse de personne, ne détient rien.
        assert!(n.backup.status(&alice).await.unwrap()["hosting"].as_array().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(n.dir);
    }

    // ───────────────── Secours par les gardiens : la part P3 revient seule ─────────────────

    const NODE_NEW: [u8; 32] = [0x0D; 32]; // clé NODE de la station où Alice se rétablit
    const DAVE: [u8; 32] = [0xD4; 32]; // un autre compte, dont Carol ne garde rien

    fn rescue_rumor(from: &[u8; 32], to: &str, tags: Vec<Vec<String>>, content: &str) -> Event {
        let mut all = vec![vec!["p".to_owned(), to.to_owned()], vec!["t".into(), TAG.into()]];
        all.extend(tags);
        let r = Rumor { pubkey: hex(from), created_at: u64::try_from(now()).unwrap(), kind: kind::CHAT, tags: all, content: content.to_owned() };
        gift_wrap(from, to, &r, r.created_at).unwrap()
    }

    async fn inject(n: &Node, wrap: Event) {
        n.db.writer().transaction(move |tx| insert_event(tx, &wrap).map(|_| ())).await.unwrap();
    }

    #[tokio::test]
    async fn a_guardian_confirms_and_the_third_share_reaches_the_new_station_without_anyone_reading_it() {
        let (alice, bob, carol, node_hex) = (hex(&ALICE), hex(&BOB), hex(&CAROL), hex(&NODE_NEW));
        // La station d'Alice a brûlé ; Carol (station H) garde sa part P3 ; Bob est un contact qui ne garde rien.
        let old = std::env::temp_dir().join(format!("astroportd-rescue-old-{}", std::process::id()));
        let shares = write_rescue_shares(&old, &ALICE, "alice@example.org");
        let (p1, p3) = (shares[0].to_text(), shares[2].to_text());
        let host = node("rescue-guard", &[(&CAROL, "carol@example.org")], &[(&ALICE, "alice@example.org")], "12D3KooWGuard", None, None).await;
        accept_pact(&host.db, &ALICE, &CAROL).await;
        host.backup.kv_put(SHARE_HELD, &format!("{carol}:{alice}"), &p3, now()).await;
        let mut fresh = node("rescue-new", &[], &[(&CAROL, "carol@example.org")], "12D3KooWNew", None, None).await;
        fresh.backup = fresh.backup.with_rescue(Some(NODE_NEW), Some("station du village".into()), None);

        // Seule la carte imprimée (P1) ouvre une demande ; pas encore de contacts connus : on les cherche (crawl de la clé d'Alice).
        assert!(fresh.backup.rescue_ask(&p3, true, now()).await.is_err(), "P3 n'est pas une carte imprimée");
        assert!(fresh.backup.rescue_ask("n'importe quoi", true, now()).await.is_err());
        assert_eq!(fresh.backup.rescue_ask(&p1, false, now()).await.unwrap()["status"], "none", "lire l'état n'ouvre rien");
        assert_eq!(fresh.backup.rescue_ask(&p1, true, now()).await.unwrap()["status"], "searching");
        assert_eq!(count(&fresh.db, "SELECT COUNT(*) FROM kv WHERE scope = 'crawl_wanted' AND key = ?1", alice.clone()).await, 1, "le crawl doit chercher Alice");

        // Le crawl a rapporté ses contacts : chacun est sollicité, depuis la clé NODE de la station.
        give(&fresh.db, follows(&ALICE, &[&BOB, &CAROL])).await;
        assert_eq!(fresh.backup.rescue_step(now()).await, 2);
        assert_eq!(fresh.backup.rescue_step(now()).await, 0, "une seule sollicitation par contact");
        let asked = fresh.outbox.0.lock().unwrap().clone();
        let to_carol = asked.iter().find(|(to, _)| *to == carol).unwrap();
        let opened = nip59::unwrap(&CAROL, &to_carol.1).unwrap();
        assert_eq!((opened.sender.as_str(), tag(&opened.rumor.tags, "op"), tag(&opened.rumor.tags, "owner")), (node_hex.as_str(), Some("rescue"), Some(alice.as_str())));
        assert!(!opened.rumor.content.contains("astro-share"), "la carte P1 ne quitte jamais la nouvelle station");
        assert_eq!(fresh.backup.rescue_ask(&p1, false, now()).await.unwrap()["status"], "waiting");

        // Chez Carol : la demande attend sa confirmation (Ma sécurité), avec le nom de la station.
        pump(&fresh, &host).await;
        host.backup.poll_once(now()).await;
        let seen = host.backup.status(&carol).await.unwrap();
        assert_eq!(seen["hosting"][0]["rescue"]["station"], "station du village");

        // Hostile : demande pour un compte dont Carol ne garde rien → silence ; au-delà de 3 demandes par jour → ignorées.
        inject(&host, rescue_rumor(&NODE_NEW, &carol, vec![vec!["op".into(), "rescue".into()], vec!["owner".into(), hex(&DAVE)]], "")).await;
        host.backup.poll_once(now()).await;
        assert!(host.backup.rescue_request(&carol, &hex(&DAVE)).await.is_none());
        host.db.writer().execute("DELETE FROM kv WHERE scope = ?1", vec![RESCUE_IN.to_owned().into()]).await.unwrap();
        for _ in 0..3 {
            inject(&host, rescue_rumor(&NODE_NEW, &carol, vec![vec!["op".into(), "rescue".into()], vec!["owner".into(), alice.clone()]], "")).await;
        }
        host.backup.poll_once(now()).await;
        let asked_n: Vec<i64> = serde_json::from_str(&host.backup.kv_get(RESCUE_ASKED, &format!("{carol}:{alice}")).await.unwrap().0).unwrap();
        assert_eq!(asked_n.len(), 3, "trois demandes par jour au plus");
        assert!(host.backup.rescue_request(&carol, &alice).await.is_some());

        // Avant la confirmation, la nouvelle station n'a rien ; un inconnu ne peut pas répondre à la place du gardien.
        assert!(fresh.backup.rescued_share(&alice).await.is_none());
        inject(&fresh, rescue_rumor(&DAVE, &node_hex, vec![vec!["op".into(), "rescued".into()], vec!["owner".into(), alice.clone()]], &p3)).await;
        fresh.backup.poll_once(now()).await;
        assert!(fresh.backup.rescued_share(&alice).await.is_none(), "Dave n'a pas été sollicité : sa réponse est ignorée");

        // Carol a appelé Alice et confirme : la part part vers la clé NODE ; Carol n'a rien eu à lire ni recopier.
        fresh.outbox.0.lock().unwrap().clear();
        host.backup.rescue_answer(&carol, &alice, true).await.unwrap();
        assert!(host.backup.rescue_request(&carol, &alice).await.is_none(), "la demande est close chez Carol");
        assert_eq!(host.outbox.0.lock().unwrap()[0].0, node_hex, "la réponse va à la station qui demande");
        pump(&host, &fresh).await;
        fresh.backup.poll_once(now()).await;
        // La page publique ne montre que le nom public (profil) des contacts, jamais leur adresse : Carol a un profil, Bob non.
        give(&fresh.db, UnsignedEvent { created_at: 1, kind: 0, tags: vec![], content: r#"{"name":"Carol"}"#.into() }.sign(&CAROL).unwrap()).await;
        let st = fresh.backup.rescue_ask(&p1, false, now()).await.unwrap();
        assert_eq!((st["status"].as_str(), st["by"].as_str()), (Some("ready"), Some("un contact")), "réponse reçue avant que le profil ne soit connu");
        assert_eq!(st["contacts"][0], json!({ "name": "Carol", "state": "confirmed", "since": null }));
        assert_eq!(st["contacts"][1], json!({ "name": null, "state": "asked", "since": null }), "Bob, sans profil public, reste anonyme");
        assert!(!st.to_string().contains("example.org") && !st.to_string().contains(&carol), "ni adresse ni clé sur la page publique");
        let got = fresh.backup.rescued_share(&alice).await.unwrap();
        let recovered = astro_lifecycle::share::Recovery::parse(&format!("{p1}\n{got}")).unwrap();
        assert_eq!((recovered.card.salt.as_str(), recovered.card.pepper.as_str(), recovered.card.hex.as_str()), ("Sel123", "Poivre456", alice.as_str()), "P1 + P3 rendue = le compte d'Alice");
        // Bob, sollicité aussi, est prévenu que c'est réglé.
        let closed = fresh.outbox.0.lock().unwrap().iter().any(|(to, w)| *to == bob && tag(&nip59::unwrap(&BOB, w).unwrap().rumor.tags, "op") == Some("rescue-closed"));
        assert!(closed);
        fresh.backup.rescue_done(&alice).await;
        assert!(fresh.backup.rescued_share(&alice).await.is_none());
        for n in [host, fresh] {
            let _ = std::fs::remove_dir_all(n.dir);
        }
        let _ = std::fs::remove_dir_all(old);
    }

    #[tokio::test]
    async fn a_guardian_who_does_not_recognise_the_caller_refuses_and_his_captain_is_warned() {
        let (alice, carol, node_hex) = (hex(&ALICE), hex(&CAROL), hex(&NODE_NEW));
        let old = std::env::temp_dir().join(format!("astroportd-rescue-old2-{}", std::process::id()));
        let shares = write_rescue_shares(&old, &ALICE, "alice@example.org");
        let host = node("rescue-deny", &[(&CAROL, "carol@example.org")], &[(&ALICE, "alice@example.org")], "12D3KooWDeny", None, None).await;
        accept_pact(&host.db, &ALICE, &CAROL).await;
        host.backup.kv_put(SHARE_HELD, &format!("{carol}:{alice}"), &shares[2].to_text(), now()).await;
        inject(&host, rescue_rumor(&NODE_NEW, &carol, vec![vec!["op".into(), "rescue".into()], vec!["owner".into(), alice.clone()], vec!["station".into(), "station louche".into()]], "")).await;
        host.backup.poll_once(now()).await;
        host.backup.rescue_answer(&carol, &alice, false).await.unwrap();
        let sent = host.outbox.0.lock().unwrap().clone();
        let r = nip59::unwrap(&NODE_NEW, &sent[0].1).unwrap().rumor;
        assert_eq!((sent[0].0.as_str(), tag(&r.tags, "op"), r.content.as_str()), (node_hex.as_str(), Some("rescue-denied"), ""), "un refus, sans la part");
        let (log, _) = host.backup.activity.subscribe(0);
        assert!(log.iter().any(|a| a.message.contains("usurpation") && a.audience == Audience::Captain), "le Capitaine de Carol est alerté");
        assert!(host.backup.rescue_answer(&carol, &alice, true).await.is_err(), "la demande refusée est close");
        let _ = std::fs::remove_dir_all(host.dir);
        let _ = std::fs::remove_dir_all(old);
    }

    // ───────────────── Restauration de bout en bout : trois stations, une panne ─────────────────

    struct Node {
        dir: std::path::PathBuf,
        db: Arc<Db>,
        outbox: Arc<Capture>,
        blobs: Arc<FsBlobStore>,
        backup: PrivateBackup<Capture, FsBlobStore>,
    }

    /// Station de test : `hosted` (clé, email) y sont hébergés, `remote` sont des amis d'ailleurs. `from` : station dont le
    /// maillage rapatrie les blobs (copie entre dossiers, le CID étant le SHA-256 du blob chiffré).
    async fn node(tag: &str, hosted: &[(&[u8; 32], &str)], remote: &[(&[u8; 32], &str)], peer: &str, home: Option<String>, from: Option<Arc<FsBlobStore>>) -> Node {
        let dir = std::env::temp_dir().join(format!("astroportd-restore-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
        let mut keys = StaticKeySource::new();
        for (secret, email) in hosted {
            db.writer().execute("INSERT INTO identities(hex, role, email, hosted) VALUES (?1, 'multipass', ?2, 1)", vec![hex(secret).into(), (*email).to_owned().into()]).await.unwrap();
            keys = keys.with(**secret);
        }
        for (secret, email) in remote {
            db.writer().execute("INSERT INTO identities(hex, role, email, hosted) VALUES (?1, 'multipass', ?2, 0)", vec![hex(secret).into(), (*email).to_owned().into()]).await.unwrap();
        }
        let keys: Arc<dyn KeySource> = Arc::new(keys);
        let outbox = Arc::new(Capture::default());
        let sharing = Arc::new(Sharing::new(db.clone(), keys.clone(), outbox.clone(), None, None));
        let blobs = Arc::new(FsBlobStore::open(dir.join("blobs")).unwrap());
        let mut backup = PrivateBackup::new(db.clone(), keys, outbox.clone(), blobs.clone(), sharing, ActivityBus::new()).with_station(None, Some(peer.to_owned())).with_home(home, None).with_accounts_dir(Some(dir.join("nostr")));
        if let Some(origin) = from {
            let target = blobs.clone();
            backup = backup.with_prefetch(Arc::new(move |cid: String, _peer: String| {
                let (origin, target) = (origin.clone(), target.clone());
                Box::pin(async move {
                    use astro_dav::BlobWriter as _;
                    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
                    let (mut reader, _) = origin.open(&cid).await.map_err(|e| e.to_string())?;
                    let mut bytes = Vec::new();
                    reader.read_to_end(&mut bytes).await.map_err(|e| e.to_string())?;
                    let mut w = target.begin().await.map_err(|e| e.to_string())?;
                    w.write_all(&bytes).await.map_err(|e| e.to_string())?;
                    let got = w.commit().await.map_err(|e| e.to_string())?.cid;
                    if got == cid { Ok(()) } else { Err(format!("blob altéré : {got} au lieu de {cid}")) }
                })
            }));
        }
        Node { dir, db, outbox, blobs, backup }
    }

    /// Ce que le crawl rapatrie : l'événement est rangé et projeté (graphe social, DID).
    async fn give(db: &Db, event: Event) {
        db.writer()
            .transaction(move |tx| {
                astro_db::events::insert_event(tx, &event)?;
                astro_db::apply_statements(tx, &astro_sync::Projector::plan(&event).map_err(|e| astro_db::DbError::Corrupt(e.to_string()))?)?;
                Ok(())
            })
            .await
            .unwrap();
    }

    /// Les messages que `from` a envoyés arrivent chez `to` (un relais les lui sert).
    async fn pump(from: &Node, to: &Node) -> usize {
        let msgs: Vec<(String, Event)> = from.outbox.0.lock().unwrap().drain(..).collect();
        let n = msgs.len();
        for (_, wrap) in msgs {
            to.db.writer().transaction(move |tx| insert_event(tx, &wrap).map(|_| ())).await.unwrap();
        }
        n
    }

    fn follows(secret: &[u8; 32], who: &[&[u8; 32]]) -> Event {
        UnsignedEvent { created_at: 100, kind: 3, tags: who.iter().map(|w| vec!["p".to_owned(), hex(w)]).collect(), content: String::new() }.sign(secret).unwrap()
    }

    fn did_of(secret: &[u8; 32], email: &str) -> Event {
        let id = hex(secret);
        let doc = json!({ "id": format!("did:nostr:{id}"), "type": "DIDNostr", "metadata": { "email": email, "contractStatus": "active_rental", "language": "fr" } });
        UnsignedEvent { created_at: 100, kind: kind::DID, tags: vec![vec!["d".into(), "did".into()], vec!["t".into(), "uplanet".into()], vec!["t".into(), "did-document".into()]], content: doc.to_string() }.sign(secret).unwrap()
    }

    async fn scalar(db: &Db, sql: &'static str, args: Vec<String>) -> i64 {
        db.readers().read(move |c| Ok(c.query_row(sql, rusqlite::params_from_iter(args), |r| r.get(0))?)).await.unwrap()
    }

    #[tokio::test]
    async fn a_member_whose_station_burned_gets_back_the_account_the_files_and_the_home_without_any_friend_acting() {
        const NODE: &str = "ab";
        let (alice, bob) = (hex(&ALICE), hex(&BOB));
        let new_home = format!("12D3KooWNewHome:{}", NODE.repeat(32));
        let old_home = format!("12D3KooWOldHome:{}", "cd".repeat(32));

        // Trois stations : l'ancienne (Alice), celle de Bob, et la nouvelle (Alice rétablie). Chacune rapatrie depuis l'autre.
        let old = node("old", &[(&ALICE, "alice@example.org")], &[(&BOB, "bob@example.org")], "12D3KooWOldStation", Some(old_home.clone()), None).await;
        let host = node("host", &[(&BOB, "bob@example.org")], &[(&ALICE, "alice@example.org")], "12D3KooWBobStation", None, Some(old.blobs.clone())).await;
        // La nouvelle station héberge Alice dès son rétablissement : sa clé (la même, dérivée de SALT/PEPPER) y est connue.
        let new = node("new", &[(&ALICE, "alice@example.org")], &[(&BOB, "bob@example.org")], "12D3KooWNewStation", Some(new_home.clone()), Some(host.blobs.clone())).await;

        // Chacune connaît le graphe social de la sienne.
        for (n, owner, friend) in [(&old, &ALICE, &BOB), (&host, &BOB, &ALICE)] {
            n.db.writer().execute("INSERT INTO social_graph(owner_hex, pubkey, level, updated_at) VALUES (?1, ?2, 1, 0)", vec![hex(owner).into(), hex(friend).into()]).await.unwrap();
        }
        give(&old.db, follows(&BOB, &[&ALICE])).await;
        give(&host.db, follows(&ALICE, &[&BOB])).await;

        // 1. Alice a un uCloud : trois fichiers chiffrés, leur trousseau, un manifeste publié.
        let mut files = Vec::new();
        for (i, name) in ["a.txt", "b.txt", "c.txt"].iter().enumerate() {
            let cid = put_blob(&old.backup, format!("contenu chiffré de {name}").as_bytes()).await;
            let (path, key) = (format!("/Docs/{name}"), format!("{:064x}", i + 1));
            old.db.writer().execute("INSERT INTO ucloud_entries(owner_hex, path, parent, type, cid, mime, size, mtime, data) VALUES (?1, ?2, '/Docs', 'file', ?3, 'text/plain', 20, 1, '{}')", vec![alice.clone().into(), path.into(), cid.clone().into()]).await.unwrap();
            old.db.writer().execute("INSERT INTO ucloud_keyring(owner_hex, cid, key_hex, enc) VALUES (?1, ?2, ?3, 'uenc2')", vec![alice.clone().into(), cid.clone().into(), key.into()]).await.unwrap();
            files.push(cid);
        }
        let published = manifest::publish(&old.db, &*old.blobs, &*old.backup.keys, &alice).await.unwrap();
        let manifest_cid = published.pointer.manifest_cid.clone();

        // 2. Bob accepte d'héberger ; la nuit, tout part chez lui : les trois fichiers ET le manifeste.
        old.backup.set(&alice, true, Some(&bob)).await.unwrap();
        pump(&old, &host).await;
        host.backup.poll_once(now()).await;
        host.backup.respond(&bob, &alice, true, 1_000_000, 1_000_000).await.unwrap();
        pump(&host, &old).await;
        old.backup.poll_once(now()).await;
        // Gardien d'une ancienne version (sans `rs`) : copie complète, la voie historique que ce test suit.
        old.db.writer().execute("DELETE FROM kv WHERE scope = 'backup_rs'", vec![]).await.unwrap();
        assert_eq!(old.backup.run_once().await, 4, "le manifeste et les trois fichiers privés");
        pump(&old, &host).await;
        assert_eq!(host.backup.poll_once(now()).await.stored, 4);
        assert_eq!(scalar(&host.db, "SELECT COUNT(*) FROM backup_items WHERE owner_hex = ?1 AND cid = ?2", vec![alice.clone(), manifest_cid.clone()]).await, 1, "le blob du manifeste est chez l'ami");
        let manifest_event = old.backup.latest_event(&alice, kind::APP_DATA, Some(manifest::D_TAG)).await.unwrap();
        let did = did_of(&ALICE, "alice@example.org");

        // 3. La station d'Alice brûle. Une station vierge la rétablit depuis sa carte : même clé, état « restauration en cours ».
        let old_dir = old.dir.clone();
        drop(old);
        let _ = std::fs::remove_dir_all(old_dir);
        new.backup.kv_put("restore", &alice, "pending", now()).await;

        // Aucun manifeste vide ne doit remplacer l'original tant que la restauration dure.
        assert!(manifest::restoring(&new.db, &alice).await);
        assert!(manifest::publish(&new.db, &*new.blobs, &*new.backup.keys, &alice).await.is_err());

        // 4. Le crawl rapatrie les événements d'Alice (contacts, DID, manifeste) et ceux de Bob.
        for e in [follows(&ALICE, &[&BOB]), did.clone(), manifest_event.clone(), follows(&BOB, &[&ALICE])] {
            give(&new.db, e).await;
        }
        // Un premier passage : annonce de la nouvelle station d'accueil + demande à chaque proche.
        assert_eq!(new.backup.restore_step(now()).await, 1, "une demande, pour Bob");
        assert_eq!(scalar(&new.db, "SELECT COUNT(*) FROM kv WHERE scope = 'restore_home' AND key = ?1", vec![alice.clone()]).await, 1);
        let announced: String = new.db.readers().read({ let a = alice.clone(); move |c| Ok(c.query_row("SELECT home_station FROM identities WHERE hex = ?1", [a], |r| r.get(0))?) }).await.unwrap();
        assert_eq!(announced, new_home, "le DID republié porte la nouvelle station d'accueil");
        assert_eq!(new.backup.restore_step(now()).await, 0, "la demande n'est pas renouvelée avant 6 h");

        // 5. Bob n'a RIEN à faire : sa station apprend la nouvelle Home et renvoie d'elle-même ce qu'elle détient.
        pump(&new, &host).await;
        assert_eq!(host.backup.poll_once(now()).await.control, 1);
        let bobs_view: String = host.db.readers().read({ let a = alice.clone(); move |c| Ok(c.query_row("SELECT COALESCE(home_station, '') FROM identities WHERE hex = ?1", [a], |r| r.get(0))?) }).await.unwrap();
        assert_eq!(bobs_view, new_home, "les amis écrivent désormais à la nouvelle station");
        assert_eq!(host.backup.serve_restores().await, 4, "le manifeste d'abord, puis les trois fichiers, sans doublon");
        assert_eq!(host.backup.serve_restores().await, 0);
        assert_eq!(scalar(&host.db, "SELECT COUNT(*) FROM kv WHERE scope = 'backup_restore_job'", vec![]).await, 0, "renvoi terminé");

        // 6. Alice retrouve tout : manifeste appliqué, VFS et trousseau, blobs, accord de sauvegarde rétabli.
        pump(&host, &new).await;
        let stats = new.backup.poll_once(now()).await;
        assert_eq!((stats.stored, stats.ignored, stats.deferred), (4, 0, 0));
        assert_eq!(new.backup.kv_get("restore", &alice).await.map(|v| v.0), Some("done".to_owned()));
        assert!(!manifest::restoring(&new.db, &alice).await);
        assert_eq!(scalar(&new.db, "SELECT COUNT(*) FROM ucloud_entries WHERE owner_hex = ?1 AND type = 'file'", vec![alice.clone()]).await, 3);
        assert_eq!(scalar(&new.db, "SELECT COUNT(*) FROM ucloud_keyring WHERE owner_hex = ?1", vec![alice.clone()]).await, 3);
        for cid in &files {
            assert!(new.blobs.open(cid).await.is_ok(), "le blob {cid} est revenu");
        }
        assert_eq!(scalar(&new.db, "SELECT COUNT(*) FROM backup_pacts WHERE owner_hex = ?1 AND host_hex = ?2 AND status = 'accepted' AND enabled = 1", vec![alice.clone(), bob.clone()]).await, 1);
        assert_eq!(new.backup.run_once().await, 0, "tout est déjà chez Bob : rien à renvoyer cette nuit");

        // 7. Garde-fous : un proche quelconque ne récupère pas les sauvegardes d'autrui.
        let eve = [0xE5u8; 32];
        host.db.writer().execute("INSERT INTO identities(hex, role, email, hosted) VALUES (?1, 'multipass', 'eve@example.org', 0)", vec![hex(&eve).into()]).await.unwrap();
        host.db.writer().execute("INSERT INTO social_graph(owner_hex, pubkey, level, updated_at) VALUES (?1, ?2, 1, 0)", vec![bob.clone().into(), hex(&eve).into()]).await.unwrap();
        give(&host.db, follows(&eve, &[&BOB])).await;
        let ask = Rumor { pubkey: hex(&eve), created_at: u64::try_from(now()).unwrap(), kind: kind::CHAT, tags: vec![vec!["p".into(), bob.clone()], vec!["t".into(), TAG.into()], vec!["op".into(), "restore".into()], vec!["cid".into(), files[0].clone()]], content: String::new() };
        let wrap = gift_wrap(&eve, &bob, &ask, ask.created_at).unwrap();
        host.db.writer().transaction(move |tx| insert_event(tx, &wrap).map(|_| ())).await.unwrap();
        host.backup.poll_once(now()).await;
        assert_eq!(host.backup.serve_restores().await, 0, "le CID d'un fichier d'Alice ne se redemande pas au nom d'Eve");
        assert_eq!(scalar(&host.db, "SELECT COUNT(*) FROM kv WHERE scope = 'backup_restore_job'", vec![]).await, 0);

        for n in [&new, &host] {
            let _ = std::fs::remove_dir_all(&n.dir);
        }
    }

    /// Bob, le gardien, est hors ligne plus de 3 jours : le compte d'Alice repart (elle publie de nouveau), la station relance
    /// Bob une fois par jour, et quand il revient son manifeste est FUSIONNÉ sans rien écraser ; au bout de 30 jours, abandon.
    #[tokio::test]
    async fn a_guardian_back_after_three_days_gets_his_files_merged_without_overwriting_and_absent_ones_are_asked_daily_for_thirty_days() {
        const DAY: i64 = 86_400;
        let (alice, bob) = (hex(&ALICE), hex(&BOB));
        let new_home = format!("12D3KooWNewHome:{}", "ab".repeat(32));
        let old = node("late-old", &[(&ALICE, "alice@example.org")], &[(&BOB, "bob@example.org")], "12D3KooWOldStation", None, None).await;
        let host = node("late-host", &[(&BOB, "bob@example.org")], &[(&ALICE, "alice@example.org")], "12D3KooWBobStation", None, Some(old.blobs.clone())).await;
        let new = node("late-new", &[(&ALICE, "alice@example.org")], &[(&BOB, "bob@example.org")], "12D3KooWNewStation", Some(new_home), Some(host.blobs.clone())).await;
        for (n, owner, friend) in [(&old, &ALICE, &BOB), (&host, &BOB, &ALICE)] {
            n.db.writer().execute("INSERT INTO social_graph(owner_hex, pubkey, level, updated_at) VALUES (?1, ?2, 1, 0)", vec![hex(owner).into(), hex(friend).into()]).await.unwrap();
        }
        give(&old.db, follows(&BOB, &[&ALICE])).await;
        give(&host.db, follows(&ALICE, &[&BOB])).await;
        let mut files = Vec::new();
        for name in ["a.txt", "b.txt"] {
            let cid = put_blob(&old.backup, format!("ancien {name}").as_bytes()).await;
            old.db.writer().execute("INSERT INTO ucloud_entries(owner_hex, path, parent, type, cid, mime, size, mtime, data) VALUES (?1, ?2, '/Docs', 'file', ?3, 'text/plain', 20, 1, '{}')", vec![alice.clone().into(), format!("/Docs/{name}").into(), cid.clone().into()]).await.unwrap();
            old.db.writer().execute("INSERT INTO ucloud_keyring(owner_hex, cid, key_hex, enc) VALUES (?1, ?2, ?3, 'uenc2')", vec![alice.clone().into(), cid.clone().into(), format!("{:064x}", files.len() + 1).into()]).await.unwrap();
            files.push(cid);
        }
        old.db.writer().execute("INSERT INTO ucloud_entries(owner_hex, path, parent, type, data) VALUES (?1, '/Docs', '/', 'dir', '{}')", vec![alice.clone().into()]).await.unwrap();
        manifest::publish(&old.db, &*old.blobs, &*old.backup.keys, &alice).await.unwrap();
        old.backup.set(&alice, true, Some(&bob)).await.unwrap();
        pump(&old, &host).await;
        host.backup.poll_once(now()).await;
        host.backup.respond(&bob, &alice, true, 1_000_000, 1_000_000).await.unwrap();
        pump(&host, &old).await;
        old.backup.poll_once(now()).await;
        // Gardien d'une ancienne version (sans `rs`) : copie complète, la voie historique que ce test suit.
        old.db.writer().execute("DELETE FROM kv WHERE scope = 'backup_rs'", vec![]).await.unwrap();
        old.backup.run_once().await;
        pump(&old, &host).await;
        host.backup.poll_once(now()).await;
        let manifest_event = old.backup.latest_event(&alice, kind::APP_DATA, Some(manifest::D_TAG)).await.unwrap();
        drop(old);

        // Rétablie il y a 4 jours ; Bob n'a jamais répondu. Le crawl a rapatrié le manifeste d'origine.
        let t0 = now();
        new.backup.kv_put("restore", &alice, "pending", t0 - 4 * DAY).await;
        for e in [follows(&ALICE, &[&BOB]), did_of(&ALICE, "alice@example.org"), manifest_event, follows(&BOB, &[&ALICE])] {
            give(&new.db, e).await;
        }
        assert_eq!(new.backup.restore_step(t0).await, 0);
        assert_eq!(new.backup.kv_get("restore", &alice).await.map(|v| v.0), Some("late".to_owned()), "plus d'abandon sec à 3 jours");
        assert!(new.backup.kv_get(ORIGIN, &alice).await.is_some(), "le manifeste d'origine est gardé avant d'être remplacé");
        assert!(!manifest::restoring(&new.db, &alice).await, "la publication des manifestes est débloquée");
        // Alice vit sans ses fichiers : elle réécrit a.txt, et son nouveau manifeste remplace celui d'origine sur le réseau.
        let recent = put_blob(&new.backup, b"a.txt d'aujourd'hui").await;
        new.db.writer().execute("INSERT INTO ucloud_entries(owner_hex, path, parent, type, data) VALUES (?1, '/Docs', '/', 'dir', '{}')", vec![alice.clone().into()]).await.unwrap();
        new.db.writer().execute("INSERT INTO ucloud_entries(owner_hex, path, parent, type, cid, mime, size, mtime, data) VALUES (?1, '/Docs/a.txt', '/Docs', 'file', ?2, 'text/plain', 20, 2, '{}')", vec![alice.clone().into(), recent.clone().into()]).await.unwrap();
        new.db.writer().execute("INSERT INTO ucloud_keyring(owner_hex, cid, key_hex, enc) VALUES (?1, ?2, ?3, 'uenc2')", vec![alice.clone().into(), recent.clone().into(), format!("{:064x}", 99).into()]).await.unwrap();
        manifest::publish(&new.db, &*new.blobs, &*new.backup.keys, &alice).await.unwrap();

        // Relance quotidienne des absents : une demande, puis rien avant 24 h, puis de nouveau.
        assert_eq!(new.backup.restore_step(t0).await, 1, "Bob est relancé");
        assert_eq!(new.backup.restore_step(t0 + DAY - 60).await, 0, "pas deux relances le même jour");
        new.outbox.0.lock().unwrap().clear();
        assert_eq!(new.backup.restore_step(t0 + DAY + 60).await, 1, "relance du lendemain");
        let view = new.backup.status(&alice).await.unwrap();
        assert_eq!(view["restore"]["state"], "late");
        assert_eq!(view["restore"]["waiting"][0]["label"], "bob");

        // Bob revient : sa station renvoie le manifeste d'origine et les fichiers ; ils sont fusionnés sans rien écraser.
        pump(&new, &host).await;
        host.backup.poll_once(now()).await;
        while host.backup.serve_restores().await > 0 {}
        pump(&host, &new).await;
        new.backup.poll_once(now()).await;
        assert_eq!(new.backup.kv_get("restore", &alice).await.map(|v| v.0), Some("done".to_owned()));
        let at = |path: &'static str| {
            let (db, a) = (new.db.clone(), alice.clone());
            async move { db.readers().read(move |c| Ok(c.query_row("SELECT COALESCE((SELECT cid FROM ucloud_entries WHERE owner_hex = ?1 AND path = ?2), '')", [a, path.to_owned()], |r| r.get::<_, String>(0))?)).await.unwrap() }
        };
        assert_eq!(at("/Docs/a.txt").await, recent, "le fichier d'aujourd'hui n'est pas écrasé");
        assert_eq!(at("/Docs/a (ancien).txt").await, files[0], "l'ancien est gardé à côté, renommé");
        assert_eq!(at("/Docs/b.txt").await, files[1], "ce qui manquait revient à sa place");
        assert!(new.backup.kv_get(ORIGIN, &alice).await.is_none());
        let view = new.backup.status(&alice).await.unwrap();
        assert_eq!((view["restore"]["state"].as_str(), view["restore"]["returned"][0].as_str()), (Some("done"), Some("bob")), "suivi : Bob a renvoyé");
        // Rejeu : les mêmes envois relivrés ne changent plus rien.
        let before = scalar(&new.db, "SELECT COUNT(*) FROM ucloud_entries WHERE owner_hex = ?1", vec![alice.clone()]).await;
        assert!(!new.backup.try_restore_manifest(&alice, now()).await);
        assert_eq!(scalar(&new.db, "SELECT COUNT(*) FROM ucloud_entries WHERE owner_hex = ?1", vec![alice.clone()]).await, before);

        // Au bout de 30 jours de relances sans retour : abandon, et le manifeste publié ici n'est jamais pris pour l'original.
        new.backup.kv_put("restore", &alice, "late", t0 - 31 * DAY).await;
        assert_eq!(new.backup.restore_step(t0).await, 0);
        assert_eq!(new.backup.kv_get("restore", &alice).await.map(|v| v.0), Some("abandoned".to_owned()));
        for n in [&new, &host] {
            let _ = std::fs::remove_dir_all(&n.dir);
        }
    }

    /// Ma sécurité : un gardien = un ami qui héberge la sauvegarde ET garde la part ; sa disponibilité vient des jours où il
    /// s'est montré ; sans 7 jours de recul, pas de chiffre ; un gardien sur la même station ne compte pas.
    #[tokio::test]
    async fn the_safety_estimate_counts_only_guardians_off_this_station_and_waits_for_enough_hindsight() {
        const DAY: i64 = 86_400;
        let (alice, carol) = (hex(&ALICE), hex(&CAROL));
        let n = node("safety", &[(&ALICE, "alice@example.org"), (&BOB, "bob@example.org")], &[(&CAROL, "carol@example.org")], "12D3KooWSafety", None, None).await;
        let empty = n.backup.status(&alice).await.unwrap();
        assert_eq!((empty["guardians"].as_array().map(Vec::len), &empty["safety"]["permille"]), (Some(0), &Value::Null));
        let t = now();
        accept_pact(&n.db, &ALICE, &CAROL).await;
        n.db.writer().execute("UPDATE backup_pacts SET decided_at = ?1", vec![(t - 2 * DAY).into()]).await.unwrap();
        let st = n.backup.status(&alice).await.unwrap();
        assert_eq!(st["guardians"].as_array().map(Vec::len), Some(0), "pas encore de part confiée : pas encore gardienne");
        n.backup.kv_put(SHARE_SENT, &format!("{alice}:{carol}"), "f", t).await;
        let st = n.backup.status(&alice).await.unwrap();
        assert_eq!((st["guardians"][0]["label"].as_str(), &st["safety"]["permille"]), (Some("carol"), &Value::Null), "2 jours de recul : pas de chiffre inventé");
        // 20 jours de recul, vue 18 jours (dont aujourd'hui par un événement que le crawl a rapporté).
        n.db.writer().execute("UPDATE backup_pacts SET decided_at = ?1", vec![(t - 19 * DAY).into()]).await.unwrap();
        for d in 1..18 {
            n.db.writer().execute("INSERT INTO guardian_presence(hex, day) VALUES (?1, ?2)", vec![carol.clone().into(), (t / DAY - d).into()]).await.unwrap();
        }
        give(&n.db, UnsignedEvent { created_at: u64::try_from(t).unwrap(), kind: 1, tags: vec![], content: "bonjour".into() }.sign(&CAROL).unwrap()).await;
        assert_eq!(n.backup.observe_presence(t).await, 1);
        assert_eq!(n.backup.observe_presence(t + 60).await, 0, "au plus une fois par heure");
        let st = n.backup.status(&alice).await.unwrap();
        assert_eq!(st["guardians"][0]["availability"], 900);
        assert_eq!((&st["safety"]["permille"], &st["safety"]["next_permille"]), (&json!(900), &json!(990)), "un gardien de plus : 99 %");
        // Bob, hébergé sur cette même station, devient gardien aussi : il ne protège pas d'une panne de la station.
        accept_pact(&n.db, &ALICE, &BOB).await;
        n.backup.kv_put(SHARE_SENT, &format!("{alice}:{}", hex(&BOB)), "f", t).await;
        let st = n.backup.status(&alice).await.unwrap();
        assert_eq!(st["guardians"].as_array().map(Vec::len), Some(2));
        assert_eq!(st["safety"]["permille"], 900, "même station : ne compte pas");
        let _ = std::fs::remove_dir_all(&n.dir);
    }

    /// Gardien conseillé : d'abord un proche sur un **autre Astroport**, puis le cercle le plus proche (Mes cercles, privé),
    /// puis rencontré en personne ; un proche de la même station vient en dernier, même rangé en Famille.
    #[tokio::test]
    async fn the_advised_guardian_is_on_another_astroport_then_in_the_closest_circle() {
        const DAN: [u8; 32] = [0xD5; 32];
        let (alice, bob, carol, dan) = (hex(&ALICE), hex(&BOB), hex(&CAROL), hex(&DAN));
        let n = node("advise", &[(&ALICE, "alice@example.org"), (&BOB, "bob@example.org")], &[(&CAROL, "carol@example.org"), (&DAN, "dan@example.org")], "12D3KooWAdvise", None, None).await;
        for (sk, who) in [(&BOB, &bob), (&CAROL, &carol), (&DAN, &dan)] {
            n.db.writer().execute("INSERT INTO social_graph(owner_hex, pubkey, level, updated_at) VALUES (?1, ?2, 1, 0)", vec![alice.clone().into(), who.clone().into()]).await.unwrap();
            give(&n.db, UnsignedEvent { created_at: 10, kind: 3, tags: vec![vec!["p".into(), alice.clone()]], content: String::new() }.sign(sk).unwrap()).await;
        }
        let order = |st: &Value| st["friends"].as_array().unwrap().iter().map(|f| f["hex"].as_str().unwrap().to_owned()).collect::<Vec<_>>();
        let st = n.backup.status(&alice).await.unwrap();
        assert_eq!(order(&st), [carol.clone(), dan.clone(), bob.clone()], "ailleurs d'abord, puis par nom");
        assert_eq!((&st["friends"][2]["same_station"], &st["friends"][0]["level"]), (&json!(true), &json!(3)), "Ami par défaut");
        // Alice range Dan en Famille et Bob aussi (même station) ; elle a rencontré Carol en personne.
        let ranks: crate::proximity::Ranks = [(dan.clone(), 5), (bob.clone(), 5)].into_iter().collect();
        give(&n.db, crate::proximity::event(n.backup.keys.as_ref(), &alice, &ranks, 20, 0).unwrap()).await;
        n.db.writer().execute("INSERT INTO meetings(owner_hex, target_hex, how, verified, at, src_event) VALUES (?1, ?2, 'qr', 1, 30, 'x')", vec![alice.clone().into(), carol.clone().into()]).await.unwrap();
        let st = n.backup.status(&alice).await.unwrap();
        assert_eq!(order(&st), [dan, carol, bob], "Famille d'abord, mais jamais la même station avant un autre Astroport");
        assert_eq!((&st["friends"][0]["level"], &st["friends"][1]["met_verified"]), (&json!(5), &json!(true)));
        let _ = std::fs::remove_dir_all(&n.dir);
    }

    #[tokio::test]
    async fn files_that_a_friend_received_from_the_member_come_back_by_cid_without_creating_a_backup_agreement() {
        let (alice, bob) = (hex(&ALICE), hex(&BOB));
        let host = node("share-host", &[(&BOB, "bob@example.org")], &[(&ALICE, "alice@example.org")], "12D3KooWBobStation", None, None).await;
        let new = node("share-new", &[(&ALICE, "alice@example.org")], &[(&BOB, "bob@example.org")], "12D3KooWNewStation", None, Some(host.blobs.clone())).await;
        for (n, owner, friend) in [(&new, &ALICE, &BOB), (&host, &BOB, &ALICE)] {
            n.db.writer().execute("INSERT INTO social_graph(owner_hex, pubkey, level, updated_at) VALUES (?1, ?2, 1, 0)", vec![hex(owner).into(), hex(friend).into()]).await.unwrap();
        }
        for n in [&new, &host] {
            give(&n.db, follows(&ALICE, &[&BOB])).await;
            give(&n.db, follows(&BOB, &[&ALICE])).await;
        }

        // Bob a reçu un fichier d'Alice (rangé sous /Partages) ; Alice, rétablie, le connaît par son manifeste mais n'a plus le blob.
        let shared = put_blob(&host.backup, b"photo partagee avec Bob").await;
        host.db
            .writer()
            .execute("INSERT INTO ucloud_entries(owner_hex, path, parent, type, cid, mime, size, mtime, data) VALUES (?1, '/Partages/De_alice/x.jpg', '/Partages/De_alice', 'file', ?2, 'image/jpeg', 23, 1, ?3)", vec![bob.clone().into(), shared.clone().into(), json!({ "shared_by": alice }).to_string().into()])
            .await
            .unwrap();
        // Un fichier privé de Bob, que personne d'autre ne doit pouvoir se faire rendre.
        let private = put_blob(&host.backup, b"journal prive de Bob").await;
        host.db.writer().execute("INSERT INTO ucloud_entries(owner_hex, path, parent, type, cid, mime, size, mtime, data) VALUES (?1, '/Docs/journal.txt', '/Docs', 'file', ?2, 'text/plain', 20, 1, '{}')", vec![bob.clone().into(), private.clone().into()]).await.unwrap();

        for cid in [&shared, &private] {
            new.db.writer().execute("INSERT INTO ucloud_entries(owner_hex, path, parent, type, cid, mime, size, mtime, data) VALUES (?1, ?2, '/Docs', 'file', ?3, 'image/jpeg', 23, 1, '{}')", vec![alice.clone().into(), format!("/Docs/{}", &cid[cid.len() - 6..]).into(), cid.clone().into()]).await.unwrap();
        }
        let at = now();
        new.backup.kv_put("restore", &alice, "done", at - 100_000).await;
        new.backup.kv_put("restore_missing", &alice, "0", at - 100_000).await;

        assert_eq!(new.backup.restore_step(at).await, 1, "une demande par CID manquant, pour Bob");
        pump(&new, &host).await;
        host.backup.poll_once(now()).await;
        assert_eq!(host.backup.serve_restores().await, 1, "seul le fichier qu'Alice lui avait partagé est rendu, pas le journal de Bob");
        pump(&host, &new).await;
        let stats = new.backup.poll_once(now()).await;
        assert_eq!((stats.stored, stats.ignored), (1, 0));
        assert!(new.blobs.open(&shared).await.is_ok() && new.blobs.open(&private).await.is_err());
        assert_eq!(scalar(&new.db, "SELECT COUNT(*) FROM backup_pacts", vec![]).await, 0, "un fichier partagé ne crée aucun accord de sauvegarde");
        assert_eq!(new.backup.kv_get("restore_missing", &alice).await.map(|v| v.0), Some("1".to_owned()));
        assert_eq!(new.backup.restore_step(now()).await, 0, "pas de relance avant 6 h");

        // Un message `restored` qu'aucune restauration n'attendait est écarté sans bruit.
        let stranger = node("share-stranger", &[(&CAROL, "carol@example.org")], &[(&BOB, "bob@example.org")], "12D3KooWCarolStation", None, None).await;
        stranger.db.writer().execute("INSERT INTO social_graph(owner_hex, pubkey, level, updated_at) VALUES (?1, ?2, 1, 0)", vec![hex(&CAROL).into(), bob.clone().into()]).await.unwrap();
        give(&stranger.db, follows(&BOB, &[&CAROL])).await;
        give(&host.db, follows(&CAROL, &[&BOB])).await;
        host.db.writer().execute("INSERT INTO identities(hex, role, email, hosted) VALUES (?1, 'multipass', 'carol@example.org', 0)", vec![hex(&CAROL).into()]).await.unwrap();
        host.db.writer().execute("INSERT INTO social_graph(owner_hex, pubkey, level, updated_at) VALUES (?1, ?2, 1, 0)", vec![bob.clone().into(), hex(&CAROL).into()]).await.unwrap();
        host.backup.send_restored(&bob, &hex(&CAROL), &shared, 23, 0, "share").await.unwrap();
        pump(&host, &stranger).await;
        let stats = stranger.backup.poll_once(now()).await;
        assert_eq!((stats.stored, stats.ignored), (0, 1));

        for n in [&new, &host, &stranger] {
            let _ = std::fs::remove_dir_all(&n.dir);
        }
    }

    fn did_at(secret: &[u8; 32], email: &str, home: &str) -> Event {
        let id = hex(secret);
        let doc = json!({ "id": format!("did:nostr:{id}"), "type": "DIDNostr", "metadata": { "email": email, "contractStatus": "active_rental", "language": "fr", "astroportStation": home } });
        UnsignedEvent { created_at: 100, kind: kind::DID, tags: vec![vec!["d".into(), "did".into()], vec!["t".into(), "uplanet".into()], vec!["home_station".into(), home.to_owned()]], content: doc.to_string() }.sign(secret).unwrap()
    }

    /// Un message `moved` emballé pour `to`, signé de `from` (le compte lui-même, ou un tiers qui le contrefait).
    async fn moved_wrap(db: &Db, from: &[u8; 32], to: &[u8; 32], home: &str, at: u64) {
        let rumor = Rumor { pubkey: hex(from), created_at: at, kind: kind::CHAT, tags: vec![vec!["p".into(), hex(to)], vec!["t".into(), TAG.into()], vec!["op".into(), "moved".into()], vec!["home".into(), home.to_owned()]], content: String::new() };
        let wrap = gift_wrap(from, &hex(to), &rumor, at).unwrap();
        db.writer().transaction(move |tx| insert_event(tx, &wrap).map(|_| ())).await.unwrap();
    }

    #[tokio::test]
    async fn a_member_who_moves_away_on_purpose_makes_the_old_station_stop_hosting_the_account() {
        let alice = hex(&ALICE);
        let (old_home, new_home) = (format!("12D3KooWOldHome:{}", "cd".repeat(32)), format!("12D3KooWNewHome:{}", "ab".repeat(32)));
        let old = node("mv-old", &[(&ALICE, "alice@example.org")], &[], "12D3KooWOldStation", Some(old_home.clone()), None).await;
        let new = node("mv-new", &[(&ALICE, "alice@example.org")], &[], "12D3KooWNewStation", Some(new_home.clone()), None).await;
        let folder = old.dir.join("nostr/alice@example.org");
        std::fs::create_dir_all(&folder).unwrap();
        let nsec = bech32::encode::<bech32::Bech32>(bech32::Hrp::parse("nsec").unwrap(), &ALICE).unwrap();
        std::fs::write(folder.join(".secret.nostr"), format!("NSEC={nsec}; NPUB=x; HEX={alice}\n")).unwrap();
        let files = astro_crypto::FileKeySource::new(old.dir.join("nostr"));
        assert!(files.secret(&alice).is_some(), "avant : la station lit la clé du compte dans son dossier");

        // La nouvelle station connaît l'ancienne Home par le DID rapatrié ; elle s'annonce et prévient l'ancienne.
        give(&new.db, did_at(&ALICE, "alice@example.org", &old_home)).await;
        new.backup.kv_put("restore", &alice, "pending", now()).await;
        new.backup.restore_step(now()).await;
        assert_eq!(new.backup.kv_get("restore_moved", &alice).await.map(|v| v.0), Some("sent".to_owned()));
        assert_eq!(new.outbox.0.lock().unwrap().len(), 1, "un seul message `moved`, adressé au compte lui-même");
        new.backup.restore_step(now() + 3 * 3600).await;
        assert_eq!(new.outbox.0.lock().unwrap().len(), 1, "pas renvoyé une fois parti");

        // L'ancienne station le reçoit : le compte n'est plus hébergé, pointe vers la nouvelle Home, son dossier est mis de côté.
        pump(&new, &old).await;
        assert_eq!(old.backup.poll_once(now()).await.control, 1);
        let (hosted, home): (i64, String) = old.db.readers().read({ let a = alice.clone(); move |c| Ok(c.query_row("SELECT hosted, COALESCE(home_station, '') FROM identities WHERE hex = ?1", [a], |r| Ok((r.get(0)?, r.get(1)?)))?) }).await.unwrap();
        assert_eq!((hosted, home.as_str()), (0, new_home.as_str()));
        assert!(!folder.exists(), "plus de clé du compte dans le dossier lu par la station");
        let aside: Vec<_> = std::fs::read_dir(old.dir.join("trash/moved")).unwrap().flatten().collect();
        assert_eq!(aside.len(), 1);
        assert!(aside[0].path().join(".secret.nostr").is_file(), "mis de côté, jamais effacé");
        assert_eq!(old.backup.kv_get("moved", &alice).await.map(|v| v.0), Some(new_home.clone()));
        // La corbeille garde le compte 30 jours (il contient encore ses clés), puis la nuit l'efface ; un nom inattendu est laissé.
        let stray = old.dir.join("trash/moved/pas-de-date");
        std::fs::create_dir_all(&stray).unwrap();
        assert_eq!(old.backup.purge_moved(now() + MOVED_KEEP_SECS - 3600).await.unwrap(), 0, "trop tôt");
        assert!(aside[0].path().exists());
        assert_eq!(old.backup.purge_moved(now() + MOVED_KEEP_SECS + 3600).await.unwrap(), 1);
        assert!(!aside[0].path().exists() && stray.exists(), "le compte est effacé, le dossier sans date est laissé");
        assert!(files.secret(&alice).is_none(), "après : la station ne peut plus signer pour ce compte");

        // Garde-fous, sur une autre ancienne station (le compte y est hébergé) : rien de tout cela ne doit le débarquer à tort.
        let station = node("mv-guard", &[(&ALICE, "alice@example.org")], &[(&BOB, "bob@example.org")], "12D3KooWGuardStation", Some(old_home.clone()), None).await;
        let kept = || async { scalar(&station.db, "SELECT hosted FROM identities WHERE hex = ?1", vec![hex(&ALICE)]).await };
        let t = u64::try_from(now()).unwrap();
        moved_wrap(&station.db, &BOB, &ALICE, &new_home, t).await; // contrefait par un tiers
        moved_wrap(&station.db, &ALICE, &ALICE, &old_home, t).await; // la station annoncée est celle-ci
        moved_wrap(&station.db, &ALICE, &ALICE, "pas-une-station", t).await; // adresse illisible
        station.backup.poll_once(now()).await;
        assert_eq!(kept().await, 1, "ni un tiers, ni la même station, ni une adresse invalide ne comptent");
        // Le Capitaine n'est pas débarqué par un message : le Capitaine tranche.
        station.db.writer().execute("UPDATE identities SET role = 'captain' WHERE hex = ?1", vec![hex(&ALICE).into()]).await.unwrap();
        moved_wrap(&station.db, &ALICE, &ALICE, &new_home, t).await;
        station.backup.poll_once(now()).await;
        assert_eq!(kept().await, 1);
        station.db.writer().execute("UPDATE identities SET role = 'multipass' WHERE hex = ?1", vec![hex(&ALICE).into()]).await.unwrap();
        // Un ancien message rejoué après un rétablissement ici (postérieur au message) est ignoré ; un message plus récent compte.
        station.backup.kv_put("restore", &hex(&ALICE), "done", now() + 1000).await;
        moved_wrap(&station.db, &ALICE, &ALICE, &new_home, t).await;
        station.backup.poll_once(now()).await;
        assert_eq!(kept().await, 1, "un message antérieur au dernier rétablissement ici est un rejeu");
        station.backup.kv_put("restore", &hex(&ALICE), "done", now() - 1000).await;
        moved_wrap(&station.db, &ALICE, &ALICE, &new_home, t).await;
        station.backup.poll_once(now()).await;
        assert_eq!(kept().await, 0, "un vrai déménagement, postérieur à tout, est appliqué");
        for n in [&old, &new, &station] {
            let _ = std::fs::remove_dir_all(&n.dir);
        }
    }

    // ───────────────── Pièces jointes de messages copiées chez les amis hébergeurs ─────────────────

    async fn blob_in(n: &Node, bytes: &[u8]) -> String {
        use astro_dav::BlobWriter as _;
        use tokio::io::AsyncWriteExt as _;
        let mut w = n.blobs.begin().await.unwrap();
        w.write_all(bytes).await.unwrap();
        w.commit().await.unwrap().cid
    }

    fn uenc(fill: u8, len: usize) -> Vec<u8> {
        let mut v = b"UENC\x02\x01\x10".to_vec();
        v.resize(len, fill);
        v
    }

    #[tokio::test]
    async fn an_attachment_is_copied_to_the_friend_who_hosts_the_backup_only_if_encrypted_within_his_volume_and_until_its_expiry() {
        let (alice, carol) = (hex(&ALICE), hex(&CAROL));
        let owner = node("att-o", &[(&ALICE, "alice@example.org")], &[(&CAROL, "carol@example.org")], "12D3KooWAttOwner", None, None).await;
        let host = node("att-h", &[(&CAROL, "carol@example.org")], &[(&ALICE, "alice@example.org")], "12D3KooWAttHost", None, Some(owner.blobs.clone())).await;
        for (n, a, b) in [(&owner, &ALICE, &CAROL), (&host, &CAROL, &ALICE)] {
            n.db.writer().execute("INSERT INTO social_graph(owner_hex, pubkey, level, updated_at) VALUES (?1, ?2, 1, 0)", vec![hex(a).into(), hex(b).into()]).await.unwrap();
        }
        give(&host.db, follows(&ALICE, &[&CAROL])).await;
        let at = now();
        let expires = at + 3 * 86_400;

        // Sans accord d'hébergement : rien ne part.
        let good = blob_in(&owner, &uenc(7, 200)).await;
        assert_eq!(owner.backup.replicate_attachment(&alice, &good, 200, expires).await, 0);
        assert!(owner.outbox.0.lock().unwrap().is_empty());

        // Accord accepté (volume de 1 000 octets) : la copie part, avec l'échéance et sans aucune clé.
        accept_pact(&owner.db, &ALICE, &CAROL).await;
        accept_pact(&host.db, &ALICE, &CAROL).await;
        assert_eq!(owner.backup.replicate_attachment(&alice, &good, 200, expires).await, 1);
        let sent = owner.outbox.0.lock().unwrap().clone();
        let rumor = nip59::unwrap(&CAROL, &sent[0].1).unwrap().rumor;
        assert_eq!((tag(&rumor.tags, "op"), tag(&rumor.tags, "cid"), tag(&rumor.tags, "expires")), (Some("attachment"), Some(good.as_str()), Some(expires.to_string().as_str())));
        pump(&owner, &host).await;
        let stats = host.backup.poll_once(at).await;
        assert_eq!((stats.stored, stats.ignored), (1, 0), "{stats:?}");
        assert!(host.blobs.open(&good).await.is_ok(), "le blob est chez l'ami, vérifié par son CID");
        assert_eq!(scalar(&host.db, "SELECT count(*) FROM kv WHERE scope = 'guest_share' AND json_extract(value, '$.kind') = 'replica' AND json_extract(value, '$.owner') = ?1 AND json_extract(value, '$.expires_at') = CAST(?2 AS INTEGER)", vec![alice.clone(), expires.to_string()]).await, 1, "servi par la passerelle jusqu'à l'échéance");
        assert_eq!(astro_dav::share::replica_bytes(&host.db, &alice, at).await, 200);
        assert_eq!(scalar(&host.db, "SELECT count(*) FROM ucloud_entries WHERE path LIKE '/Sauvegardes/%'", vec![]).await, 0, "hors du VFS : pas de semaine de grâce, pas de nettoyage de nuit");
        assert_eq!(scalar(&host.db, "SELECT count(*) FROM ucloud_keyring", vec![]).await, 0, "l'ami n'a jamais de clé");

        // Du clair n'est jamais hébergé, et le blob apporté par ce message est retiré.
        let plain = blob_in(&owner, b"PNG pas chiffre pas chiffre pas chiffre").await;
        owner.backup.replicate_attachment(&alice, &plain, 39, expires).await;
        pump(&owner, &host).await;
        let stats = host.backup.poll_once(at).await;
        assert_eq!((stats.stored, stats.ignored), (0, 1), "{stats:?}");
        assert!(host.blobs.open(&plain).await.is_err());
        let (log, _) = host.backup.activity.subscribe(0);
        assert!(log.iter().any(|a| a.message.contains("copie de pièce jointe refusée")), "le Capitaine est prévenu");

        // Volume dépassé (800 octets annoncés, 200 déjà pris sur 1 000, puis 900) : refusé avant tout rapatriement.
        let big = blob_in(&owner, &uenc(9, 900)).await;
        owner.backup.replicate_attachment(&alice, &big, 900, expires).await;
        pump(&owner, &host).await;
        let stats = host.backup.poll_once(at).await;
        assert_eq!((stats.stored, stats.ignored), (0, 1), "{stats:?}");
        assert!(host.blobs.open(&big).await.is_err(), "rien n'a été rapatrié");

        // Échéance dépassée ou au-delà de 90 jours : refusé. Un mensonge sur la taille annoncée ne passe pas non plus.
        for exp in [at - 10, at + astro_dav::share::ATTACHMENT_MAX_TTL + 100] {
            let c = blob_in(&owner, &uenc(3, 100)).await;
            owner.backup.replicate_attachment(&alice, &c, 100, exp).await;
            pump(&owner, &host).await;
            assert_eq!(host.backup.poll_once(at).await.stored, 0, "{exp}");
        }
        let liar = blob_in(&owner, &uenc(5, 950)).await;
        owner.backup.replicate_attachment(&alice, &liar, 10, expires).await;
        pump(&owner, &host).await;
        assert_eq!(host.backup.poll_once(at).await.stored, 0, "la taille réelle (950) dépasse le volume restant");
        assert!(host.blobs.open(&liar).await.is_err());

        // Même copie renvoyée : pas de second enregistrement.
        owner.backup.replicate_attachment(&alice, &good, 200, expires).await;
        pump(&owner, &host).await;
        host.backup.poll_once(at).await;
        assert_eq!(scalar(&host.db, "SELECT count(*) FROM kv WHERE scope = 'guest_share'", vec![]).await, 1);

        // À l'échéance la passerelle ferme, puis la purge supprime le blob de chez l'ami.
        assert_eq!(astro_dav::share::purge_expired(&host.db, host.blobs.as_ref(), expires - 1).await.unwrap(), 0);
        assert_eq!(astro_dav::share::purge_expired(&host.db, host.blobs.as_ref(), expires + 1).await.unwrap(), 1);
        assert!(host.blobs.open(&good).await.is_err());
        assert_eq!(astro_dav::share::replica_bytes(&host.db, &alice, expires + 1).await, 0, "le volume est libéré");
        let _ = (std::fs::remove_dir_all(&owner.dir), std::fs::remove_dir_all(&host.dir));
        let _ = carol;
    }

    // ───────────────── Sauvegarde k-sur-n : fragments chez les gardiens, restauration par tirage ─────────────────

    const GUARDS: [[u8; 32]; 5] = [[0x11; 32], [0x12; 32], [0x13; 32], [0x14; 32], [0x15; 32]];
    fn guard(i: usize) -> &'static [u8; 32] {
        &GUARDS[i]
    }

    fn gx(i: usize) -> String {
        hex(guard(i))
    }

    const OWNER_PEER: &str = "12D3KooWOwnerStation";

    /// Le maillage de test : PeerID → magasin de la station. Une station retirée de la carte est éteinte.
    type Mesh = Arc<Mutex<std::collections::HashMap<String, Arc<FsBlobStore>>>>;

    fn mesh_prefetch(mesh: Mesh, target: Arc<FsBlobStore>) -> Prefetch {
        Arc::new(move |cid: String, peer: String| {
            let (mesh, target) = (mesh.clone(), target.clone());
            Box::pin(async move {
                use astro_dav::BlobWriter as _;
                use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
                let origin = mesh.lock().unwrap().get(&peer).cloned().ok_or("station éteinte")?;
                let (mut reader, _) = origin.open(&cid).await.map_err(|e| e.to_string())?;
                let mut bytes = Vec::new();
                reader.read_to_end(&mut bytes).await.map_err(|e| e.to_string())?;
                let mut w = target.begin().await.map_err(|e| e.to_string())?;
                w.write_all(&bytes).await.map_err(|e| e.to_string())?;
                let got = w.commit().await.map_err(|e| e.to_string())?.cid;
                if got == cid { Ok(()) } else { Err(format!("blob altéré : {got} au lieu de {cid}")) }
            })
        })
    }

    fn guard_peer(i: usize) -> String {
        format!("12D3KooWGuard{i}Station")
    }

    fn guard_home(i: usize) -> String {
        format!("{}:{}", guard_peer(i), format!("{:02x}", 0x40 + i).repeat(32))
    }

    fn guard_email(i: usize) -> String {
        format!("g{i}@example.org")
    }

    struct World {
        owner: Node,
        guards: Vec<Node>,
        mesh: Mesh,
    }

    async fn mutual_friends(n: &Node, me: &[u8; 32], other: &[u8; 32]) {
        n.db.writer().execute("INSERT OR IGNORE INTO social_graph(owner_hex, pubkey, level, updated_at) VALUES (?1, ?2, 1, 0)", vec![hex(me).into(), hex(other).into()]).await.unwrap();
        give(&n.db, follows(other, &[me])).await;
    }

    /// Alice (station du propriétaire) et `count` gardiens, chacun sur sa station, amis réciproques d'Alice.
    async fn rs_world(tag: &str, count: usize) -> World {
        let mesh: Mesh = Arc::default();
        let emails: Vec<String> = (0..count).map(guard_email).collect();
        let remote: Vec<(&[u8; 32], &str)> = (0..count).map(|i| (guard(i), emails[i].as_str())).collect();
        let owner = node(&format!("{tag}-o"), &[(&ALICE, "alice@example.org")], &remote, OWNER_PEER, None, None).await;
        mesh.lock().unwrap().insert(OWNER_PEER.to_owned(), owner.blobs.clone());
        let mut guards = Vec::new();
        for (i, email) in emails.iter().enumerate() {
            let mut g = node(&format!("{tag}-g{i}"), &[(guard(i), email)], &[(&ALICE, "alice@example.org")], &guard_peer(i), None, None).await;
            g.backup = g.backup.with_prefetch(mesh_prefetch(mesh.clone(), g.blobs.clone()));
            mesh.lock().unwrap().insert(guard_peer(i), g.blobs.clone());
            mutual_friends(&g, guard(i), &ALICE).await;
            mutual_friends(&owner, &ALICE, guard(i)).await;
            owner.db.writer().execute("UPDATE identities SET home_station = ?2 WHERE hex = ?1", vec![gx(i).into(), guard_home(i).into()]).await.unwrap();
            guards.push(g);
        }
        World { owner, guards, mesh }
    }

    /// Les messages de `from` adressés à `who` arrivent chez `to` (les autres restent dans la boîte).
    async fn pump_for(from: &Node, to: &Node, who: &str) -> usize {
        let mine: Vec<(String, Event)> = {
            let mut q = from.outbox.0.lock().unwrap();
            let (mine, rest): (Vec<_>, Vec<_>) = q.drain(..).partition(|(t, _)| t == who);
            *q = rest;
            mine
        };
        let n = mine.len();
        for (_, wrap) in mine {
            to.db.writer().transaction(move |tx| insert_event(tx, &wrap).map(|_| ())).await.unwrap();
        }
        n
    }

    /// Alice demande au gardien `i`, qui accepte `quota` octets ; `rs = false` : gardien d'une ancienne version.
    async fn rs_pact(w: &World, i: usize, quota: u64, rs: bool) {
        let (alice, g) = (hex(&ALICE), gx(i));
        w.owner.backup.set(&alice, true, Some(&g)).await.unwrap();
        pump_for(&w.owner, &w.guards[i], &g).await;
        w.guards[i].backup.poll_once(now()).await;
        w.guards[i].backup.respond(&g, &alice, true, quota, quota).await.unwrap();
        pump_for(&w.guards[i], &w.owner, &alice).await;
        w.owner.backup.poll_once(now()).await;
        if !rs {
            w.owner.db.writer().execute("DELETE FROM kv WHERE scope = 'backup_rs' AND key = ?1", vec![format!("{alice}:{g}").into()]).await.unwrap();
        }
    }

    /// Une nuit : Alice envoie, les gardiens joignables (`alive`) rangent et acquittent, Alice reçoit les accusés.
    async fn rs_night(w: &World, alive: &[usize]) -> usize {
        let sent = w.owner.backup.run_once().await;
        for &i in alive {
            pump_for(&w.owner, &w.guards[i], &gx(i)).await;
            w.guards[i].backup.poll_once(now()).await;
            pump_for(&w.guards[i], &w.owner, &hex(&ALICE)).await;
        }
        w.owner.backup.poll_once(now()).await;
        sent
    }

    /// Fichiers privés d'Alice (contenu chiffré simulé de 200 Ko chacun), trousseau et manifeste publié.
    async fn rs_files(n: &Node, names: &[&str], mtime0: i64) -> Vec<String> {
        let alice = hex(&ALICE);
        let mut cids = Vec::new();
        for (j, name) in names.iter().enumerate() {
            let bytes: Vec<u8> = (0..200_000u32).map(|x| (x.wrapping_mul(7) ^ (j as u32 * 31 + mtime0 as u32)) as u8).collect();
            let cid = blob_in(n, &bytes).await;
            n.db.writer()
                .execute(
                    "INSERT INTO ucloud_entries(owner_hex, path, parent, type, cid, mime, size, mtime, data) VALUES (?1, ?2, '/Docs', 'file', ?3, 'image/jpeg', 200000, ?4, '{}')",
                    vec![alice.clone().into(), format!("/Docs/{name}").into(), cid.clone().into(), (mtime0 + j as i64).into()],
                )
                .await
                .unwrap();
            n.db.writer().execute("INSERT INTO ucloud_keyring(owner_hex, cid, key_hex, enc) VALUES (?1, ?2, ?3, 'uenc2')", vec![alice.clone().into(), cid.clone().into(), format!("{:064x}", mtime0 + j as i64 + 1).into()]).await.unwrap();
            cids.push(cid);
        }
        cids
    }

    async fn frag_rows(n: &Node, owner: &str, host: &str) -> i64 {
        scalar(&n.db, "SELECT COUNT(*) FROM backup_frags WHERE owner_hex = ?1 AND host_hex = ?2", vec![owner.to_owned(), host.to_owned()]).await
    }

    fn frag_wrap(to: &str, tags: Vec<Vec<String>>) -> Event {
        let mut all = vec![vec!["p".to_owned(), to.to_owned()], vec!["t".into(), TAG.into()], vec!["op".into(), "frag".into()], vec!["station".into(), OWNER_PEER.into()]];
        all.extend(tags);
        let r = Rumor { pubkey: hex(&ALICE), created_at: u64::try_from(now()).unwrap(), kind: kind::FILE, tags: all, content: String::new() };
        gift_wrap(&ALICE, to, &r, r.created_at).unwrap()
    }

    /// Deux stations : un gardien qui annonce `rs` reçoit un fragment par paquet (k = n = 1), l'index et le manifeste ; il
    /// vérifie tout ce qui arrive (rejeu, second fragment du même paquet, blob forgé, autre indice, quota) et acquitte ;
    /// le fragment local d'Alice disparaît à l'accusé.
    #[tokio::test]
    async fn two_stations_a_coded_guardian_gets_one_fragment_per_packet_checks_everything_and_acks() {
        let w = rs_world("rs2", 1).await;
        let (alice, g0) = (hex(&ALICE), hex(&GUARDS[0]));
        let files = rs_files(&w.owner, &["a.jpg", "b.jpg", "c.jpg"], 10).await;
        manifest::publish(&w.owner.db, &*w.owner.blobs, &*w.owner.backup.keys, &alice).await.unwrap();
        rs_pact(&w, 0, 4_000_000, true).await;
        assert!(w.owner.backup.kv_get(rs::RS, &format!("{alice}:{g0}")).await.is_some(), "l'accord annonce le codage k-sur-n");

        // La nuit : un paquet (k = n = 1), son fragment, l'index et le manifeste ; aucun fichier en copie complète.
        assert_eq!(rs_night(&w, &[0]).await, 3);
        assert_eq!(scalar(&w.owner.db, "SELECT COUNT(*) FROM backup_packets WHERE owner_hex = ?1 AND k = 1", vec![alice.clone()]).await, 1);
        assert_eq!(scalar(&w.owner.db, "SELECT COUNT(*) FROM backup_blobs WHERE owner_hex = ?1", vec![alice.clone()]).await, 3);
        let g = &w.guards[0];
        assert_eq!(frag_rows(g, &alice, &g0).await, 1);
        for cid in &files {
            assert!(g.blobs.open(cid).await.is_err(), "le gardien ne voit plus les blobs eux-mêmes");
        }
        assert_eq!(scalar(&g.db, "SELECT COUNT(*) FROM backup_items WHERE owner_hex = ?1", vec![alice.clone()]).await, 2, "manifeste + index en copie complète");
        let (frag_cid, packet, size): (String, String, i64) = g.db.readers().read(|c| Ok(c.query_row("SELECT cid, packet_id, size FROM backup_frags", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?)).await.unwrap();
        assert_eq!(scalar(&g.db, "SELECT COUNT(*) FROM ucloud_entries WHERE owner_hex = ?1 AND cid = ?2 AND path LIKE '/Sauvegardes/De_alice/%'", vec![g0.clone(), frag_cid.clone()]).await, 1, "rangé dans son uCloud, servi par le CidGate");
        assert_eq!(scalar(&w.owner.db, "SELECT COUNT(*) FROM backup_frags WHERE acked_at IS NOT NULL", vec![]).await, 1, "acquitté");
        assert!(w.owner.blobs.open(&frag_cid).await.is_err(), "le fragment local est effacé une fois acquitté");
        assert!(w.owner.blobs.open(&files[0]).await.is_ok());
        // En bassin partagé, le `CidGate` ne laisse sortir que ce que la base référence : fragments et index le sont.
        let (fc, ic) = (frag_cid.clone(), w.owner.db.readers().read(|c| Ok(c.query_row("SELECT index_cid FROM backup_index", [], |r| r.get::<_, String>(0))?)).await.unwrap());
        let gate = w.owner.db.readers().read(move |c| Ok((astro_dav::pool::referenced_by(c, &fc), astro_dav::pool::referenced_by(c, &ic)))).await.unwrap();
        assert_eq!(gate, (Ok(true), Ok(true)));
        assert_eq!(rs_night(&w, &[0]).await, 0, "rien de neuf : rien ne repart");

        // Rejeu du même fragment : rien de compté deux fois, simple nouvel accusé.
        let same = vec![vec!["cid".to_owned(), frag_cid.clone()], vec!["size".into(), size.to_string()], vec!["packet".into(), packet.clone()], vec!["i".into(), "0".into()]];
        inject(g, frag_wrap(&g0, same)).await;
        let st = g.backup.poll_once(now()).await;
        assert_eq!((st.stored, st.control), (0, 1));
        assert_eq!(frag_rows(g, &alice, &g0).await, 1);

        // Un second fragment du même paquet est refusé (une station n'en garde qu'un), avant tout rapatriement.
        let other = blob_in(&w.owner, b"autre chose").await;
        inject(g, frag_wrap(&g0, vec![vec!["cid".into(), other.clone()], vec!["size".into(), "11".into()], vec!["packet".into(), packet.clone()], vec!["i".into(), "1".into()]])).await;
        assert_eq!(g.backup.poll_once(now()).await.ignored, 1);
        assert!(g.blobs.open(&other).await.is_err());

        // Un blob forgé (pas un fragment ARSF), puis un vrai fragment annoncé sous un autre indice : refusés et retirés.
        let forged = blob_in(&w.owner, &[0x41; 4096]).await;
        inject(g, frag_wrap(&g0, vec![vec!["cid".into(), forged.clone()], vec!["size".into(), "4096".into()], vec!["packet".into(), "ab".repeat(16)], vec!["i".into(), "0".into()]])).await;
        assert_eq!(g.backup.poll_once(now()).await.ignored, 1);
        assert!(g.blobs.open(&forged).await.is_err(), "le blob forgé ne reste pas chez le gardien");
        let (log, _) = g.backup.activity.subscribe(0);
        assert!(log.iter().any(|a| a.message.contains("fragment de sauvegarde refusé")), "le Capitaine du gardien est prévenu");

        // Quota : un fragment annoncé au-delà du volume restant est refusé sans rien rapatrier.
        let big = blob_in(&w.owner, &[7u8; 1000]).await;
        inject(g, frag_wrap(&g0, vec![vec!["cid".into(), big.clone()], vec!["size".into(), "3900000".into()], vec!["packet".into(), "cd".repeat(16)], vec!["i".into(), "0".into()]])).await;
        assert_eq!(g.backup.poll_once(now()).await.ignored, 1);
        assert!(g.blobs.open(&big).await.is_err());
        assert_eq!(frag_rows(g, &alice, &g0).await, 1);

        // Un nouveau fichier : un nouveau paquet, et un nouvel index qui remplace l'ancien chez le gardien.
        let old_index: String = w.owner.db.readers().read(|c| Ok(c.query_row("SELECT index_cid FROM backup_index", [], |r| r.get(0))?)).await.unwrap();
        rs_files(&w.owner, &["d.jpg"], 50).await;
        assert_eq!(rs_night(&w, &[0]).await, 2, "un fragment et l'index");
        assert_eq!(frag_rows(g, &alice, &g0).await, 2);
        assert!(g.blobs.open(&old_index).await.is_err(), "l'ancien index est remplacé");
        assert_eq!(scalar(&w.owner.db, "SELECT rev FROM backup_index WHERE owner_hex = ?1", vec![alice.clone()]).await, 2);
        let a2 = alice.clone();
        let m = w.owner.db.readers().read(move |c| astro_dav::manifest::load_manifest(c, &a2, 1, 1)).await.unwrap();
        let current: String = w.owner.db.readers().read(|c| Ok(c.query_row("SELECT index_cid FROM backup_index", [], |r| r.get(0))?)).await.unwrap();
        assert_eq!(m.backup_index, Some(current), "le manifeste porte le CID de l'index");
        for n in std::iter::once(&w.owner).chain(&w.guards) {
            let _ = std::fs::remove_dir_all(&n.dir);
        }
    }

    /// Trois stations amies (et une quatrième d'une ancienne version) : chaque paquet va à **tous** les gardiens qui
    /// savent garder des fragments, nouveaux fichiers compris ; le gardien sans `rs` garde la copie complète.
    #[tokio::test]
    async fn every_packet_goes_to_all_coded_guardians_and_an_old_guardian_keeps_full_copies() {
        let w = rs_world("rs3", 3).await;
        let alice = hex(&ALICE);
        let files = rs_files(&w.owner, &["a.jpg", "b.jpg"], 10).await;
        rs_pact(&w, 0, 4_000_000, true).await;
        rs_pact(&w, 1, 4_000_000, true).await;
        rs_pact(&w, 2, 4_000_000, false).await;
        assert_eq!(scalar(&w.owner.db, "SELECT COUNT(*) FROM backup_pacts WHERE owner_hex = ?1 AND enabled = 1", vec![alice.clone()]).await, 3, "un gardien de plus ne désactive pas les autres");
        rs_night(&w, &[0, 1, 2]).await;
        // Deux gardiens sans recul : 90 % supposés, k = 1 (miroir) ; un fragment chacun, d'indices différents.
        assert_eq!(scalar(&w.owner.db, "SELECT COUNT(*) FROM backup_packets WHERE k = 1", vec![]).await, 1);
        for i in 0..2 {
            assert_eq!(frag_rows(&w.guards[i], &alice, &gx(i)).await, 1);
        }
        assert_eq!(scalar(&w.owner.db, "SELECT COUNT(DISTINCT i) FROM backup_frags", vec![]).await, 2);
        let g2 = &w.guards[2];
        assert_eq!(frag_rows(g2, &alice, &hex(&GUARDS[2])).await, 0);
        for cid in &files {
            assert!(g2.blobs.open(cid).await.is_ok(), "copie complète chez le gardien sans `rs`");
        }

        // Le défaut d'avant : les nouveaux fichiers n'allaient qu'au dernier gardien choisi. Ils vont maintenant à tous.
        let fresh = rs_files(&w.owner, &["c.jpg"], 90).await;
        rs_night(&w, &[0, 1, 2]).await;
        for i in 0..2 {
            assert_eq!(frag_rows(&w.guards[i], &alice, &gx(i)).await, 2, "gardien {i} : un fragment du nouveau paquet");
        }
        assert!(g2.blobs.open(&fresh[0]).await.is_ok());
        assert_eq!(scalar(&w.owner.db, "SELECT COUNT(*) FROM backup_frags WHERE acked_at IS NULL", vec![]).await, 0);
        for n in std::iter::once(&w.owner).chain(&w.guards) {
            let _ = std::fs::remove_dir_all(&n.dir);
        }
    }

    /// Coupure du relais en plein envoi : ce qui n'est pas parti repart à la nuit suivante, sans réencoder ni rien
    /// envoyer deux fois.
    #[tokio::test]
    async fn a_cut_in_the_middle_of_the_night_resumes_without_reencoding() {
        let w = rs_world("rscut", 3).await;
        let alice = hex(&ALICE);
        rs_files(&w.owner, &["a.jpg", "b.jpg"], 10).await;
        for i in 0..3 {
            rs_pact(&w, i, 4_000_000, true).await;
        }
        w.owner.outbox.0.lock().unwrap().clear();
        *w.owner.outbox.1.lock().unwrap() = Some(1);
        w.owner.backup.run_once().await;
        assert_eq!(scalar(&w.owner.db, "SELECT COUNT(*) FROM backup_packets", vec![]).await, 1);
        assert_eq!(scalar(&w.owner.db, "SELECT COUNT(*) FROM backup_frags WHERE sent_at IS NOT NULL", vec![]).await, 1, "un seul fragment parti avant la coupure");
        let packet: String = w.owner.db.readers().read(|c| Ok(c.query_row("SELECT packet_id FROM backup_packets", [], |r| r.get(0))?)).await.unwrap();
        // Le relais revient : la reprise envoie le reste du même paquet.
        *w.owner.outbox.1.lock().unwrap() = None;
        rs_night(&w, &[0, 1, 2]).await;
        assert_eq!(scalar(&w.owner.db, "SELECT COUNT(*) FROM backup_packets WHERE packet_id = ?1", vec![packet]).await, 1, "pas de nouveau paquet");
        assert_eq!(scalar(&w.owner.db, "SELECT COUNT(*) FROM backup_packets", vec![]).await, 1);
        assert_eq!(scalar(&w.owner.db, "SELECT COUNT(*) FROM backup_frags WHERE acked_at IS NOT NULL", vec![]).await, 3);
        for i in 0..3 {
            assert_eq!(frag_rows(&w.guards[i], &alice, &gx(i)).await, 1, "un seul fragment chez le gardien {i}");
        }
        assert_eq!(rs_night(&w, &[0, 1, 2]).await, 0);
        for n in std::iter::once(&w.owner).chain(&w.guards) {
            let _ = std::fs::remove_dir_all(&n.dir);
        }
    }

    /// Cinq gardiens présents 29 jours sur 30 : codage 3 sur 5 ; puis la station d'Alice brûle et une nouvelle station
    /// la rétablit avec les gardiens joignables `alive`. Rend la nouvelle station et les fichiers.
    async fn rs_burn(w: &mut World, tag: &str, alive: &[usize]) -> (Node, Vec<String>) {
        const DAY: i64 = 86_400;
        let alice = hex(&ALICE);
        let files = rs_files(&w.owner, &["a.jpg", "b.jpg", "c.jpg"], 10).await;
        manifest::publish(&w.owner.db, &*w.owner.blobs, &*w.owner.backup.keys, &alice).await.unwrap();
        let t = now();
        for i in 0..5 {
            rs_pact(w, i, 4_000_000, true).await;
            for d in 0..29 {
                w.owner.db.writer().execute("INSERT OR IGNORE INTO guardian_presence(hex, day) VALUES (?1, ?2)", vec![gx(i).into(), (t / DAY - d).into()]).await.unwrap();
            }
        }
        w.owner.db.writer().execute("UPDATE backup_pacts SET decided_at = ?1", vec![(t - 29 * DAY).into()]).await.unwrap();
        rs_night(w, &[0, 1, 2, 3, 4]).await;
        assert_eq!(scalar(&w.owner.db, "SELECT COUNT(*) FROM backup_packets WHERE k = 3", vec![]).await, 1, "3 sur 5");
        assert_eq!(scalar(&w.owner.db, "SELECT COUNT(*) FROM backup_frags WHERE acked_at IS NOT NULL", vec![]).await, 5);
        let manifest_event = w.owner.backup.latest_event(&alice, kind::APP_DATA, Some(manifest::D_TAG)).await.unwrap();

        // La station d'Alice brûle ; certains gardiens sont éteints.
        w.mesh.lock().unwrap().remove(OWNER_PEER);
        for i in 0..5 {
            if !alive.contains(&i) {
                w.mesh.lock().unwrap().remove(&guard_peer(i));
            }
        }
        let emails: Vec<String> = (0..5).map(guard_email).collect();
        let remote: Vec<(&[u8; 32], &str)> = (0..5).map(|i| (guard(i), emails[i].as_str())).collect();
        let mut new = node(tag, &[(&ALICE, "alice@example.org")], &remote, "12D3KooWNewStation", None, None).await;
        new.backup = new.backup.with_prefetch(mesh_prefetch(w.mesh.clone(), new.blobs.clone()));
        w.mesh.lock().unwrap().insert("12D3KooWNewStation".to_owned(), new.blobs.clone());
        for i in 0..5 {
            mutual_friends(&new, &ALICE, guard(i)).await;
            new.db.writer().execute("UPDATE identities SET home_station = ?2 WHERE hex = ?1", vec![gx(i).into(), guard_home(i).into()]).await.unwrap();
        }
        give(&new.db, did_of(&ALICE, "alice@example.org")).await;
        give(&new.db, manifest_event).await;
        new.backup.kv_put("restore", &alice, "pending", now()).await;

        // Demande à chaque proche ; seuls les gardiens allumés répondent : manifeste et index d'abord.
        assert_eq!(new.backup.restore_step(now()).await, 5);
        for &i in alive {
            pump_for(&new, &w.guards[i], &gx(i)).await;
            w.guards[i].backup.poll_once(now()).await;
            while w.guards[i].backup.serve_restores().await > 0 {}
            pump_for(&w.guards[i], &new, &alice).await;
        }
        new.outbox.0.lock().unwrap().clear();
        new.backup.poll_once(now()).await;
        assert_eq!(new.backup.kv_get("restore", &alice).await.map(|v| v.0), Some("done".to_owned()), "le manifeste est revenu");
        assert!(new.backup.kv_get(rs::RESTORE_INDEX, &format!("{alice}:{}", hex(&GUARDS[alive[0]]))).await.is_some(), "l'index est revenu");
        (new, files)
    }

    #[tokio::test]
    async fn five_guardians_two_off_the_new_station_pulls_and_decodes_everything() {
        let mut w = rs_world("rs5", 5).await;
        let alice = hex(&ALICE);
        let (new, files) = rs_burn(&mut w, "rs5-new", &[0, 2, 4]).await;
        for cid in &files {
            assert!(new.blobs.open(cid).await.is_err());
        }
        // Le passage suivant tire 3 fragments chez les gardiens joignables, décode, revérifie chaque blob.
        new.backup.restore_step(now()).await;
        for cid in &files {
            assert!(new.blobs.open(cid).await.is_ok(), "le blob {cid} est revenu");
        }
        let st: Value = serde_json::from_str(&new.backup.kv_get(rs::RESTORE_RS, &alice).await.unwrap().0).unwrap();
        assert_eq!(st["state"], "done");
        // Les tables locales sont reprises de l'index : rien ne repartira en paquet, l'accord k-sur-n est connu.
        assert_eq!(scalar(&new.db, "SELECT COUNT(*) FROM backup_blobs WHERE owner_hex = ?1", vec![alice.clone()]).await, 3);
        assert_eq!(scalar(&new.db, "SELECT COUNT(*) FROM backup_frags WHERE owner_hex = ?1", vec![alice.clone()]).await, 5);
        assert_eq!(scalar(&new.db, "SELECT COUNT(*) FROM ucloud_entries WHERE owner_hex = ?1 AND type = 'file'", vec![alice.clone()]).await, 3);
        assert!(new.backup.pending_raw(&alice, now()).await.is_empty());
        // Aucun fragment ne traîne dans le magasin de la nouvelle station.
        let frags: Vec<String> = new.db.readers().read(|c| Ok(c.prepare("SELECT cid FROM backup_frags")?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?)).await.unwrap();
        for f in frags {
            assert!(new.blobs.open(&f).await.is_err());
        }
        for n in std::iter::once(&w.owner).chain(&w.guards).chain(std::iter::once(&new)) {
            let _ = std::fs::remove_dir_all(&n.dir);
        }
    }

    async fn rs_state(n: &Node, owner: &str) -> Value {
        serde_json::from_str(&n.backup.kv_get(rs::RESTORE_RS, owner).await.unwrap().0).unwrap()
    }

    #[tokio::test]
    async fn three_guardians_off_in_three_of_five_fails_cleanly_retries_and_finishes_when_one_comes_back() {
        const H: i64 = 3600;
        let mut w = rs_world("rs5off", 5).await;
        let alice = hex(&ALICE);
        let (new, files) = rs_burn(&mut w, "rs5off-new", &[1, 3]).await;
        let t = now();
        new.backup.restore_step(t).await;
        for cid in &files {
            assert!(new.blobs.open(cid).await.is_err(), "deux fragments sur trois : rien n'est inventé");
        }
        assert_eq!((rs_state(&new, &alice).await["rounds"].as_u64(), rs_state(&new, &alice).await["state"].as_str()), (Some(1), None));
        // Repli : les gardiens injoignables sont priés de renvoyer eux-mêmes leur fragment.
        let frag_cids: Vec<String> = new.db.readers().read(|c| Ok(c.prepare("SELECT cid FROM backup_frags")?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?)).await.unwrap();
        let asked: Vec<String> = new
            .outbox
            .0
            .lock()
            .unwrap()
            .iter()
            .filter_map(|(to, wrap)| {
                let g = GUARDS.iter().find(|g| hex(g) == *to)?;
                let r = nip59::unwrap(g, wrap).ok()?.rumor;
                (tag(&r.tags, "op") == Some("restore") && r.tags.iter().any(|x| x.first().map(String::as_str) == Some("cid") && x.get(1).is_some_and(|c| frag_cids.contains(c)))).then(|| to.clone())
            })
            .collect();
        for i in [0, 2, 4] {
            assert!(asked.contains(&gx(i)), "gardien {i} relancé");
        }
        let (log, _) = new.backup.activity.subscribe(0);
        assert!(log.iter().any(|a| a.message.contains("en attente de gardiens")), "le Capitaine le sait");
        // Pas de nouvel essai avant 6 h ; puis un nouvel essai, toujours sans succès.
        new.backup.restore_step(t + 60).await;
        assert_eq!(rs_state(&new, &alice).await["rounds"].as_u64(), Some(1));
        new.backup.restore_step(t + 6 * H + 1).await;
        assert_eq!(rs_state(&new, &alice).await["rounds"].as_u64(), Some(2));
        // Le gardien 4 se rallume : au passage suivant, tout revient.
        w.mesh.lock().unwrap().insert(guard_peer(4), w.guards[4].blobs.clone());
        new.backup.restore_step(t + 12 * H + 2).await;
        for cid in &files {
            assert!(new.blobs.open(cid).await.is_ok(), "le blob {cid} est revenu");
        }
        assert_eq!(rs_state(&new, &alice).await["state"], "done");
        for n in std::iter::once(&w.owner).chain(&w.guards).chain(std::iter::once(&new)) {
            let _ = std::fs::remove_dir_all(&n.dir);
        }
    }

    fn mesh_probe(mesh: Mesh) -> Probe {
        Arc::new(move |cids: Vec<String>, peer: String| {
            let mesh = mesh.clone();
            Box::pin(async move {
                let store = mesh.lock().unwrap().get(&peer).cloned().ok_or_else(|| "station éteinte".to_owned())?;
                let mut out = Vec::new();
                for c in &cids {
                    out.push(store.open(c).await.is_ok());
                }
                Ok(out)
            })
        })
    }

    async fn copy_blob(from: &FsBlobStore, to: &FsBlobStore, cid: &str) {
        use astro_dav::BlobWriter as _;
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        let (mut r, _) = from.open(cid).await.unwrap();
        let mut bytes = Vec::new();
        r.read_to_end(&mut bytes).await.unwrap();
        let mut w = to.begin().await.unwrap();
        w.write_all(&bytes).await.unwrap();
        assert_eq!(w.commit().await.unwrap().cid, cid, "même blob, même CID chez l'ami");
    }

    /// Un fichier partagé n'est plus exclu d'office : il ne reste hors des paquets que tant que des copies VÉRIFIÉES (sonde
    /// `HASN` sur notre CID, stations distinctes d'amis réciproques, moins de 7 jours) atteignent la cible.
    #[tokio::test]
    async fn a_shared_file_stays_out_of_packets_only_while_enough_verified_copies_exist() {
        const FRIENDS: [[u8; 32]; 4] = [[0x21; 32], [0x22; 32], [0x23; 32], [0x24; 32]];
        let mut w = rs_world("copies", 1).await;
        let alice = hex(&ALICE);
        w.owner.backup = w.owner.backup.with_probe(mesh_probe(w.mesh.clone()));
        // Quatre amis réciproques : deux sur la station A (une seule copie possible), un sur B, un sur C.
        let stations = ["12D3KooWFriendA", "12D3KooWFriendA", "12D3KooWFriendB", "12D3KooWFriendC"];
        let mut stores = std::collections::HashMap::new();
        for (n, (secret, peer)) in FRIENDS.iter().zip(stations).enumerate() {
            let home = format!("{peer}:{}", format!("{:02x}", 0x60 + n).repeat(32));
            w.owner.db.writer().execute("INSERT INTO identities(hex, role, email, hosted, home_station) VALUES (?1, 'multipass', ?2, 0, ?3)", vec![hex(secret).into(), format!("f{n}@example.org").into(), home.into()]).await.unwrap();
            mutual_friends(&w.owner, &ALICE, secret).await;
            if !stores.contains_key(peer) {
                let store = Arc::new(FsBlobStore::open(w.owner.dir.join(format!("station-{n}"))).unwrap());
                w.mesh.lock().unwrap().insert(peer.to_owned(), store.clone());
                stores.insert(peer, store);
            }
        }
        rs_pact(&w, 0, 40_000_000, true).await;
        let files = rs_files(&w.owner, &["x.jpg", "y.jpg", "z.jpg"], 10).await;
        let (x, y, z) = (files[0].clone(), files[1].clone(), files[2].clone());
        astro_dav::adopt::mark_shared(&w.owner.db, &alice, vec![x.clone(), y.clone()], "friend").await;
        astro_dav::adopt::mark_shared(&w.owner.db, &alice, vec![z.clone()], "link").await;
        // x chez les deux amis de A ; y chez A, B et C ; z (lien /s/ : rechiffré, aucun ami ne tient notre CID) nulle part.
        copy_blob(&w.owner.blobs, &stores["12D3KooWFriendA"], &x).await;
        for peer in ["12D3KooWFriendA", "12D3KooWFriendB", "12D3KooWFriendC"] {
            copy_blob(&w.owner.blobs, &stores[peer], &y).await;
        }
        rs_night(&w, &[0]).await;
        let seen = w.owner.backup.file_copies(&alice, &files, now()).await;
        assert_eq!((seen[0].copies, seen[0].packed), (1, true), "deux amis sur la même station = une copie, insuffisante : en paquet");
        assert_eq!((seen[1].copies, seen[1].packed), (3, false), "trois stations à 90 % : 99,9 %, hors paquet");
        assert_eq!((seen[2].copies, seen[2].packed), (0, true), "lien sans copie vérifiée : en paquet");
        assert!(seen[1].verified_at.is_some());
        // Médiathèque : une lecture groupée, par chemin ; y est assez couvert seul, x n'a qu'une copie (deux amis, une station).
        let m = w.owner.backup.media_copies(&alice, now()).await.unwrap();
        assert_eq!(m["files"]["/Docs/y.jpg"]["copies"], 3);
        assert_eq!(m["files"]["/Docs/y.jpg"]["safe"], true);
        assert_eq!(m["files"]["/Docs/x.jpg"]["copies"], 1);
        assert_eq!(m["files"]["/Docs/x.jpg"]["packed"], true);
        assert_eq!(m["files"]["/Docs/x.jpg"]["safe"], false);
        assert_eq!(m["files"]["/Docs/x.jpg"]["at"].as_array().map(Vec::len), Some(1));

        // La copie de C disparaît : la nuit suivante, y n'est plus assez couvert et entre au paquet suivant.
        stores["12D3KooWFriendC"].remove(&y).await.unwrap();
        rs_night(&w, &[0]).await;
        let seen = w.owner.backup.file_copies(&alice, std::slice::from_ref(&y), now()).await;
        assert_eq!((seen[0].copies, seen[0].packed), (2, true));
        assert_eq!(scalar(&w.owner.db, "SELECT COUNT(*) FROM backup_packets", vec![]).await, 2);
        for n in std::iter::once(&w.owner).chain(&w.guards) {
            let _ = std::fs::remove_dir_all(&n.dir);
        }
    }

    // ───────────────────────── Lot 5 : réparation nocturne ─────────────────────────

    /// Lecture d'une plage chez une station du maillage ; la station `liar` (s'il y en a une) répond des octets faux.
    fn mesh_sample(mesh: Mesh, liar: Option<String>) -> Sample {
        Arc::new(move |cid: String, peer: String, off: u64, len: u64, _total: u64| {
            let (mesh, liar) = (mesh.clone(), liar.clone());
            Box::pin(async move {
                use tokio::io::AsyncReadExt as _;
                let store = mesh.lock().unwrap().get(&peer).cloned().ok_or_else(|| "station éteinte".to_owned())?;
                let Ok((mut r, _)) = store.open(&cid).await else { return Ok(None) };
                let mut all = Vec::new();
                r.read_to_end(&mut all).await.unwrap();
                let mut part = all[off as usize..(off + len) as usize].to_vec();
                if liar.as_deref() == Some(peer.as_str()) {
                    for b in &mut part {
                        *b ^= 0x5A;
                    }
                }
                Ok(Some(part))
            })
        })
    }

    async fn lost_rows(n: &Node, host: &str) -> i64 {
        scalar(&n.db, "SELECT COUNT(*) FROM backup_frags WHERE host_hex = ?1 AND lost_at IS NOT NULL", vec![host.to_owned()]).await
    }

    /// Messages `op` partis de `n` vers `to`.
    fn sent_ops(n: &Node, to: &str, op: &str) -> usize {
        let g = GUARDS.iter().find(|g| hex(g) == to).unwrap();
        n.outbox.0.lock().unwrap().iter().filter(|(t, _)| t == to).filter(|(_, w)| nip59::unwrap(g, w).ok().is_some_and(|u| tag(&u.rumor.tags, "op") == Some(op))).count()
    }

    fn cleanup(w: &World) {
        for n in std::iter::once(&w.owner).chain(&w.guards) {
            let _ = std::fs::remove_dir_all(&n.dir);
        }
    }

    /// Un gardien répond « oui » à la sonde de présence puis sert des octets faux : le contrôle de possession (fragment
    /// recalculé ici) le démasque ; son fragment est perdu, sa disponibilité retombe, il reçoit `frag-drop`, et le paquet
    /// tombé à `k` fragments sûrs déclenche l'alerte du membre (une seule fois).
    #[tokio::test]
    async fn a_guardian_who_says_yes_then_serves_wrong_bytes_loses_his_fragment_and_his_availability() {
        let mut w = rs_world("liar", 2).await;
        let alice = hex(&ALICE);
        rs_files(&w.owner, &["a.jpg", "b.jpg"], 10).await;
        rs_pact(&w, 0, 4_000_000, true).await;
        rs_pact(&w, 1, 4_000_000, true).await;
        rs_night(&w, &[0, 1]).await;
        assert_eq!(scalar(&w.owner.db, "SELECT COUNT(*) FROM backup_frags WHERE acked_at IS NOT NULL", vec![]).await, 2);
        let t = now();
        for d in 0..10 {
            w.owner.db.writer().execute("INSERT OR IGNORE INTO guardian_presence(hex, day) VALUES (?1, ?2)", vec![gx(1).into(), (t / 86_400 - d).into()]).await.unwrap();
        }
        w.owner.backup = w.owner.backup.with_probe(mesh_probe(w.mesh.clone())).with_sample(mesh_sample(w.mesh.clone(), Some(guard_peer(1))));
        w.owner.outbox.0.lock().unwrap().clear();
        // Un fragment contrôlé par nuit (1 % de 2, arrondi au-dessus), tiré au hasard : le menteur finit par l'être.
        let mut total = RepairStats::default();
        for _ in 0..64 {
            let st = w.owner.backup.repair_night(t).await;
            assert_eq!(st.checked, 1);
            total.liars += st.liars;
            total.alerts += st.alerts;
            if total.liars > 0 {
                break;
            }
        }
        assert_eq!(total.liars, 1);
        assert_eq!(lost_rows(&w.owner, &gx(0)).await, 0, "le gardien honnête passe le contrôle");
        assert_eq!(lost_rows(&w.owner, &gx(1)).await, 1);
        assert_eq!(scalar(&w.owner.db, "SELECT COUNT(*) FROM guardian_presence WHERE hex = ?1", vec![gx(1)]).await, 0, "sa disponibilité retombe");
        assert_eq!(sent_ops(&w.owner, &gx(1), "frag-drop"), 1);
        assert_eq!(total.alerts, 1, "plus qu'un fragment sûr pour k = 1 : le membre est prévenu");
        let (log, _) = w.owner.backup.activity.subscribe(0);
        assert!(log.iter().any(|a| a.message.contains("n'a plus de marge")));
        assert!(log.iter().any(|a| a.message.contains("a mal répondu")));
        // Plus de nouvelle alerte les nuits suivantes, et le fragment perdu sort de l'index.
        assert_eq!(w.owner.backup.repair_night(t + 60).await.alerts, 0);
        assert_eq!(scalar(&w.owner.db, "SELECT COUNT(*) FROM kv WHERE scope = 'backup_rs_alert' AND key LIKE ?1 || ':%'", vec![alice]).await, 1);
        cleanup(&w);
    }

    /// Absence d'un gardien : 13 jours sans signe de vie ne déclenchent rien ; 15 jours, son fragment est perdu, `frag-drop`
    /// lui est adressé et un nouveau gardien reçoit un fragment d'indice neuf, recalculé ici, qu'il acquitte.
    #[tokio::test]
    async fn an_absence_of_13_days_does_nothing_15_days_triggers_a_repair() {
        const DAY: i64 = 86_400;
        let mut w = rs_world("absent", 3).await;
        let alice = hex(&ALICE);
        rs_files(&w.owner, &["a.jpg", "b.jpg"], 10).await;
        rs_pact(&w, 0, 4_000_000, true).await;
        rs_pact(&w, 1, 4_000_000, true).await;
        rs_night(&w, &[0, 1]).await;
        let t = now();
        w.owner.db.writer().execute("DELETE FROM guardian_presence", vec![]).await.unwrap();
        w.owner.db.writer().execute("UPDATE backup_frags SET acked_at = ?1", vec![t.into()]).await.unwrap();
        w.owner.backup = w.owner.backup.with_probe(mesh_probe(w.mesh.clone()));
        w.mesh.lock().unwrap().remove(&guard_peer(0)); // la station du gardien 0 s'éteint
        w.owner.outbox.0.lock().unwrap().clear();

        let st = w.owner.backup.repair_night(t + 13 * DAY).await;
        assert_eq!((st.lost, st.rebuilt), (0, 0), "13 jours : pas de tempête de réparation");
        assert_eq!(lost_rows(&w.owner, &gx(0)).await, 0);
        assert_eq!(sent_ops(&w.owner, &gx(0), "frag-drop"), 0);
        assert_eq!(st.seen, 1, "le gardien 1 répond à la sonde");

        // Le gardien 2 arrive entre-temps ; à 15 jours le fragment du gardien 0 est perdu et refait chez lui.
        rs_pact(&w, 2, 4_000_000, true).await;
        let st = w.owner.backup.repair_night(t + 15 * DAY).await;
        assert_eq!(st.lost, 1);
        assert_eq!(lost_rows(&w.owner, &gx(0)).await, 1);
        assert_eq!(sent_ops(&w.owner, &gx(0), "frag-drop"), 1);
        assert_eq!(st.rebuilt, 1);
        assert_eq!(scalar(&w.owner.db, "SELECT i FROM backup_frags WHERE host_hex = ?1", vec![gx(2)]).await, 2, "indice neuf");
        rs_night(&w, &[1, 2]).await;
        assert_eq!(frag_rows(&w.guards[2], &alice, &gx(2)).await, 1, "le remplaçant range et acquitte");
        assert_eq!(scalar(&w.owner.db, "SELECT COUNT(*) FROM backup_frags WHERE acked_at IS NOT NULL AND lost_at IS NULL", vec![]).await, 2);
        cleanup(&w);
    }

    /// Un gardien qui retire son accord : son fragment ne compte plus, sans attendre 14 jours ; la parité donnée au
    /// troisième gardien garde la marge (pas d'alerte).
    #[tokio::test]
    async fn a_revoked_guardian_is_repaired_elsewhere_at_once() {
        let w = rs_world("revoke", 3).await;
        rs_files(&w.owner, &["a.jpg"], 10).await;
        rs_pact(&w, 0, 4_000_000, true).await;
        rs_pact(&w, 1, 4_000_000, true).await;
        rs_night(&w, &[0, 1]).await;
        rs_pact(&w, 2, 4_000_000, true).await;
        // Le gardien 2 a reçu sa part par extension ; puis le gardien 0 se retire.
        w.owner.backup.repair_night(now()).await;
        rs_night(&w, &[0, 1, 2]).await;
        w.owner.db.writer().execute("UPDATE backup_pacts SET status = 'revoked' WHERE host_hex = ?1", vec![gx(0).into()]).await.unwrap();
        let st = w.owner.backup.repair_night(now()).await;
        assert_eq!(st.lost, 1);
        assert_eq!(lost_rows(&w.owner, &gx(0)).await, 1);
        assert_eq!(scalar(&w.owner.db, "SELECT COUNT(*) FROM backup_frags WHERE acked_at IS NOT NULL AND lost_at IS NULL", vec![]).await, 2);
        assert_eq!(st.alerts, 0, "deux fragments sûrs pour k = 1 : il reste de la marge");
        cleanup(&w);
    }

    /// Un gardien de plus : il reçoit, à la nuit suivante, une parité d'indice neuf de chaque paquet existant, que l'on
    /// peut décoder seule (k = 1) ; la sûreté estimée monte.
    #[tokio::test]
    async fn a_new_guardian_gets_a_fresh_parity_of_every_packet_and_safety_rises() {
        let w = rs_world("grow", 3).await;
        let alice = hex(&ALICE);
        rs_files(&w.owner, &["a.jpg", "b.jpg"], 10).await;
        rs_pact(&w, 0, 4_000_000, true).await;
        rs_pact(&w, 1, 4_000_000, true).await;
        rs_night(&w, &[0, 1]).await;
        rs_files(&w.owner, &["c.jpg"], 40).await;
        rs_night(&w, &[0, 1]).await;
        assert_eq!(scalar(&w.owner.db, "SELECT COUNT(*) FROM backup_packets", vec![]).await, 2);
        let before = crate::guardians::proba_au_moins_k(&[900; 2], 1);
        rs_pact(&w, 2, 4_000_000, true).await;
        let st = w.owner.backup.repair_night(now()).await;
        assert_eq!(st.rebuilt, 2, "une parité par paquet existant");
        rs_night(&w, &[0, 1, 2]).await;
        assert_eq!(frag_rows(&w.guards[2], &alice, &gx(2)).await, 2);
        let per_packet: Vec<i64> = w.owner.db.readers().read(|c| Ok(c.prepare("SELECT COUNT(*) FROM backup_frags WHERE acked_at IS NOT NULL AND lost_at IS NULL GROUP BY packet_id")?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?)).await.unwrap();
        assert_eq!(per_packet, vec![3, 3]);
        assert!(crate::guardians::proba_au_moins_k(&[900; 3], 1) > before);
        // La parité neuve, seule, redonne le paquet.
        let (id, key, len, k, cid, i): (String, String, i64, i64, String, i64) = w
            .owner
            .db
            .readers()
            .read(|c| Ok(c.query_row("SELECT p.packet_id, p.key_hex, p.len, p.k, f.cid, f.i FROM backup_packets p JOIN backup_frags f ON f.packet_id = p.packet_id WHERE f.i = 2 LIMIT 1", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?))
            .await
            .unwrap();
        let bytes = {
            use tokio::io::AsyncReadExt as _;
            let (mut r, _) = w.guards[2].blobs.open(&cid).await.unwrap();
            let mut v = Vec::new();
            r.read_to_end(&mut v).await.unwrap();
            v
        };
        let entry = astro_dav::backup_pack::PacketEntry { id, key, len: len as u64, k: k as u8, frags: vec![astro_dav::backup_pack::FragEntry { i: i as u8, cid, host: None }] };
        assert!(astro_dav::backup_pack::open_packet(&entry, &[bytes.as_slice()]).await.is_ok());
        // Rien de plus la nuit suivante : chacun tient déjà sa part.
        assert_eq!(w.owner.backup.repair_night(now()).await.rebuilt, 0);
        cleanup(&w);
    }

    /// Coupure en plein encodage : des fragments écrits mais jamais inscrits sont ramassés ; ce qui est inscrit et en
    /// attente reste ; un second passage ne trouve plus rien.
    #[tokio::test]
    async fn fragments_left_by_a_cut_are_collected_once() {
        let w = rs_world("orphans", 1).await;
        rs_files(&w.owner, &["a.jpg"], 10).await;
        let orphan = blob_in(&w.owner, b"fragment orphelin").await;
        let kept = blob_in(&w.owner, b"fragment en attente").await;
        for cid in [&orphan, &kept] {
            w.owner.backup.kv_put(rs::SPOOL, cid, &hex(&ALICE), now()).await;
        }
        w.owner.db.writer().execute("INSERT INTO backup_frags(owner_hex, host_hex, packet_id, i, cid, size) VALUES (?1, ?2, ?3, 0, ?4, 19)", vec![hex(&ALICE).into(), gx(0).into(), "ab".repeat(16).into(), kept.clone().into()]).await.unwrap();
        assert_eq!(w.owner.backup.collect_orphans().await, 1);
        assert!(w.owner.blobs.open(&orphan).await.is_err());
        assert!(w.owner.blobs.open(&kept).await.is_ok(), "inscrit et pas encore acquitté : il reste");
        assert_eq!(scalar(&w.owner.db, "SELECT COUNT(*) FROM kv WHERE scope = 'backup_spool'", vec![]).await, 0);
        assert_eq!(w.owner.backup.collect_orphans().await, 0, "idempotent");
        // Le passage ordinaire ne laisse aucune ligne de journal derrière lui.
        rs_pact(&w, 0, 4_000_000, true).await;
        rs_night(&w, &[0]).await;
        assert_eq!(scalar(&w.owner.db, "SELECT COUNT(*) FROM kv WHERE scope = 'backup_spool'", vec![]).await, 0);
        cleanup(&w);
    }

    /// Côté gardien : au-delà du plafond de fragments rangés par 24 h et par propriétaire, le message attend, sans rien
    /// rapatrier ; il est repris quand la fenêtre glisse.
    #[tokio::test]
    async fn a_guardian_takes_a_bounded_number_of_fragments_per_night_and_owner() {
        let w = rs_world("cap", 1).await;
        let (alice, g0) = (hex(&ALICE), gx(0));
        rs_pact(&w, 0, 40_000_000, true).await;
        let g = &w.guards[0];
        for p in 0..rs::NIGHT_FRAGS {
            g.db.writer().execute("INSERT INTO backup_frags(owner_hex, host_hex, packet_id, i, cid, size, sent_at, acked_at) VALUES (?1, ?2, ?3, 0, ?4, 100, ?5, ?5)", vec![alice.clone().into(), g0.clone().into(), format!("{p:032x}").into(), format!("bafkreifake{p}").into(), now().into()]).await.unwrap();
        }
        let forged = blob_in(&w.owner, &[0x41; 4096]).await;
        inject(g, frag_wrap(&g0, vec![vec!["cid".into(), forged.clone()], vec!["size".into(), "4096".into()], vec!["packet".into(), "ef".repeat(16)], vec!["i".into(), "0".into()]])).await;
        let st = g.backup.poll_once(now()).await;
        assert_eq!((st.deferred, st.ignored), (1, 0));
        assert!(g.blobs.open(&forged).await.is_err(), "rien n'est rapatrié au-delà du plafond");
        // Un jour plus tard, le message est repris (et refusé ici : ce n'est pas un fragment).
        g.db.writer().execute("UPDATE backup_frags SET acked_at = acked_at - 90000", vec![]).await.unwrap();
        let st = g.backup.poll_once(now()).await;
        assert_eq!((st.deferred, st.ignored), (0, 1));
        cleanup(&w);
    }

    /// Restauration : un blob qu'une station amie tient en entier (`held_whole`) revient directement, sans paquet.
    #[tokio::test]
    async fn restoration_fetches_a_blob_held_whole_by_a_friend_directly() {
        let mut w = rs_world("whole", 1).await;
        let files = rs_files(&w.owner, &["film.mp4"], 10).await;
        copy_blob(&w.owner.blobs, &w.guards[0].blobs, &files[0]).await;
        w.owner.blobs.remove(&files[0]).await.unwrap();
        w.owner.backup = w.owner.backup.with_prefetch(mesh_prefetch(w.mesh.clone(), w.owner.blobs.clone()));
        let mut index = astro_dav::backup_pack::PackIndex::new(&hex(&ALICE), 3);
        index.packets.push(astro_dav::backup_pack::PacketEntry { id: "00".repeat(16), key: "11".repeat(32), len: 1 << 20, k: 1, frags: Vec::new() });
        index.blobs.push(astro_dav::backup_pack::BlobEntry { cid: files[0].clone(), packet: 0, off: 0, len: 200_000, held_whole: vec![gx(0)] });
        assert_eq!(w.owner.backup.pull_whole(&index).await, 1);
        assert!(w.owner.blobs.open(&files[0]).await.is_ok(), "rapatrié de chez l'ami, CID vérifié");
        assert_eq!(w.owner.backup.pull_whole(&index).await, 0, "déjà là : rien à refaire");
        cleanup(&w);
    }

    /// Q5 : un très gros blob tenu en entier par deux stations amies vérifiées reste hors des paquets ; une seule copie ne
    /// suffit pas.
    #[tokio::test]
    async fn a_very_big_blob_held_whole_by_two_friend_stations_stays_out_of_packets() {
        const FRIENDS: [[u8; 32]; 2] = [[0x31; 32], [0x32; 32]];
        let mut w = rs_world("big", 1).await;
        let alice = hex(&ALICE);
        w.owner.backup = w.owner.backup.with_probe(mesh_probe(w.mesh.clone()));
        let mut stores = Vec::new();
        for (n, secret) in FRIENDS.iter().enumerate() {
            let peer = format!("12D3KooWBig{n}");
            let home = format!("{peer}:{}", format!("{:02x}", 0x70 + n).repeat(32));
            w.owner.db.writer().execute("INSERT INTO identities(hex, role, email, hosted, home_station) VALUES (?1, 'multipass', ?2, 0, ?3)", vec![hex(secret).into(), format!("b{n}@example.org").into(), home.into()]).await.unwrap();
            mutual_friends(&w.owner, &ALICE, secret).await;
            let store = Arc::new(FsBlobStore::open(w.owner.dir.join(format!("big-{n}"))).unwrap());
            w.mesh.lock().unwrap().insert(peer, store.clone());
            stores.push(store);
        }
        rs_pact(&w, 0, 40_000_000, true).await;
        let size = BIG_BLOB as usize + 1;
        let (one, two) = (blob_in(&w.owner, &vec![1u8; size]).await, blob_in(&w.owner, &vec![2u8; size]).await);
        for (name, cid) in [("un.mkv", &one), ("deux.mkv", &two)] {
            w.owner.db.writer().execute("INSERT INTO ucloud_entries(owner_hex, path, parent, type, cid, mime, size, mtime, data) VALUES (?1, ?2, '/Films', 'file', ?3, 'video/x-matroska', ?4, 1, '{}')", vec![alice.clone().into(), format!("/Films/{name}").into(), cid.clone().into(), (size as i64).into()]).await.unwrap();
        }
        copy_blob(&w.owner.blobs, &stores[0], &one).await;
        copy_blob(&w.owner.blobs, &stores[0], &two).await;
        copy_blob(&w.owner.blobs, &stores[1], &two).await;
        rs_night(&w, &[0]).await;
        let seen = w.owner.backup.file_copies(&alice, &[one.clone(), two.clone()], now()).await;
        assert_eq!((seen[0].copies, seen[0].packed), (1, true), "une seule copie : en paquet");
        assert_eq!((seen[1].copies, seen[1].packed), (2, false), "deux stations amies : hors paquet");
        cleanup(&w);
    }

    // ───────────────── Correction du lot 5 : une plage fausse ne condamne pas ─────────────────

    /// Lecture scriptée : `Some(true)` octets justes, `Some(false)` octets faux, `None` pas de réponse ; le dernier se répète.
    fn scripted_sample(mesh: Mesh, script: Vec<Option<bool>>) -> Sample {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        Arc::new(move |cid: String, peer: String, off: u64, len: u64, _total: u64| {
            let (mesh, script, calls) = (mesh.clone(), script.clone(), calls.clone());
            Box::pin(async move {
                use tokio::io::AsyncReadExt as _;
                let n = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let step = script[n.min(script.len() - 1)];
                let Some(honest) = step else { return Err("station injoignable".to_owned()) };
                let store = mesh.lock().unwrap().get(&peer).cloned().ok_or_else(|| "station éteinte".to_owned())?;
                let (mut r, _) = store.open(&cid).await.map_err(|e| e.to_string())?;
                let mut all = Vec::new();
                r.read_to_end(&mut all).await.unwrap();
                let mut part = all[off as usize..(off + len) as usize].to_vec();
                if !honest {
                    part.iter_mut().for_each(|b| *b ^= 0x5A);
                }
                Ok(Some(part))
            })
        })
    }

    async fn suspects(n: &Node) -> i64 {
        scalar(&n.db, "SELECT COUNT(*) FROM kv WHERE scope = 'backup_rs_suspect'", vec![]).await
    }

    /// Une plage fausse puis une juste : pas condamné. Une plage fausse puis plus de réponse : suspect, repris une autre
    /// nuit (une erreur de transport n'est jamais une preuve). Deux plages fausses : condamné.
    #[tokio::test]
    async fn one_wrong_range_is_retried_on_another_two_wrong_ranges_condemn() {
        let mut w = rs_world("retry", 1).await;
        rs_files(&w.owner, &["a.jpg"], 10).await;
        rs_pact(&w, 0, 4_000_000, true).await;
        rs_night(&w, &[0]).await;
        let t = now();
        // Faux une fois, puis juste.
        w.owner.backup.sample = Some(scripted_sample(w.mesh.clone(), vec![Some(false), Some(true)]));
        let st = w.owner.backup.repair_night(t).await;
        assert_eq!((st.checked, st.liars, st.lost), (1, 0, 0), "une plage fausse puis juste : pas condamné");
        assert_eq!(suspects(&w.owner).await, 0);
        // Faux, puis la station ne répond plus : suspect, rien de perdu.
        w.owner.backup.sample = Some(scripted_sample(w.mesh.clone(), vec![Some(false), None]));
        let st = w.owner.backup.repair_night(t).await;
        assert_eq!((st.liars, st.lost), (0, 0));
        assert_eq!(suspects(&w.owner).await, 1, "contrôle à finir une autre nuit");
        // La nuit suivante, la station ne répond pas du tout : toujours rien de conclu.
        w.owner.backup.sample = Some(scripted_sample(w.mesh.clone(), vec![None]));
        assert_eq!(w.owner.backup.repair_night(t + 86_400).await.liars, 0);
        assert_eq!(suspects(&w.owner).await, 1);
        // Puis elle répond juste : le soupçon est levé.
        w.owner.backup.sample = Some(scripted_sample(w.mesh.clone(), vec![Some(true)]));
        assert_eq!(w.owner.backup.repair_night(t + 2 * 86_400).await.liars, 0);
        assert_eq!(suspects(&w.owner).await, 0);
        // Deux plages fausses : condamné.
        w.owner.backup.sample = Some(scripted_sample(w.mesh.clone(), vec![Some(false)]));
        let st = w.owner.backup.repair_night(t + 3 * 86_400).await;
        assert_eq!((st.liars, st.lost), (1, 1));
        assert_eq!(lost_rows(&w.owner, &gx(0)).await, 1);
        assert_eq!(suspects(&w.owner).await, 0);
        cleanup(&w);
    }

    // ───────────────────────── Lot 6 : compactage et migration ─────────────────────────

    /// Ce que tient chaque gardien permet-il de tout retrouver ? Prend l'index de plus haute révision détenu par les
    /// gardiens (comme une station neuve), puis, pour chaque fichier vivant, décode son paquet depuis `k` fragments réellement
    /// présents chez les gardiens et revérifie le blob par son CID.
    async fn restorable(w: &World, live: &[String]) -> bool {
        use sha2::Digest as _;
        use tokio::io::AsyncReadExt as _;
        let alice = hex(&ALICE);
        let key = astro_dav::backup_pack::index_key(&w.owner.backup.keys.secret(&alice).unwrap());
        let read = |store: Arc<FsBlobStore>, cid: String| async move {
            let (mut r, _) = store.open(&cid).await.ok()?;
            let mut v = Vec::new();
            r.read_to_end(&mut v).await.ok()?;
            Some(v)
        };
        let mut best: Option<astro_dav::backup_pack::PackIndex> = None;
        for (i, g) in w.guards.iter().enumerate() {
            let Some((cid, _)) = g.backup.kv_get(rs::INDEX_HELD, &format!("{}:{alice}", gx(i))).await else { continue };
            let Some(bytes) = read(g.blobs.clone(), cid).await else { continue };
            if let Ok(index) = astro_dav::backup_pack::PackIndex::open(&bytes, &key, &alice).await
                && best.as_ref().is_none_or(|b| index.rev > b.rev)
            {
                best = Some(index);
            }
        }
        let Some(index) = best else { return false };
        for cid in live {
            let Some(b) = index.blobs.iter().find(|b| &b.cid == cid) else { return false };
            let entry = &index.packets[b.packet as usize];
            let mut frags = Vec::new();
            for f in &entry.frags {
                let Some(i) = (0..w.guards.len()).find(|&i| Some(gx(i)) == f.host) else { continue };
                if let Some(v) = read(w.guards[i].blobs.clone(), f.cid.clone()).await {
                    frags.push(v);
                }
            }
            if frags.len() < usize::from(entry.k) {
                return false;
            }
            let refs: Vec<&[u8]> = frags.iter().map(Vec::as_slice).collect();
            let Ok(plain) = astro_dav::backup_pack::open_packet(entry, &refs).await else { return false };
            let Some(slice) = plain.get(b.off as usize..(b.off + b.len) as usize) else { return false };
            if &astro_dav::blob::cid_raw_sha256(&sha2::Sha256::digest(slice).into()) != cid {
                return false;
            }
        }
        true
    }

    async fn delete_file(n: &Node, cid: &str) {
        n.db.writer().execute("DELETE FROM ucloud_entries WHERE cid = ?1", vec![cid.to_owned().into()]).await.unwrap();
    }

    /// Trois fichiers sur quatre supprimés : passé le délai de grâce, le paquet (25 % vivant) est réécrit. Une coupure en
    /// pleine construction reprend sans rien refaire ; à chaque étape (construction, envoi, accusés, bascule, nouvel index,
    /// retrait) un gardien peut toujours tout rendre ; à la fin chaque gardien ne tient plus que le paquet neuf.
    #[tokio::test]
    async fn a_mostly_dead_packet_is_compacted_step_by_step_and_nothing_is_ever_lost() {
        let w = rs_world("compact", 3).await;
        let alice = hex(&ALICE);
        let files = rs_files(&w.owner, &["a.jpg", "b.jpg", "c.jpg", "d.jpg"], 10).await;
        for i in 0..3 {
            rs_pact(&w, i, 8_000_000, true).await;
        }
        rs_night(&w, &[0, 1, 2]).await;
        rs_night(&w, &[0, 1, 2]).await;
        let live = vec![files[3].clone()];
        assert!(restorable(&w, &files).await, "tout est retrouvable au départ");
        let old: String = w.owner.db.readers().read(|c| Ok(c.query_row("SELECT packet_id FROM backup_packets", [], |r| r.get(0))?)).await.unwrap();
        for cid in &files[..3] {
            delete_file(&w.owner, cid).await;
        }
        let t = now();
        // Avant le délai de grâce : rien ne bouge.
        w.owner.backup.compact_step(&alice, t).await;
        assert_eq!(scalar(&w.owner.db, "SELECT COUNT(*) FROM backup_packets WHERE staged = 1", vec![]).await, 0);
        assert_eq!(scalar(&w.owner.db, "SELECT COUNT(*) FROM backup_blobs WHERE gone_at IS NOT NULL", vec![]).await, 3);
        // Après : un paquet neuf, en attente ; les blobs restent rattachés à l'ancien.
        w.owner.backup.compact_step(&alice, t + GRACE_SECS + 1).await;
        assert_eq!(scalar(&w.owner.db, "SELECT COUNT(*) FROM backup_packets WHERE staged = 1", vec![]).await, 1);
        assert_eq!(scalar(&w.owner.db, "SELECT COUNT(*) FROM backup_compact", vec![]).await, 1);
        assert_eq!(scalar(&w.owner.db, "SELECT COUNT(*) FROM backup_blobs WHERE seq = (SELECT seq FROM backup_packets WHERE packet_id = ?1)", vec![old.clone()]).await, 4);
        assert!(restorable(&w, &live).await, "construction : l'ancien paquet suffit");
        // Coupure puis reprise : rien n'est reconstruit deux fois.
        w.owner.backup.compact_step(&alice, t + GRACE_SECS + 2).await;
        assert_eq!(scalar(&w.owner.db, "SELECT COUNT(*) FROM backup_packets", vec![]).await, 2, "idempotent");
        // Envoi et accusés : les fragments neufs partent ; la bascule attend les accusés.
        rs_night(&w, &[0, 1, 2]).await;
        assert!(restorable(&w, &live).await, "envoi : rien de perdu");
        assert_eq!(scalar(&w.owner.db, "SELECT COUNT(*) FROM backup_frags WHERE acked_at IS NOT NULL", vec![]).await, 6);
        // Nuit suivante : bascule, puis index neuf envoyé ; les anciens fragments attendent dans la file de retrait.
        rs_night(&w, &[0, 1, 2]).await;
        assert_eq!(scalar(&w.owner.db, "SELECT COUNT(*) FROM backup_packets WHERE packet_id = ?1", vec![old.clone()]).await, 0, "bascule faite");
        assert_eq!(scalar(&w.owner.db, "SELECT COUNT(*) FROM backup_blobs", vec![]).await, 1, "les morts sortent, le vivant change de paquet");
        assert_eq!(scalar(&w.owner.db, "SELECT COUNT(*) FROM kv WHERE scope = 'backup_rs_drop'", vec![]).await, 3);
        for i in 0..3 {
            assert_eq!(frag_rows(&w.guards[i], &alice, &gx(i)).await, 2, "gardien {i} : l'ancien et le neuf, le temps de la bascule");
        }
        assert!(restorable(&w, &live).await, "bascule : l'index neuf et le paquet neuf suffisent");
        // Nuit suivante : l'index neuf est chez tous, les anciens fragments sont retirés.
        rs_night(&w, &[0, 1, 2]).await;
        assert_eq!(scalar(&w.owner.db, "SELECT COUNT(*) FROM kv WHERE scope = 'backup_rs_drop'", vec![]).await, 0);
        for i in 0..3 {
            assert_eq!(frag_rows(&w.guards[i], &alice, &gx(i)).await, 1, "gardien {i} : seul le paquet neuf reste");
        }
        assert!(restorable(&w, &live).await, "retrait : rien de perdu");
        assert_eq!(rs_night(&w, &[0, 1, 2]).await, 0, "plus rien à faire");
        cleanup(&w);
    }

    /// Un paquet repris d'une restauration (sans en-tête local) est réécrit : il redevient contrôlable et réparable.
    #[tokio::test]
    async fn a_packet_back_from_a_restoration_is_rewritten_and_becomes_checkable_again() {
        let mut w = rs_world("headless", 2).await;
        let alice = hex(&ALICE);
        let files = rs_files(&w.owner, &["a.jpg", "b.jpg"], 10).await;
        rs_pact(&w, 0, 8_000_000, true).await;
        rs_pact(&w, 1, 8_000_000, true).await;
        rs_night(&w, &[0, 1]).await;
        // Comme après `import_index` : l'en-tête du paquet n'est pas connu ici.
        w.owner.db.writer().execute("UPDATE backup_packets SET head_hex = NULL", vec![]).await.unwrap();
        assert!(w.owner.backup.rebuild(&alice, &scalar_text(&w.owner, "SELECT packet_id FROM backup_packets").await, &[0]).await.is_err());
        for _ in 0..4 {
            rs_night(&w, &[0, 1]).await;
            assert!(restorable(&w, &files).await, "à chaque nuit, tout reste retrouvable");
        }
        assert_eq!(scalar(&w.owner.db, "SELECT COUNT(*) FROM backup_packets WHERE head_hex IS NULL OR staged = 1", vec![]).await, 0, "réécrit");
        assert_eq!(scalar(&w.owner.db, "SELECT COUNT(*) FROM backup_packets", vec![]).await, 1);
        w.owner.backup = w.owner.backup.with_sample(mesh_sample(w.mesh.clone(), None));
        let st = w.owner.backup.repair_night(now()).await;
        assert_eq!((st.checked, st.liars), (1, 0), "le paquet neuf se contrôle de nouveau");
        cleanup(&w);
    }

    async fn scalar_text(n: &Node, sql: &'static str) -> String {
        n.db.readers().read(move |c| Ok(c.query_row(sql, [], |r| r.get(0))?)).await.unwrap()
    }

    /// Des copies complètes d'avant le codage : retirées seulement une fois leurs blobs dans des paquets sûrs depuis le délai
    /// de grâce, jamais quand un paquet n'a plus de marge.
    #[tokio::test]
    async fn full_copies_are_dropped_once_packets_are_safe_and_never_without_margin() {
        let w = rs_world("legacy", 3).await;
        let alice = hex(&ALICE);
        let files = rs_files(&w.owner, &["a.jpg", "b.jpg"], 10).await;
        for i in 0..3 {
            rs_pact(&w, i, 8_000_000, false).await;
        }
        rs_night(&w, &[0, 1, 2]).await;
        for i in 0..3 {
            assert!(w.guards[i].blobs.open(&files[0]).await.is_ok(), "copie complète chez le gardien {i}");
        }
        // Les trois stations passent à une version qui sait garder des fragments ; elles sont présentes chaque jour.
        let today = now() / 86_400;
        for i in 0..3 {
            for d in today - 2..=today + 8 {
                w.owner.db.writer().execute("INSERT OR IGNORE INTO guardian_presence(hex, day) VALUES (?1, ?2)", vec![gx(i).into(), d.into()]).await.unwrap();
            }
            w.owner.backup.kv_put(rs::RS, &format!("{alice}:{}", gx(i)), "1", now()).await;
        }
        rs_night(&w, &[0, 1, 2]).await;
        assert_eq!(scalar(&w.owner.db, "SELECT COUNT(*) FROM backup_frags WHERE acked_at IS NOT NULL", vec![]).await, 3);
        let items = "SELECT COUNT(*) FROM backup_items WHERE owner_hex = ?1 AND cid LIKE 'bafkrei%' AND cid IN (SELECT cid FROM backup_blobs)";
        assert_eq!(scalar(&w.owner.db, items, vec![alice.clone()]).await, 6);
        w.owner.outbox.0.lock().unwrap().clear();
        // Avant le délai de grâce : rien.
        assert_eq!(w.owner.backup.compact_step(&alice, now()).await, 0);
        // Plus de marge (un seul fragment sûr pour k = 1) : rien, même après le délai.
        w.owner.db.writer().execute("UPDATE backup_frags SET lost_at = 1 WHERE host_hex IN (?1, ?2)", vec![gx(0).into(), gx(1).into()]).await.unwrap();
        assert_eq!(w.owner.backup.compact_step(&alice, now() + GRACE_SECS + 1).await, 0);
        assert_eq!(scalar(&w.owner.db, items, vec![alice.clone()]).await, 6);
        // Marge revenue, cible atteinte : `drop` à chaque gardien, les copies disparaissent chez eux.
        w.owner.db.writer().execute("UPDATE backup_frags SET lost_at = NULL", vec![]).await.unwrap();
        assert_eq!(w.owner.backup.compact_step(&alice, now() + GRACE_SECS + 1).await, 3);
        assert_eq!(scalar(&w.owner.db, items, vec![alice.clone()]).await, 0);
        for i in 0..3 {
            assert_eq!(sent_ops(&w.owner, &gx(i), "drop"), 1);
            pump_for(&w.owner, &w.guards[i], &gx(i)).await;
            w.guards[i].backup.poll_once(now()).await;
            for f in &files {
                assert!(w.guards[i].blobs.open(f).await.is_err(), "copie complète retirée chez le gardien {i}");
            }
        }
        assert!(restorable(&w, &files).await, "les paquets suffisent");
        // Ma sécurité : 1 paquet, 3 gardiens dont 1 suffit, place prise ≈ 3 fois (miroir k = 1), cible atteinte.
        let v = w.owner.backup.coded_view(&alice, now()).await.unwrap();
        assert_eq!((v["packets"].as_i64(), v["holders"].as_i64(), v["need"].as_i64()), (Some(1), Some(3), Some(1)));
        assert!(v["safety_permille"].as_u64().unwrap() >= 999);
        assert!(v["ratio_pct"].as_i64().unwrap() >= 300, "trois miroirs : au moins 3 fois");
        let st = w.owner.backup.status(&alice).await.unwrap();
        assert_eq!(st["coded"]["need"], 1);
        // Espace Capitaine : le résumé de la station.
        let sum = w.owner.db.readers().read(|c| Ok(captain_summary(c)?)).await.unwrap();
        assert_eq!((sum["packets"].as_i64(), sum["frags_acked"].as_i64(), sum["frags_lost"].as_i64()), (Some(1), Some(3), Some(0)));
        cleanup(&w);
    }
}
