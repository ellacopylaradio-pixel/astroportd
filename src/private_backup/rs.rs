//! Sauvegarde **k-sur-n** entre amis (codage à effacement, `docs/design/SAUVEGARDE_RS.md`) : envoi (lot 3) et
//! restauration (lot 4), branchés sur [`PrivateBackup`].
//!
//! **Envoi** (20h12, [`PrivateBackup::run_once`]) : pour un gardien **d'une autre station** qui a annoncé le codage
//! (`["rs","1"]` dans `accept`), les blobs privés ne partent plus un à un : ils entrent une seule fois dans un flux de
//! paquets de 16 Mio (`astro_dav::backup_pack`), chaque paquet est rechiffré puis codé en `n` fragments dont `k`
//! suffisent ; chaque gardien retenu reçoit **un** fragment de chaque paquet (`op = frag`), une station n'en tient jamais
//! deux du même paquet (Q9). `k` est le plus grand qui garde la chance de tout retrouver au-dessus de
//! [`RS_TARGET_PERMILLE`] d'après les disponibilités mesurées ([`crate::guardians::proba_au_moins_k`]). Le gardien vérifie
//! (accord, quota en octets de fragment, en-tête ARSF, un seul fragment par paquet) puis acquitte (`frag-ack`) ; le
//! fragment local est alors effacé. L'index chiffré des paquets (clé dérivée du secret du compte) et le manifeste vont en
//! copie complète chez tous les gardiens (Q10). Les gardiens sans `rs` (ancienne version) et ceux de cette même station
//! gardent la copie complète (`op = item`), inchangée.
//!
//! **Restauration** : chaque gardien renvoie l'index qu'il tient (`restored`, `["kind","index"]`) ; la nouvelle station
//! garde le plus récent qui se déchiffre, reconstruit ses tables locales depuis lui, puis **tire** les fragments
//! (sonde `HASN`, `GET` sur `/astro/blob`), données d'abord puis gardiens les plus présents, décode dès `k` fragments
//! valides et revérifie chaque blob par son CID, les paquets des blobs les plus récents d'abord. Ce qu'elle n'a pas pu
//! tirer est redemandé aux gardiens par CID (renvoi poussé, repli hors maillage) et retenté toutes les 6 h (24 h une fois
//! la restauration « en retard »), pendant 30 jours.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll};

use astro_core::Audience;
use astro_crypto::Cipher;
use astro_crypto::nip59::Rumor;
use astro_dav::adopt::adopt_backup_blob;
use astro_dav::backup_pack::{self, BlobOut, Extractor, FRAG_HEADER_LEN, FragEntry, FragHeader, FragmentSink, MAX_FRAGMENT_BYTES, MAX_INDEX_BYTES, PAYLOAD, PackBuilder, PackError, PackIndex, PacketEntry, SealedPacket};
use astro_dav::{BlobStore, BlobWriter};
use astro_facecloud::Outbox;
use astro_nostr_proto::kind;
use rusqlite::OptionalExtension;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt, ReadBuf};

use super::{BackupStats, PrivateBackup, RESTORE_LATE_RETRY_SECS, RESTORE_LATE_SECS, RESTORE_RETRY_SECS, RESTORE_WANT, SEND_PACE, tag, valid_home};

/// Cible de sûreté (Q3) : chance, en ‰, de pouvoir réunir `k` gardiens au moment de restaurer.
pub const RS_TARGET_PERMILLE: u32 = 999;
/// Disponibilité supposée d'un gardien observé depuis moins d'une semaine (‰) : prudente, elle ne sert qu'à choisir `k`.
pub(super) const UNKNOWN_AVAILABILITY: u32 = 900;
/// Volume de blobs mis en paquets au plus par passage et par compte (4 paquets pleins) ; la nuit enchaîne les passages.
pub(super) const PASS_BYTES: u64 = 4 * PAYLOAD as u64;
/// Blobs candidats lus au plus par passage.
const PASS_FILES: usize = 2000;
/// Un fragment parti mais pas acquitté est renvoyé au bout d'un jour (le gardien était peut-être éteint).
const RESEND_SECS: i64 = 24 * 3600;
/// Côté gardien : fragments rangés au plus par 24 h et par propriétaire (§ 6.3). 512 fragments ≤ 8 Gio (16 Mio chacun au
/// pire) : une première sauvegarde de 50 Gio en 3 sur 5 (≈ 3 200 fragments de 5,3 Mio par gardien) passe en une semaine,
/// le rythme de l'envoi lui-même (4 paquets par passage, 200 passages par nuit au plus) n'est jamais freiné en temps
/// normal, et un propriétaire qui tournerait en boucle (envoyer, retirer, renvoyer) ne fait pas télécharger plus de 8 Gio
/// par jour à l'ami, quota ou pas.
pub(super) const NIGHT_FRAGS: i64 = if cfg!(test) { 4 } else { 512 };
/// Un Mio.
const MIB: u64 = 1 << 20;
/// Une copie vérifiée chez un ami compte 7 jours ; au-delà, le fichier repart dans les paquets.
const COPY_FRESH_SECS: i64 = 7 * 86_400;
/// Q5 : un blob d'au moins 256 Mio (16 paquets pleins) est « très gros » ; tenu en entier par au moins
/// [`BIG_BLOB_COPIES`] stations amies vérifiées depuis moins de 7 jours, il reste hors des paquets (le coder coûterait
/// `n/k ×` sa taille pour une sûreté que ces copies donnent déjà). En dessous, il entre au paquet comme les autres.
pub const BIG_BLOB: i64 = if cfg!(test) { 300_000 } else { 256 << 20 };
/// Q5 : copies vérifiées exigées pour laisser un très gros blob hors des paquets.
pub const BIG_BLOB_COPIES: usize = 2;
/// kv : le gardien `<owner>:<host>` a annoncé le codage k-sur-n.
pub(super) const RS: &str = "backup_rs";
/// kv côté propriétaire : dernier index envoyé à chaque gardien (`<owner>:<host>` → CID).
pub(super) const INDEX_SENT: &str = "backup_index_sent";
/// kv côté propriétaire : empreinte du contenu de l'index courant (révision exclue), pour ne le resceller que s'il change.
const INDEX_FP: &str = "backup_index_fp";
/// kv côté gardien : index tenu pour un ami (`<host>:<owner>` → CID), remplacé par le suivant.
pub(super) const INDEX_HELD: &str = "backup_index_held";
/// kv côté nouvelle station : index renvoyé par chaque gardien (`<owner>:<host>` → CID).
pub(super) const RESTORE_INDEX: &str = "restore_index";
/// kv côté nouvelle station : avancement du tirage des paquets (`owner` → JSON `state`, `rounds`, `first`, `last`, `dirty`).
pub(super) const RESTORE_RS: &str = "restore_rs";
/// kv : station (PeerID) d'un gardien, vue sur ses renvois, pour tirer ses fragments quand son DID manque.
pub(super) const PEER: &str = "backup_peer";

/// Couverture d'un fichier (lecture pour la Médiathèque).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct FileCopies {
    /// CID du blob.
    pub cid: String,
    /// Copies vérifiées : stations distinctes d'amis réciproques, vérifiées depuis moins de 7 jours.
    pub copies: usize,
    /// PeerID de ces stations.
    pub stations: Vec<String>,
    /// Dernière vérification.
    pub verified_at: Option<i64>,
    /// Le blob est dans les paquets k-sur-n.
    pub packed: bool,
}

/// Sonde groupée de présence (`HASN`) : `(CID, PeerID)` → présence de chacun.
pub type Probe = Arc<dyn Fn(Vec<String>, String) -> Pin<Box<dyn Future<Output = Result<Vec<bool>, String>> + Send>> + Send + Sync>;

/// Plus grand `k` (1..=min(n, 32)) tel qu'au moins `k` gardiens sur `n` soient joignables avec une chance ≥ `target` (‰) ;
/// `1` si même un seul ne l'atteint pas (copie complète chez chacun, le mieux qu'on puisse faire).
pub fn choose_k(dispos: &[u32], target: u32) -> u8 {
    let max = dispos.len().min(usize::from(backup_pack::MAX_K));
    (1..=max).rev().find(|&k| crate::guardians::proba_au_moins_k(dispos, k) >= target).and_then(|k| u8::try_from(k).ok()).unwrap_or(1)
}

/// Octets de fragments (en-têtes compris) que reçoit **chaque** gardien pour `volume` octets de blobs codés en `k`-sur-n.
fn frag_bytes(volume: u64, k: u8) -> u64 {
    let one = |len: u64| FragHeader::frag_len_for(len, k).map_or(u64::MAX / 8, |l| l + FRAG_HEADER_LEN as u64);
    let payload = PAYLOAD as u64;
    let (full, rest) = (volume / payload, volume % payload);
    let mut total = full.saturating_mul(one(backup_pack::PACKET_SIZE as u64));
    if rest > 0 || volume == 0 {
        let last = (1..=backup_pack::PACKET_SIZE as u64 / MIB).find(|m| backup_pack::payload_of(m * MIB).is_some_and(|p| p >= rest)).unwrap_or(16) * MIB;
        total = total.saturating_add(one(last));
    }
    total
}

/// Gardien candidat pour les paquets d'un passage.
#[derive(Debug, Clone)]
pub(super) struct Keeper {
    pub(super) hex: String,
    /// Station qui l'héberge (PeerID de sa station d'accueil, sinon sa clé) : une station = un fragment par paquet.
    pub(super) station: String,
    pub(super) avail: u32,
    pub(super) room: u64,
}

/// Retient les gardiens qui ont la place de leur fragment et calcule `k` ; un gardien trop plein sort et `k` est recalculé.
pub(super) fn select(mut keepers: Vec<Keeper>, volume: u64) -> (Vec<Keeper>, u8) {
    loop {
        if keepers.is_empty() {
            return (keepers, 1);
        }
        let k = choose_k(&keepers.iter().map(|g| g.avail).collect::<Vec<_>>(), RS_TARGET_PERMILLE);
        let need = frag_bytes(volume, k);
        let before = keepers.len();
        keepers.retain(|g| g.room >= need);
        if keepers.len() == before {
            return (keepers, k);
        }
    }
}

pub(super) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(super) type Queue = Arc<Mutex<Vec<SealedPacket>>>;
pub(super) type Written = Arc<Mutex<Vec<(String, u64)>>>;
pub(super) type Heads = Arc<Mutex<HashMap<u32, [u8; astro_crypto::uenc::HEADER_LEN]>>>;

/// kv : fragment écrit dans le magasin local mais pas encore inscrit (`cid` → propriétaire). Une coupure en plein encodage
/// laisse ces lignes ; le ramassage nocturne efface les blobs qu'aucune table ne référence (lot 5).
pub(super) const SPOOL: &str = "backup_spool";

