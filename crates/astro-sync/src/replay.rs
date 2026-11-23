//! `astroportd replay` : reconstruit les tables de projection depuis `nostr_events`.

use astro_crypto::KeySource;
use astro_dav::BlobStore;
use astro_dav::manifest::{self, D_TAG, ManifestError};
use astro_db::events::row_to_event;
use astro_db::Db;

use crate::projector::Projector;
use crate::SyncError;

/// Kinds rejoués, dans l'ordre : identités, réseau social, puis virements.
const PHASES: [&[u32]; 4] = [&[30_800], &[3], &[30_850], &[30_852, 30_851, 31_903]];

/// Bilan d'un replay.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ReplayStats {
    /// Événements examinés.
    pub scanned: u64,
    /// Événements dont la projection a produit au moins une requête.
    pub projected: u64,
    /// Événements non projetables (voir `errors`).
    pub skipped: u64,
    /// Manifestes uCloud restaurés (VFS + trousseau).
    pub manifests: u64,
    /// Manifestes ignorés : clé du compte indisponible ici, blob absent ou contenu refusé.
    pub manifests_skipped: u64,
    /// Les 20 premières erreurs `(event_id, raison)`.
    pub errors: Vec<(String, String)>,
}

/// Rejoue tous les événements stockés. La base reste utilisable pendant l'opération ; les tables de projection
/// se remplissent progressivement. Le cercle social, lui, n'est jamais vidé : l'ancien reste en place jusqu'au recalcul
/// de fin de phase (le pare-feu du relai ne se ferme pas pendant le replay).
///
/// L'état LOCAL (identités `hosted = 1`, marqueurs `done` sans événement source) est conservé.
pub async fn replay_local(db: &Db, page_size: usize) -> Result<ReplayStats, SyncError> {
    let page_size = page_size.clamp(1, 10_000);
    let mut stats = ReplayStats::default();
    db.writer().batch(Projector::reset()).await?;
    for kinds in PHASES {
        // Un kind après l'autre, page par page dans l'ordre d'insertion (`seq`) : la reprise est exacte et chaque page ne
        // relit pas ce qui précède (les projections ne dépendent pas de l'ordre d'arrivée).
        for &k in kinds {
            let mut cursor: i64 = 0;
            loop {
                let after = cursor;
                let page = db
                    .readers()
                    .read(move |c| {
                        let mut stmt = c.prepare_cached(
                            "SELECT id, pubkey, kind, created_at, content, sig, tags, seq FROM nostr_events \
                             WHERE seq > ?1 AND kind = ?2 ORDER BY seq LIMIT ?3",
                        )?;
                        let mut rows = stmt.query(rusqlite::params![after, i64::from(k), i64::try_from(page_size).unwrap_or(i64::MAX)])?;
                        let mut out = Vec::new();
                        while let Some(r) = rows.next()? {
                            let seq: i64 = r.get(7)?;
                            out.push((seq, row_to_event(r)?));
                        }
                        Ok(out)
                    })
                    .await?;
                let Some((last, _)) = page.last() else { break };
                cursor = *last;
                let mut batch = Vec::new();
                for (_, event) in &page {
                    stats.scanned += 1;
                    match Projector::plan_replay(event) {
                        Ok(plan) if plan.is_empty() => {}
                        Ok(plan) => {
                            stats.projected += 1;
                            batch.extend(plan);
                        }
                        Err(e) => {
                            stats.skipped += 1;
                            if stats.errors.len() < 20 {
                                stats.errors.push((event.id.clone(), e.to_string()));
                            }
                        }
                    }
                }
                if !batch.is_empty() {
                    db.writer().batch(batch).await?;
                }
            }
        }
        if kinds.contains(&3) {
            // Fin de la phase sociale : un seul recalcul des N2, une seule transaction.
            db.writer().batch(Projector::rebuild_social_graph()).await?;
        }
    }
    Ok(stats)
}

