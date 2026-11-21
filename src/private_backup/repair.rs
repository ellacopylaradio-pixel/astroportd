//! Réparation nocturne de la sauvegarde k-sur-n (lot 5, `docs/design/SAUVEGARDE_RS.md` § 5.4 et § 5.5), côté
//! propriétaire, à 20h12 ([`PrivateBackup::repair_night`], appelé par [`PrivateBackup::sync_night`]).
//!
//! 1. **Ramassage** des fragments orphelins : écrits dans le magasin mais jamais inscrits (coupure en plein encodage,
//!    journal `kv` [`super::rs::SPOOL`]), ou acquittés mais encore là (coupure entre l'accusé et l'effacement). Idempotent.
//! 2. **Présence** : une sonde `HASN` groupée par station gardienne. Oui : `seen_at`. Non, d'une station qui répond : le
//!    fragment est perdu. Station muette : rien.
//! 3. **Possession** (échantillon d'environ 1 % des fragments par nuit) : 4 Kio lus à un décalage tiré au hasard, comparés
//!    au fragment **recalculé ici** depuis les blobs du membre ([`astro_dav::backup_pack::refragment`]). Une plage fausse
//!    fait un suspect : une autre plage est demandée aussitôt (ou une autre nuit si la station ne répond plus). Deux
//!    réponses fausses : fragment perdu, disponibilité du gardien remise à zéro sur la fenêtre, `frag-drop`, Capitaine
//!    prévenu. Une erreur de transport n'est jamais une preuve.
//! 4. **Absence** : un gardien sans aucun signe de vie (sonde, accusé, présence observée) depuis [`REPAIR_AFTER`] = 14 jours
//!    (Q6) perd ses fragments ; une absence plus courte ne déclenche **rien** (pas de tempête de réparation après une
//!    coupure de courant). Accord retiré ou refusé : perdu tout de suite.
//! 5. **Réparation et extension** : chaque gardien `rs` retenu (un par station) qui ne tient rien d'un paquet reçoit, dans
//!    son quota, un fragment d'**indice neuf** recalculé depuis les blobs locaux — c'est à la fois la réparation d'un
//!    fragment perdu et l'arrivée d'un nouveau gardien (§ 5.5), au plus [`NEW_FRAGS_PER_NIGHT`] par membre et par nuit.
//!    L'envoi et l'attente du `frag-ack` sont ceux de l'envoi ordinaire (`send_frags`).
//! 6. **Alerte** au membre quand un paquet qui a perdu un fragment n'a plus que `k` fragments sûrs (plus aucune marge).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use astro_core::Audience;
use astro_dav::backup_pack::{self, FRAG_HEADER_LEN, FragHeader, MAX_FRAG_INDEX, PacketEntry};
use astro_dav::{BlobStore, BlobWriter};
use astro_facecloud::Outbox;
use rusqlite::OptionalExtension;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::PrivateBackup;
use super::rs::{Journal, SPOOL, journal};

/// Q6 : un gardien muet depuis 14 jours est tenu pour perdu ; jamais avant.
pub const REPAIR_AFTER: i64 = 14 * 86_400;
/// Part des fragments vérifiés par nuit (‰) : 10 ‰ = 1 %, au moins un.
const SAMPLE_PERMILLE: i64 = 10;
/// Octets lus par contrôle de possession.
const SAMPLE_LEN: u64 = 4096;
/// Fragments recalculés au plus par membre et par nuit (1 Gio au pire) : la réparation et l'arrivée d'un gardien passent
/// au fil des nuits, sans saturer la liaison de la station.
const NEW_FRAGS_PER_NIGHT: usize = 64;
/// kv : alerte « plus de marge » déjà donnée pour `<owner>:<packet>`.
const ALERTED: &str = "backup_rs_alert";
/// kv : fragment `<owner>:<host>:<cid>` qui a servi une plage fausse sans être encore condamné (contrôle à finir).
pub(super) const SUSPECT: &str = "backup_rs_suspect";

/// Réponse à une lecture de plage.
enum Range {
    /// Octets justes.
    Good,
    /// La station répond, mais faux (ou ne sert pas la plage).
    Bad,
    /// Pas de réponse : rien à en conclure.
    Down,
}