/// Destinataire des paquets scellés : une file vidée dans le magasin avant la lecture suivante ([`Throttle`]) ; l'en-tête
/// UENC de chaque paquet est gardé pour le recalcul de ses fragments.
struct Spool(Queue, Heads);

impl FragmentSink for Spool {
    fn put(&mut self, sealed: SealedPacket) -> Result<(), PackError> {
        lock(&self.1).insert(sealed.packet, sealed.head);
        lock(&self.0).push(sealed);
        Ok(())
    }
}

/// Journal des fragments écrits : base et propriétaire.
pub(super) type Journal = Option<(Arc<astro_db::Db>, String)>;

/// Note un fragment écrit avant de l'inscrire (voir [`SPOOL`]).
pub(super) async fn journal(j: &Journal, cid: &str) -> std::io::Result<()> {
    let Some((db, owner)) = j else { return Ok(()) };
    db.writer()
        .execute(
            "INSERT INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(scope, key) DO NOTHING",
            vec![SPOOL.to_owned().into(), cid.to_owned().into(), owner.clone().into(), super::now().into()],
        )
        .await
        .map(|_| ())
        .map_err(std::io::Error::other)
}

/// Écrit dans le magasin les fragments en file (et note leur CID et leur taille). Chaque fragment est d'abord journalisé :
/// une coupure entre l'écriture et l'inscription ne laisse jamais de blob introuvable.
async fn flush<B: BlobStore>(queue: Queue, store: Arc<B>, written: Written, j: Journal) -> std::io::Result<()> {
    let packets = std::mem::take(&mut *lock(&queue));
    for packet in packets {
        for fragment in packet.fragments {
            let cid = astro_dav::blob::cid_raw_sha256(&Sha256::digest(&fragment).into());
            journal(&j, &cid).await?;
            let mut w = store.begin().await.map_err(std::io::Error::other)?;
            w.write_all(&fragment).await?;
            let meta = w.commit().await.map_err(std::io::Error::other)?;
            lock(&written).push((meta.cid, fragment.len() as u64));
        }
    }
    Ok(())
}

type Flush = Pin<Box<dyn Future<Output = std::io::Result<()>> + Send>>;

/// Lecteur d'un blob qui, avant chaque lecture, vide la file des paquets scellés dans le magasin : un gros blob (vidéo de
/// plusieurs Gio) ne garde jamais plus d'un paquet et de ses fragments en mémoire.
struct Throttle<R, B: BlobStore> {
    inner: R,
    queue: Queue,
    store: Arc<B>,
    written: Written,
    journal: Journal,
    pending: Option<Flush>,
}

impl<R: AsyncRead + Unpin, B: BlobStore> AsyncRead for Throttle<R, B> {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        loop {
            if let Some(f) = this.pending.as_mut() {
                match f.as_mut().poll(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(r) => {
                        this.pending = None;
                        r?;
                    }
                }
            }
            if lock(&this.queue).is_empty() {
                return Pin::new(&mut this.inner).poll_read(cx, buf);
            }
            this.pending = Some(Box::pin(flush(this.queue.clone(), this.store.clone(), this.written.clone(), this.journal.clone())));
        }
    }
}

/// Ce que l'[`Extractor`] rend pour un paquet, rejoué ensuite dans le magasin (écriture asynchrone).
enum Ev {
    Write(String, Vec<u8>),
    Done(String, bool),
}

#[derive(Default)]
struct BufOut(Vec<Ev>);

impl BlobOut for BufOut {
    fn write(&mut self, cid: &str, bytes: &[u8]) -> Result<(), PackError> {
        self.0.push(Ev::Write(cid.to_owned(), bytes.to_vec()));
        Ok(())
    }
    fn done(&mut self, cid: &str, ok: bool) -> Result<(), PackError> {
        self.0.push(Ev::Done(cid.to_owned(), ok));
        Ok(())
    }
}

/// Paquets `[début, fin]` sur lesquels s'étale chaque blob de l'index.
fn spans(index: &PackIndex) -> Vec<(usize, usize)> {
    index
        .blobs
        .iter()
        .map(|b| {
            let start = b.packet as usize;
            let mut end = start;
            let mut left = b.off.saturating_add(b.len);
            while let Some(p) = index.packets.get(end) {
                let room = backup_pack::payload_of(p.len).unwrap_or(0);
                if left <= room || end + 1 >= index.packets.len() {
                    break;
                }
                left -= room;
                end += 1;
            }
            (start, end)
        })
        .collect()
}

impl<O: Outbox, B: BlobStore> PrivateBackup<O, B> {
    /// Sonde groupée de présence des blobs chez une station (`HASN`), pour la restauration.
    pub fn with_probe(mut self, probe: Probe) -> Self {
        self.probe = Some(probe);
        self
    }

    // ───────────────────────────── Envoi ─────────────────────────────

