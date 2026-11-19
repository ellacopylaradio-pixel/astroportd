//! Médiathèque des amis : la liste des films partagés par un proche réciproque, la copie au clic « Lire » (rangée au même
//! endroit que chez l'ami, fiche et affiche comprises), et les refus (fichier non partagé, personne hors du cercle).

#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use astro_crypto::nip59::{Rumor, gift_wrap};
use astro_crypto::{KeySource, StaticKeySource};
use astro_dav::{DavConfig, DavState, FsBlobStore};
use astro_db::events::insert_event;
use astro_db::{Db, SqliteReplayStore};
use astro_facecloud::{Outbox, OutboxError};
use astro_nostr_proto::{Event, UnsignedEvent, pubkey_of};
use astroportd::friends_media::{FriendsMedia, TAG};
use astroportd::sharing::Sharing;
use serde_json::json;
use sha2::{Digest, Sha256};

const ALICE: [u8; 32] = [0xA1; 32];
const BOB: [u8; 32] = [0xB2; 32];
const CAROL: [u8; 32] = [0xC3; 32];

fn hex(s: &[u8; 32]) -> String {
    pubkey_of(s).unwrap()
}

/// Boîte d'envoi hors ligne : ici, tout le monde est hébergé sur la même station.
struct Offline;
impl Outbox for Offline {
    async fn send(&self, _: &str, _: Event) -> Result<(), OutboxError> {
        Err(OutboxError("hors ligne".into()))
    }
}

