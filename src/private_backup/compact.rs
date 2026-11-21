//! Compactage nocturne des paquets et migration des copies complètes (lot 6, `docs/design/SAUVEGARDE_RS.md` § 2.4 et
//! § 4.4), côté propriétaire, à chaque passage k-sur-n ([`PrivateBackup::compact_step`], appelé par `rs_once`).
//!
//! **Compactage.** Les paquets sont immuables ; un fichier disparu depuis [`GRACE_SECS`] y laisse une place « morte »
//! (`backup_blobs.gone_at`). Une suite de paquets (liés par un blob étalé) dont la part vivante tombe sous 50 %, ou qui
//! vient d'une restauration (pas d'en-tête UENC local, donc ni contrôlable ni réparable), est réécrite : ses blobs vivants
//! repartent dans un flux neuf, depuis le magasin local (rien n'est téléchargé), avec le `k` d'aujourd'hui. Pas à pas,
//! chaque étape idempotente et sans perte :
//!
//! 1. **Construction** : paquets neufs marqués `staged`, leurs blobs dans `backup_compact`, la liste des anciens paquets
//!    dans `kv` [`COMPACT_FROM`] — le tout en une transaction (une coupure avant ne laisse que des fragments journalisés,
//!    ramassés la nuit). Les blobs restent rattachés aux anciens paquets : l'index publié décrit toujours une sauvegarde
//!    complète.
//! 2. **Envoi** des fragments neufs par l'envoi ordinaire ; attente des `frag-ack`.
//! 3. **Bascule** quand chaque paquet neuf est acquitté par assez de gardiens (au moins `k`, et `k + 1` s'il en a reçu
//!    davantage) : en une transaction, les blobs passent aux paquets neufs, les anciens paquets sortent des tables, et
//!    leurs fragments entrent dans la file de retrait [`DROP_Q`] avec la révision de l'index d'alors.
//! 4. **Retrait** (`frag-drop`) seulement quand un index plus récent que la bascule a été envoyé à tous les gardiens : un
//!    gardien ne jette jamais un ancien fragment tant que l'index qu'il tient en a besoin.
//!
//! **Migration des copies complètes** (`backup_items` chez un gardien `rs`) : une copie est retirée (`drop`) quand son
//! blob est dans des paquets mis en place depuis au moins [`GRACE_SECS`], qui ont tous de la marge (plus de `k` fragments
//! sûrs) et dont la chance de réunir `k` gardiens atteint la cible ([`RS_TARGET_PERMILLE`]). Jamais sinon.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use astro_core::Audience;
use astro_dav::BlobStore;
use astro_dav::backup_pack;
use astro_facecloud::Outbox;
use rusqlite::OptionalExtension;
use serde_json::Value;

use super::rs::{Heads, INDEX_SENT, PASS_BYTES, Queue, RESTORE_RS, RS, RS_TARGET_PERMILLE, SPOOL, UNKNOWN_AVAILABILITY, Written, lock, select};
use super::{GRACE_SECS, PrivateBackup};

/// kv : compactage en cours de `owner` → identifiants (JSON) des anciens paquets à remplacer.
pub(super) const COMPACT_FROM: &str = "backup_compact_from";
/// kv : fragment d'un ancien paquet à retirer, `<owner>:<host>:<cid>` → révision de l'index au moment de la bascule.
pub(super) const DROP_Q: &str = "backup_rs_drop";

/// Ancien paquet vu par le compactage.
struct Old {
    id: String,
    seq: i64,
    payload: u64,
    headless: bool,
}

/// Blob vu par le compactage : `(cid, seq, off, len, mort)`.
type BlobRow = (String, i64, i64, i64, bool);

impl<O: Outbox, B: BlobStore> PrivateBackup<O, B> {
    /// Une étape de compactage et de migration pour `owner`. Rend le nombre de messages partis.
    pub(super) async fn compact_step(&self, owner: &str, at: i64) -> usize {
        // Pendant une restauration, les blobs ne sont pas tous là : on ne réécrit rien.
        if let Some((v, _)) = self.kv_get(RESTORE_RS, owner).await
            && !serde_json::from_str::<Value>(&v).ok().is_some_and(|st| matches!(st["state"].as_str(), Some("done" | "abandoned")))
        {
            return 0;
        }
        let mut sent = self.send_queued_drops(owner).await;
        self.mark_gone(owner, at).await;
        if self.kv_get(COMPACT_FROM, owner).await.is_none()
            && let Err(e) = self.start_compaction(owner, at).await
        {
            self.activity.publish_to(Audience::Captain, "alert", "backup", format!("sauvegarde k-sur-n de …{} : compactage non construit ({e})", short(owner)), None);
        }
        if let Some((from, _)) = self.kv_get(COMPACT_FROM, owner).await {
            self.commit_compaction(owner, &from, at).await;
        }
        sent += self.migrate_legacy(owner, at).await;
        sent
    }

