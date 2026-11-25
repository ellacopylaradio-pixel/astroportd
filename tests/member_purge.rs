//! Suppression complète d'un MULTIPASS (`astroportd::member_purge`), de bout en bout sur une vraie base et un vrai magasin
//! de blobs : adieu Nostr signé de sa clé, données et recherche effacées, blob partagé avec un autre membre gardé, blob
//! propre effacé, et un `replay` qui ne fait pas revenir le compte.
#![allow(clippy::unwrap_used)]

use astro_dav::{BlobStore, BlobWriter, FsBlobStore};
use astro_db::Db;
use astro_nostr_proto::UnsignedEvent;
use astroportd::member_purge;
use tokio::io::AsyncWriteExt;

async fn blob(store: &FsBlobStore, bytes: &[u8]) -> String {
    let mut w = store.begin().await.unwrap();
    w.write_all(bytes).await.unwrap();
    w.commit().await.unwrap().cid
}

fn count(db: &Db, sql: &str, hex: &str) -> i64 {
    let (s, h) = (sql.to_owned(), hex.to_owned());
    futures_lite(db.readers().read(move |c| Ok(c.query_row(&s, [h], |r| r.get::<_, i64>(0))?))).unwrap()
}

// Lecture synchrone dans le test (le lecteur rend un futur).
fn futures_lite<F: std::future::Future>(f: F) -> F::Output {
    tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(f))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_deleted_member_leaves_nothing_but_the_blobs_others_still_use_and_never_comes_back() {
    let dir = std::env::temp_dir().join(format!("astroportd-member-purge-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = Db::open(&dir.join("u.db"), 2).unwrap();
    let store = FsBlobStore::open(dir.join("blobs")).unwrap();
    let secret = [7u8; 32];
    let sign = |kind: u32, tags: Vec<Vec<String>>, content: &str, at: u64| UnsignedEvent { created_at: at, kind, tags, content: content.to_owned() }.sign(&secret).unwrap();
    let probe = sign(1, vec![], "x", 1);
    let alice = probe.pubkey.clone();
    let bob = "bb".repeat(32);

    // Alice : un fichier à elle seule, un fichier qu'elle partage (même blob) avec Bob, sa recherche, un appareil, ses réglages.
    let own = blob(&store, b"photo d'alice").await;
    let shared = blob(&store, b"document commun").await;
    let key = "11".repeat(32);
    let (a, b, o, s) = (alice.clone(), bob.clone(), own.clone(), shared.clone());
    db.writer()
        .transaction(move |tx| {
            tx.execute("INSERT INTO identities(hex, role, email, hosted, created_at) VALUES (?1, 'multipass', 'alice@exemple.org', 1, 1)", [&a])?;
            tx.execute("INSERT INTO identities(hex, role, email, hosted, created_at) VALUES (?1, 'multipass', 'bob@exemple.org', 1, 1)", [&b])?;
            for (owner, path, cid, sha) in [(&a, "/Photos/a.jpg", &o, "s-own"), (&a, "/Docs/c.pdf", &s, "s-shared"), (&b, "/c.pdf", &s, "s-shared")] {
                tx.execute(
                    "INSERT INTO ucloud_entries(owner_hex, path, parent, type, cid, mime, size, mtime, sha_plain, data) VALUES (?1, ?2, '/', 'file', ?3, 'image/jpeg', 100, 1, ?4, '{}')",
                    rusqlite::params![owner, path, cid, sha],
                )?;
                tx.execute("INSERT OR IGNORE INTO ucloud_keyring(owner_hex, cid, key_hex) VALUES (?1, ?2, ?3)", rusqlite::params![owner, cid, key])?;
            }
            tx.execute("INSERT INTO search_text(body, owner_hex, sha, src, t_ms) VALUES ('FACTURE ÉLECTRICITÉ', ?1, 's-own', 'ocr', 0)", [&a])?;
            tx.execute("INSERT INTO search_text(body, owner_hex, sha, src, t_ms) VALUES ('FACTURE DE BOB', ?1, 's-shared', 'ocr', 0)", [&b])?;
            tx.execute("INSERT INTO search_index(owner_hex, sha, task, state, updated_at) VALUES (?1, 's-own', 'vision', 'done', 1)", [&a])?;
            tx.execute("INSERT INTO search_frames(vis_id, owner_hex, sha, t_ms) VALUES (41, ?1, 's-own', 0)", [&a])?;
            let v = format!("[{}]", vec!["0.01"; 768].join(","));
            tx.execute("INSERT INTO search_vision_vectors(vis_id, owner_hex, embedding) VALUES (41, ?1, ?2)", rusqlite::params![a, v])?;
            tx.execute("INSERT INTO search_vision_vectors(vis_id, owner_hex, embedding) VALUES (42, ?1, ?2)", rusqlite::params![b, v])?;
            tx.execute("INSERT INTO dav_tokens(owner_hex, id, label, token_sha256, root, mode, created_at) VALUES (?1, 't', 'téléphone', 'x', '/', 'write', 1)", [&a])?;
            tx.execute("INSERT INTO kv(scope, key, value, updated_at) VALUES ('member_seen', ?1, '1', 1), ('member_removed', ?1, '2', 2)", [&a])?;
            Ok(())
        })
        .await
        .unwrap();
    // Ses événements : un DID (son identité), une note, un manifeste (remplaçable paramétré).
    let did = sign(30_800, vec![vec!["d".into(), "did".into()]], &format!(r#"{{"id":"did:nostr:{alice}","metadata":{{"email":"alice@exemple.org"}}}}"#), 100);
    let note = sign(1, vec![], "bonjour", 101);
    let manifest = sign(30_078, vec![vec!["d".into(), "ucloud".into()]], "{}", 102);
    let (d2, n2, m2) = (did.clone(), note.clone(), manifest.clone());
    db.writer()
        .transaction(move |tx| {
            for e in [&d2, &n2, &m2] {
                astro_db::events::insert_event(tx, e)?;
            }
            Ok(())
        })
        .await
        .unwrap();

    let before = db.readers().read({ let a = alice.clone(); move |c| Ok(member_purge::loss(c, &a)?) }).await.unwrap();
    assert_eq!((before.files, before.bytes, before.events), (2, 200, 3), "ce qui sera perdu est dit avant");

    // 2. Adieu Nostr : profil vidé + une suppression qui vise ses trois événements.
    let farewell = member_purge::farewell(&db, &alice, &secret, 1_000).await.unwrap();
    assert_eq!(farewell.iter().map(|e| e.kind).collect::<Vec<_>>(), vec![0, 5]);
    let refs: Vec<&str> = farewell[1].tags.iter().map(|t| t[1].as_str()).collect();
    assert!(refs.contains(&note.id.as_str()) && refs.contains(&format!("30800:{alice}:did").as_str()) && refs.contains(&format!("30078:{alice}:ucloud").as_str()), "{refs:?}");
    // 3. Données : le blob propre est rendu, le blob partagé non.
    let cids = member_purge::purge(&db, &alice).await.unwrap();
    assert_eq!(cids, vec![own.clone()], "seul le blob qu'elle était seule à référencer");
    for cid in &cids {
        store.remove(cid).await.unwrap();
    }
    assert!(store.open(&own).await.is_err(), "blob propre effacé");
    assert!(store.open(&shared).await.is_ok(), "blob partagé avec Bob gardé");
    member_purge::forget_identity(&db, &alice, 2_000, cids.len()).await.unwrap();

    for (what, sql) in [
        ("uCloud", "SELECT COUNT(*) FROM ucloud_entries WHERE owner_hex = ?1"),
        ("trousseau", "SELECT COUNT(*) FROM ucloud_keyring WHERE owner_hex = ?1"),
        ("texte lu", "SELECT COUNT(*) FROM search_text WHERE owner_hex = ?1"),
        ("état de la recherche", "SELECT COUNT(*) FROM search_index WHERE owner_hex = ?1"),
        ("images regardées", "SELECT COUNT(*) FROM search_frames WHERE owner_hex = ?1"),
        ("vecteurs", "SELECT COUNT(*) FROM search_vision_vectors WHERE vis_id = 41 AND ?1 <> ''"),
        ("appareils", "SELECT COUNT(*) FROM dav_tokens WHERE owner_hex = ?1"),
        ("réglages", "SELECT COUNT(*) FROM kv WHERE key = ?1"),
        ("identité", "SELECT COUNT(*) FROM identities WHERE hex = ?1"),
        ("événements (hors adieu)", "SELECT COUNT(*) FROM nostr_events WHERE pubkey = ?1 AND kind NOT IN (0, 5)"),
    ] {
        assert_eq!(count(&db, sql, &alice), 0, "{what} effacé(s)");
    }
    // Bob n'a rien perdu.
    assert_eq!(count(&db, "SELECT COUNT(*) FROM ucloud_entries WHERE owner_hex = ?1", &bob), 1);
    assert_eq!(count(&db, "SELECT COUNT(*) FROM search_text WHERE owner_hex = ?1", &bob), 1);
    assert_eq!(count(&db, "SELECT COUNT(*) FROM search_vision_vectors WHERE vis_id = 42 AND ?1 <> ''", &bob), 1);

    // Le DID et la note ne reviennent pas : ni par le relai (suppression retenue), ni par un replay.
    let back = db.writer().transaction(move |tx| astro_db::events::insert_event(tx, &did)).await.unwrap();
    assert_eq!(back, astro_db::events::InsertOutcome::Deleted, "son DID supprimé ne se réinsère pas");
    astro_sync::replay::replay_local(&db, 100).await.unwrap();
    assert_eq!(count(&db, "SELECT COUNT(*) FROM identities WHERE hex = ?1", &alice), 0, "le replay ne fait pas revenir le compte");
    let _ = std::fs::remove_dir_all(&dir);
}
