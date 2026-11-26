//! Test HTTP de bout en bout de la cascade N¹ + DAG de remix + liste officielle WoT + paiement Voie A.
//!
//! Exerce les 6 routes REST sur le vrai routeur `StationUi::into_router` :
//! 1. `POST /api/v1/ucloud/media/remix` (Alice publie sa photo, Bob publie sa fiche botanique en remix recette)
//! 2. `POST /api/v1/ucloud/media/credit/attest` (Bob contresigne son générique sans casser la signature principale)
//! 3. `POST /api/v1/ucloud/media/chain/register` + `GET /api/v1/ucloud/media/chain` (liste officielle WoT)
//! 4. `POST /api/v1/ucloud/media/pay/preview` + `POST /api/v1/ucloud/media/pay` (Voie A : 25 déci-Ẑen → 8 Alice, 12 Bob, 5 Charlie + idempotence).

#![allow(clippy::unwrap_used)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use astro_core::{ActivityBus, Amount};
use astro_crypto::{KeySource, StaticKeySource};
use astro_dav::auth::derive_dav_token;
use astro_dav::{DavConfig, DavState, FsBlobStore};
use astro_db::events::insert_event;
use astro_db::{Db, SqliteReplayStore};
use astro_duniter::{Account, Balance, Chain, ChainError, Ed25519Signer, TransferEvent, TransferMode, TxOutcome};
use astro_economy::cascade::CommonsBlock;
use astro_nostr_proto::{Event, pubkey_of};
use astro_p2p::routing::Hardware;
use astroportd::coop_wallets::Wallets;
use astroportd::media_share::MediaShare;
use astroportd::register::PublishFn;
use astroportd::station::{StationDeps, StationInfo, StationServices};
use astroportd::ui_backend::{MediaFriendsFns, StationUi};
use astroportd::zen_credit::ZenCredit;
use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use base64::Engine;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

const SECRET: &[u8] = b"secret-cascade-e2e";
const ALICE: [u8; 32] = [0xA1; 32];
const BOB: [u8; 32] = [0xB2; 32];
const CHARLIE: [u8; 32] = [0xC3; 32];
const DAVID: [u8; 32] = [0xD4; 32];

fn pk(sk: &[u8; 32]) -> String {
    pubkey_of(sk).unwrap()
}

fn basic(email: &str, sk: &[u8; 32]) -> String {
    format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{email}:{}", derive_dav_token(SECRET, &pk(sk)))))
}

#[derive(Default)]
struct FakeChain {
    balances: Mutex<HashMap<Account, u64>>,
    sent: Mutex<Vec<(Account, Account, u64, String)>>,
}

impl Chain for FakeChain {
    async fn balance(&self, account: &Account) -> Result<Balance, ChainError> {
        let total = *self.balances.lock().unwrap().get(account).unwrap_or(&0);
        Ok(Balance { total: Amount(total), transferable: Amount(total), unclaimed_uds: Amount(0) })
    }
    async fn transfer(&self, from: &Ed25519Signer, to: &Account, amount: Amount, _: TransferMode, memo: Option<&str>) -> Result<TxOutcome, ChainError> {
        let mut b = self.balances.lock().unwrap();
        let src = b.get(&from.account()).copied().unwrap_or(0);
        if src < amount.0 + 100 {
            return Err(ChainError::Rejected("solde insuffisant".into()));
        }
        b.insert(from.account(), src - amount.0);
        *b.entry(*to).or_insert(0) += amount.0;
        self.sent.lock().unwrap().push((from.account(), *to, amount.0, memo.unwrap_or("").to_owned()));
        Ok(TxOutcome { block_hash: [0x01; 32], transferred: amount })
    }
    async fn watch_transfers(&self, _: tokio::sync::mpsc::Sender<TransferEvent>) -> Result<(), ChainError> {
        std::future::pending().await
    }
    fn existential_deposit(&self) -> Amount {
        Amount(100)
    }
}

