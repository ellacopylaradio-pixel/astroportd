//! « Retrouver ma station » : un identifiant mène à sa station d'accueil, jamais à un secret ; une adresse de balise
//! piégée n'est jamais proposée.

#![allow(clippy::unwrap_used)]

use astro_db::Db;
use astroportd::locate::{Found, locate};

#[tokio::test]
async fn an_identifier_leads_home_and_never_to_a_trapped_address() {
    let d = std::env::temp_dir().join(format!("astroportd-locate-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let db = Db::open(&d.join("u.db"), 2).unwrap();
    let (here, there, lost, trap) = ("a1".repeat(32), "b2".repeat(32), "c3".repeat(32), "d4".repeat(32));
    let w = db.writer();
    for (hex, email, hosted, home) in [
        (&here, "alice@example.org", 1, None),
        (&there, "Bob@Example.org", 0, Some("did:node1:x")),
        (&lost, "carol@example.org", 0, Some("did:node2:x")),
        (&trap, "dave@example.org", 0, Some("did:node3:x")),
    ] {
        w.execute(
            "INSERT INTO identities(hex, role, email, hosted, home_station) VALUES (?1, 'multipass', ?2, ?3, ?4)",
            vec![hex.clone().into(), email.to_owned().into(), i64::from(hosted).into(), home.map_or(rusqlite::types::Value::Null, |h| h.to_owned().into())],
        )
        .await
        .unwrap();
    }
    // Balises des stations : node1 a une adresse propre ; node2 n'a pas de balise ; node3 annonce une adresse piégée.
    for (node, url) in [("node1", "https://toulouse.coop.example/"), ("node3", "https://evil.example/phish?x=1")] {
        w.execute("INSERT INTO kv(scope, key, value, updated_at) VALUES ('economic_health', ?1, ?2, 0)", vec![node.to_owned().into(), serde_json::json!({ "tags": { "url": url } }).to_string().into()]).await.unwrap();
    }

    assert_eq!(locate(&db, "alice@example.org").await.unwrap(), Found::Here);
    assert_eq!(locate(&db, &here).await.unwrap(), Found::Here, "par la clé hexadécimale");
    assert_eq!(locate(&db, " bob@EXAMPLE.org ").await.unwrap(), Found::Elsewhere("https://toulouse.coop.example".into()), "casse et espaces ignorés");
    let npub = bech32::encode::<bech32::Bech32>(bech32::Hrp::parse("npub").unwrap(), &hex::decode(&there).unwrap()).unwrap();
    assert_eq!(locate(&db, &npub).await.unwrap(), Found::Elsewhere("https://toulouse.coop.example".into()), "par le npub");
    assert_eq!(locate(&db, "carol@example.org").await.unwrap(), Found::Unknown, "station d'accueil sans balise : neutre");
    assert_eq!(locate(&db, "dave@example.org").await.unwrap(), Found::Unknown, "adresse avec chemin ou paramètres : jamais proposée");
    for q in ["inconnu@example.org", "pas-un-identifiant", "", "' OR 1=1 --"] {
        assert_eq!(locate(&db, q).await.unwrap(), Found::Unknown, "{q:?}");
    }
    let _ = std::fs::remove_dir_all(d);
}