/// Lit une plage tirée au hasard du fragment chez `peer` et la compare au fragment recalculé ici.
async fn check_range(sample: &Sample, cid: &str, peer: &str, expected: &[u8]) -> Range {
    let size = expected.len() as u64;
    let len = SAMPLE_LEN.min(size);
    let off = random_below(size - len + 1);
    let Ok(answer) = sample(cid.to_owned(), peer.to_owned(), off, len, size).await else { return Range::Down };
    let start = usize::try_from(off).unwrap_or(usize::MAX);
    let good = answer.as_deref().is_some_and(|b| b.len() as u64 == len && expected.get(start..start + b.len()) == Some(b));
    if good { Range::Good } else { Range::Bad }
}

/// Lecture d'une plage d'un blob chez une station : `(CID, PeerID, décalage, longueur, taille totale)`. `Ok(None)` : la
/// station a répondu mais ne sert pas cette plage ; `Err` : pas de réponse (rien à en conclure).
pub type Sample = Arc<dyn Fn(String, String, u64, u64, u64) -> Pin<Box<dyn Future<Output = Result<Option<Vec<u8>>, String>> + Send>> + Send + Sync>;

/// Bilan d'une nuit de réparation.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RepairStats {
    /// Fragments trouvés par la sonde de présence.
    pub seen: usize,
    /// Fragments contrôlés octet pour octet.
    pub checked: usize,
    /// Fragments tenus pour perdus cette nuit (toutes causes).
    pub lost: usize,
    /// Dont : mauvaise réponse au contrôle de possession.
    pub liars: usize,
    /// Fragments recalculés et confiés à un gardien.
    pub rebuilt: usize,
    /// Alertes « plus de marge » données au membre.
    pub alerts: usize,
    /// Blobs orphelins effacés du magasin.
    pub orphans: usize,
}

/// Un fragment côté propriétaire.
#[derive(Debug, Clone)]
struct Frag {
    host: String,
    packet: String,
    i: u8,
    cid: String,
}

impl<O: Outbox, B: BlobStore> PrivateBackup<O, B> {
    /// Contrôle de possession : lecture d'une plage d'un fragment chez la station de son gardien.
    pub fn with_sample(mut self, sample: Sample) -> Self {
        self.sample = Some(sample);
        self
    }