/// Replay complet **avec** restauration d'uCloud : après les projections habituelles, chaque événement
/// `30078 d="ucloud:manifest"` d'un compte dont la station détient la clé est déchiffré (NIP-44), son manifeste lu
/// dans `blobs`, puis le VFS et le trousseau du compte sont **remplacés** par ceux du manifeste.
///
/// Un compte sans manifeste n'est pas touché. Les modifications du VFS postérieures au dernier manifeste publié
/// (publication différée en cours) sont perdues : publier (`flush`) avant de lancer un replay sur une base vivante.
pub async fn replay_with_ucloud<B: BlobStore>(db: &Db, page_size: usize, blobs: &B, keys: &dyn KeySource) -> Result<ReplayStats, SyncError> {
    let mut stats = replay_local(db, page_size).await?;
    let events = db
        .readers()
        .read(|c| {
            let mut stmt = c.prepare(
                "SELECT id, pubkey, kind, created_at, content, sig, tags FROM nostr_events WHERE kind = 30078 AND d_tag = ?1 ORDER BY created_at, id",
            )?;
            let mut rows = stmt.query([D_TAG])?;
            let mut out = Vec::new();
            while let Some(r) = rows.next()? {
                out.push(row_to_event(r)?);
            }
            Ok(out)
        })
        .await?;
    for event in &events {
        stats.scanned += 1;
        match manifest::restore_from_event(db, blobs, keys, event).await {
            Ok(_) => stats.manifests += 1,
            Err(ManifestError::NoKey) => stats.manifests_skipped += 1, // compte hébergé ailleurs : normal
            Err(e) => {
                stats.manifests_skipped += 1;
                if stats.errors.len() < 20 {
                    stats.errors.push((event.id.clone(), e.to_string()));
                }
            }
        }
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use astro_db::events::insert_event;
    use astro_nostr_proto::{Event, UnsignedEvent, pubkey_of};

    use super::*;
    use crate::ingest_event;

    const ALICE: [u8; 32] = [0xA1; 32];
    const BOB: [u8; 32] = [0xB2; 32];
    const CAROL: [u8; 32] = [0xC3; 32];
    const NODE: [u8; 32] = [0xD4; 32];

    fn hex(s: &[u8; 32]) -> String {
        pubkey_of(s).unwrap()
    }

    fn ev(s: &[u8; 32], kind: u32, ts: u64, tags: Vec<Vec<String>>, content: &str) -> Event {
        UnsignedEvent { created_at: ts, kind, tags, content: content.into() }.sign(s).unwrap()
    }

    fn tag(a: &str, b: &str) -> Vec<String> {
        vec![a.to_owned(), b.to_owned()]
    }

    fn db(tag: &str) -> (std::path::PathBuf, Arc<Db>) {
        let dir = std::env::temp_dir().join(format!("astro-sync-replay-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
        (dir, db)
    }

    async fn store(db: &Db, e: Event) {
        db.writer().transaction(move |tx| insert_event(tx, &e).map(|_| ())).await.unwrap();
    }

    async fn levels(db: &Db) -> Vec<(String, i64)> {
        db.readers()
            .read(|c| Ok(c.prepare("SELECT pubkey, level FROM social_graph ORDER BY pubkey")?.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<Vec<_>, _>>()?))
            .await
            .unwrap()
    }

    #[test]
    fn the_replay_keeps_the_circle_and_does_not_recompute_n2_list_by_list() {
        assert!(Projector::reset().iter().all(|s| !s.sql.contains("social_graph")), "le pare-feu ne se ferme pas pendant le replay");
        let list = ev(&ALICE, 3, 1, vec![tag("p", &hex(&BOB)), tag("p", &hex(&CAROL))], "");
        let replay = Projector::plan_replay(&list).unwrap();
        assert!(replay.iter().all(|s| !s.sql.contains("level = 2")), "pas de recalcul N2 par liste au replay");
        assert!(Projector::plan(&list).unwrap().iter().any(|s| s.sql.contains("nostr_tags")), "temps réel : les p viennent de l'index des tags");
    }

    #[tokio::test]
    async fn a_replay_rebuilds_n1_and_n2_once_and_drops_what_no_list_supports() {
        let (dir, db) = db("graph");
        db.writer().batch(vec![Projector::mark_hosted(&hex(&ALICE), "multipass")]).await.unwrap();
        store(&db, ev(&ALICE, 3, 10, vec![tag("p", &hex(&BOB))], "")).await;
        store(&db, ev(&BOB, 3, 11, vec![tag("p", &hex(&CAROL)), tag("p", &hex(&ALICE))], "")).await;
        // Un ancien N1 que plus aucune liste ne porte : il reste visible pendant le replay, et part à la fin de la phase.
        db.writer()
            .execute("INSERT INTO social_graph(owner_hex, pubkey, level, updated_at, src_event) VALUES (?1, ?2, 1, 0, 'disparu')", vec![hex(&ALICE).into(), hex(&NODE).into()])
            .await
            .unwrap();
        let stats = replay_local(&db, 1).await.unwrap();
        assert_eq!(stats.skipped, 0);
        let mut want = vec![(hex(&BOB), 1), (hex(&CAROL), 2)];
        want.sort();
        assert_eq!(levels(&db).await, want);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_rent_that_arrives_before_the_payer_s_did_is_caught_up() {
        let (dir, db) = db("rent");
        db.writer().batch(vec![Projector::mark_hosted(&hex(&NODE), "node"), Projector::mark_hosted(&hex(&ALICE), "multipass")]).await.unwrap();
        // Virement N² de Bob (clé du même membre) vers le NODE : signé par le payeur seul, ce n'est jamais un loyer.
        let transfer = ev(&BOB, 30_852, 5, vec![tag("d", "1"), tag("p", &hex(&NODE)), tag("amount", "12.5"), tag("prev", "genesis"), tag("t", "g1-n2")], "");
        ingest_event(db.writer(), transfer, "t").await.unwrap();
        // Preuve de loyer 30851 du NODE pour l'email d'Alice, avant que la station connaisse cet email.
        let proof = ev(&NODE, 30_851, 6, vec![tag("d", "w1"), tag("type", "rent"), tag("s", "OK"), tag("email", "alice@example.org"), tag("amount", "3")], "");
        ingest_event(db.writer(), proof.clone(), "t").await.unwrap();
        let rents = |db: Arc<Db>| async move {
            db.readers().read(|c| Ok(c.prepare("SELECT event_id, amount_cents FROM rent_payments ORDER BY event_id")?.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?.collect::<Result<Vec<_>, _>>()?)).await.unwrap()
        };
        assert!(rents(db.clone()).await.is_empty());
        let did = |s: &[u8; 32], ts| ev(s, 30_800, ts, vec![tag("d", "did"), tag("email", "alice@example.org")], "{}");
        ingest_event(db.writer(), did(&ALICE, 7), "t").await.unwrap();
        ingest_event(db.writer(), did(&BOB, 8), "t").await.unwrap();
        assert_eq!(rents(db.clone()).await, vec![(proof.id, 300)], "le loyer attesté est rattrapé (centimes) ; le virement N² ne compte pas");
        let _ = std::fs::remove_dir_all(dir);
    }
}