    /// Un passage k-sur-n pour chaque compte hébergé ici qui a des gardiens `rs` : mise en paquets de ce qui reste, envoi
    /// des fragments en attente, index rescellé et renvoyé s'il a changé. Rend le nombre de messages partis.
    pub(super) async fn rs_once(&self, at: i64) -> usize {
        let owners: Vec<String> = self
            .db
            .readers()
            .read(|c| {
                let mut stmt = c.prepare(
                    "SELECT DISTINCT p.owner_hex FROM backup_pacts p JOIN identities i ON i.hex = p.owner_hex AND i.hosted = 1 \
                     JOIN kv k ON k.scope = ?1 AND k.key = p.owner_hex || ':' || p.host_hex WHERE p.status = 'accepted' AND p.enabled = 1",
                )?;
                Ok(stmt.query_map([RS], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .unwrap_or_default();
        let mut sent = 0;
        for owner in owners {
            self.verify_copies(&owner, at).await;
            if let Err(e) = self.pack_pass(&owner, at).await {
                self.activity.publish_to(Audience::Captain, "alert", "backup", format!("sauvegarde k-sur-n de …{} : paquets non construits ({e})", &owner[..8.min(owner.len())]), None);
            }
            // Compactage et migration des copies complètes (lot 6) : les fragments neufs partent avec l'envoi ci-dessous.
            sent += self.compact_step(&owner, at).await;
            sent += self.send_frags(&owner, at).await;
            if self.seal_index(&owner, at).await.is_some() {
                sent += self.send_index(&owner).await;
            }
        }
        sent
    }

    /// Gardiens `rs` de `owner` sur d'autres stations, un par station (le plus présent, puis le plus spacieux).
    pub(super) async fn rs_keepers(&self, owner: &str, at: i64) -> Vec<Keeper> {
        let (o, today) = (owner.to_owned(), at / 86_400);
        let rows: Vec<(String, i64, i64, String, i64, i64)> = self
            .db
            .readers()
            .read(move |c| {
                let mut stmt = c.prepare(
                    "SELECT p.host_hex, p.quota_bytes, COALESCE(p.decided_at, p.asked_at), COALESCE(i.home_station, ''), \
                            (SELECT COUNT(*) FROM guardian_presence g WHERE g.hex = p.host_hex AND g.day > ?2 - ?3 AND g.day <= ?2), \
                            COALESCE((SELECT SUM(size) FROM backup_items b WHERE b.owner_hex = p.owner_hex AND b.host_hex = p.host_hex), 0) \
                            + COALESCE((SELECT SUM(size) FROM backup_frags f WHERE f.owner_hex = p.owner_hex AND f.host_hex = p.host_hex AND f.lost_at IS NULL), 0) \
                     FROM backup_pacts p JOIN kv k ON k.scope = ?4 AND k.key = p.owner_hex || ':' || p.host_hex \
                     LEFT JOIN identities i ON i.hex = p.host_hex \
                     WHERE p.owner_hex = ?1 AND p.status = 'accepted' AND p.enabled = 1 AND COALESCE(i.hosted, 0) = 0",
                )?;
                Ok(stmt.query_map(rusqlite::params![o, today, crate::guardians::WINDOW_DAYS, RS], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .unwrap_or_default();
        let mut all: Vec<Keeper> = rows
            .into_iter()
            .map(|(hex, quota, since, home, seen, used)| {
                let station = if valid_home(&home) { home.split(':').next().unwrap_or_default().to_owned() } else { hex.clone() };
                let avail = crate::guardians::availability(seen, today - since / 86_400 + 1).unwrap_or(UNKNOWN_AVAILABILITY);
                Keeper { hex, station, avail, room: u64::try_from(quota.saturating_sub(used)).unwrap_or(0) }
            })
            .collect();
        all.sort_by(|a, b| b.avail.cmp(&a.avail).then(b.room.cmp(&a.room)).then(a.hex.cmp(&b.hex)));
        let mut stations = HashSet::new();
        all.retain(|g| stations.insert(g.station.clone()));
        all.truncate(usize::from(backup_pack::MAX_FRAG_INDEX));
        all
    }

    /// Blobs privés de `owner` pas encore dans un paquet (seuls les CID `bafkrei…`, vérifiables, y entrent). Un fichier
    /// partagé (lien, ami, FaceCloud, invitation) n'en est plus exclu d'office : il ne reste dehors que si ses copies
    /// vérifiées chez des amis suffisent à la cible ([`Self::covered`]) ; ce qui est reçu d'autrui (`/Partages/`) ou gardé
    /// pour autrui (`/Sauvegardes/`) n'entre jamais.
    pub(super) async fn pending_raw(&self, owner: &str, at: i64) -> Vec<String> {
        let o = owner.to_owned();
        let rows: Vec<(String, bool, bool)> = self
            .db
            .readers()
            .read(move |c| {
                let mut stmt = c.prepare(
                    "SELECT e.cid, \
                            MAX(EXISTS (SELECT 1 FROM ucloud_sharing s WHERE s.owner_hex = e.owner_hex AND s.cid = e.cid) \
                            OR EXISTS (SELECT 1 FROM face_shares f WHERE f.owner_hex = e.owner_hex AND f.cid = e.cid) \
                            OR EXISTS (SELECT 1 FROM invites i WHERE i.owner_hex = e.owner_hex AND i.revoked_at IS NULL AND instr(i.items, e.cid) > 0)), \
                            MAX(COALESCE(e.size, 0)) >= ?3 \
                     FROM ucloud_entries e \
                     WHERE e.owner_hex = ?1 AND e.type = 'file' AND e.cid LIKE 'bafkrei%' \
                       AND e.path NOT LIKE '/Partages/%' AND e.path NOT LIKE '/Sauvegardes/%' \
                       AND NOT EXISTS (SELECT 1 FROM backup_blobs b WHERE b.owner_hex = e.owner_hex AND b.cid = e.cid) \
                     GROUP BY e.cid ORDER BY MIN(COALESCE(e.mtime, 0)), e.cid LIMIT ?2",
                )?;
                Ok(stmt.query_map(rusqlite::params![o, PASS_FILES as i64, BIG_BLOB], |r| Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?, r.get::<_, bool>(2)?)))?.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .unwrap_or_default();
        let covered = if rows.iter().any(|r| r.1) { self.covered(owner, at).await } else { HashSet::new() };
        // Q5 : un très gros blob déjà tenu en entier par au moins deux stations amies vérifiées reste hors des paquets.
        let fresh = if rows.iter().any(|r| r.2) { self.fresh_copies(owner, at).await } else { BTreeMap::new() };
        let held_twice = |cid: &str| fresh.get(cid).is_some_and(|(peers, _)| peers.len() >= BIG_BLOB_COPIES);
        rows.into_iter().filter(|(cid, shared, big)| !((*shared && covered.contains(cid)) || (*big && held_twice(cid)))).map(|(cid, ..)| cid).collect()
    }

    /// Stations distinctes des amis réciproques de `owner` hébergés ailleurs (pas cette station-ci), avec leurs comptes.
    async fn friend_stations(&self, owner: &str) -> BTreeMap<String, Vec<String>> {
        let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for friend in self.sharing.friends(owner).await.unwrap_or_default().into_iter().filter(|f| !f.hosted) {
            let Some(peer) = self.home_peer(&friend.hex).await else { continue };
            if Some(&peer) != self.station_peer.as_ref() {
                out.entry(peer).or_default().push(friend.hex);
            }
        }
        out
    }

    /// PeerID de la station d'accueil (DID) d'un compte.
    async fn home_peer(&self, hex: &str) -> Option<String> {
        let h = hex.to_owned();
        let home: Option<String> = self.db.readers().read(move |c| Ok(c.query_row("SELECT home_station FROM identities WHERE hex = ?1", [h], |r| r.get::<_, Option<String>>(0)).optional()?.flatten())).await.ok().flatten();
        home.filter(|h| valid_home(h)).and_then(|h| h.split(':').next().map(str::to_owned))
    }

    /// Sonde de nuit des copies : pour chaque fichier partagé pas encore en paquet, quelles stations d'amis réciproques
    /// détiennent NOTRE CID (`HASN`, 4 096 CID par flux) ? Une station joignable qui ne l'a plus perd sa ligne ; une station
    /// injoignable garde la sienne, qui vieillit (7 jours). Rend le nombre de copies vérifiées.
    pub(super) async fn verify_copies(&self, owner: &str, at: i64) -> usize {
        let Some(probe) = self.probe.clone() else { return 0 };
        let o = owner.to_owned();
        let cids: Vec<String> = self
            .db
            .readers()
            .read(move |c| {
                let mut stmt = c.prepare(
                    "SELECT DISTINCT e.cid FROM ucloud_entries e \
                     WHERE e.owner_hex = ?1 AND e.type = 'file' AND e.cid LIKE 'bafkrei%' \
                       AND e.path NOT LIKE '/Partages/%' AND e.path NOT LIKE '/Sauvegardes/%' \
                       AND NOT EXISTS (SELECT 1 FROM backup_blobs b WHERE b.owner_hex = e.owner_hex AND b.cid = e.cid) \
                       AND (COALESCE(e.size, 0) >= ?3 \
                            OR EXISTS (SELECT 1 FROM ucloud_sharing s WHERE s.owner_hex = e.owner_hex AND s.cid = e.cid) \
                            OR EXISTS (SELECT 1 FROM face_shares f WHERE f.owner_hex = e.owner_hex AND f.cid = e.cid) \
                            OR EXISTS (SELECT 1 FROM invites i WHERE i.owner_hex = e.owner_hex AND i.revoked_at IS NULL AND instr(i.items, e.cid) > 0)) \
                     ORDER BY e.cid LIMIT ?2",
                )?;
                Ok(stmt.query_map(rusqlite::params![o, astro_p2p::blob::MAX_HAS_MANY as i64, BIG_BLOB], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .unwrap_or_default();
        if cids.is_empty() {
            return 0;
        }
        let mut verified = 0;
        for peer in self.friend_stations(owner).await.into_keys() {
            let Ok(bits) = probe(cids.clone(), peer.clone()).await else { continue };
            let (o, p, list) = (owner.to_owned(), peer, cids.clone());
            let n = self
                .db
                .writer()
                .transaction(move |tx| {
                    let mut n = 0;
                    for (cid, has) in list.iter().zip(bits) {
                        if has {
                            tx.execute(
                                "INSERT INTO blob_copies(owner_hex, cid, station_peer, verified_at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(owner_hex, cid, station_peer) DO UPDATE SET verified_at = excluded.verified_at",
                                rusqlite::params![o, cid, p, at],
                            )?;
                            n += 1;
                        } else {
                            tx.execute("DELETE FROM blob_copies WHERE owner_hex = ?1 AND cid = ?2 AND station_peer = ?3", rusqlite::params![o, cid, p])?;
                        }
                    }
                    Ok(n)
                })
                .await;
            match n {
                Ok(n) => verified += n,
                Err(e) => {
                    self.activity.publish_to(Audience::Captain, "alert", "backup", format!("copies vérifiées non notées ({e})"), None);
                }
            }
        }
        verified
    }

    /// Copies vérifiées depuis moins de [`COPY_FRESH_SECS`] : CID → (stations, dernière vérification).
    async fn fresh_copies(&self, owner: &str, at: i64) -> BTreeMap<String, (Vec<String>, i64)> {
        let o = owner.to_owned();
        let rows: Vec<(String, String, i64)> = self
            .db
            .readers()
            .read(move |c| {
                let mut s = c.prepare("SELECT cid, station_peer, verified_at FROM blob_copies WHERE owner_hex = ?1 AND verified_at >= ?2 ORDER BY cid, station_peer")?;
                Ok(s.query_map(rusqlite::params![o, at - COPY_FRESH_SECS], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .unwrap_or_default();
        let mut out: BTreeMap<String, (Vec<String>, i64)> = BTreeMap::new();
        for (cid, peer, when) in rows {
            let e = out.entry(cid).or_insert_with(|| (Vec::new(), when));
            e.0.push(peer);
            e.1 = e.1.max(when);
        }
        out
    }

    /// Disponibilité (‰) d'une station d'amis : la meilleure de ses comptes ; jamais vue, 900 ‰ supposés.
    async fn station_availability(&self, friends: &[String], at: i64) -> u32 {
        let mut best: Option<u32> = None;
        for f in friends {
            let (h, today) = (f.clone(), at / 86_400);
            let seen: i64 = self.db.readers().read(move |c| Ok(c.query_row("SELECT COUNT(*) FROM guardian_presence WHERE hex = ?1 AND day > ?2 - ?3 AND day <= ?2", rusqlite::params![h, today, crate::guardians::WINDOW_DAYS], |r| r.get(0))?)).await.unwrap_or(0);
            let p = if seen == 0 { UNKNOWN_AVAILABILITY } else { crate::guardians::availability(seen, crate::guardians::WINDOW_DAYS).unwrap_or(UNKNOWN_AVAILABILITY) };
            best = Some(best.map_or(p, |b| b.max(p)));
        }
        best.unwrap_or(UNKNOWN_AVAILABILITY)
    }

    /// Fichiers partagés dont les copies vérifiées suffisent à la cible : `proba_au_moins_k(stations, 1) ≥ 999 ‰`.
    async fn covered(&self, owner: &str, at: i64) -> HashSet<String> {
        let stations = self.friend_stations(owner).await;
        let mut dispo = HashMap::new();
        for (peer, friends) in &stations {
            dispo.insert(peer.clone(), self.station_availability(friends, at).await);
        }
        self.fresh_copies(owner, at)
            .await
            .into_iter()
            .filter(|(_, (peers, _))| {
                let d: Vec<u32> = peers.iter().filter_map(|p| dispo.get(p).copied()).collect();
                crate::guardians::proba_au_moins_k(&d, 1) >= RS_TARGET_PERMILLE
            })
            .map(|(cid, _)| cid)
            .collect()
    }

    /// Par fichier, copies vérifiées chez des amis (stations distinctes, date de la dernière vérification) et s'il est
    /// couvert par les paquets k-sur-n. Lecture groupée : une requête pour tous les fichiers demandés.
    pub async fn file_copies(&self, owner: &str, cids: &[String], at: i64) -> Vec<FileCopies> {
        let fresh = self.fresh_copies(owner, at).await;
        let o = owner.to_owned();
        let packed: HashSet<String> = self.db.readers().read(move |c| Ok(c.prepare("SELECT cid FROM backup_blobs WHERE owner_hex = ?1")?.query_map([o], |r| r.get::<_, String>(0))?.collect::<Result<HashSet<_>, _>>()?)).await.unwrap_or_default();
        cids.iter()
            .map(|cid| {
                let (stations, verified_at) = fresh.get(cid).map_or((Vec::new(), None), |(p, w)| (p.clone(), Some(*w)));
                FileCopies { cid: cid.clone(), copies: stations.len(), stations, verified_at, packed: packed.contains(cid) }
            })
            .collect()
    }

    /// Médiathèque : pour chaque fichier **partagé** du membre, ses copies vérifiées chez des amis (prénoms, date) et s'il
    /// est aussi dans sa sauvegarde ; `safe` : ces copies suffisent seules à la cible. Quelques lectures en tout, jamais
    /// une par fichier : `{ files: { <chemin>: { copies, at: [noms], verified_at, packed, safe } } }`.
    pub async fn media_copies(&self, owner: &str, at: i64) -> Result<Value, String> {
        let o = owner.to_owned();
        let rows: Vec<(String, String)> = self
            .db
            .readers()
            .read(move |c| {
                let mut s = c.prepare(
                    "SELECT e.path, e.cid FROM ucloud_entries e WHERE e.owner_hex = ?1 AND e.type = 'file' AND e.cid IS NOT NULL \
                       AND e.path NOT LIKE '/Partages/%' AND e.path NOT LIKE '/Sauvegardes/%' \
                       AND (EXISTS (SELECT 1 FROM ucloud_sharing s WHERE s.owner_hex = e.owner_hex AND s.cid = e.cid) \
                            OR EXISTS (SELECT 1 FROM face_shares f WHERE f.owner_hex = e.owner_hex AND f.cid = e.cid)) LIMIT 20000",
                )?;
                Ok(s.query_map([o], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .map_err(|e| e.to_string())?;
        if rows.is_empty() {
            return Ok(json!({ "files": {} }));
        }
        let cids: Vec<String> = rows.iter().map(|r| r.1.clone()).collect();
        let copies: HashMap<String, FileCopies> = self.file_copies(owner, &cids, at).await.into_iter().map(|f| (f.cid.clone(), f)).collect();
        let covered = self.covered(owner, at).await;
        let mut names: HashMap<String, String> = HashMap::new();
        for (peer, friends) in self.friend_stations(owner).await {
            if let Some(f) = friends.first() {
                names.insert(peer, self.label_of(f).await);
            }
        }
        let mut files = serde_json::Map::new();
        for (path, cid) in rows {
            let Some(f) = copies.get(&cid) else { continue };
            let at: Vec<&String> = f.stations.iter().filter_map(|p| names.get(p)).collect();
            files.insert(path, json!({ "copies": f.copies, "at": at, "verified_at": f.verified_at, "packed": f.packed, "safe": covered.contains(&cid) }));
        }
        Ok(json!({ "files": files }))
    }

    /// Ma sécurité : la sauvegarde répartie vue par le membre — paquets, gardiens qui en tiennent une part, combien en
    /// suffisent, chance estimée de tout retrouver (le plus faible des paquets) face à la cible, place prise chez les amis
    /// (pour cent du volume sauvegardé), réparation ou rangement en cours. `None` sans paquet.
    pub async fn coded_view(&self, owner: &str, at: i64) -> Option<Value> {
        let o = owner.to_owned();
        type Rows = (Vec<(String, i64, bool)>, Vec<(String, String)>, i64, i64, i64, i64);
        let (packets, safe, frag_bytes, blob_bytes, repairing, lost): Rows = self
            .db
            .readers()
            .read(move |c| {
                let mut s = c.prepare("SELECT packet_id, k, staged = 1 FROM backup_packets WHERE owner_hex = ?1")?;
                let packets = s.query_map([&o], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<Result<Vec<_>, _>>()?;
                let mut s = c.prepare("SELECT packet_id, host_hex FROM backup_frags WHERE owner_hex = ?1 AND acked_at IS NOT NULL AND lost_at IS NULL")?;
                let safe = s.query_map([&o], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<Vec<_>, _>>()?;
                let frag: i64 = c.query_row("SELECT COALESCE(SUM(f.size), 0) FROM backup_frags f JOIN backup_packets p ON p.owner_hex = f.owner_hex AND p.packet_id = f.packet_id AND p.staged = 0 WHERE f.owner_hex = ?1 AND f.lost_at IS NULL", [&o], |r| r.get(0))?;
                let blob: i64 = c.query_row("SELECT COALESCE(SUM(len), 0) FROM backup_blobs WHERE owner_hex = ?1", [&o], |r| r.get(0))?;
                // Réparation en cours : part refaite pour un paquet qui en a perdu une, pas encore acquittée.
                let repairing: i64 = c.query_row(
                    "SELECT COUNT(*) FROM backup_frags f WHERE f.owner_hex = ?1 AND f.acked_at IS NULL AND f.lost_at IS NULL \
                     AND EXISTS (SELECT 1 FROM backup_frags x WHERE x.owner_hex = f.owner_hex AND x.packet_id = f.packet_id AND x.lost_at IS NOT NULL)",
                    [&o],
                    |r| r.get(0),
                )?;
                let lost: i64 = c.query_row("SELECT COUNT(*) FROM backup_frags WHERE owner_hex = ?1 AND lost_at IS NOT NULL", [&o], |r| r.get(0))?;
                Ok((packets, safe, frag, blob, repairing, lost))
            })
            .await
            .ok()?;
        let live: Vec<&(String, i64, bool)> = packets.iter().filter(|p| !p.2).collect();
        if live.is_empty() {
            return None;
        }
        let avail: HashMap<String, u32> = self.rs_keepers(owner, at).await.into_iter().map(|g| (g.hex, g.avail)).collect();
        let mut holders: HashMap<&str, Vec<&str>> = HashMap::new();
        for (p, h) in &safe {
            holders.entry(p.as_str()).or_default().push(h.as_str());
        }
        let (mut safety, mut thin, mut need) = (1000u32, 0usize, 0i64);
        for (id, k, _) in &live {
            let hs = holders.get(id.as_str()).map_or(&[][..], Vec::as_slice);
            let d: Vec<u32> = hs.iter().map(|h| avail.get(*h).copied().unwrap_or(UNKNOWN_AVAILABILITY)).collect();
            safety = safety.min(usize::try_from(*k).map_or(0, |k| crate::guardians::proba_au_moins_k(&d, k)));
            if i64::try_from(hs.len()).unwrap_or(0) <= *k && lost > 0 {
                thin += 1;
            }
            need = need.max(*k);
        }
        let all: HashSet<&str> = safe.iter().map(|s| s.1.as_str()).collect();
        Some(json!({
            "packets": live.len(), "holders": all.len(), "need": need,
            "safety_permille": safety, "target_permille": RS_TARGET_PERMILLE,
            "ratio_pct": if blob_bytes > 0 { frag_bytes * 100 / blob_bytes } else { 0 },
            "repairing": repairing, "thin": thin, "compacting": packets.len() > live.len(),
        }))
    }

    /// Met en paquets les blobs privés en attente (au plus [`PASS_BYTES`]), écrit les fragments dans le magasin local
    /// (référencés par `backup_frags`, donc servis par le `CidGate`) et les inscrit à envoyer. Rend le nombre de paquets.
    ///
    /// Reprise : tout est inscrit en une transaction après coup ; une coupure avant laisse des fragments orphelins dans le
    /// magasin et rien en base, et le passage suivant recommence ces blobs avec une clé neuve.
    async fn pack_pass(&self, owner: &str, at: i64) -> Result<usize, String> {
        let keepers = self.rs_keepers(owner, at).await;
        if keepers.is_empty() {
            return Ok(0);
        }
        let (mut chosen, mut volume, mut absent) = (Vec::new(), 0u64, 0usize);
        for cid in self.pending_raw(owner, at).await {
            match self.blobs.open(&cid).await {
                Ok((_, size)) => {
                    if !chosen.is_empty() && volume.saturating_add(size) > PASS_BYTES {
                        break;
                    }
                    chosen.push(cid);
                    volume = volume.saturating_add(size);
                }
                Err(_) => absent += 1,
            }
        }
        if absent > 0 {
            self.activity.publish_to(Audience::Captain, "alert", "backup", format!("sauvegarde k-sur-n de …{} : {absent} blob(s) absent(s) du magasin, pas mis en paquet", &owner[..8.min(owner.len())]), None);
        }
        if chosen.is_empty() {
            return Ok(0);
        }
        let (keepers, k) = select(keepers, volume);
        if keepers.is_empty() {
            return Ok(0); // aucun gardien n'a la place d'un fragment : rien ne part (le quota est celui de l'ami)
        }
        let n = u8::try_from(keepers.len()).map_err(|_| "trop de gardiens".to_owned())?;
        let (queue, written, heads): (Queue, Written, Heads) = (Arc::default(), Arc::default(), Arc::default());
        let index = match self.build(owner, k, n, &chosen, &queue, &written, &heads).await {
            Ok(index) => index,
            Err(e) => {
                let orphans = std::mem::take(&mut *lock(&written));
                for (cid, _) in orphans {
                    let _ = self.blobs.remove(&cid).await;
                    self.kv_del(SPOOL, &cid).await;
                }
                return Err(e.to_string());
            }
        };
        let hosts: Vec<String> = keepers.iter().map(|g| g.hex.clone()).collect();
        let packets = index.packets.len();
        self.record_build(owner, at, index, &hosts, &written, &heads, None).await?;
        Ok(packets)
    }

    /// Inscrit, en une transaction, les paquets d'un flux construit : paquets (avec l'en-tête UENC gardé pour le recalcul),
    /// fragments à envoyer (journal vidé), position des blobs. `staged = Some(anciens)` : paquets neufs d'un compactage —
    /// ils sont marqués `staged`, leurs blobs vont dans `backup_compact` (les blobs restent rattachés aux anciens paquets
    /// jusqu'à la bascule) et la liste des anciens paquets est notée dans le même mouvement.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn record_build(&self, owner: &str, at: i64, index: PackIndex, hosts: &[String], written: &Written, heads: &Heads, staged: Option<String>) -> Result<(), String> {
        let sizes: HashMap<String, u64> = lock(written).iter().cloned().collect();
        let heads: HashMap<u32, String> = lock(heads).iter().map(|(p, h)| (*p, hex::encode(h))).collect();
        let (o, hosts) = (owner.to_owned(), hosts.to_vec());
        self.db
            .writer()
            .transaction(move |tx| {
                let base: i64 = tx.query_row("SELECT COALESCE(MAX(seq) + 1, 0) FROM backup_packets WHERE owner_hex = ?1", [&o], |r| r.get(0))?;
                for (j, p) in index.packets.iter().enumerate() {
                    let seq = base + i64::try_from(j).unwrap_or(i64::MAX);
                    let head = u32::try_from(j).ok().and_then(|j| heads.get(&j));
                    tx.execute(
                        "INSERT INTO backup_packets(owner_hex, packet_id, seq, key_hex, len, k, at, head_hex, staged) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                        rusqlite::params![o, p.id, seq, p.key, i64::try_from(p.len).unwrap_or(0), p.k, at, head, i64::from(staged.is_some())],
                    )?;
                    for f in &p.frags {
                        tx.execute("DELETE FROM kv WHERE scope = ?1 AND key = ?2", rusqlite::params![SPOOL, f.cid])?;
                        let Some(host) = hosts.get(usize::from(f.i)) else { continue };
                        let size = sizes.get(&f.cid).copied().unwrap_or(0);
                        tx.execute(
                            "INSERT INTO backup_frags(owner_hex, host_hex, packet_id, i, cid, size) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                            rusqlite::params![o, host, p.id, f.i, f.cid, i64::try_from(size).unwrap_or(0)],
                        )?;
                    }
                }
                let table = if staged.is_some() { "backup_compact(owner_hex, cid, seq, off, len) VALUES (?1, ?2, ?3, ?4, ?5)" } else { "backup_blobs(owner_hex, cid, seq, off, len, at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)" };
                for b in &index.blobs {
                    let (seq, off, len) = (base + i64::from(b.packet), i64::try_from(b.off).unwrap_or(0), i64::try_from(b.len).unwrap_or(0));
                    if staged.is_some() {
                        tx.execute(&format!("INSERT OR IGNORE INTO {table}"), rusqlite::params![o, b.cid, seq, off, len])?;
                    } else {
                        tx.execute(&format!("INSERT OR IGNORE INTO {table}"), rusqlite::params![o, b.cid, seq, off, len, at])?;
                    }
                }
                if let Some(from) = &staged {
                    tx.execute(
                        "INSERT INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                        rusqlite::params![super::compact::COMPACT_FROM, o, from, at],
                    )?;
                }
                Ok(())
            })
            .await
            .map_err(|e| e.to_string())
    }

    /// Construit le flux de paquets des blobs `chosen`, fragments écrits dans le magasin au fil de l'eau.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn build(&self, owner: &str, k: u8, n: u8, chosen: &[String], queue: &Queue, written: &Written, heads: &Heads) -> Result<PackIndex, PackError> {
        let mut builder = PackBuilder::new(owner, 0, k, n, Cipher::detect(), Vec::<String>::new())?;
        let mut sink = Spool(queue.clone(), heads.clone());
        let j: Journal = Some((self.db.clone(), owner.to_owned()));
        for cid in chosen {
            let (reader, _) = self.blobs.open(cid).await.map_err(|e| PackError::Sink(e.to_string()))?;
            let mut reader = Throttle { inner: reader, queue: queue.clone(), store: self.blobs.clone(), written: written.clone(), journal: j.clone(), pending: None };
            builder.push_blob(cid, &mut reader, &mut sink).await?;
        }
        let index = builder.finish(&mut sink).await?;
        flush(queue.clone(), self.blobs.clone(), written.clone(), j).await?;
        Ok(index)
    }

    /// Envoie les fragments pas encore partis (ou partis depuis un jour sans accusé). Rend le nombre de messages partis.
    async fn send_frags(&self, owner: &str, at: i64) -> usize {
        let o = owner.to_owned();
        let rows: Vec<(String, String, i64, String, i64)> = self
            .db
            .readers()
            .read(move |c| {
                let mut stmt = c.prepare(
                    "SELECT f.host_hex, f.packet_id, f.i, f.cid, f.size FROM backup_frags f \
                     JOIN backup_pacts p ON p.owner_hex = f.owner_hex AND p.host_hex = f.host_hex AND p.status = 'accepted' \
                     WHERE f.owner_hex = ?1 AND f.acked_at IS NULL AND (f.sent_at IS NULL OR f.sent_at <= ?2) ORDER BY f.host_hex, f.packet_id",
                )?;
                Ok(stmt.query_map(rusqlite::params![o, at - RESEND_SECS], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .unwrap_or_default();
        let (mut sent, mut down) = (0, HashSet::new());
        for (host, packet, i, cid, size) in rows {
            if down.contains(&host) || self.blobs.open(&cid).await.is_err() {
                continue; // fragment local perdu : il sera recalculé par la réparation (lot 5)
            }
            let mut tags = vec![vec!["cid".to_owned(), cid.clone()], vec!["size".into(), size.to_string()], vec!["op".into(), "frag".into()], vec!["packet".into(), packet], vec!["i".into(), i.to_string()]];
            if let Some(peer) = &self.station_peer {
                tags.push(vec!["station".into(), peer.clone()]);
            }
            let result = self.send_to(owner, &host, kind::FILE, tags, &format!("ipfs://{cid}")).await;
            tokio::time::sleep(SEND_PACE).await;
            match result {
                Ok(()) => {
                    let (o, h, c) = (owner.to_owned(), host.clone(), cid.clone());
                    let _ = self.db.writer().execute("UPDATE backup_frags SET sent_at = ?4 WHERE owner_hex = ?1 AND host_hex = ?2 AND cid = ?3", vec![o.into(), h.into(), c.into(), at.into()]).await;
                    sent += 1;
                }
                Err(e) => {
                    self.activity.publish_to(Audience::Captain, "alert", "backup", format!("fragment de sauvegarde non envoyé à …{} : {e}", &host[..8.min(host.len())]), None);
                    down.insert(host);
                }
            }
        }
        sent
    }

    /// Index des paquets tel que les tables locales le décrivent (révision 0) ; `None` sans paquet.
    async fn current_index(&self, owner: &str) -> Option<PackIndex> {
        let o = owner.to_owned();
        type Rows = (Vec<(String, i64, String, i64, i64)>, Vec<(String, i64, String, String)>, Vec<(String, i64, i64, i64)>, Vec<(String, String)>);
        let (packets, frags, blobs, legacy): Rows = self
            .db
            .readers()
            .read(move |c| {
                let mut s = c.prepare("SELECT packet_id, seq, key_hex, len, k FROM backup_packets WHERE owner_hex = ?1 ORDER BY seq")?;
                let packets = s.query_map([&o], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?.collect::<Result<Vec<_>, _>>()?;
                let mut s = c.prepare("SELECT packet_id, i, cid, host_hex FROM backup_frags WHERE owner_hex = ?1 AND lost_at IS NULL ORDER BY packet_id, i")?;
                let frags = s.query_map([&o], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<Result<Vec<_>, _>>()?;
                let mut s = c.prepare("SELECT cid, seq, off, len FROM backup_blobs WHERE owner_hex = ?1 ORDER BY seq, off")?;
                let blobs = s.query_map([&o], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<Result<Vec<_>, _>>()?;
                let mut s = c.prepare("SELECT b.host_hex, b.cid FROM backup_items b WHERE b.owner_hex = ?1 \
                     AND EXISTS (SELECT 1 FROM ucloud_entries e WHERE e.owner_hex = b.owner_hex AND e.cid = b.cid AND e.type = 'file') ORDER BY b.host_hex, b.cid")?;
                let legacy = s.query_map([&o], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<Vec<_>, _>>()?;
                Ok((packets, frags, blobs, legacy))
            })
            .await
            .ok()?;
        if packets.is_empty() {
            return None;
        }
        let mut index = PackIndex::new(owner, 0);
        let mut pos = HashMap::new();
        let mut by_id: HashMap<String, Vec<FragEntry>> = HashMap::new();
        for (id, i, cid, host) in frags {
            by_id.entry(id).or_default().push(FragEntry { i: u8::try_from(i).unwrap_or(u8::MAX), cid, host: Some(host) });
        }
        for (n, (id, seq, key, len, k)) in packets.into_iter().enumerate() {
            pos.insert(seq, u32::try_from(n).unwrap_or(u32::MAX));
            let frags = by_id.remove(&id).unwrap_or_default();
            index.packets.push(PacketEntry { id, key, len: u64::try_from(len).unwrap_or(0), k: u8::try_from(k).unwrap_or(1), frags });
        }
        // Copies vérifiées chez des amis : raccourci à la restauration (`held_whole` : comptes des stations qui les tiennent).
        let fresh = self.fresh_copies(owner, super::now()).await;
        let stations = if fresh.is_empty() { BTreeMap::new() } else { self.friend_stations(owner).await };
        for (cid, seq, off, len) in blobs {
            let Some(&packet) = pos.get(&seq) else { continue };
            let held_whole = fresh.get(&cid).map(|(peers, _)| peers.iter().filter_map(|p| stations.get(p)).flatten().cloned().collect()).unwrap_or_default();
            index.blobs.push(backup_pack::BlobEntry { cid, packet, off: u64::try_from(off).unwrap_or(0), len: u64::try_from(len).unwrap_or(0), held_whole });
        }
        for (host, cid) in legacy {
            index.legacy.entry(host).or_default().push(cid);
        }
        Some(index)
    }

    /// Resscelle l'index s'il a changé (révision suivante, clé dérivée du secret du compte), le range dans le magasin et
    /// dans `backup_index` (le manifeste suivant le porte). Rend le CID courant, `None` s'il n'y a pas d'index.
    async fn seal_index(&self, owner: &str, at: i64) -> Option<String> {
        let mut index = self.current_index(owner).await?;
        let o = owner.to_owned();
        let current: Option<(i64, String)> = self.db.readers().read(move |c| Ok(c.query_row("SELECT rev, index_cid FROM backup_index WHERE owner_hex = ?1", [o], |r| Ok((r.get(0)?, r.get(1)?))).optional()?)).await.ok().flatten();
        let fp = hex::encode(Sha256::digest(serde_json::to_vec(&index).ok()?));
        if let Some((_, cid)) = &current
            && self.kv_get(INDEX_FP, owner).await.is_some_and(|(v, _)| v == fp)
        {
            return Some(cid.clone());
        }
        let secret = self.keys.secret(owner)?;
        index.rev = current.as_ref().map_or(0, |c| u64::try_from(c.0).unwrap_or(0)) + 1;
        let sealed = match index.seal(&backup_pack::index_key(&secret), Cipher::detect()).await {
            Ok(s) => s,
            Err(e) => {
                self.activity.publish_to(Audience::Captain, "alert", "backup", format!("index de sauvegarde non scellé ({e})"), None);
                return current.map(|c| c.1);
            }
        };
        let cid = match self.put_bytes(&sealed).await {
            Ok(cid) => cid,
            Err(e) => {
                self.activity.publish_to(Audience::Captain, "alert", "backup", format!("index de sauvegarde non rangé ({e})"), None);
                return current.map(|c| c.1);
            }
        };
        let (o, c, rev) = (owner.to_owned(), cid.clone(), i64::try_from(index.rev).unwrap_or(i64::MAX));
        let stored = self
            .db
            .writer()
            .execute(
                "INSERT INTO backup_index(owner_hex, rev, index_cid, at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(owner_hex) DO UPDATE SET rev = excluded.rev, index_cid = excluded.index_cid, at = excluded.at",
                vec![o.into(), rev.into(), c.into(), at.into()],
            )
            .await;
        if let Err(e) = stored {
            self.activity.publish_to(Audience::Captain, "alert", "backup", format!("index de sauvegarde non inscrit ({e})"), None);
            return current.map(|c| c.1);
        }
        self.kv_put(INDEX_FP, owner, &fp, at).await;
        if let Some((_, old)) = current.filter(|c| c.1 != cid) {
            let o2 = old.clone();
            let used = self.db.readers().read(move |c| astro_dav::vfs::cid_in_use(c, &o2).map_err(|_| astro_db::DbError::Aborted)).await.unwrap_or(true);
            if !used {
                let _ = self.blobs.remove(&old).await;
            }
        }
        if let Some(p) = &self.publisher {
            p.notify(owner); // le manifeste suivant porte le CID de l'index
        }
        Some(cid)
    }

    pub(super) async fn put_bytes(&self, bytes: &[u8]) -> Result<String, String> {
        let mut w = self.blobs.begin().await.map_err(|e| e.to_string())?;
        w.write_all(bytes).await.map_err(|e| e.to_string())?;
        Ok(w.commit().await.map_err(|e| e.to_string())?.cid)
    }

    /// Envoie l'index courant à chaque gardien `rs` d'une autre station qui ne l'a pas encore. Rend le nombre d'envois.
    async fn send_index(&self, owner: &str) -> usize {
        let o = owner.to_owned();
        let (cid, hosts): (Option<String>, Vec<String>) = self
            .db
            .readers()
            .read(move |c| {
                let cid = c.query_row("SELECT index_cid FROM backup_index WHERE owner_hex = ?1", [&o], |r| r.get(0)).optional()?;
                let mut s = c.prepare(
                    "SELECT p.host_hex FROM backup_pacts p JOIN kv k ON k.scope = ?2 AND k.key = p.owner_hex || ':' || p.host_hex \
                     WHERE p.owner_hex = ?1 AND p.status = 'accepted' AND NOT EXISTS (SELECT 1 FROM identities i WHERE i.hex = p.host_hex AND i.hosted = 1)",
                )?;
                let hosts = s.query_map(rusqlite::params![o, RS], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
                Ok((cid, hosts))
            })
            .await
            .unwrap_or_default();
        let Some(cid) = cid else { return 0 };
        let Ok((_, size)) = self.blobs.open(&cid).await else { return 0 };
        let mut sent = 0;
        for host in hosts {
            let key = format!("{owner}:{host}");
            if self.kv_get(INDEX_SENT, &key).await.is_some_and(|(v, _)| v == cid) {
                continue;
            }
            let mut tags = vec![vec!["cid".to_owned(), cid.clone()], vec!["size".into(), size.to_string()], vec!["op".into(), "frag".into()], vec!["kind".into(), "index".into()]];
            if let Some(peer) = &self.station_peer {
                tags.push(vec!["station".into(), peer.clone()]);
            }
            let result = self.send_to(owner, &host, kind::FILE, tags, &format!("ipfs://{cid}")).await;
            tokio::time::sleep(SEND_PACE).await;
            match result {
                Ok(()) => {
                    self.kv_put(INDEX_SENT, &key, &cid, super::now()).await;
                    sent += 1;
                }
                Err(e) => {
                    self.activity.publish_to(Audience::Captain, "alert", "backup", format!("index de sauvegarde non envoyé à …{} : {e}", &host[..8.min(host.len())]), None);
                }
            }
        }
        sent
    }

    // ───────────────────────────── Côté gardien ─────────────────────────────

    /// Un fragment (ou l'index) d'un ami à ranger : accord, quota en octets de fragment, taille plafonnée, un seul fragment
    /// par paquet ; le blob rapatrié est vérifié (en-tête ARSF du paquet et de l'indice annoncés, ou index chiffré UENC v2)
    /// avant d'être adopté sous `/Sauvegardes/De_<nom>/`, puis acquitté.
    pub(super) async fn store_frag(&self, me: &str, sender: &str, wrap_id: &str, rumor: &Rumor, at: i64, stats: &mut BackupStats) {
        let is_index = tag(&rumor.tags, "kind") == Some("index");
        let max = if is_index { MAX_INDEX_BYTES as u64 } else { MAX_FRAGMENT_BYTES };
        let packet = tag(&rumor.tags, "packet").filter(|p| p.len() == 32 && p.bytes().all(|b| b.is_ascii_hexdigit())).map(str::to_ascii_lowercase);
        let index_no = tag(&rumor.tags, "i").and_then(|i| i.parse::<u8>().ok());
        let (Some(cid), Some(size)) = (tag(&rumor.tags, "cid"), tag(&rumor.tags, "size").and_then(|s| s.parse::<u64>().ok())) else {
            self.claim(wrap_id, at).await;
            stats.ignored += 1;
            return;
        };
        let quota = self.pact_quota(sender, me).await;
        let used = self.used(sender, me).await;
        if !astro_dav::blob::is_cid(cid) || size > max || (!is_index && (packet.is_none() || index_no.is_none())) || used.saturating_add(size) > quota {
            self.claim(wrap_id, at).await; // hors accord, volume dépassé ou mal formé : refusé, sans bruit
            stats.ignored += 1;
            return;
        }
        if let (Some(packet), false) = (&packet, is_index) {
            let (s, m, p) = (sender.to_owned(), me.to_owned(), packet.clone());
            let held: Option<String> = self.db.readers().read(move |c| Ok(c.query_row("SELECT cid FROM backup_frags WHERE owner_hex = ?1 AND host_hex = ?2 AND packet_id = ?3", [s, m, p], |r| r.get(0)).optional()?)).await.ok().flatten();
            match held {
                // Rejeu ou renvoi : déjà rangé, on acquitte de nouveau, sans rien compter deux fois.
                Some(c) if c == cid => {
                    self.ack(me, sender, cid).await;
                    self.claim(wrap_id, at).await;
                    stats.control += 1;
                    return;
                }
                // Un second fragment du même paquet : une station n'en garde qu'un (Q9).
                Some(_) => {
                    self.claim(wrap_id, at).await;
                    stats.ignored += 1;
                    return;
                }
                None => {}
            }
        }
        // Débit (§ 6.3) : pas plus de NIGHT_FRAGS fragments rangés par 24 h et par propriétaire ; le reste attend (message
        // non réclamé, repris au passage suivant), sans rien rapatrier.
        if !is_index {
            let (s, m) = (sender.to_owned(), me.to_owned());
            let recent: i64 = self.db.readers().read(move |c| Ok(c.query_row("SELECT COUNT(*) FROM backup_frags WHERE owner_hex = ?1 AND host_hex = ?2 AND acked_at > ?3", rusqlite::params![s, m, at - 86_400], |r| r.get(0))?)).await.unwrap_or(i64::MAX);
            if recent >= NIGHT_FRAGS {
                stats.deferred += 1;
                return;
            }
        }
        let existed = self.blobs.open(cid).await.is_ok();
        if !self.ensure_blob(cid, rumor, wrap_id, at, stats).await {
            return;
        }
        // La forme et la taille sont celles du blob reçu, jamais celles annoncées.
        let verdict = self.check_received(cid, is_index, packet.as_deref(), index_no, max).await;
        let size = match verdict {
            Ok(real) if used.saturating_add(real) <= quota => real,
            other => {
                if !existed {
                    let _ = self.blobs.remove(cid).await; // seulement ce que ce message vient d'apporter
                }
                if let Err(why) = other {
                    self.activity.publish_to(Audience::Captain, "alert", "backup", format!("fragment de sauvegarde refusé : {why}"), None);
                }
                self.claim(wrap_id, at).await;
                stats.ignored += 1;
                return;
            }
        };
        let label = self.label_of(sender).await;
        if let Err(e) = adopt_backup_blob(&self.db, me, sender, &label, cid, size).await {
            self.activity.publish_to(Audience::Captain, "alert", "backup", format!("fragment d'un ami non rangé : {e}"), None);
            stats.deferred += 1;
            return;
        }
        if is_index {
            let _ = self.record_item(sender, me, cid, size).await;
            let key = format!("{me}:{sender}");
            if let Some((old, _)) = self.kv_get(INDEX_HELD, &key).await.filter(|(old, _)| old != cid) {
                self.drop_hosted(me, sender, Some(&[old])).await; // l'index précédent est remplacé
            }
            self.kv_put(INDEX_HELD, &key, cid, at).await;
        } else if let (Some(packet), Some(i)) = (packet, index_no) {
            let (s, m, c) = (sender.to_owned(), me.to_owned(), cid.to_owned());
            let recorded = self
                .db
                .writer()
                .execute(
                    "INSERT OR IGNORE INTO backup_frags(owner_hex, host_hex, packet_id, i, cid, size, sent_at, acked_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
                    vec![s.into(), m.into(), packet.into(), i64::from(i).into(), c.into(), i64::try_from(size).unwrap_or(0).into(), at.into()],
                )
                .await;
            if let Err(e) = recorded {
                self.activity.publish_to(Audience::Captain, "alert", "backup", format!("fragment d'un ami non inscrit : {e}"), None);
            }
        }
        self.ack(me, sender, cid).await;
        if let Some(p) = &self.publisher {
            p.notify(me);
        }
        self.claim(wrap_id, at).await;
        stats.stored += 1;
    }

    /// Accusé de réception d'un fragment rangé.
    async fn ack(&self, me: &str, owner: &str, cid: &str) {
        if let Err(e) = self.control(me, owner, "frag-ack", vec![vec!["cid".to_owned(), cid.to_owned()]]).await {
            self.activity.publish_to(Audience::Captain, "alert", "backup", format!("accusé de fragment non envoyé : {e}"), None);
        }
    }

    /// Le blob reçu est-il bien ce qu'annonce le message ? Rend sa taille réelle.
    async fn check_received(&self, cid: &str, is_index: bool, packet: Option<&str>, index_no: Option<u8>, max: u64) -> Result<u64, String> {
        let (mut reader, size) = self.blobs.open(cid).await.map_err(|e| e.to_string())?;
        if size > max {
            return Err("trop gros".into());
        }
        if is_index {
            let mut head = [0u8; 6];
            reader.read_exact(&mut head).await.map_err(|e| e.to_string())?;
            return if astro_dav::share::is_uenc_v2(&head) { Ok(size) } else { Err("index non chiffré".into()) };
        }
        let mut bytes = Vec::with_capacity(usize::try_from(size).unwrap_or(0));
        reader.read_to_end(&mut bytes).await.map_err(|e| e.to_string())?;
        let (h, _) = FragHeader::parse(&bytes).map_err(|e| format!("en-tête ARSF invalide ({e})"))?;
        if Some(hex::encode(h.packet_id()).as_str()) != packet || Some(h.index) != index_no {
            return Err("fragment d'un autre paquet ou d'un autre indice".into());
        }
        Ok(size)
    }

    /// Côté propriétaire : le gardien a rangé ces fragments ; leur copie locale (recalculable) est effacée.
    pub(super) async fn frag_ack(&self, me: &str, sender: &str, rumor: &Rumor, at: i64) {
        let cids: Vec<String> = rumor.tags.iter().filter(|t| t.first().map(String::as_str) == Some("cid")).filter_map(|t| t.get(1).cloned()).take(100).collect();
        let (m, s) = (me.to_owned(), sender.to_owned());
        let acked: Vec<String> = self
            .db
            .writer()
            .transaction(move |tx| {
                let mut done = Vec::new();
                for cid in cids {
                    if tx.execute("UPDATE backup_frags SET acked_at = ?4 WHERE owner_hex = ?1 AND host_hex = ?2 AND cid = ?3 AND acked_at IS NULL", rusqlite::params![m, s, cid, at])? > 0 {
                        done.push(cid);
                    }
                }
                Ok(done)
            })
            .await
            .unwrap_or_default();
        for cid in acked {
            let c = cid.clone();
            let used = self
                .db
                .readers()
                .read(move |conn| {
                    let vfs = astro_dav::vfs::cid_in_use(conn, &c).map_err(|_| astro_db::DbError::Aborted)?;
                    let index = conn.query_row("SELECT 1 FROM backup_index WHERE index_cid = ?1", [&c], |_| Ok(())).optional()?.is_some();
                    Ok(vfs || index)
                })
                .await
                .unwrap_or(true);
            if !used {
                let _ = self.blobs.remove(&cid).await;
            }
        }
    }

    /// Côté gardien : le propriétaire retire des fragments (compactage, réparation, migration).
    pub(super) async fn frag_drop(&self, me: &str, sender: &str, rumor: &Rumor) {
        let cids: Vec<String> = rumor.tags.iter().filter(|t| t.first().map(String::as_str) == Some("cid")).filter_map(|t| t.get(1).cloned()).take(100).collect();
        if self.pact_quota(sender, me).await > 0 {
            self.drop_hosted(me, sender, Some(&cids)).await;
        }
    }

    // ───────────────────────────── Restauration ─────────────────────────────

    /// Une restauration attend des fragments : un index ou un fragment vient d'arriver, le tirage reprend sans attendre.
    pub(super) async fn rs_dirty(&self, owner: &str, at: i64) {
        let mut st = self.kv_get(RESTORE_RS, owner).await.and_then(|(v, _)| serde_json::from_str::<Value>(&v).ok()).unwrap_or_else(|| json!({}));
        st["dirty"] = json!(true);
        self.kv_put(RESTORE_RS, owner, &st.to_string(), at).await;
    }

    /// Station (PeerID) d'un gardien : sa station d'accueil (DID), sinon celle vue sur ses renvois.
    pub(super) async fn peer_of(&self, host: &str) -> Option<String> {
        let h = host.to_owned();
        let home: Option<String> = self.db.readers().read(move |c| Ok(c.query_row("SELECT home_station FROM identities WHERE hex = ?1", [h], |r| r.get::<_, Option<String>>(0)).optional()?.flatten())).await.ok().flatten();
        if let Some(home) = home.filter(|h| valid_home(h)) {
            return home.split(':').next().map(str::to_owned);
        }
        self.kv_get(PEER, host).await.map(|(v, _)| v)
    }

    /// Plus récent index qui se déchiffre parmi ceux renvoyés par les gardiens (et celui connu ici) : `(CID, index)`.
    async fn best_index(&self, owner: &str) -> Option<(String, PackIndex)> {
        let o = owner.to_owned();
        let cids: Vec<String> = self
            .db
            .readers()
            .read(move |c| {
                let mut s = c.prepare("SELECT value FROM kv WHERE scope = ?1 AND key LIKE ?2 || ':%' UNION SELECT index_cid FROM backup_index WHERE owner_hex = ?2")?;
                Ok(s.query_map(rusqlite::params![RESTORE_INDEX, o], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .unwrap_or_default();
        let secret = self.keys.secret(owner)?;
        let key = backup_pack::index_key(&secret);
        let mut best: Option<(String, PackIndex)> = None;
        for cid in cids {
            let Ok((mut reader, size)) = self.blobs.open(&cid).await else { continue };
            if size > MAX_INDEX_BYTES as u64 {
                continue;
            }
            let mut bytes = Vec::new();
            if reader.read_to_end(&mut bytes).await.is_err() {
                continue;
            }
            // Un index forgé (il ne se déchiffre pas) ou d'un autre compte est ignoré ; on garde la plus haute révision.
            if let Ok(index) = PackIndex::open(&bytes, &key, owner).await
                && best.as_ref().is_none_or(|(_, b)| index.rev > b.rev)
            {
                best = Some((cid, index));
            }
        }
        best
    }

    /// Reconstruit les tables locales depuis l'index (idempotent) : les blobs déjà en paquets ne repartiront pas, les
    /// fragments tenus par les gardiens sont connus (et comptés dans leur quota), le prochain index prend la suite.
    async fn import_index(&self, owner: &str, cid: &str, index: &PackIndex, at: i64) {
        let (o, c, index) = (owner.to_owned(), cid.to_owned(), index.clone());
        let hosts: Vec<String> = index.packets.iter().flat_map(|p| p.frags.iter().filter_map(|f| f.host.clone())).collect::<HashSet<_>>().into_iter().collect();
        let done = self
            .db
            .writer()
            .transaction(move |tx| {
                for (j, p) in index.packets.iter().enumerate() {
                    tx.execute(
                        "INSERT OR IGNORE INTO backup_packets(owner_hex, packet_id, seq, key_hex, len, k, at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                        rusqlite::params![o, p.id, i64::try_from(j).unwrap_or(i64::MAX), p.key, i64::try_from(p.len).unwrap_or(0), p.k, at],
                    )?;
                    let size = FragHeader::frag_len_for(p.len, p.k).map_or(0, |l| l + FRAG_HEADER_LEN as u64);
                    for f in &p.frags {
                        let Some(host) = &f.host else { continue };
                        tx.execute(
                            "INSERT OR IGNORE INTO backup_frags(owner_hex, host_hex, packet_id, i, cid, size, sent_at, acked_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
                            rusqlite::params![o, host, p.id, f.i, f.cid, i64::try_from(size).unwrap_or(0), at],
                        )?;
                    }
                }
                for b in &index.blobs {
                    tx.execute(
                        "INSERT OR IGNORE INTO backup_blobs(owner_hex, cid, seq, off, len, at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        rusqlite::params![o, b.cid, b.packet, i64::try_from(b.off).unwrap_or(0), i64::try_from(b.len).unwrap_or(0), at],
                    )?;
                }
                tx.execute(
                    "INSERT INTO backup_index(owner_hex, rev, index_cid, at) VALUES (?1, ?2, ?3, ?4) \
                     ON CONFLICT(owner_hex) DO UPDATE SET rev = excluded.rev, index_cid = excluded.index_cid, at = excluded.at WHERE excluded.rev > backup_index.rev",
                    rusqlite::params![o, i64::try_from(index.rev).unwrap_or(i64::MAX), c, at],
                )?;
                Ok(())
            })
            .await;
        if let Err(e) = done {
            self.activity.publish_to(Audience::Captain, "alert", "backup", format!("restauration : index des paquets non repris ({e})"), None);
            return;
        }
        // Ces gardiens tiennent des fragments : ils savent le codage k-sur-n.
        for host in hosts {
            self.kv_put(RS, &format!("{owner}:{host}"), "1", at).await;
        }
    }

    /// Tire et décode les paquets dont des blobs manquent ici. Rend le nombre de blobs revenus.
    pub(super) async fn restore_packets(&self, owner: &str, at: i64) -> usize {
        let mut st = self.kv_get(RESTORE_RS, owner).await.and_then(|(v, _)| serde_json::from_str::<Value>(&v).ok()).unwrap_or_else(|| json!({}));
        if matches!(st["state"].as_str(), Some("done" | "abandoned")) {
            return 0;
        }
        let late = self.kv_get("restore", owner).await.is_some_and(|(s, _)| s == "late");
        let last = st["last"].as_i64().unwrap_or(0);
        let wait = if late { RESTORE_LATE_RETRY_SECS } else { RESTORE_RETRY_SECS };
        if !st["dirty"].as_bool().unwrap_or(false) && last > 0 && at - last < wait {
            return 0;
        }
        let Some((cid, index)) = self.best_index(owner).await else { return 0 };
        self.import_index(owner, &cid, &index, at).await;
        let (restored, missing, unfinished) = self.pull_packets(owner, &index).await;
        st["last"] = json!(at);
        st["dirty"] = json!(false);
        if unfinished == 0 {
            st["state"] = json!("done");
            if restored > 0 {
                self.tell(owner, format!("✅ {restored} fichier(s) reconstitué(s) depuis les parts gardées par vos amis.")).await;
            }
        } else {
            let first = st["first"].as_i64().unwrap_or(at);
            st["first"] = json!(first);
            st["rounds"] = json!(st["rounds"].as_u64().unwrap_or(0) + 1);
            if at - first > RESTORE_LATE_SECS {
                st["state"] = json!("abandoned");
                self.activity.publish_to(Audience::Captain, "alert", "backup", format!("restauration de …{} : {unfinished} paquet(s) introuvable(s) chez assez de gardiens après 30 jours", &owner[..8.min(owner.len())]), None);
            } else {
                // Repli : chaque gardien injoignable par le maillage est prié de renvoyer lui-même ses fragments.
                for (host, cids) in missing {
                    let tags: Vec<Vec<String>> = cids.into_iter().take(RESTORE_WANT).map(|c| vec!["cid".to_owned(), c]).collect();
                    if let Err(e) = self.control(owner, &host, "restore", tags).await {
                        self.activity.publish_to(Audience::Captain, "alert", "backup", format!("restauration : fragments non redemandés à …{} ({e})", &host[..8.min(host.len())]), None);
                    }
                }
                self.activity.publish_to(Audience::Captain, "alert", "backup", format!("restauration de …{} : {unfinished} paquet(s) en attente de gardiens, nouvel essai plus tard", &owner[..8.min(owner.len())]), None);
            }
        }
        self.kv_put(RESTORE_RS, owner, &st.to_string(), at).await;
        restored
    }

    /// Tire les paquets nécessaires, par suites de paquets liés par un blob étalé, les suites des blobs les plus récents
    /// d'abord. Rend `(blobs revenus, fragments manquants par gardien, paquets non décodés)`.
    async fn pull_packets(&self, owner: &str, index: &PackIndex) -> (usize, BTreeMap<String, Vec<String>>, usize) {
        // Raccourci : un blob tenu en entier par une station amie (`held_whole`) revient directement, vérifié par son CID.
        let whole = self.pull_whole(index).await;
        let spans = spans(index);
        let count = index.packets.len();
        let (mut need, mut link) = (vec![false; count], vec![false; count]);
        for (b, &(start, end)) in index.blobs.iter().zip(&spans) {
            if self.blobs.open(&b.cid).await.is_err() {
                for flag in need.iter_mut().take(end + 1).skip(start) {
                    *flag = true;
                }
            }
            for flag in link.iter_mut().take(end).skip(start) {
                *flag = true;
            }
        }
        // Suites de paquets : un blob qui déborde lie un paquet au suivant.
        let mut runs: Vec<Vec<usize>> = Vec::new();
        for p in 0..count {
            match runs.last_mut() {
                Some(run) if run.last().is_some_and(|&q| link[q]) => run.push(p),
                _ => runs.push(vec![p]),
            }
        }
        runs.retain(|run| run.iter().any(|&p| need[p]));
        let o = owner.to_owned();
        let mtimes: HashMap<String, i64> = self
            .db
            .readers()
            .read(move |c| {
                let mut s = c.prepare("SELECT cid, MAX(COALESCE(mtime, 0)) FROM ucloud_entries WHERE owner_hex = ?1 AND type = 'file' AND cid IS NOT NULL GROUP BY cid")?;
                Ok(s.query_map([o], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?.collect::<Result<HashMap<_, _>, _>>()?)
            })
            .await
            .unwrap_or_default();
        let recency = |run: &[usize]| index.blobs.iter().filter(|b| run.contains(&(b.packet as usize))).map(|b| mtimes.get(&b.cid).copied().unwrap_or(0)).max().unwrap_or(0);
        runs.sort_by_key(|run| (std::cmp::Reverse(recency(run)), std::cmp::Reverse(run[0])));
        let hosts: Vec<String> = index.packets.iter().flat_map(|p| p.frags.iter().filter_map(|f| f.host.clone())).collect::<HashSet<_>>().into_iter().collect();
        let today = super::now() / 86_400;
        let mut avail = HashMap::new();
        for h in &hosts {
            let h2 = h.clone();
            let seen: i64 = self.db.readers().read(move |c| Ok(c.query_row("SELECT COUNT(*) FROM guardian_presence WHERE hex = ?1 AND day > ?2", rusqlite::params![h2, today - crate::guardians::WINDOW_DAYS], |r| r.get(0))?)).await.unwrap_or(0);
            avail.insert(h.clone(), seen);
        }
        let mut probes: HashMap<String, Option<HashSet<String>>> = HashMap::new();
        let (mut restored, mut missing, mut unfinished) = (whole, BTreeMap::new(), 0);
        for run in runs {
            let mut ex = Extractor::new(index);
            let (mut writers, mut skip) = (HashMap::new(), HashSet::new());
            let mut whole = true;
            for &p in &run {
                let Some(plain) = self.fetch_packet(index, &index.packets[p], &avail, &mut probes, &mut missing).await else {
                    whole = false;
                    break;
                };
                let mut out = BufOut::default();
                if let Err(e) = ex.feed(u32::try_from(p).unwrap_or(u32::MAX), &plain, &mut out) {
                    self.activity.publish_to(Audience::Captain, "alert", "backup", format!("restauration : paquet illisible ({e})"), None);
                    whole = false;
                    break;
                }
                restored += self.replay(out.0, &mut writers, &mut skip).await;
            }
            if whole {
                let mut out = BufOut::default();
                if ex.finish(&mut out).is_ok() {
                    restored += self.replay(out.0, &mut writers, &mut skip).await;
                }
            } else {
                unfinished += run.iter().filter(|&&p| need[p]).count();
            }
        }
        (restored, missing, unfinished)
    }

    /// Rapatrie directement les blobs absents d'ici qu'une station amie tient en entier (`held_whole` de l'index), sans
    /// décoder de paquet. Le rapatriement vérifie le CID. Rend le nombre de blobs revenus.
    pub(super) async fn pull_whole(&self, index: &PackIndex) -> usize {
        let Some(prefetch) = &self.prefetch else { return 0 };
        let mut back = 0;
        let mut dead: HashSet<String> = HashSet::new();
        for b in index.blobs.iter().filter(|b| !b.held_whole.is_empty()) {
            if self.blobs.open(&b.cid).await.is_ok() {
                continue;
            }
            for holder in &b.held_whole {
                let Some(peer) = self.peer_of(holder).await else { continue };
                if dead.contains(&peer) {
                    continue;
                }
                if prefetch(b.cid.clone(), peer.clone()).await.is_ok() && self.blobs.open(&b.cid).await.is_ok() {
                    back += 1;
                    break;
                }
                dead.insert(peer); // station injoignable ou sans ce blob : les paquets prendront le relais
            }
        }
        back
    }

    /// Écrit dans le magasin ce que l'extracteur a rendu ; un blob n'entre que si son CID est juste. Rend le nombre de blobs.
    async fn replay(&self, events: Vec<Ev>, writers: &mut HashMap<String, B::Writer>, skip: &mut HashSet<String>) -> usize {
        let mut done = 0;
        for ev in events {
            match ev {
                Ev::Write(cid, bytes) => {
                    if skip.contains(&cid) {
                        continue;
                    }
                    if !writers.contains_key(&cid) {
                        if self.blobs.open(&cid).await.is_ok() {
                            skip.insert(cid); // déjà là (copie complète, partage) : rien à écrire
                            continue;
                        }
                        match self.blobs.begin().await {
                            Ok(w) => {
                                writers.insert(cid.clone(), w);
                            }
                            Err(e) => {
                                self.activity.publish_to(Audience::Captain, "alert", "backup", format!("restauration : blob non écrit ({e})"), None);
                                skip.insert(cid);
                                continue;
                            }
                        }
                    }
                    if let Some(w) = writers.get_mut(&cid)
                        && w.write_all(&bytes).await.is_err()
                    {
                        writers.remove(&cid);
                        skip.insert(cid);
                    }
                }
                Ev::Done(cid, ok) => {
                    if skip.remove(&cid) {
                        continue;
                    }
                    let Some(w) = writers.remove(&cid) else { continue };
                    if !ok {
                        continue; // empreinte fausse : l'écrivain est abandonné sans rien publier
                    }
                    match w.commit().await {
                        Ok(meta) if meta.cid == cid => done += 1,
                        Ok(meta) => {
                            let _ = self.blobs.remove(&meta.cid).await;
                            self.activity.publish_to(Audience::Captain, "alert", "backup", format!("restauration : blob {cid} écarté (CID faux)"), None);
                        }
                        Err(e) => {
                            self.activity.publish_to(Audience::Captain, "alert", "backup", format!("restauration : blob non rangé ({e})"), None);
                        }
                    }
                }
            }
        }
        done
    }

    /// `k` fragments valides d'un paquet → son clair. Ordre : fragments déjà ici, puis données (sans calcul si les `k`
    /// données sont là), puis gardiens les plus présents. Un fragment absent ou invalide ⇒ on en prend un autre. En cas
    /// d'échec, les fragments non obtenus sont notés dans `missing` (repli : renvoi poussé par le gardien).
    async fn fetch_packet(&self, index: &PackIndex, entry: &PacketEntry, avail: &HashMap<String, i64>, probes: &mut HashMap<String, Option<HashSet<String>>>, missing: &mut BTreeMap<String, Vec<String>>) -> Option<Vec<u8>> {
        let k = usize::from(entry.k);
        let mut order = Vec::new();
        for f in &entry.frags {
            let local = self.blobs.open(&f.cid).await.is_ok();
            order.push((f, local));
        }
        order.sort_by_key(|(f, local)| (!*local, f.i >= entry.k, std::cmp::Reverse(f.host.as_ref().and_then(|h| avail.get(h)).copied().unwrap_or(0))));
        let (mut got, mut lacking): (Vec<Vec<u8>>, Vec<&FragEntry>) = (Vec::new(), Vec::new());
        for (f, local) in order {
            if got.len() >= k {
                break;
            }
            if !local && !self.pull_fragment(index, f, probes).await {
                lacking.push(f);
                continue;
            }
            match self.read_fragment(&f.cid).await {
                Some(bytes) if FragHeader::parse(&bytes).is_ok_and(|(h, _)| h.index == f.i) => got.push(bytes),
                _ => {
                    let _ = self.blobs.remove(&f.cid).await;
                    lacking.push(f);
                }
            }
        }
        if got.len() < k {
            for f in lacking {
                if let Some(h) = &f.host {
                    missing.entry(h.clone()).or_default().push(f.cid.clone());
                }
            }
            return None;
        }
        let refs: Vec<&[u8]> = got.iter().map(Vec::as_slice).collect();
        match backup_pack::open_packet(entry, &refs).await {
            Ok(plain) => {
                for f in &entry.frags {
                    let _ = self.blobs.remove(&f.cid).await; // fragments tirés : inutiles une fois le paquet décodé
                }
                Some(plain)
            }
            Err(e) => {
                self.activity.publish_to(Audience::Captain, "alert", "backup", format!("restauration : paquet {} non décodé ({e})", entry.id), None);
                None
            }
        }
    }

    /// Rapatrie un fragment depuis la station de son gardien (sondée une fois par passage). `true` s'il est arrivé.
    async fn pull_fragment(&self, index: &PackIndex, f: &FragEntry, probes: &mut HashMap<String, Option<HashSet<String>>>) -> bool {
        let (Some(host), Some(prefetch)) = (&f.host, &self.prefetch) else { return false };
        let Some(peer) = self.peer_of(host).await else { return false };
        if let Some(probe) = &self.probe {
            if !probes.contains_key(&peer) {
                let cids: Vec<String> = index.packets.iter().flat_map(|p| p.frags.iter()).filter(|g| g.host.as_ref() == Some(host)).map(|g| g.cid.clone()).take(astro_p2p::blob::MAX_HAS_MANY).collect();
                let seen = probe(cids.clone(), peer.clone()).await.ok().map(|bits| cids.into_iter().zip(bits).filter(|(_, b)| *b).map(|(c, _)| c).collect());
                probes.insert(peer.clone(), seen);
            }
            match probes.get(&peer) {
                Some(None) => return false, // station injoignable : inutile d'insister pendant ce passage
                Some(Some(seen)) if !seen.contains(&f.cid) && seen.len() < astro_p2p::blob::MAX_HAS_MANY => return false,
                _ => {}
            }
        }
        prefetch(f.cid.clone(), peer).await.is_ok() && self.blobs.open(&f.cid).await.is_ok()
    }

    async fn read_fragment(&self, cid: &str) -> Option<Vec<u8>> {
        let (mut reader, size) = self.blobs.open(cid).await.ok()?;
        if size > MAX_FRAGMENT_BYTES {
            return None;
        }
        let mut bytes = Vec::with_capacity(usize::try_from(size).unwrap_or(0));
        reader.read_to_end(&mut bytes).await.ok()?;
        Some(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn k_follows_the_measured_availability_and_the_target() {
        assert_eq!(choose_k(&[], 999), 1);
        assert_eq!(choose_k(&[900], 999), 1, "un gardien : copie complète");
        assert_eq!(choose_k(&[900, 900], 999), 1, "deux gardiens : miroir");
        assert_eq!(choose_k(&[900; 5], 999), 2);
        assert_eq!(choose_k(&[966; 5], 999), 3, "présents 29 jours sur 30 : 3 sur 5");
        assert_eq!(choose_k(&[999; 64], 999), 32, "jamais plus de 32");
        assert_eq!(choose_k(&[500; 3], 999), 1);
    }

    #[test]
    fn a_guardian_without_room_for_his_fragment_is_left_out_and_k_recomputed() {
        let g = |hex: &str, room: u64| Keeper { hex: hex.into(), station: hex.into(), avail: 966, room };
        let one = frag_bytes(1000, 3);
        assert!(one > 72 && one < 1 << 20);
        let (kept, k) = select(vec![g("a", 1 << 30), g("b", 1 << 30), g("c", 1 << 30), g("d", 1 << 30), g("e", 1 << 30)], 1000);
        assert_eq!((kept.len(), k), (5, 3));
        let (kept, k) = select(vec![g("a", 1 << 30), g("b", 1 << 30), g("c", 10)], 1000);
        assert_eq!((kept.iter().map(|g| g.hex.as_str()).collect::<Vec<_>>(), k), (vec!["a", "b"], 1));
        assert!(frag_bytes(PAYLOAD as u64 + 1, 1) > PAYLOAD as u64, "un paquet plein et un bout");
    }
}