struct Rig {
    db: Arc<Db>,
    dav: Arc<DavState<FsBlobStore>>,
    fm: FriendsMedia<Offline>,
    dir: std::path::PathBuf,
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn rig(tag: &str) -> Rig {
    let dir = std::env::temp_dir().join(format!("astroportd-fm-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
    for (s, email) in [(&ALICE, "alice@example.org"), (&BOB, "bob@example.org"), (&CAROL, "carol@example.org")] {
        db.writer().execute("INSERT INTO identities(hex, role, email, hosted) VALUES (?1, 'multipass', ?2, 1)", vec![hex(s).into(), email.to_owned().into()]).await.unwrap();
    }
    // Alice et Bob : proches réciproques. Carol suit Bob, qui ne la suit pas.
    for (a, b) in [(&ALICE, &BOB), (&BOB, &ALICE), (&CAROL, &BOB)] {
        db.writer().execute("INSERT INTO social_graph(owner_hex, pubkey, level, updated_at) VALUES (?1, ?2, 1, 0)", vec![hex(a).into(), hex(b).into()]).await.unwrap();
        let ev = UnsignedEvent { created_at: 100, kind: 3, tags: vec![vec!["p".into(), hex(b)]], content: String::new() }.sign(a).unwrap();
        db.writer().transaction(move |tx| insert_event(tx, &ev).map(|_| ())).await.unwrap();
    }
    let blobs = Arc::new(FsBlobStore::open(dir.join("blobs")).unwrap());
    let dav = Arc::new(DavState { db: db.clone(), blobs, replay: Arc::new(SqliteReplayStore::in_memory().unwrap()), publisher: None, config: DavConfig::new(vec![7; 32]), on_upload: None });
    let keys: Arc<dyn KeySource> = Arc::new(StaticKeySource::new().with(ALICE).with(BOB).with(CAROL));
    let outbox = Arc::new(Offline);
    let sharing = Arc::new(Sharing::new(db.clone(), keys.clone(), outbox.clone(), None, None));
    let fm = FriendsMedia::new(db.clone(), keys, outbox, sharing, None);
    Rig { db, dav, fm, dir }
}

async fn put(r: &Rig, who: &[u8; 32], dir: &str, name: &str, data: &[u8]) {
    astro_dav::ingest::ingest(&r.dav, &hex(who), dir, name, None, data).await.unwrap();
}

async fn publish(r: &Rig, who: &[u8; 32], title: &str, data: &[u8]) -> String {
    let x = hex::encode(Sha256::digest(data));
    let ev = UnsignedEvent { created_at: 200, kind: 21, tags: vec![vec!["title".into(), title.into()], vec!["x".into(), x], vec!["t".into(), "film".into()], vec!["-".into()]], content: String::new() }.sign(who).unwrap();
    let id = ev.id.clone();
    r.db.writer().transaction(move |tx| insert_event(tx, &ev).map(|_| ())).await.unwrap();
    id
}

async fn paths(r: &Rig, who: &[u8; 32]) -> Vec<String> {
    let h = hex(who);
    r.db.readers().read(move |c| Ok(c.prepare("SELECT path FROM ucloud_entries WHERE owner_hex = ?1 AND type = 'file' ORDER BY path")?.query_map([h], |r| r.get(0))?.collect::<Result<Vec<String>, _>>()?)).await.unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_friends_film_is_listed_then_copied_on_play_into_the_same_folder_with_its_fiche() {
    let r = rig("copy").await;
    let film: Vec<u8> = (0..50_000u32).map(|i| (i % 253) as u8).collect();
    put(&r, &BOB, "/Vidéos/Films", "Chihiro (2001).mp4", &film).await;
    put(&r, &BOB, "/Vidéos/Films", "Chihiro (2001).poster.jpg", b"\xff\xd8\xff affiche").await;
    let fiche = json!({ "schema": "media/v1", "generated_at": 1, "type": "film", "file": "Chihiro (2001).mp4", "title": "Chihiro", "artwork": { "poster": "Chihiro (2001).poster.jpg" }, "technical": { "duration_s": 7500.0 }, "source": { "kind": "disc", "rights": "private_copy" }, "sharing": { "level": "circle" } });
    put(&r, &BOB, "/Vidéos/Films", "Chihiro (2001).media.json", fiche.to_string().as_bytes()).await;
    put(&r, &BOB, "/Vidéos/Films", "Secret.mp4", b"pas pour les amis").await;
    let event = publish(&r, &BOB, "Chihiro (2001)", &film).await;

    // La liste d'Alice : le film de Bob, pas encore chez elle.
    let list = r.fm.list(&hex(&ALICE), false).await.unwrap();
    assert_eq!(list["items"].as_array().unwrap().len(), 1, "{list}");
    let item = &list["items"][0];
    assert_eq!((item["title"].as_str(), item["author_label"].as_str(), item["copied"].is_null()), (Some("Chihiro (2001)"), Some("bob"), true));

    // « Lire » : la copie est faite (Bob est hébergé ici), rangée comme chez lui, fiche et affiche comprises.
    assert_eq!(r.fm.request(&hex(&ALICE), &event).await.unwrap()["state"], "sent");
    assert_eq!(paths(&r, &ALICE).await, ["/Vidéos/Films/Chihiro (2001).media.json", "/Vidéos/Films/Chihiro (2001).mp4", "/Vidéos/Films/Chihiro (2001).poster.jpg"]);
    let list = r.fm.list(&hex(&ALICE), false).await.unwrap();
    assert_eq!(list["items"][0]["copied"], "/Vidéos/Films/Chihiro (2001).mp4");
    assert_eq!(r.fm.request(&hex(&ALICE), &event).await.unwrap()["state"], "copied", "déjà là : rien n'est redemandé");
    // La copie entre dans la médiathèque d'Alice comme un film à elle, et se lit.
    let mine = astroportd::media::library(&r.dav, &hex(&ALICE)).await.unwrap();
    assert_eq!(mine["items"][0]["path"], "/Vidéos/Films/Chihiro (2001).mp4");
    // La fiche entre dans la recherche multimédia, rattachée au film par son empreinte ; elle en sort avec la fiche.
    let found = |q: &'static str| {
        let (db, me) = (r.db.clone(), hex(&ALICE));
        async move { db.readers().read(move |c| Ok(c.prepare("SELECT sha FROM search_text WHERE search_text MATCH ?1 AND owner_hex = ?2")?.query_map([q, me.as_str()], |r| r.get(0))?.collect::<Result<Vec<String>, _>>()?)).await.unwrap() }
    };
    assert_eq!(found("chihiro").await, [hex::encode(Sha256::digest(&film))]);
    let me = hex(&ALICE);
    r.db.writer().execute("DELETE FROM ucloud_entries WHERE owner_hex = ?1 AND path GLOB '*.media.json'", vec![me.into()]).await.unwrap();
    astroportd::media::library(&r.dav, &hex(&ALICE)).await.unwrap();
    assert!(found("chihiro").await.is_empty(), "fiche retirée : plus trouvée");
    assert_eq!(astro_dav::carddav::read_file(&r.dav, &hex(&ALICE), "/Vidéos/Films/Chihiro (2001).mp4").await.unwrap(), film);

    // Refus : un fichier que Bob n'a pas publié pour son cercle, une personne hors de son cercle.
    let secret = hex::encode(Sha256::digest(b"pas pour les amis"));
    assert!(r.fm.serve(&hex(&BOB), &hex(&ALICE), &secret, 1000).await.unwrap_err().contains("pas partagé"));
    let x = hex::encode(Sha256::digest(&film));
    assert!(r.fm.serve(&hex(&BOB), &hex(&CAROL), &x, 1000).await.unwrap_err().contains("proche réciproque"));
    assert!(r.fm.list(&hex(&CAROL), false).await.unwrap()["items"].as_array().unwrap().is_empty(), "Carol ne voit rien de Bob");
}

#[tokio::test(flavor = "multi_thread")]
async fn before_a_deposit_the_same_work_is_found_at_a_friends_by_its_catalogue_id() {
    use astroportd::friends_media::{Work, best_copy};
    let r = rig("work").await;
    let film: Vec<u8> = (0..30_000u32).map(|i| (i % 241) as u8).collect();
    put(&r, &BOB, "/Vidéos/Films", "Chihiro (2001).mp4", &film).await;
    let x = hex::encode(Sha256::digest(&film));
    let tags = [["title", "Chihiro (2001)"], ["x", &x], ["t", "film"], ["i", "tmdb:129"], ["-", ""]].map(|t| t.iter().filter(|s| !s.is_empty()).map(|s| (*s).to_owned()).collect::<Vec<_>>()).to_vec();
    let ev = UnsignedEvent { created_at: 200, kind: 21, tags, content: String::new() }.sign(&BOB).unwrap();
    let event = ev.id.clone();
    r.db.writer().transaction(move |tx| insert_event(tx, &ev).map(|_| ())).await.unwrap();

    let chihiro = Work { tmdb: Some(129), ..Work::default() };
    let items = |v: serde_json::Value| v["items"].as_array().cloned().unwrap_or_default();
    let alice = items(r.fm.list(&hex(&ALICE), false).await.unwrap());
    let found = best_copy(&alice, &chihiro).unwrap(); // Bob a ce film
    assert_eq!((found["event_id"].as_str(), found["author_label"].as_str(), found["copied"].is_null()), (Some(event.as_str()), Some("bob"), true));
    assert!(best_copy(&alice, &Work { tmdb: Some(130), ..Work::default() }).is_none(), "un autre film");
    assert!(best_copy(&items(r.fm.list(&hex(&CAROL), false).await.unwrap()), &chihiro).is_none(), "hors du cercle de Bob : rien");
    // La copie demandée est le même fichier ; ensuite, l'œuvre est « déjà chez vous ».
    assert_eq!(r.fm.request(&hex(&ALICE), &event).await.unwrap()["state"], "sent");
    let alice = items(r.fm.list(&hex(&ALICE), false).await.unwrap());
    assert_eq!(best_copy(&alice, &chihiro).unwrap()["copied"], "/Vidéos/Films/Chihiro (2001).mp4");
    assert_eq!(cid_of(&r, &ALICE, "/Vidéos/Films/Chihiro (2001).mp4").await, cid_of(&r, &BOB, "/Vidéos/Films/Chihiro (2001).mp4").await, "un seul blob sur le disque");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_from_another_station_is_served_by_the_inbox_and_an_unshared_file_is_not() {
    let r = rig("inbox").await;
    let film = b"un court metrage".to_vec();
    put(&r, &BOB, "/Vidéos/Autres", "Court.mp4", &film).await;
    publish(&r, &BOB, "Court", &film).await;
    let x = hex::encode(Sha256::digest(&film));
    // Une demande arrive comme elle viendrait d'une autre station : enveloppe NIP-59 d'Alice pour Bob, posée sur le relai.
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let wrap = |x: &str| {
        let rumor = Rumor { pubkey: hex(&ALICE), created_at: now, kind: 30078, tags: vec![vec!["p".into(), hex(&BOB)], vec!["t".into(), TAG.into()], vec!["op".into(), "request".into()], vec!["x".into(), x.to_owned()]], content: String::new() };
        gift_wrap(&ALICE, &hex(&BOB), &rumor, now).unwrap()
    };
    for w in [wrap(&x), wrap(&"00".repeat(32))] {
        r.db.writer().transaction(move |tx| insert_event(tx, &w).map(|_| ())).await.unwrap();
    }
    let now = now as i64;
    assert_eq!(r.fm.poll_once(now).await, 2, "les deux demandes sont lues");
    assert_eq!(paths(&r, &ALICE).await, ["/Vidéos/Autres/Court.mp4"], "la demande valable est servie, au même endroit");
    assert_eq!(r.fm.poll_once(now).await, 0, "chaque enveloppe n'est lue qu'une fois");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_same_film_shared_by_two_friends_is_one_entry_with_two_seeders() {
    let r = rig("seeders").await;
    // Carol devient aussi une proche réciproque d'Alice ; elle et Bob ont le même film (même empreinte).
    for (a, b) in [(&ALICE, &CAROL), (&CAROL, &ALICE)] {
        r.db.writer().execute("INSERT OR IGNORE INTO social_graph(owner_hex, pubkey, level, updated_at) VALUES (?1, ?2, 1, 0)", vec![hex(a).into(), hex(b).into()]).await.unwrap();
        let ev = UnsignedEvent { created_at: 101, kind: 3, tags: vec![vec!["p".into(), hex(b)], vec!["p".into(), hex(&BOB)]], content: String::new() }.sign(a).unwrap();
        r.db.writer().transaction(move |tx| insert_event(tx, &ev).map(|_| ())).await.unwrap();
    }
    let film = b"le meme film chez deux amis".to_vec();
    for who in [&BOB, &CAROL] {
        put(&r, who, "/Vidéos/Films", "Commun.mp4", &film).await;
        publish(&r, who, "Commun", &film).await;
    }
    let list = r.fm.list(&hex(&ALICE), true).await.unwrap();
    assert_eq!(list["items"].as_array().unwrap().len(), 1, "une seule entrée : {list}");
    let mut seeders: Vec<&str> = list["items"][0]["seeders"].as_array().unwrap().iter().map(|s| s["label"].as_str().unwrap()).collect();
    seeders.sort_unstable();
    assert_eq!(seeders, ["bob", "carol"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_goes_to_a_source_of_the_most_widespread_blob() {
    let r = rig("converge").await;
    const DAVE: [u8; 32] = [0xD4; 32];
    r.db.writer().execute("INSERT INTO identities(hex, role, email, hosted) VALUES (?1, 'multipass', 'dave@example.org', 1)", vec![hex(&DAVE).into()]).await.unwrap();
    for (a, b) in [(&ALICE, &CAROL), (&CAROL, &ALICE), (&ALICE, &DAVE), (&DAVE, &ALICE)] {
        r.db.writer().execute("INSERT OR IGNORE INTO social_graph(owner_hex, pubkey, level, updated_at) VALUES (?1, ?2, 1, 0)", vec![hex(a).into(), hex(b).into()]).await.unwrap();
    }
    for (who, peers) in [(&ALICE, vec![&BOB, &CAROL, &DAVE]), (&CAROL, vec![&ALICE, &BOB]), (&DAVE, vec![&ALICE])] {
        let ev = UnsignedEvent { created_at: 150, kind: 3, tags: peers.iter().map(|p| vec!["p".into(), hex(p)]).collect(), content: String::new() }.sign(who).unwrap();
        r.db.writer().transaction(move |tx| insert_event(tx, &ev).map(|_| ())).await.unwrap();
    }
    // Le même film : Bob l'a déposé lui-même (blob B) ; Carol et Dave ont le blob A (Dave le tient de Carol).
    let x = "ee".repeat(32);
    let cid = |seed: &[u8]| astro_dav::blob::cid_raw_sha256(&Sha256::digest(seed).into());
    let (a, b) = (cid(b"blob A"), cid(b"blob B"));
    for (who, c) in [(&BOB, &b), (&CAROL, &a), (&DAVE, &a)] {
        let ev = UnsignedEvent { created_at: 200, kind: 21, tags: vec![vec!["title".into(), "Commun".into()], vec!["x".into(), x.clone()], vec!["url".into(), format!("/ipfs/{c}")]], content: String::new() }.sign(who).unwrap();
        r.db.writer().transaction(move |tx| insert_event(tx, &ev).map(|_| ())).await.unwrap();
    }
    let list = r.fm.list(&hex(&ALICE), true).await.unwrap();
    let item = &list["items"][0];
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    assert_eq!(item["sources"], 2, "deux sources pour le blob le plus répandu : {item}");
    assert!([hex(&CAROL), hex(&DAVE)].contains(&item["author"].as_str().unwrap().to_owned()), "la demande irait vers le blob A, pas vers la copie isolée de Bob");
    let bob = item["seeders"].as_array().unwrap().iter().find(|s| s["hex"] == hex(&BOB)).unwrap();
    assert_eq!(bob["same_blob"], false, "Bob a le film, mais sous un autre blob");
}

fn share_svc(r: &Rig) -> astroportd::media_share::MediaShare<FsBlobStore> {
    // Le « relai » : les événements signés par la station entrent en base comme sur le vrai relai (NIP-09 compris).
    let db = r.db.clone();
    let publish: astroportd::register::PublishFn = Arc::new(move |e: Event| {
        let db = db.clone();
        Box::pin(async move { db.writer().transaction(move |tx| insert_event(tx, &e).map(|_| ())).await.map_err(|e| e.to_string()) })
    });
    astroportd::media_share::MediaShare::new(r.dav.clone(), Arc::new(StaticKeySource::new().with(ALICE).with(BOB).with(CAROL)), publish)
}

async fn cid_of(r: &Rig, who: &[u8; 32], path: &str) -> String {
    let (h, p) = (hex(who), path.to_owned());
    r.db.readers().read(move |c| Ok(c.query_row("SELECT cid FROM ucloud_entries WHERE owner_hex = ?1 AND path = ?2", [h, p], |r| r.get(0))?)).await.unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn shared_from_the_library_a_film_travels_friend_to_friend_under_one_blob_and_leaves_a_trace() {
    let r = rig("trace").await;
    let ms = share_svc(&r);
    let film: Vec<u8> = (0..30_000u32).map(|i| (i % 241) as u8).collect();
    let fiche = json!({ "schema": "media/v1", "generated_at": 1, "type": "film", "file": "Totoro (1988).mp4", "title": "Totoro", "year": 1988, "technical": { "duration_s": 5160.0, "size": 30000, "mime": "video/mp4" }, "source": { "kind": "disc", "rights": "private_copy" }, "sharing": { "level": "private" } });
    put(&r, &BOB, "/Vidéos/Films", "Totoro (1988).mp4", &film).await;
    put(&r, &BOB, "/Vidéos/Films", "Totoro (1988).media.json", fiche.to_string().as_bytes()).await;
    let bob_fiche = "/Vidéos/Films/Totoro (1988).media.json".to_owned();

    // Bob partage depuis sa médiathèque : l'événement porte son blob, la fiche passe au cercle.
    ms.set(&hex(&BOB), std::slice::from_ref(&bob_fiche), true, None).await.unwrap();
    assert_eq!(astroportd::media::library(&r.dav, &hex(&BOB)).await.unwrap()["items"][0]["level"], "circle");
    let list = r.fm.list(&hex(&ALICE), true).await.unwrap();
    let item = &list["items"][0];
    let bob_cid = cid_of(&r, &BOB, "/Vidéos/Films/Totoro (1988).mp4").await;

    // Alice le copie : même blob, aucun nouveau CID. Puis elle le partage à son tour, sous ce même blob.
    r.fm.request(&hex(&ALICE), item["event_id"].as_str().unwrap()).await.unwrap();
    assert_eq!(cid_of(&r, &ALICE, "/Vidéos/Films/Totoro (1988).mp4").await, bob_cid, "la copie n'est pas rechiffrée");
    ms.set(&hex(&ALICE), &["/Vidéos/Films/Totoro (1988).media.json".to_owned()], true, None).await.unwrap();
    // La liste de Bob se relit : c'est là qu'il apprend qu'Alice partage le film à son tour.
    r.fm.list(&hex(&BOB), true).await.unwrap();
    let alice_cid_in_event = r.db.readers().read({
        let a = hex(&ALICE);
        move |c| Ok(c.query_row("SELECT json_extract(j.value, '$[1]') FROM nostr_events e, json_each(e.tags) j WHERE e.pubkey = ?1 AND e.kind IN (21, 22) AND json_extract(j.value, '$[0]') = 'url'", [a], |r| r.get::<_, String>(0))?)
    }).await.unwrap();
    assert_eq!(alice_cid_in_event, format!("/ipfs/{bob_cid}"), "le repartage désigne le même blob");

    // La trace de Bob : Alice l'a copié, et le partage à son tour. Celle d'Alice : reçu de Bob.
    let trace = ms.trace(&hex(&BOB), &bob_fiche).await.unwrap();
    assert_eq!((trace["shared"].as_bool(), trace["copies"][0]["label"].as_str(), trace["reshared"][0].as_str()), (Some(true), Some("alice"), Some("alice")), "{trace}");
    let mine = ms.trace(&hex(&ALICE), "/Vidéos/Films/Totoro (1988).media.json").await.unwrap();
    assert_eq!(mine["from"], "bob");

    // Bob retire son partage : son événement est supprimé (NIP-09), Alice ne le voit plus chez lui.
    ms.set(&hex(&BOB), std::slice::from_ref(&bob_fiche), false, None).await.unwrap();
    assert_eq!(ms.trace(&hex(&BOB), &bob_fiche).await.unwrap()["shared"], false);
    assert!(r.fm.list(&hex(&ALICE), true).await.unwrap()["items"].as_array().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_series_is_shared_as_an_ordered_list_of_its_episodes() {
    let r = rig("series").await;
    let ms = share_svc(&r);
    let mut fiches = Vec::new();
    for (n, body) in [(2u8, b"episode deux".as_slice()), (1, b"episode un".as_slice())] {
        let name = format!("Kaamelott S01E0{n}");
        let fiche = json!({ "schema": "media/v1", "generated_at": 1, "type": "episode", "file": format!("{name}.mp4"), "title": format!("Épisode {n}"), "series": { "name": "Kaamelott", "season": 1, "episode": n },
            "technical": { "duration_s": 210.0, "size": body.len(), "mime": "video/mp4" }, "source": { "kind": "broadcast", "rights": "private_copy" }, "sharing": { "level": "private" } });
        put(&r, &BOB, "/Vidéos/Séries/Kaamelott/Saison 01", &format!("{name}.mp4"), body).await;
        put(&r, &BOB, "/Vidéos/Séries/Kaamelott/Saison 01", &format!("{name}.media.json"), fiche.to_string().as_bytes()).await;
        fiches.push(format!("/Vidéos/Séries/Kaamelott/Saison 01/{name}.media.json"));
    }
    fiches.reverse(); // dans l'ordre de la série : épisode 1, puis 2
    let list = astroportd::media_share::SharedList { kind: "series".into(), title: "Kaamelott".into() };
    assert_eq!(ms.set(&hex(&BOB), &fiches, true, Some(list.clone())).await.unwrap()["list"], "series:kaamelott");
    let (ids, order): (Vec<String>, Vec<String>) = r.db.readers().read({
        let b = hex(&BOB);
        move |c| {
            let ids = c.prepare("SELECT e.id FROM nostr_events e, json_each(e.tags) j WHERE e.pubkey = ?1 AND e.kind IN (21, 22) AND json_extract(j.value, '$[0]') = 'episode_number' ORDER BY CAST(json_extract(j.value, '$[1]') AS INTEGER)")?.query_map([&b], |r| r.get(0))?.collect::<Result<Vec<String>, _>>()?;
            let tags: String = c.query_row("SELECT tags FROM nostr_events WHERE pubkey = ?1 AND kind = 30005", [&b], |r| r.get(0))?;
            let order = serde_json::from_str::<Vec<Vec<String>>>(&tags).unwrap().into_iter().filter(|t| t[0] == "e").map(|t| t[1].clone()).collect();
            Ok((ids, order))
        }
    }).await.unwrap();
    assert_eq!(order, ids, "la liste range les épisodes dans l'ordre");
    // Retirer la série : épisodes et liste supprimés.
    ms.set(&hex(&BOB), &fiches, false, Some(list)).await.unwrap();
    let left: i64 = r.db.readers().read({ let b = hex(&BOB); move |c| Ok(c.query_row("SELECT count(*) FROM nostr_events WHERE pubkey = ?1 AND kind IN (21, 22, 30005)", [b], |r| r.get(0))?) }).await.unwrap();
    assert_eq!(left, 0);
}