async fn api(app: &Router, method: Method, uri: &str, auth: &str, body: Option<Value>) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(uri).header(header::AUTHORIZATION, auth);
    let body = match body {
        Some(v) => {
            req = req.header(header::CONTENT_TYPE, "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    let resp = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let st = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or_else(|_| json!({ "raw": String::from_utf8_lossy(&bytes) }));
    (st, json)
}

#[tokio::test]
async fn cascade_http_routes_end_to_end() {
    let dir = std::env::temp_dir().join(format!("astroportd-cascade-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("blobs")).unwrap();

    let db = Arc::new(Db::open(&dir.join("u.db"), 4).unwrap());
    let blobs = Arc::new(FsBlobStore::open(dir.join("blobs")).unwrap());
    let dav = Arc::new(DavState {
        db: db.clone(),
        blobs,
        replay: Arc::new(SqliteReplayStore::in_memory().unwrap()),
        publisher: None,
        config: DavConfig::new(SECRET.to_vec()),
        on_upload: None,
    });
    let keys: Arc<dyn KeySource> = Arc::new(StaticKeySource::new().with(ALICE).with(BOB).with(CHARLIE).with(DAVID));
    let chain = Arc::new(FakeChain::default());
    let bus = ActivityBus::new();

    let accounts = [
        (&ALICE, "alice@exemple.org", "captain", 0x71u8, 100u64),
        (&BOB, "bob@exemple.org", "multipass", 0x72u8, 100u64),
        (&CHARLIE, "charlie@exemple.org", "captain", 0x73u8, 100u64),
        (&DAVID, "david@exemple.org", "multipass", 0x74u8, 1_100u64),
    ];
    let mut acc_ids = HashMap::new();
    for (sk, email, role, seed, bal) in accounts {
        let hex = pk(sk);
        db.writer()
            .execute(
                "INSERT INTO identities(hex, role, email, hosted) VALUES (?1, ?2, ?3, 1)",
                vec![hex.clone().into(), role.to_owned().into(), email.to_owned().into()],
            )
            .await
            .unwrap();
        std::fs::create_dir_all(dir.join("nostr").join(email)).unwrap();
        std::fs::write(dir.join("nostr").join(email).join(".secret.dunikey"), astro_lifecycle::dunikey::render(&[seed; 32])).unwrap();
        let acc = Ed25519Signer::from_seed([seed; 32]).account();
        chain.balances.lock().unwrap().insert(acc, bal);
        acc_ids.insert(email, acc);
    }

    let wallets = Arc::new(Wallets::new("UPlanetCascadeE2E".into(), db.clone()));
    let db_pub = db.clone();
    let publish: PublishFn = Arc::new(move |ev: Event| {
        let db = db_pub.clone();
        Box::pin(async move { db.writer().transaction(move |tx| insert_event(tx, &ev).map(|_| ())).await.map_err(|e| e.to_string()) })
    });
    let zc = Arc::new(
        ZenCredit::new(db.clone(), chain.clone(), wallets, dir.join("nostr"), bus.clone()).with_cascade_hooks(
            keys.clone(),
            publish.clone(),
            Arc::new(|_, _| Box::pin(async {})),
        ),
    );
    let ms = Arc::new(MediaShare::new(dav.clone(), keys, publish));
    let (ms1, ms2, ms3, ms4, ms5, ms6) = (ms.clone(), ms.clone(), ms.clone(), ms.clone(), ms.clone(), ms);

    let info = StationInfo { node_hex: pk(&[1; 32]), peer_id: Some("12D3KooWE2E".into()), domain: Some("station.test".into()), started: Instant::now() };
    let services = Arc::new(StationServices::new(StationDeps {
        db: db.clone(),
        info,
        nostr_dir: None,
        chain: None,
        hardware: Hardware { vram_gb: 0.0, cpu_cores: 4, ram_gb: 8.0 },
        p2p: None,
        weekly_rent_cents: 100,
        activity: bus.clone(),
        swarm_dir: None,
        origin: None,
        exposed: vec![],
        weekly_costs_cents: 0,
        uplanet_name: "UPlanetCascadeE2E".into(),
    }));

    let app = StationUi::new(services, dav.clone(), None, None, false)
        .with_zen_credit(Arc::new(move |_ip, op, body| {
            let svc = zc.clone();
            Box::pin(async move {
                match op.strip_prefix("member_") {
                    Some(op) => svc.handle_member(op, &body).await,
                    None => svc.handle(op, &body).await,
                }
            })
        }))
        .with_media_friends(MediaFriendsFns {
            list: Arc::new(|_, _| Box::pin(async { Ok(json!({ "items": [], "friends": [] })) })),
            request: Arc::new(|_, _| Box::pin(async { Err("unused".into()) })),
            share: Arc::new(move |o, p, on, l| {
                let m = ms1.clone();
                Box::pin(async move { m.set(&o, &p, on, l.map(|(k, t)| astroportd::media_share::SharedList { kind: k, title: t })).await })
            }),
            trace: Arc::new(move |o, p| {
                let m = ms2.clone();
                Box::pin(async move { m.trace(&o, &p).await })
            }),
            chain_info: Arc::new(move |o, p| {
                let m = ms3.clone();
                Box::pin(async move { m.chain_info(&o, &p).await })
            }),
            register_official: Arc::new(move |o, p, r| {
                let m = ms4.clone();
                Box::pin(async move { m.register_official(&o, &p, r).await })
            }),
            publish_remix: Arc::new(move |o, body| {
                let m = ms5.clone();
                Box::pin(async move {
                    let req: astroportd::cascade_share::PublishRemixRequest = serde_json::from_value(body).map_err(|e| e.to_string())?;
                    m.publish_remix(&o, req).await
                })
            }),
            attest_credit: Arc::new(move |o, p| {
                let m = ms6.clone();
                Box::pin(async move { m.attest_credit(&o, &p).await })
            }),
        })
        .into_router(bus, None);

    let auth_a = basic("alice@exemple.org", &ALICE);
    let auth_b = basic("bob@exemple.org", &BOB);
    let auth_d = basic("david@exemple.org", &DAVID);

    // 1. Alice dépose sa photo et la publie via POST /api/v1/ucloud/media/remix
    astro_dav::ingest::replace(&dav, &pk(&ALICE), "/Photos", "ortie.jpg", Some("image/jpeg"), &b"photo-ortie"[..]).await.unwrap();
    let (st, res_a) = api(
        &app,
        Method::POST,
        "/api/v1/ucloud/media/remix",
        &auth_a,
        Some(json!({
            "title": "Ortie au soleil",
            "kind": "photo",
            "spdx": "CC-BY-SA-4.0",
            "allow_remix": true,
            "remix_min_bps": 4000,
            "pricing": { "mode": "hat", "suggested_deci_zen": 10 },
            "file_path": "/Photos/ortie.jpg"
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{res_a}");
    let fiche_a_path = res_a["fiche_path"].as_str().unwrap().to_owned();

    // Copie de la fiche A chez Bob + remix botanique recette via HTTP
    let raw_a = astro_dav::carddav::read_file(&dav, &pk(&ALICE), &fiche_a_path).await.unwrap();
    astro_dav::ingest::replace(&dav, &pk(&BOB), "/Partages/De_alice", "ortie.media.json", Some("application/json"), raw_a.as_slice()).await.unwrap();
    let (st, res_b) = api(
        &app,
        Method::POST,
        "/api/v1/ucloud/media/remix",
        &auth_b,
        Some(json!({
            "title": "Urtica dioica",
            "kind": "plant",
            "spdx": "CC-BY-SA-4.0",
            "allow_remix": true,
            "remix_min_bps": 3000,
            "pricing": { "mode": "fixed", "price_deci_zen": 25 },
            "parent_fiche_path": "/Partages/De_alice/ortie.media.json",
            "parent_share_bps": 4000,
            "plant": { "scientific": "Urtica dioica", "common": ["Grande ortie"], "family": "Urticaceae", "umap": "_43.61_1.44", "edible": "Jeunes pousses cuites" }
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{res_b}");
    let fiche_b_path = res_b["fiche_path"].as_str().unwrap().to_owned();

    // 2. Contresignature du générique par Bob via POST /api/v1/ucloud/media/credit/attest
    let (st, att) = api(&app, Method::POST, "/api/v1/ucloud/media/credit/attest", &auth_b, Some(json!({ "path": fiche_b_path }))).await;
    assert_eq!(st, StatusCode::OK, "{att}");
    assert_eq!((att["attested"].as_bool(), att["commons_valid"].as_bool()), (Some(true), Some(true)));

    // Vérification P0 : la fiche recette (sans `file`) se partage dans la médiathèque et son DAG est vérifié localement
    let (st, sh_b) = api(&app, Method::POST, "/api/v1/ucloud/media/share", &auth_b, Some(json!({ "paths": [fiche_b_path], "on": true }))).await;
    assert_eq!(st, StatusCode::OK, "{sh_b}");
    assert_eq!(sh_b["shared"].as_bool(), Some(true));
    let (st, info_b) = api(&app, Method::GET, &format!("/api/v1/ucloud/media/chain?path={fiche_b_path}"), &auth_b, None).await;
    assert_eq!(st, StatusCode::OK, "{info_b}");
    assert_eq!((info_b["dag_verified"].as_bool(), info_b["can_attest"].as_bool()), (Some(true), Some(false)));

    // 3. Inscription d'Alice (Capitaine = racine WoT) sur la liste officielle + lecture via GET /api/v1/ucloud/media/chain
    let (st, reg) = api(&app, Method::POST, "/api/v1/ucloud/media/chain/register", &auth_a, Some(json!({ "path": fiche_a_path, "register": true }))).await;
    assert_eq!(st, StatusCode::OK, "{reg}");
    let (st, info) = api(&app, Method::GET, &format!("/api/v1/ucloud/media/chain?path={fiche_a_path}"), &auth_a, None).await;
    assert_eq!(st, StatusCode::OK, "{info}");
    assert_eq!((info["official"]["wot_count"].as_u64(), info["official"]["registered"].as_bool()), (Some(1), Some(true)));

    // 4. Fiche B (recette) transmise de Bob -> Charlie -> David via extend_for_send : le work_sha suit commons.content_sha !
    let bob_files = astroportd::sharing::resolve_files(&db, &pk(&BOB), std::slice::from_ref(&fiche_b_path)).await.unwrap();
    let (_, file_b) = &bob_files[0];
    let commons_b: CommonsBlock = serde_json::from_value(res_b["commons"].clone()).unwrap();
    let c1 = astroportd::cascade_share::extend_for_send(&db, &pk(&BOB), &BOB, &pk(&CHARLIE), file_b, None, 1_790_000_001).await.unwrap();
    assert_eq!(c1.work_sha, commons_b.content_sha, "la chaîne recette porte commons.content_sha, pas le sha brut du JSON");
    assert!(astroportd::cascade_share::save_chain(&db, &pk(&CHARLIE), &c1).await);
    let c2 = astroportd::cascade_share::extend_for_send(&db, &pk(&CHARLIE), &CHARLIE, &pk(&DAVID), file_b, None, 1_790_000_002).await.unwrap();
    assert!(astroportd::cascade_share::save_chain(&db, &pk(&DAVID), &c2).await);

    let raw_b = astro_dav::carddav::read_file(&dav, &pk(&BOB), &fiche_b_path).await.unwrap();
    let david_fiche = "/Partages/De_charlie/urtica_dioica.media.json";
    astro_dav::ingest::replace(&dav, &pk(&DAVID), "/Partages/De_charlie", "urtica_dioica.media.json", Some("application/json"), raw_b.as_slice()).await.unwrap();

    // Chez David : can_attest == false, paid_by_me == null avant paiement
    let (st, info_d) = api(&app, Method::GET, &format!("/api/v1/ucloud/media/chain?path={david_fiche}"), &auth_d, None).await;
    assert_eq!(st, StatusCode::OK, "{info_d}");
    assert_eq!((info_d["can_attest"].as_bool(), info_d["paid_by_me"].is_null()), (Some(false), true));

    // Aperçu Voie A par David via HTTP
    let (st, prev) = api(&app, Method::POST, "/api/v1/ucloud/media/pay/preview", &auth_d, Some(json!({ "path": david_fiche, "amount_deci_zen": 25 }))).await;
    assert_eq!(st, StatusCode::OK, "{prev}");
    assert_eq!((prev["payable"].as_bool(), prev["g1_cents"].as_u64(), prev["lines"].as_array().map(Vec::len)), (Some(true), Some(25), Some(3)));

    // Paiement effectif Voie A via HTTP avec mot de merci (#2) + reçu Nostr kind 30852 (#7)
    let (st, paid) = api(
        &app,
        Method::POST,
        "/api/v1/ucloud/media/pay",
        &auth_d,
        Some(json!({ "path": david_fiche, "amount_deci_zen": 25, "credit_id": "e2e-cascade-1", "confirm": true, "note": "Merci pour l'identification !", "anonymous": false })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{paid}");
    assert_eq!((paid["ok"].as_bool(), paid["already"].as_bool(), paid["transfers"].as_u64()), (Some(true), Some(false), Some(3)));
    assert!(paid["receipt_event_id"].as_str().is_some(), "reçu Nostr kind 30852 publié");
    let bal = chain.balances.lock().unwrap().clone();
    assert_eq!(
        (bal[&acc_ids["alice@exemple.org"]], bal[&acc_ids["bob@exemple.org"]], bal[&acc_ids["charlie@exemple.org"]], bal[&acc_ids["david@exemple.org"]]),
        (108, 112, 105, 1_075)
    );

    // #1 Mes retombées : Alice (8 création), Bob (12 création), Charlie (5 relais) + mot de merci
    let auth_c = basic("charlie@exemple.org", &CHARLIE);
    let (st, earn_b) = api(&app, Method::GET, "/api/v1/ucloud/media/earnings", &auth_b, None).await;
    assert_eq!(st, StatusCode::OK, "{earn_b}");
    assert_eq!((earn_b["total_received_deci_zen"].as_u64(), earn_b["creation_deci_zen"].as_u64(), earn_b["recent"][0]["note"].as_str()), (Some(12), Some(12), Some("Merci pour l'identification !")));
    let (st, earn_c) = api(&app, Method::GET, "/api/v1/ucloud/media/earnings", &auth_c, None).await;
    assert_eq!(st, StatusCode::OK, "{earn_c}");
    assert_eq!((earn_c["total_received_deci_zen"].as_u64(), earn_c["relay_deci_zen"].as_u64()), (Some(5), Some(5)));

    // Après paiement : paid_by_me est visible dans chain_info
    let (st, info_d2) = api(&app, Method::GET, &format!("/api/v1/ucloud/media/chain?path={david_fiche}"), &auth_d, None).await;
    assert_eq!(st, StatusCode::OK, "{info_d2}");
    assert_eq!(info_d2["paid_by_me"]["amount_deci_zen"].as_u64(), Some(25));

    // Rejeu idempotent
    let (st, again) = api(
        &app,
        Method::POST,
        "/api/v1/ucloud/media/pay",
        &auth_d,
        Some(json!({ "path": david_fiche, "amount_deci_zen": 25, "credit_id": "e2e-cascade-1", "confirm": true })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{again}");
    assert_eq!(again["already"].as_bool(), Some(true));
    assert_eq!(chain.sent.lock().unwrap().len(), 3);

    // #6 Atlas botanique collaboratif : David voit l'ortie sur sa maille UMAP _43.61_1.44
    let (st, atlas) = api(&app, Method::GET, "/api/v1/ucloud/plants/atlas", &auth_d, None).await;
    assert_eq!(st, StatusCode::OK, "{atlas}");
    assert_eq!(atlas["count"].as_u64(), Some(1));
    assert_eq!(atlas["points"][0]["umap"].as_str(), Some("_43.61_1.44"));

    // #4 Sous-titres en recette (.vtt) + #5 Document PDF et Article de blog
    astro_dav::ingest::replace(&dav, &pk(&ALICE), "/Documents", "guide.pdf", Some("application/pdf"), &b"%PDF-1.4"[..]).await.unwrap();
    let (st, doc_res) = api(
        &app,
        Method::POST,
        "/api/v1/ucloud/media/remix",
        &auth_a,
        Some(json!({ "title": "Guide permaculture", "kind": "document", "spdx": "CC-BY-SA-4.0", "pricing": { "mode": "hat", "suggested_deci_zen": 10 }, "file_path": "/Documents/guide.pdf" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{doc_res}");
    let (st, art_res) = api(
        &app,
        Method::POST,
        "/api/v1/ucloud/media/remix",
        &auth_a,
        Some(json!({ "title": "Notes sur les orties", "kind": "article", "spdx": "CC-BY-4.0", "pricing": { "mode": "hat", "suggested_deci_zen": 10 }, "overview": "Un texte libre et partageable sur les usages culinaires." })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{art_res}");
    astro_dav::ingest::replace(&dav, &pk(&BOB), "/SousTitres", "ortie.fr.vtt", Some("text/vtt"), &b"WEBVTT\n\n00:00:00.000 --> 00:00:02.000\nBonjour\n"[..]).await.unwrap();
    let (st, sub_res) = api(
        &app,
        Method::POST,
        "/api/v1/ucloud/media/remix",
        &auth_b,
        Some(json!({
            "title": "Ortie (sous-titres FR)", "kind": "video", "spdx": "CC-BY-SA-4.0",
            "pricing": { "mode": "hat", "suggested_deci_zen": 10 },
            "parent_fiche_path": "/Partages/De_alice/ortie.media.json", "parent_share_bps": 6000,
            "subtitle_path": "/SousTitres/ortie.fr.vtt", "subtitle_lang": "fr"
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{sub_res}");
    assert_eq!(sub_res["item"]["subtitles"][0]["lang"].as_str(), Some("fr"));

    // #3 Ardoise groupée : David empile 2 œuvres (la fiche B + l'article d'Alice copié chez lui) et règle en 1 lot fusionné !
    let raw_art = astro_dav::carddav::read_file(&dav, &pk(&ALICE), art_res["fiche_path"].as_str().unwrap()).await.unwrap();
    let david_art = "/Partages/De_alice/notes_sur_les_orties.media.json";
    astro_dav::ingest::replace(&dav, &pk(&DAVID), "/Partages/De_alice", "notes_sur_les_orties.media.json", Some("application/json"), raw_art.as_slice()).await.unwrap();

    let (st, sl1) = api(&app, Method::POST, "/api/v1/ucloud/media/slate", &auth_d, Some(json!({ "path": david_fiche, "amount_deci_zen": 25 }))).await;
    assert_eq!(st, StatusCode::OK, "{sl1}");
    let (st, sl2) = api(&app, Method::POST, "/api/v1/ucloud/media/slate", &auth_d, Some(json!({ "path": david_art, "amount_deci_zen": 10 }))).await;
    assert_eq!(st, StatusCode::OK, "{sl2}");
    assert_eq!((sl2["count"].as_u64(), sl2["total_deci_zen"].as_u64()), (Some(2), Some(35)));

    let (st, sl_prev) = api(&app, Method::POST, "/api/v1/ucloud/media/slate/settle", &auth_d, Some(json!({ "preview": true }))).await;
    assert_eq!(st, StatusCode::OK, "{sl_prev}");
    // Alice apparaît dans les 2 œuvres (8 + 10 = 18) : fusionnée en 1 seule ligne -> 3 virements au total (Alice 18, Bob 12, Charlie 5) !
    assert_eq!((sl_prev["payable"].as_bool(), sl_prev["g1_cents"].as_u64(), sl_prev["lines"].as_array().map(Vec::len)), (Some(true), Some(35), Some(3)));

    let (st, sl_paid) = api(&app, Method::POST, "/api/v1/ucloud/media/slate/settle", &auth_d, Some(json!({ "preview": false, "credit_id": "e2e-slate-1", "confirm": true }))).await;
    assert_eq!(st, StatusCode::OK, "{sl_paid}");
    assert_eq!((sl_paid["ok"].as_bool(), sl_paid["transfers"].as_u64(), sl_paid["g1_cents"].as_u64()), (Some(true), Some(3), Some(35)));
    let (st, sl_empty) = api(&app, Method::GET, "/api/v1/ucloud/media/slate", &auth_d, None).await;
    assert_eq!(st, StatusCode::OK, "{sl_empty}");
    assert_eq!(sl_empty["count"].as_u64(), Some(0), "ardoise vidée après règlement");

    let _ = std::fs::remove_dir_all(dir);
}