    /// La nuit de réparation (20h12) pour chaque membre hébergé ici qui a des paquets.
    pub async fn repair_night(&self, at: i64) -> RepairStats {
        let mut stats = RepairStats { orphans: self.collect_orphans().await, ..RepairStats::default() };
        let owners: Vec<String> = self
            .db
            .readers()
            .read(|c| {
                let mut s = c.prepare("SELECT DISTINCT p.owner_hex FROM backup_packets p JOIN identities i ON i.hex = p.owner_hex AND i.hosted = 1 ORDER BY 1")?;
                Ok(s.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .unwrap_or_default();
        for owner in owners {
            self.repair_owner(&owner, at, &mut stats).await;
        }
        stats
    }

    async fn repair_owner(&self, owner: &str, at: i64, stats: &mut RepairStats) {
        // Accord retiré ou refusé : ses fragments ne comptent plus (le gardien les a déjà retirés au `revoke`).
        let o = owner.to_owned();
        let revoked = self
            .db
            .writer()
            .execute(
                "UPDATE backup_frags SET lost_at = ?2 WHERE owner_hex = ?1 AND lost_at IS NULL \
                 AND NOT EXISTS (SELECT 1 FROM backup_pacts p WHERE p.owner_hex = backup_frags.owner_hex AND p.host_hex = backup_frags.host_hex AND p.status = 'accepted')",
                vec![o.into(), at.into()],
            )
            .await
            .map_or(0, |w| w.changes);
        stats.lost += revoked;
        let unreachable = self.presence(owner, at, stats).await;
        self.possession(owner, at, &unreachable, stats).await;
        self.absence(owner, at, stats).await;
        self.extend(owner, at, stats).await;
        self.margins(owner, at, stats).await;
    }

    /// Fragments acquittés et pas perdus.
    async fn safe_frags(&self, owner: &str) -> Vec<Frag> {
        let o = owner.to_owned();
        self.db
            .readers()
            .read(move |c| {
                let mut s = c.prepare("SELECT host_hex, packet_id, i, cid FROM backup_frags WHERE owner_hex = ?1 AND acked_at IS NOT NULL AND lost_at IS NULL ORDER BY host_hex, packet_id")?;
                Ok(s.query_map([o], |r| Ok(Frag { host: r.get(0)?, packet: r.get(1)?, i: u8::try_from(r.get::<_, i64>(2)?).unwrap_or(u8::MAX), cid: r.get(3)? }))?.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .unwrap_or_default()
    }

    /// Station (PeerID) de chaque gardien, à défaut sa clé.
    async fn stations_of(&self, hosts: Vec<String>) -> HashMap<String, String> {
        let mut out = HashMap::new();
        for h in hosts {
            if let std::collections::hash_map::Entry::Vacant(e) = out.entry(h) {
                let peer = self.peer_of(e.key()).await.unwrap_or_else(|| e.key().clone());
                e.insert(peer);
            }
        }
        out
    }

    /// Sonde `HASN` groupée par station. Rend les stations injoignables cette nuit.
    async fn presence(&self, owner: &str, at: i64, stats: &mut RepairStats) -> HashSet<String> {
        let mut unreachable = HashSet::new();
        let Some(probe) = self.probe.clone() else { return unreachable };
        let frags = self.safe_frags(owner).await;
        let stations = self.stations_of(frags.iter().map(|f| f.host.clone()).collect()).await;
        let mut by_peer: BTreeMap<String, Vec<Frag>> = BTreeMap::new();
        for f in frags {
            if let Some(p) = stations.get(&f.host) {
                by_peer.entry(p.clone()).or_default().push(f);
            }
        }
        for (peer, frags) in by_peer {
            for chunk in frags.chunks(astro_p2p::blob::MAX_HAS_MANY) {
                let Ok(bits) = probe(chunk.iter().map(|f| f.cid.clone()).collect(), peer.clone()).await else {
                    unreachable.insert(peer.clone());
                    break;
                };
                let (mut yes, mut no) = (Vec::new(), Vec::new());
                for (f, has) in chunk.iter().zip(bits) {
                    if has { yes.push(f.clone()) } else { no.push(f.clone()) }
                }
                stats.seen += yes.len();
                let o = owner.to_owned();
                let (y, n) = (yes.iter().map(|f| (f.host.clone(), f.cid.clone())).collect::<Vec<_>>(), no.iter().map(|f| (f.host.clone(), f.cid.clone())).collect::<Vec<_>>());
                let noted = self
                    .db
                    .writer()
                    .transaction(move |tx| {
                        for (h, c) in &y {
                            tx.execute("UPDATE backup_frags SET seen_at = ?4 WHERE owner_hex = ?1 AND host_hex = ?2 AND cid = ?3", rusqlite::params![o, h, c, at])?;
                        }
                        // Absent de chez un gardien qui répond : perdu.
                        for (h, c) in &n {
                            tx.execute("UPDATE backup_frags SET lost_at = ?4 WHERE owner_hex = ?1 AND host_hex = ?2 AND cid = ?3 AND lost_at IS NULL", rusqlite::params![o, h, c, at])?;
                        }
                        Ok(())
                    })
                    .await;
                if let Err(e) = noted {
                    self.activity.publish_to(Audience::Captain, "alert", "backup", format!("sauvegarde k-sur-n : présence des fragments non notée ({e})"), None);
                } else if !no.is_empty() {
                    stats.lost += no.len();
                    self.activity.publish_to(Audience::Captain, "alert", "backup", format!("sauvegarde k-sur-n de …{} : {} fragment(s) introuvable(s) chez la station {peer}, refaits ailleurs", short(owner), no.len()), None);
                }
            }
        }
        unreachable
    }

    /// Contrôle de possession par échantillon : ~1 % des fragments sûrs, 4 Kio à un décalage tiré au hasard, plus les
    /// fragments restés **suspects** d'une nuit précédente. Une plage fausse ne condamne pas : une autre plage, tirée au
    /// hasard, est demandée aussitôt ; seules deux réponses fausses font un menteur. Une erreur de transport n'est jamais
    /// une preuve : le fragment reste suspect et sera repris une autre nuit.
    async fn possession(&self, owner: &str, at: i64, unreachable: &HashSet<String>, stats: &mut RepairStats) {
        let Some(sample) = self.sample.clone() else { return };
        let o = owner.to_owned();
        let picked: Vec<Frag> = self
            .db
            .readers()
            .read(move |c| {
                let total: i64 = c.query_row("SELECT COUNT(*) FROM backup_frags WHERE owner_hex = ?1 AND acked_at IS NOT NULL AND lost_at IS NULL", [&o], |r| r.get(0))?;
                let n = (total * SAMPLE_PERMILLE + 999) / 1000;
                // Les suspects d'abord (contrôle à finir), puis l'échantillon du hasard.
                let mut s = c.prepare(
                    "SELECT host_hex, packet_id, i, cid FROM backup_frags f WHERE owner_hex = ?1 AND acked_at IS NOT NULL AND lost_at IS NULL \
                     ORDER BY EXISTS (SELECT 1 FROM kv WHERE kv.scope = ?3 AND kv.key = f.owner_hex || ':' || f.host_hex || ':' || f.cid) DESC, random() \
                     LIMIT ?2 + (SELECT COUNT(*) FROM kv WHERE kv.scope = ?3 AND kv.key LIKE ?1 || ':%')",
                )?;
                Ok(s.query_map(rusqlite::params![o, n, SUSPECT], |r| Ok(Frag { host: r.get(0)?, packet: r.get(1)?, i: u8::try_from(r.get::<_, i64>(2)?).unwrap_or(u8::MAX), cid: r.get(3)? }))?.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .unwrap_or_default();
        for f in picked {
            let Some(peer) = self.peer_of(&f.host).await else { continue };
            if unreachable.contains(&peer) {
                continue;
            }
            let expected = match self.rebuild(owner, &f.packet, &[f.i]).await {
                Ok(mut v) if v.len() == 1 => v.remove(0),
                Ok(_) => continue,
                Err(e) => {
                    self.activity.publish_to(Audience::Captain, "alert", "backup", format!("sauvegarde k-sur-n de …{} : fragment non recalculable ({e})", short(owner)), None);
                    continue;
                }
            };
            let suspect_key = format!("{owner}:{}:{}", f.host, f.cid);
            let was_suspect = self.kv_get(SUSPECT, &suspect_key).await.is_some();
            // Un suspect d'une nuit précédente n'a droit qu'à une plage ; sinon deux.
            let mut wrong = u8::from(was_suspect);
            let mut answered = false;
            let mut verdict = None;
            for _ in 0..(2 - wrong) {
                match check_range(&sample, &f.cid, &peer, &expected).await {
                    Range::Good => {
                        verdict = Some(true);
                        answered = true;
                        break;
                    }
                    Range::Bad => {
                        answered = true;
                        wrong += 1;
                        if wrong >= 2 {
                            verdict = Some(false);
                            break;
                        }
                    }
                    Range::Down => break,
                }
            }
            if answered {
                stats.checked += 1;
            }
            match verdict {
                Some(true) => {
                    self.kv_del(SUSPECT, &suspect_key).await;
                    let (o, h, c) = (owner.to_owned(), f.host.clone(), f.cid.clone());
                    let _ = self.db.writer().execute("UPDATE backup_frags SET seen_at = ?4 WHERE owner_hex = ?1 AND host_hex = ?2 AND cid = ?3", vec![o.into(), h.into(), c.into(), at.into()]).await;
                    continue;
                }
                None => {
                    // Une plage fausse puis plus de réponse : suspect, repris une autre nuit (jamais condamné sur une seule).
                    if wrong > 0 && !was_suspect {
                        self.kv_put(SUSPECT, &suspect_key, "1", at).await;
                    }
                    continue;
                }
                Some(false) => {}
            }
            self.kv_del(SUSPECT, &suspect_key).await;
            // Deux réponses fausses : le fragment est perdu, la disponibilité du gardien retombe à zéro sur la fenêtre.
            let (o, h, c, today) = (owner.to_owned(), f.host.clone(), f.cid.clone(), at / 86_400);
            let done = self
                .db
                .writer()
                .transaction(move |tx| {
                    tx.execute("UPDATE backup_frags SET lost_at = ?4 WHERE owner_hex = ?1 AND host_hex = ?2 AND cid = ?3", rusqlite::params![o, h, c, at])?;
                    tx.execute("DELETE FROM guardian_presence WHERE hex = ?1 AND day > ?2", rusqlite::params![h, today - crate::guardians::WINDOW_DAYS])?;
                    Ok(())
                })
                .await;
            if let Err(e) = done {
                self.activity.publish_to(Audience::Captain, "alert", "backup", format!("sauvegarde k-sur-n : mauvaise réponse non notée ({e})"), None);
                continue;
            }
            stats.liars += 1;
            stats.lost += 1;
            self.activity.publish_to(Audience::Captain, "alert", "backup", format!("sauvegarde k-sur-n de …{} : le gardien …{} a mal répondu deux fois au contrôle d'un fragment ; fragment refait ailleurs", short(owner), short(&f.host)), None);
            self.drop_at(owner, &f.host, vec![f.cid.clone()]).await;
        }
    }

    /// Gardiens muets depuis [`REPAIR_AFTER`] : leurs fragments sont perdus (jamais avant).
    async fn absence(&self, owner: &str, at: i64, stats: &mut RepairStats) {
        let o = owner.to_owned();
        let gone: Vec<(String, String)> = self
            .db
            .writer()
            .transaction(move |tx| {
                let mut s = tx.prepare(
                    "SELECT f.host_hex, f.cid FROM backup_frags f WHERE f.owner_hex = ?1 AND f.acked_at IS NOT NULL AND f.lost_at IS NULL \
                     AND MAX(COALESCE(f.seen_at, 0), f.acked_at, COALESCE((SELECT MAX(g.day) * 86400 + 86399 FROM guardian_presence g WHERE g.hex = f.host_hex), 0)) < ?2",
                )?;
                let rows = s.query_map(rusqlite::params![o, at - REPAIR_AFTER], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?.collect::<Result<Vec<_>, _>>()?;
                drop(s);
                for (h, c) in &rows {
                    tx.execute("UPDATE backup_frags SET lost_at = ?4 WHERE owner_hex = ?1 AND host_hex = ?2 AND cid = ?3", rusqlite::params![o, h, c, at])?;
                }
                Ok(rows)
            })
            .await
            .unwrap_or_default();
        if gone.is_empty() {
            return;
        }
        stats.lost += gone.len();
        self.activity.publish_to(Audience::Captain, "alert", "backup", format!("sauvegarde k-sur-n de …{} : {} fragment(s) chez des gardiens muets depuis 14 jours, refaits ailleurs", short(owner), gone.len()), None);
        let mut by_host: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (h, c) in gone {
            by_host.entry(h).or_default().push(c);
        }
        for (h, cids) in by_host {
            self.drop_at(owner, &h, cids).await; // reçu à son retour : la place est rendue, le fragment refait ailleurs
        }
    }

    /// Demande au gardien de retirer des fragments (100 par message au plus).
    pub(super) async fn drop_at(&self, owner: &str, host: &str, cids: Vec<String>) {
        for chunk in cids.chunks(100) {
            let tags = chunk.iter().map(|c| vec!["cid".to_owned(), c.clone()]).collect();
            if let Err(e) = self.control(owner, host, "frag-drop", tags).await {
                self.activity.publish_to(Audience::Captain, "alert", "backup", format!("retrait de fragments non envoyé à …{} : {e}", short(host)), None);
            }
        }
    }

    /// Réparation et extension : chaque gardien retenu qui ne tient rien d'un paquet (et dont la station n'en tient rien)
    /// reçoit un fragment d'indice neuf, dans son quota ; les paquets qui ont perdu un fragment passent d'abord.
    async fn extend(&self, owner: &str, at: i64, stats: &mut RepairStats) {
        let mut keepers = self.rs_keepers(owner, at).await;
        if keepers.is_empty() {
            return;
        }
        let o = owner.to_owned();
        type Rows = (Vec<(String, i64, i64, bool)>, Vec<(String, String, i64, bool)>);
        let (packets, rows): Rows = self
            .db
            .readers()
            .read(move |c| {
                let mut s = c.prepare(
                    "SELECT p.packet_id, p.len, p.k, EXISTS (SELECT 1 FROM backup_frags f WHERE f.owner_hex = p.owner_hex AND f.packet_id = p.packet_id AND f.lost_at IS NOT NULL) AS hurt \
                     FROM backup_packets p WHERE p.owner_hex = ?1 AND p.head_hex IS NOT NULL ORDER BY hurt DESC, p.seq",
                )?;
                let packets = s.query_map([&o], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<Result<Vec<_>, _>>()?;
                let mut s = c.prepare("SELECT packet_id, host_hex, i, lost_at IS NOT NULL FROM backup_frags WHERE owner_hex = ?1")?;
                let rows = s.query_map([&o], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<Result<Vec<_>, _>>()?;
                Ok((packets, rows))
            })
            .await
            .unwrap_or_default();
        let stations = self.stations_of(rows.iter().map(|r| r.1.clone()).collect()).await;
        let mut budget = NEW_FRAGS_PER_NIGHT;
        for (packet, len, k, _) in packets {
            if budget == 0 {
                break;
            }
            let mine: Vec<&(String, String, i64, bool)> = rows.iter().filter(|r| r.0 == packet).collect();
            let hosts: HashSet<&str> = mine.iter().map(|r| r.1.as_str()).collect();
            // Une station = un fragment par paquet (Q9) ; un fragment perdu ne retient pas sa station.
            let held: HashSet<&str> = mine.iter().filter(|r| !r.3).filter_map(|r| stations.get(&r.1).map(String::as_str)).collect();
            let mut next = mine.iter().map(|r| r.2).max().map_or(0, |m| m + 1);
            let size = FragHeader::frag_len_for(u64::try_from(len).unwrap_or(0), u8::try_from(k).unwrap_or(1)).map_or(u64::MAX, |l| l + FRAG_HEADER_LEN as u64);
            let mut chosen: Vec<(usize, u8)> = Vec::new();
            for (n, g) in keepers.iter().enumerate() {
                if chosen.len() >= budget || hosts.contains(g.hex.as_str()) || held.contains(g.station.as_str()) || g.room < size || chosen.iter().any(|(m, _)| keepers[*m].station == g.station) {
                    continue;
                }
                let Some(i) = u8::try_from(next).ok().filter(|&i| i <= MAX_FRAG_INDEX) else { break };
                chosen.push((n, i));
                next += 1;
            }
            if chosen.is_empty() {
                continue;
            }
            let indices: Vec<u8> = chosen.iter().map(|c| c.1).collect();
            let fragments = match self.rebuild(owner, &packet, &indices).await {
                Ok(f) => f,
                Err(e) => {
                    self.activity.publish_to(Audience::Captain, "alert", "backup", format!("sauvegarde k-sur-n de …{} : paquet non réparable ici ({e})", short(owner)), None);
                    continue;
                }
            };
            let j: Journal = Some((self.db.clone(), owner.to_owned()));
            let mut out = Vec::new();
            for ((n, i), bytes) in chosen.iter().zip(fragments) {
                match self.write_fragment(&j, &bytes).await {
                    Ok(cid) => out.push((keepers[*n].hex.clone(), *i, cid, bytes.len() as u64)),
                    Err(e) => {
                        self.activity.publish_to(Audience::Captain, "alert", "backup", format!("sauvegarde k-sur-n : fragment recalculé non écrit ({e})"), None);
                    }
                }
            }
            let (o, p, rows_in) = (owner.to_owned(), packet.clone(), out.clone());
            let done = self
                .db
                .writer()
                .transaction(move |tx| {
                    for (host, i, cid, size) in &rows_in {
                        tx.execute(
                            "INSERT OR IGNORE INTO backup_frags(owner_hex, host_hex, packet_id, i, cid, size) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                            rusqlite::params![o, host, p, i, cid, i64::try_from(*size).unwrap_or(i64::MAX)],
                        )?;
                        tx.execute("DELETE FROM kv WHERE scope = ?1 AND key = ?2", rusqlite::params![SPOOL, cid])?;
                    }
                    Ok(())
                })
                .await;
            if let Err(e) = done {
                self.activity.publish_to(Audience::Captain, "alert", "backup", format!("sauvegarde k-sur-n : fragments recalculés non inscrits ({e})"), None);
                continue; // le journal les ramassera
            }
            for (host, _, _, size) in &out {
                if let Some(g) = keepers.iter_mut().find(|g| &g.hex == host) {
                    g.room = g.room.saturating_sub(*size);
                }
            }
            stats.rebuilt += out.len();
            budget = budget.saturating_sub(out.len());
        }
    }

    /// Écrit un fragment recalculé dans le magasin (journalisé d'abord). Rend son CID.
    async fn write_fragment(&self, j: &Journal, bytes: &[u8]) -> Result<String, String> {
        let cid = astro_dav::blob::cid_raw_sha256(&Sha256::digest(bytes).into());
        journal(j, &cid).await.map_err(|e| e.to_string())?;
        let mut w = self.blobs.begin().await.map_err(|e| e.to_string())?;
        w.write_all(bytes).await.map_err(|e| e.to_string())?;
        let meta = w.commit().await.map_err(|e| e.to_string())?;
        if meta.cid != cid {
            return Err("CID inattendu".into());
        }
        Ok(cid)
    }

    /// Recalcule, depuis les blobs locaux du membre, les fragments `indices` du paquet `packet_id`. Le clair est
    /// reconstitué à partir des positions de `backup_blobs` (un blob peut déborder d'un paquet sur le suivant).
    pub(super) async fn rebuild(&self, owner: &str, packet_id: &str, indices: &[u8]) -> Result<Vec<Vec<u8>>, String> {
        let o = owner.to_owned();
        type Rows = (Vec<(String, i64, String, i64, i64, Option<String>)>, Vec<(String, i64, i64, i64)>);
        let (packets, blobs): Rows = self
            .db
            .readers()
            .read(move |c| {
                let mut s = c.prepare("SELECT packet_id, seq, key_hex, len, k, head_hex FROM backup_packets WHERE owner_hex = ?1 ORDER BY seq")?;
                let packets = s.query_map([&o], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?.collect::<Result<Vec<_>, _>>()?;
                // Blobs à leur place, et dans les paquets neufs d'un compactage en cours (lot 6).
                let mut s = c.prepare("SELECT cid, seq, off, len FROM backup_blobs WHERE owner_hex = ?1 UNION ALL SELECT cid, seq, off, len FROM backup_compact WHERE owner_hex = ?1 ORDER BY 2, 3")?;
                let blobs = s.query_map([&o], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<Result<Vec<_>, _>>()?;
                Ok((packets, blobs))
            })
            .await
            .map_err(|e| e.to_string())?;
        // Position de chaque paquet dans le flux clair.
        let mut start: HashMap<i64, u64> = HashMap::new();
        let mut pos = 0u64;
        let mut target = None;
        for (id, seq, key, len, k, head) in &packets {
            let len = u64::try_from(*len).map_err(|_| "longueur de paquet")?;
            let payload = backup_pack::payload_of(len).ok_or("longueur de paquet")?;
            start.insert(*seq, pos);
            if id == packet_id {
                target = Some((pos, payload, PacketEntry { id: id.clone(), key: key.clone(), len, k: u8::try_from(*k).map_err(|_| "k")?, frags: Vec::new() }, head.clone()));
            }
            pos = pos.saturating_add(payload);
        }
        let (begin, payload, entry, head) = target.ok_or("paquet inconnu")?;
        let head = head.ok_or("en-tête du paquet inconnu ici (paquet repris d'une restauration)")?;
        let head: [u8; astro_crypto::uenc::HEADER_LEN] = hex::decode(head).ok().and_then(|h| h.try_into().ok()).ok_or("en-tête du paquet illisible")?;
        let end = begin + payload;
        let mut plain = vec![0u8; usize::try_from(payload).map_err(|_| "paquet trop grand")?];
        for (cid, seq, off, len) in blobs {
            let Some(&s) = start.get(&seq) else { continue };
            let (b0, blen) = (s + u64::try_from(off).unwrap_or(0), u64::try_from(len).unwrap_or(0));
            let (from, to) = (b0.max(begin), (b0 + blen).min(end));
            if from >= to {
                continue;
            }
            let (mut reader, size) = self.blobs.open(&cid).await.map_err(|_| format!("blob {cid} absent d'ici"))?;
            if size != blen {
                return Err(format!("blob {cid} de taille inattendue"));
            }
            tokio::io::copy(&mut (&mut reader).take(from - b0), &mut tokio::io::sink()).await.map_err(|e| e.to_string())?;
            let a = usize::try_from(from - begin).map_err(|_| "décalage")?;
            let z = usize::try_from(to - begin).map_err(|_| "décalage")?;
            reader.read_exact(&mut plain[a..z]).await.map_err(|e| e.to_string())?;
        }
        // Le paquet recalculé doit être celui de l'index (sinon rien ne sort : voir `refragment`).
        backup_pack::refragment(&entry, &head, &plain, indices).await.map_err(|e| e.to_string())
    }

    /// Alerte au membre quand un paquet qui a perdu un fragment n'a plus que `k` fragments sûrs ; levée quand la marge revient.
    async fn margins(&self, owner: &str, at: i64, stats: &mut RepairStats) {
        let o = owner.to_owned();
        let rows: Vec<(String, i64, i64)> = self
            .db
            .readers()
            .read(move |c| {
                let mut s = c.prepare(
                    "SELECT p.packet_id, p.k, (SELECT COUNT(*) FROM backup_frags f WHERE f.owner_hex = p.owner_hex AND f.packet_id = p.packet_id AND f.acked_at IS NOT NULL AND f.lost_at IS NULL) \
                     FROM backup_packets p WHERE p.owner_hex = ?1 \
                     AND EXISTS (SELECT 1 FROM backup_frags f WHERE f.owner_hex = p.owner_hex AND f.packet_id = p.packet_id AND f.lost_at IS NOT NULL)",
                )?;
                Ok(s.query_map([o], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .unwrap_or_default();
        let mut thin = 0;
        for (packet, k, safe) in rows {
            let key = format!("{owner}:{packet}");
            if safe > k {
                self.kv_del(ALERTED, &key).await;
                continue;
            }
            if self.kv_get(ALERTED, &key).await.is_none() {
                self.kv_put(ALERTED, &key, &safe.to_string(), at).await;
                thin += 1;
            }
        }
        if thin > 0 {
            stats.alerts += thin;
            self.activity.publish_to(Audience::Captain, "alert", "backup", format!("sauvegarde k-sur-n de …{} : {thin} paquet(s) à k fragments sûrs, plus aucune marge", short(owner)), None);
            self.tell(owner, "⚠️ Une partie de vos fichiers mis à l'abri chez vos amis n'a plus de marge : si un ami de plus s'éteint, elle ne pourra plus être reconstituée. Votre station la remet à l'abri dès qu'un ami a de la place ; en ajouter un aide.".to_owned()).await;
        }
    }

    /// Ramasse les fragments orphelins : écrits mais jamais inscrits (journal `SPOOL`), ou acquittés mais encore là.
    /// Idempotent. Rend le nombre de blobs effacés.
    pub(super) async fn collect_orphans(&self) -> usize {
        let (spool, acked): (Vec<String>, Vec<String>) = self
            .db
            .readers()
            .read(|c| {
                let mut s = c.prepare("SELECT key FROM kv WHERE scope = ?1")?;
                let spool = s.query_map([SPOOL], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
                let mut s = c.prepare("SELECT DISTINCT f.cid FROM backup_frags f JOIN identities i ON i.hex = f.owner_hex AND i.hosted = 1 WHERE f.acked_at IS NOT NULL")?;
                let acked = s.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
                Ok((spool, acked))
            })
            .await
            .unwrap_or_default();
        let mut removed = 0;
        for cid in spool {
            let c = cid.clone();
            let referenced = self
                .db
                .readers()
                .read(move |conn| {
                    let frag = conn.query_row("SELECT 1 FROM backup_frags WHERE cid = ?1 AND acked_at IS NULL", [&c], |_| Ok(())).optional()?.is_some();
                    Ok(frag || in_use(conn, &c)?)
                })
                .await;
            let Ok(referenced) = referenced else { continue };
            if !referenced && self.blobs.open(&cid).await.is_ok() && self.blobs.remove(&cid).await.is_ok() {
                removed += 1;
            }
            self.kv_del(SPOOL, &cid).await;
        }
        for cid in acked {
            if self.blobs.open(&cid).await.is_err() {
                continue;
            }
            let c = cid.clone();
            let pending = self
                .db
                .readers()
                .read(move |conn| {
                    let unacked = conn.query_row("SELECT 1 FROM backup_frags WHERE cid = ?1 AND acked_at IS NULL", [&c], |_| Ok(())).optional()?.is_some();
                    Ok(unacked || in_use(conn, &c)?)
                })
                .await
                .unwrap_or(true);
            if !pending && self.blobs.remove(&cid).await.is_ok() {
                removed += 1;
            }
        }
        if removed > 0 {
            self.activity.publish_to(Audience::Captain, "info", "backup", format!("sauvegarde k-sur-n : {removed} fragment(s) orphelin(s) effacé(s) du magasin"), None);
        }
        removed
    }
}

/// Le blob sert-il ailleurs (VFS d'un compte, index courant) ?
fn in_use(conn: &rusqlite::Connection, cid: &str) -> Result<bool, astro_db::DbError> {
    let vfs = astro_dav::vfs::cid_in_use(conn, cid).map_err(|_| astro_db::DbError::Aborted)?;
    let index = conn.query_row("SELECT 1 FROM backup_index WHERE index_cid = ?1", [cid], |_| Ok(())).optional()?.is_some();
    Ok(vfs || index)
}

fn short(hex: &str) -> &str {
    &hex[..8.min(hex.len())]
}

/// Entier uniforme dans `0..n` (`n ≥ 1`), tiré du générateur du système ; `0` s'il fait défaut.
fn random_below(n: u64) -> u64 {
    let mut b = [0u8; 8];
    if getrandom::fill(&mut b).is_err() || n <= 1 {
        return 0;
    }
    u64::from_le_bytes(b) % n
}
