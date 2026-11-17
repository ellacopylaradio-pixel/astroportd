//! Écluses de la station : les trois modes (Forteresse, Village, Place publique) changent à chaud qui entre dans le cercle
//! du relai, qui peut publier, et si les photos partent seules.

#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use astro_core::ActivityBus;
use astro_db::Db;
use astro_db::events::insert_event;
use astro_nostr_proto::{UnsignedEvent, pubkey_of};
use astro_relay::{CircleProvider, DbCircle, Relay, RelayConfig};
use astroportd::locks::{Facts, Locks};
use astroportd::station_admin::KindsControl;
use serde_json::json;

const ALICE: [u8; 32] = [0xA1; 32];
const BOB: [u8; 32] = [0xB2; 32];
const CAROL: [u8; 32] = [0xC3; 32];
const DAVE: [u8; 32] = [0xD4; 32];

fn hex(s: &[u8; 32]) -> String {
    pubkey_of(s).unwrap()
}

#[tokio::test]
async fn the_three_modes_move_the_locks_live() {
    let d = std::env::temp_dir().join(format!("astroportd-locks-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let db = Arc::new(Db::open(&d.join("u.db"), 2).unwrap());
    let w = db.writer();
    w.execute("INSERT INTO identities(hex, role, email, hosted) VALUES (?1, 'multipass', 'alice@example.org', 1)", vec![hex(&ALICE).into()]).await.unwrap();
    // Alice suit Bob (qui la suit en retour) et Carol (qui ne la suit pas) ; Dave est un ami de Bob (N²).
    for (who, level) in [(&BOB, 1), (&CAROL, 1), (&DAVE, 2)] {
        w.execute("INSERT INTO social_graph(owner_hex, pubkey, level, updated_at) VALUES (?1, ?2, ?3, 0)", vec![hex(&ALICE).into(), hex(who).into(), i64::from(level).into()]).await.unwrap();
    }
    let follows_alice = UnsignedEvent { created_at: 100, kind: 3, tags: vec![vec!["p".into(), hex(&ALICE)]], content: String::new() }.sign(&BOB).unwrap();
    w.transaction(move |tx| insert_event(tx, &follows_alice)).await.unwrap();

    let conf = d.join("station.conf");
    let circle = Arc::new(DbCircle::new());
    let relay = Relay::new(db.clone(), circle.clone(), RelayConfig { policy: KindsControl::initial_policy(&db, &conf).await, ..RelayConfig::default() });
    let kinds = Arc::new(KindsControl::new(db.clone(), &conf, relay.clone()));
    let facts = Facts { origin: true, crawl: true, register: true, p2p: true, ..Facts::default() };
    let locks = Locks::new(db.clone(), kinds.clone(), circle.clone(), facts, ActivityBus::new());
    circle.refresh(&db).await.unwrap();
    let inside = |c: &DbCircle| [&BOB, &CAROL, &DAVE].map(|k| c.in_circle(&hex(k)));

    // Village (défaut) : amis et amis d'amis.
    let v = locks.view().await.unwrap();
    assert_eq!(v["mode"], "village");
    assert_eq!(v["people"], json!({ "hosted": 1, "n1": 2, "n1_mutual": 1, "n2": 1, "n2_known": 1 }));
    assert_eq!((v["gates"]["register"].as_bool(), v["gates"]["n2"].as_bool(), v["gates"]["photos_auto"].as_bool()), (Some(true), Some(true), Some(true)));
    assert_eq!(inside(&circle), [true, true, true]);
    assert!(astro_facecloud::review::auto_share(&db, &hex(&ALICE)).await.unwrap());

    // Forteresse : seul Bob (réciproque) reste ; plus d'inscriptions libres ; toutes les photos attendent un accord.
    let v = locks.set(&json!({ "mode": "forteresse" })).await.unwrap();
    assert_eq!(inside(&circle), [true, false, false], "appliqué sur-le-champ au cercle du relai");
    assert_eq!((v["gates"]["register"].as_bool(), v["gates"]["register_cli"].as_bool(), v["gates"]["mutual_only"].as_bool()), (Some(false), Some(true), Some(true)));
    assert_eq!(v["people"]["n2"], 0);
    assert!(!astro_facecloud::review::auto_share(&db, &hex(&ALICE)).await.unwrap(), "la Forteresse prime sur le réglage du membre");

    // Place publique : tout le monde peut publier son profil et ses notes ; le reste du relai ne bouge pas.
    let v = locks.set(&json!({ "mode": "place" })).await.unwrap();
    assert_eq!(v["gates"]["open_writes"], json!([0, 1]));
    assert_eq!((relay.policy().rule(1).write.name(), relay.policy().rule(7).write.name()), ("open", "circle"));
    assert_eq!(inside(&circle), [true, true, true]);
    assert_eq!(KindsControl::initial_policy(&db, &conf).await.rule(0).write.name(), "open", "le mode survit au redémarrage");
    // Les réglages fins du Capitaine gardent le dernier mot sur le mode.
    kinds.set(&json!({ "rules": { "1": "write=circle" } })).await.unwrap();
    assert_eq!(relay.policy().rule(1).write.name(), "circle");

    // Retour au Village : la porte se referme.
    locks.set(&json!({ "mode": "village" })).await.unwrap();
    assert_eq!(relay.policy().rule(0).write.name(), "circle");
    assert!(locks.set(&json!({ "mode": "grand-ouvert" })).await.is_err());
    let _ = std::fs::remove_dir_all(d);
}