    /// Note les blobs en paquet dont le fichier a disparu (et efface la marque d'un fichier revenu).
    async fn mark_gone(&self, owner: &str, at: i64) {
        const LIVE: &str = "EXISTS (SELECT 1 FROM ucloud_entries e WHERE e.owner_hex = backup_blobs.owner_hex AND e.cid = backup_blobs.cid AND e.type = 'file' \
                            AND e.path NOT LIKE '/Partages/%' AND e.path NOT LIKE '/Sauvegardes/%')";
        let o = owner.to_owned();
        let done = self
            .db
            .writer()
            .transaction(move |tx| {
                tx.execute(&format!("UPDATE backup_blobs SET gone_at = NULL WHERE owner_hex = ?1 AND gone_at IS NOT NULL AND {LIVE}"), [&o])?;
                tx.execute(&format!("UPDATE backup_blobs SET gone_at = ?2 WHERE owner_hex = ?1 AND gone_at IS NULL AND NOT {LIVE}"), rusqlite::params![o, at])?;
                Ok(())
            })
            .await;
        if let Err(e) = done {
            self.activity.publish_to(Audience::Captain, "alert", "backup", format!("sauvegarde k-sur-n : fichiers disparus non notés ({e})"), None);
        }
    }

    /// Choisit les suites de paquets à réécrire et construit leurs paquets neufs (étape 1).
    async fn start_compaction(&self, owner: &str, at: i64) -> Result<(), String> {
        let o = owner.to_owned();
        type Rows = (Vec<(String, i64, i64, bool)>, Vec<BlobRow>);
        let (packets, blobs): Rows = self
            .db
            .readers()
            .read(move |c| {
                let mut s = c.prepare("SELECT packet_id, seq, len, head_hex IS NULL FROM backup_packets WHERE owner_hex = ?1 AND staged = 0 ORDER BY seq")?;
                let packets = s.query_map([&o], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<Result<Vec<_>, _>>()?;
                let mut s = c.prepare("SELECT cid, seq, off, len, gone_at IS NOT NULL AND gone_at <= ?2 FROM backup_blobs WHERE owner_hex = ?1 ORDER BY seq, off")?;
                let blobs = s.query_map(rusqlite::params![o, at - GRACE_SECS], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?.collect::<Result<Vec<_>, _>>()?;
                Ok((packets, blobs))
            })
            .await
            .map_err(|e| e.to_string())?;
        let old: Vec<Old> = packets.into_iter().map(|(id, seq, len, headless)| Old { id, seq, payload: u64::try_from(len).ok().and_then(backup_pack::payload_of).unwrap_or(0), headless }).collect();
        let (mut chosen_ids, mut live, mut volume) = (Vec::new(), Vec::new(), 0u64);
        for run in runs(&old, &blobs) {
            let seqs: HashSet<i64> = run.iter().map(|&p| old[p].seq).collect();
            let mine: Vec<&BlobRow> = blobs.iter().filter(|b| seqs.contains(&b.1)).collect();
            let total: i64 = mine.iter().map(|b| b.3).sum();
            let alive: Vec<&BlobRow> = mine.iter().copied().filter(|b| !b.4).collect();
            let alive_bytes: i64 = alive.iter().map(|b| b.3).sum();
            // Réécrit : paquet repris d'une restauration, paquet sans blob, ou part vivante sous 50 %.
            if !(run.iter().any(|&p| old[p].headless) || total == 0 || alive_bytes * 2 < total) {
                continue;
            }
            let mut absent = false;
            for b in &alive {
                if self.blobs.open(&b.0).await.is_err() {
                    absent = true;
                    break;
                }
            }
            if absent {
                self.activity.publish_to(Audience::Captain, "alert", "backup", format!("sauvegarde k-sur-n de …{} : un paquet à réécrire a un blob absent d'ici, laissé tel quel", short(owner)), None);
                continue;
            }
            let bytes = u64::try_from(alive_bytes).unwrap_or(0);
            if !chosen_ids.is_empty() && volume.saturating_add(bytes) > PASS_BYTES {
                break;
            }
            volume = volume.saturating_add(bytes);
            chosen_ids.extend(run.iter().map(|&p| old[p].id.clone()));
            live.extend(alive.iter().map(|b| b.0.clone()));
        }
        if chosen_ids.is_empty() {
            return Ok(());
        }
        let from = serde_json::to_string(&chosen_ids).map_err(|e| e.to_string())?;
        if live.is_empty() {
            // Plus rien de vivant : rien à reconstruire, la bascule retire seulement les anciens paquets.
            let (o, f) = (owner.to_owned(), from);
            return self
                .db
                .writer()
                .execute("INSERT INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(scope, key) DO NOTHING", vec![COMPACT_FROM.to_owned().into(), o.into(), f.into(), at.into()])
                .await
                .map(|_| ())
                .map_err(|e| e.to_string());
        }
        let keepers = self.rs_keepers(owner, at).await;
        let (keepers, k) = select(keepers, volume);
        if keepers.is_empty() {
            return Ok(()); // personne n'a la place : on réessaiera une autre nuit, l'ancien paquet reste valable
        }
        let n = u8::try_from(keepers.len()).map_err(|_| "trop de gardiens".to_owned())?;
        let (queue, written, heads): (Queue, Written, Heads) = (Arc::default(), Arc::default(), Arc::default());
        let index = match self.build(owner, k, n, &live, &queue, &written, &heads).await {
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
        self.record_build(owner, at, index, &hosts, &written, &heads, Some(from)).await
    }

    /// Bascule (étape 3) si chaque paquet neuf est acquitté par assez de gardiens. Rend `true` si elle a eu lieu.
    async fn commit_compaction(&self, owner: &str, from: &str, at: i64) -> bool {
        let Ok(ids) = serde_json::from_str::<Vec<String>>(from) else {
            self.kv_del(COMPACT_FROM, owner).await;
            return false;
        };
        let o = owner.to_owned();
        let n_old = ids.len();
        let done = self
            .db
            .writer()
            .transaction(move |tx| {
                // Chaque paquet neuf : au moins k fragments sûrs, et k + 1 s'il en a reçu plus de k.
                let ready: bool = tx.query_row(
                    "SELECT NOT EXISTS (SELECT 1 FROM backup_packets p WHERE p.owner_hex = ?1 AND p.staged = 1 AND \
                       (SELECT COUNT(*) FROM backup_frags f WHERE f.owner_hex = p.owner_hex AND f.packet_id = p.packet_id AND f.acked_at IS NOT NULL AND f.lost_at IS NULL) \
                       < MAX(p.k, MIN(p.k + 1, (SELECT COUNT(*) FROM backup_frags f WHERE f.owner_hex = p.owner_hex AND f.packet_id = p.packet_id AND f.lost_at IS NULL))))",
                    [&o],
                    |r| r.get(0),
                )?;
                if !ready {
                    return Ok(false);
                }
                let rev: i64 = tx.query_row("SELECT rev FROM backup_index WHERE owner_hex = ?1", [&o], |r| r.get(0)).optional()?.unwrap_or(0);
                for id in &ids {
                    let seq: Option<i64> = tx.query_row("SELECT seq FROM backup_packets WHERE owner_hex = ?1 AND packet_id = ?2", [&o, id], |r| r.get(0)).optional()?;
                    let mut s = tx.prepare("SELECT host_hex, cid FROM backup_frags WHERE owner_hex = ?1 AND packet_id = ?2")?;
                    let frags = s.query_map([&o, id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?.collect::<Result<Vec<_>, _>>()?;
                    drop(s);
                    for (host, cid) in frags {
                        tx.execute(
                            "INSERT INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(scope, key) DO NOTHING",
                            rusqlite::params![DROP_Q, format!("{o}:{host}:{cid}"), rev.to_string(), at],
                        )?;
                    }
                    if let Some(seq) = seq {
                        tx.execute("DELETE FROM backup_blobs WHERE owner_hex = ?1 AND seq = ?2", rusqlite::params![o, seq])?;
                    }
                    tx.execute("DELETE FROM backup_frags WHERE owner_hex = ?1 AND packet_id = ?2", [&o, id])?;
                    tx.execute("DELETE FROM backup_packets WHERE owner_hex = ?1 AND packet_id = ?2", [&o, id])?;
                    tx.execute("DELETE FROM kv WHERE scope = 'backup_rs_alert' AND key = ?1", [format!("{o}:{id}")])?;
                }
                tx.execute("INSERT OR REPLACE INTO backup_blobs(owner_hex, cid, seq, off, len, at) SELECT owner_hex, cid, seq, off, len, ?2 FROM backup_compact WHERE owner_hex = ?1", rusqlite::params![o, at])?;
                tx.execute("DELETE FROM backup_compact WHERE owner_hex = ?1", [&o])?;
                tx.execute("UPDATE backup_packets SET staged = 0 WHERE owner_hex = ?1 AND staged = 1", [&o])?;
                tx.execute("DELETE FROM kv WHERE scope = ?1 AND key = ?2", rusqlite::params![COMPACT_FROM, o])?;
                Ok(true)
            })
            .await;
        match done {
            Ok(true) => {
                self.activity.publish_to(Audience::Captain, "info", "backup", format!("sauvegarde k-sur-n de …{} : {n_old} paquet(s) compacté(s)", short(owner)), None);
                true
            }
            Ok(false) => false,
            Err(e) => {
                self.activity.publish_to(Audience::Captain, "alert", "backup", format!("sauvegarde k-sur-n : bascule du compactage non faite ({e})"), None);
                false
            }
        }
    }

    /// Retrait des fragments des anciens paquets (étape 4), seulement quand un index plus récent que la bascule a été
    /// envoyé à chaque gardien. Rend le nombre de messages partis.
    async fn send_queued_drops(&self, owner: &str) -> usize {
        let o = owner.to_owned();
        type Rows = (Vec<(String, String)>, Option<(i64, String)>, Vec<String>);
        let (queued, current, hosts): Rows = self
            .db
            .readers()
            .read(move |c| {
                let mut s = c.prepare("SELECT key, value FROM kv WHERE scope = ?1 AND key LIKE ?2 || ':%'")?;
                let queued = s.query_map(rusqlite::params![DROP_Q, o], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<Vec<_>, _>>()?;
                let current = c.query_row("SELECT rev, index_cid FROM backup_index WHERE owner_hex = ?1", [&o], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
                let mut s = c.prepare(
                    "SELECT p.host_hex FROM backup_pacts p JOIN kv k ON k.scope = ?2 AND k.key = p.owner_hex || ':' || p.host_hex \
                     WHERE p.owner_hex = ?1 AND p.status = 'accepted' AND NOT EXISTS (SELECT 1 FROM identities i WHERE i.hex = p.host_hex AND i.hosted = 1)",
                )?;
                let hosts = s.query_map(rusqlite::params![o, RS], |r| r.get(0))?.collect::<Result<Vec<_>, _>>()?;
                Ok((queued, current, hosts))
            })
            .await
            .unwrap_or_default();
        let Some((rev, cid)) = current else { return 0 };
        if queued.is_empty() || queued.iter().any(|(_, r)| r.parse::<i64>().map_or(true, |r| r >= rev)) {
            return 0;
        }
        for h in &hosts {
            if self.kv_get(INDEX_SENT, &format!("{owner}:{h}")).await.is_none_or(|(v, _)| v != cid) {
                return 0; // ce gardien n'a pas encore l'index d'après la bascule
            }
        }
        let mut by_host: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let prefix = format!("{owner}:");
        for (key, _) in &queued {
            let Some((host, frag)) = key.strip_prefix(&prefix).and_then(|r| r.split_once(':')) else { continue };
            by_host.entry(host.to_owned()).or_default().push(frag.to_owned());
        }
        let mut sent = 0;
        for (host, cids) in by_host {
            sent += cids.len().div_ceil(100);
            self.drop_at(owner, &host, cids.clone()).await;
            for c in cids {
                self.kv_del(DROP_Q, &format!("{owner}:{host}:{c}")).await;
            }
        }
        sent
    }

    /// Migration des copies complètes chez les gardiens `rs` (voir l'en-tête du module). Rend le nombre de messages partis.
    async fn migrate_legacy(&self, owner: &str, at: i64) -> usize {
        let o = owner.to_owned();
        type Rows = (Vec<(String, String, i64, i64, i64)>, Vec<(i64, i64, i64, bool)>, Vec<(i64, String)>);
        let (items, packets, safe): Rows = self
            .db
            .readers()
            .read(move |c| {
                let mut s = c.prepare(
                    "SELECT b.host_hex, b.cid, x.seq, x.off, x.len FROM backup_items b \
                     JOIN backup_blobs x ON x.owner_hex = b.owner_hex AND x.cid = b.cid \
                     JOIN kv k ON k.scope = ?2 AND k.key = b.owner_hex || ':' || b.host_hex \
                     WHERE b.owner_hex = ?1 AND x.at <= ?3 AND NOT EXISTS (SELECT 1 FROM identities i WHERE i.hex = b.host_hex AND i.hosted = 1) \
                     ORDER BY b.host_hex, b.cid",
                )?;
                let items = s.query_map(rusqlite::params![o, RS, at - GRACE_SECS], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?.collect::<Result<Vec<_>, _>>()?;
                if items.is_empty() {
                    return Ok((items, Vec::new(), Vec::new()));
                }
                let mut s = c.prepare("SELECT seq, len, k, staged = 0 FROM backup_packets WHERE owner_hex = ?1 ORDER BY seq")?;
                let packets = s.query_map([&o], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<Result<Vec<_>, _>>()?;
                let mut s = c.prepare(
                    "SELECT p.seq, f.host_hex FROM backup_frags f JOIN backup_packets p ON p.owner_hex = f.owner_hex AND p.packet_id = f.packet_id \
                     WHERE f.owner_hex = ?1 AND f.acked_at IS NOT NULL AND f.lost_at IS NULL",
                )?;
                let safe = s.query_map([&o], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<Vec<_>, _>>()?;
                Ok((items, packets, safe))
            })
            .await
            .unwrap_or_default();
        if items.is_empty() {
            return 0;
        }
        let avail: HashMap<String, u32> = self.rs_keepers(owner, at).await.into_iter().map(|g| (g.hex, g.avail)).collect();
        let mut holders: HashMap<i64, Vec<String>> = HashMap::new();
        for (seq, host) in safe {
            holders.entry(seq).or_default().push(host);
        }
        // Un paquet est sûr s'il a de la marge (plus de k fragments sûrs) et atteint la cible.
        let sure = |seq: i64, k: i64| {
            let hs = holders.get(&seq).map_or(&[][..], Vec::as_slice);
            let d: Vec<u32> = hs.iter().map(|h| avail.get(h).copied().unwrap_or(UNKNOWN_AVAILABILITY)).collect();
            i64::try_from(hs.len()).unwrap_or(0) > k && usize::try_from(k).is_ok_and(|k| crate::guardians::proba_au_moins_k(&d, k) >= RS_TARGET_PERMILLE)
        };
        let mut due: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (host, cid, seq, off, len) in items {
            // Tous les paquets sur lesquels s'étale le blob.
            let Some(start) = packets.iter().position(|p| p.0 == seq) else { continue };
            let mut left = u64::try_from(off.saturating_add(len)).unwrap_or(u64::MAX);
            let mut ok = true;
            for p in &packets[start..] {
                ok &= p.3 && sure(p.0, p.2);
                let room = u64::try_from(p.1).ok().and_then(backup_pack::payload_of).unwrap_or(0);
                if !ok || left <= room {
                    break;
                }
                left -= room;
            }
            if ok {
                due.entry(host).or_default().push(cid);
            }
        }
        let mut sent = 0;
        for (host, cids) in due {
            for chunk in cids.chunks(100) {
                let tags = chunk.iter().map(|c| vec!["cid".to_owned(), c.clone()]).collect();
                if let Err(e) = self.control(owner, &host, "drop", tags).await {
                    self.activity.publish_to(Audience::Captain, "alert", "backup", format!("retrait de copies complètes non envoyé à …{} : {e}", short(&host)), None);
                    break;
                }
                sent += 1;
                let (o, h, ids) = (owner.to_owned(), host.clone(), chunk.to_vec());
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
            }
        }
        if sent > 0 {
            self.activity.publish_to(Audience::Captain, "info", "backup", format!("sauvegarde k-sur-n de …{} : copies complètes retirées chez les gardiens, les paquets suffisent", short(owner)), None);
        }
        sent
    }
}

/// Suites de paquets (positions dans `old`) liés par un blob qui déborde sur le suivant.
fn runs(old: &[Old], blobs: &[BlobRow]) -> Vec<Vec<usize>> {
    let pos: HashMap<i64, usize> = old.iter().enumerate().map(|(n, p)| (p.seq, n)).collect();
    let mut link = vec![false; old.len()];
    for (_, seq, off, len, _) in blobs {
        let Some(&start) = pos.get(seq) else { continue };
        let mut left = u64::try_from(off.saturating_add(*len)).unwrap_or(u64::MAX);
        let mut p = start;
        while p + 1 < old.len() && left > old[p].payload {
            left -= old[p].payload;
            link[p] = true;
            p += 1;
        }
    }
    let mut out: Vec<Vec<usize>> = Vec::new();
    for p in 0..old.len() {
        match out.last_mut() {
            Some(run) if run.last().is_some_and(|&q| link[q]) => run.push(p),
            _ => out.push(vec![p]),
        }
    }
    out
}

fn short(hex: &str) -> &str {
    &hex[..8.min(hex.len())]
}
