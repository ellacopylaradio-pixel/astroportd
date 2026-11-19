//! Recherche multimédia, chemin délégué : une station sans moteur OCR confie la lecture d'un document à la station d'un
//! Capitaine ami (service `ai-index`, ici un indexeur factice), **seulement** pour le membre qui l'a accepté. Le texte revenu
//! est rangé, retrouvé par la recherche de la bibliothèque, puis purgé avec le fichier.

#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use astro_core::ActivityBus;
use astro_dav::{DavConfig, DavState, FsBlobStore};
use astro_db::{Db, SqliteReplayStore};
use astro_index::catalog::Model;
use astro_index::{IndexError, Indexer, Input, LocalIndexer, Output, Task};
use astroportd::search_index::{Lenders, Opener, Worker, purge};

const ALICE: &str = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";
const BOB: &str = "b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2";

/// L'ami : lit « Facture » dans toute image, et compte ce qu'il reçoit.
struct Friend(AtomicUsize);
impl Indexer for Friend {
    fn tasks(&self) -> Vec<Task> {
        vec![Task::Ocr]
    }
    async fn index(&self, _: Task, input: Input) -> Result<Output, IndexError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        assert_eq!(&input.bytes[..], b"\x89PNG faux scan", "l'ami reçoit le contenu déchiffré");
        Ok(Output { model: Model::find("ppocrv6-small").unwrap().tag().unwrap(), text: Some("FACTURE N° 117\nÉlectricité des Écluses".into()), vector: None })
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_member_who_accepted_gets_her_scans_read_by_a_friend_and_nobody_else_s_leave() {
    let dir = std::env::temp_dir().join(format!("astroportd-search-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
    for h in [ALICE, BOB] {
        db.writer().execute("INSERT INTO identities(hex, role, hosted) VALUES (?1, 'multipass', 1)", vec![h.to_owned().into()]).await.unwrap();
    }
    let blobs = Arc::new(FsBlobStore::open(dir.join("blobs")).unwrap());
    let dav = Arc::new(DavState { db: db.clone(), blobs, replay: Arc::new(SqliteReplayStore::in_memory().unwrap()), publisher: None, config: DavConfig::new(vec![7; 32]), on_upload: None });
    for who in [ALICE, BOB] {
        astro_dav::ingest::ingest(&dav, who, "/Scans", "scan001.png", Some("image/png"), &b"\x89PNG faux scan"[..]).await.unwrap();
    }

    // L'ami, et l'accès à son service tel que le tunnel le donnerait (une adresse locale).
    let friend = Arc::new(Friend(AtomicUsize::new(0)));
    let (addr, _srv) = astro_index::server::spawn_loopback(friend.clone(), 1, Duration::from_secs(5), None).await.unwrap();
    let opened = Arc::new(AtomicUsize::new(0));
    let o = opened.clone();
    let opener: Opener = Arc::new(move |peer| {
        let o = o.clone();
        Box::pin(async move {
            assert_eq!(peer, "12D3KooWMarie");
            o.fetch_add(1, Ordering::SeqCst);
            Ok(addr)
        })
    });
    let worker = Worker::new(dav.clone(), Arc::new(LocalIndexer::new(&dir.join("models"))), Some(opener), ActivityBus::new(), Duration::ZERO);
    let mut remotes = Lenders::default();

    // Personne n'a accepté : rien ne part.
    assert_eq!(worker.round(&mut remotes).await.unwrap(), 0);
    assert_eq!(friend.0.load(Ordering::SeqCst), 0);

    // Alice accepte que Marie lise ses documents : seul son scan part, une fois.
    db.writer().execute("INSERT INTO index_delegation(owner_hex, peer, label, tasks, accepted_at) VALUES (?1, '12D3KooWMarie', 'Marie', 'ocr', 1)", vec![ALICE.to_owned().into()]).await.unwrap();
    assert_eq!(worker.round(&mut remotes).await.unwrap(), 1);
    assert_eq!(worker.round(&mut remotes).await.unwrap(), 0, "déjà lu");
    assert_eq!((friend.0.load(Ordering::SeqCst), opened.load(Ordering::SeqCst)), (1, 1));
    let via: String = db.readers().read(|c| Ok(c.query_row("SELECT via FROM search_index WHERE task = 'ocr'", [], |r| r.get(0))?)).await.unwrap();
    assert_eq!(via, "12D3KooWMarie", "on sait qui a calculé");

    // Le texte se retrouve dans la recherche d'Alice, pas dans celle de Bob (même contenu, mais pas d'accord).
    let find = |who: &'static str| {
        let db = db.clone();
        async move {
            db.readers()
                .read(move |c| Ok(astroportd::library::search(c, who, &astroportd::library::Search { q: Some("electricite".into()), limit: 10, ..Default::default() })?))
                .await
                .unwrap()["items"]
                .as_array()
                .unwrap()
                .len()
        }
    };
    assert_eq!((find(ALICE).await, find(BOB).await), (1, 0));

    // Fichier supprimé : son texte part avec lui.
    db.writer().execute("DELETE FROM ucloud_entries WHERE owner_hex = ?1", vec![ALICE.to_owned().into()]).await.unwrap();
    assert_eq!(purge(&db).await.unwrap(), 1);
    let left: i64 = db.readers().read(|c| Ok(c.query_row("SELECT count(*) FROM search_index", [], |r| r.get(0))?)).await.unwrap();
    assert_eq!(left, 0);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Un ami qui lit lentement (pour l'occuper pendant le test).
struct Slow(AtomicUsize, Duration);
impl Indexer for Slow {
    fn tasks(&self) -> Vec<Task> {
        vec![Task::Ocr]
    }
    async fn index(&self, _: Task, _: Input) -> Result<Output, IndexError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(self.1).await;
        Ok(Output { model: Model::find("ppocrv6-small").unwrap().tag().unwrap(), text: Some("Facture".into()), vector: None })
    }
}

async fn station(tag: &str) -> (std::path::PathBuf, Arc<Db>, Arc<DavState<FsBlobStore>>) {
    let dir = std::env::temp_dir().join(format!("astroportd-search-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
    db.writer().execute("INSERT INTO identities(hex, role, hosted) VALUES (?1, 'multipass', 1)", vec![ALICE.to_owned().into()]).await.unwrap();
    let blobs = Arc::new(FsBlobStore::open(dir.join("blobs")).unwrap());
    let dav = Arc::new(DavState { db: db.clone(), blobs, replay: Arc::new(SqliteReplayStore::in_memory().unwrap()), publisher: None, config: DavConfig::new(vec![7; 32]), on_upload: None });
    astro_dav::ingest::ingest(&dav, ALICE, "/Scans", "a.png", Some("image/png"), &b"\x89PNG un"[..]).await.unwrap();
    (dir, db, dav)
}

fn opener(map: Vec<(&'static str, std::net::SocketAddr)>) -> Opener {
    Arc::new(move |peer| {
        let addr = map.iter().find(|(p, _)| *p == peer).map(|(_, a)| *a);
        Box::pin(async move { addr.ok_or_else(|| "inconnue".to_owned()) })
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn work_goes_to_the_least_loaded_friend_and_a_busy_one_is_left_alone_until_it_said() {
    let (dir, db, dav) = station("balance").await;
    // Deux amis acceptés. Marie est occupée (un calcul en cours pour une autre station) ; l'Atelier est libre.
    let marie = Arc::new(Slow(AtomicUsize::new(0), Duration::from_millis(800)));
    let atelier = Arc::new(Slow(AtomicUsize::new(0), Duration::from_millis(10)));
    let (am, _) = astro_index::server::spawn_loopback(marie.clone(), 1, Duration::from_secs(5), None).await.unwrap();
    let (aa, _) = astro_index::server::spawn_loopback(atelier.clone(), 1, Duration::from_secs(5), None).await.unwrap();
    let tag = Model::find("ppocrv6-small").unwrap().tag().unwrap();
    let other = astro_index::RemoteIndexer::new(am, "autre", Duration::from_secs(5)).unwrap().expect(Task::Ocr, tag);
    let busy = tokio::spawn(async move { other.index(Task::Ocr, Input { mime: "image/png".into(), bytes: bytes::Bytes::from_static(b"x") }).await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    db.writer().execute("INSERT INTO index_delegation(owner_hex, peer, label, tasks, accepted_at) VALUES (?1, 'marie', 'Marie', 'ocr', 1), (?1, 'atelier', 'Atelier', 'ocr', 2)", vec![ALICE.to_owned().into()]).await.unwrap();
    let worker = Worker::new(dav.clone(), Arc::new(LocalIndexer::new(&dir.join("models"))), Some(opener(vec![("marie", am), ("atelier", aa)])), ActivityBus::new(), Duration::ZERO);
    let mut lenders = Lenders::default();
    assert_eq!(worker.round(&mut lenders).await.unwrap(), 1);
    let via: String = db.readers().read(|c| Ok(c.query_row("SELECT via FROM search_index", [], |r| r.get(0))?)).await.unwrap();
    assert_eq!((via.as_str(), atelier.0.load(Ordering::SeqCst)), ("atelier", 1), "la moins chargée");
    busy.await.unwrap().unwrap();
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_friend_that_says_come_back_later_is_not_asked_again_before() {
    use axum::routing::{get, post};
    let (dir, db, dav) = station("busy").await;
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let app = axum::Router::new()
        .route("/v1", get(|| async { r#"{"tasks":["ocr"],"load":{"slots":1,"running":1,"waiting":16,"avg_ms":4000}}"# }))
        .route("/v1/ocr", post(move || { c.fetch_add(1, Ordering::SeqCst); async { (axum::http::StatusCode::SERVICE_UNAVAILABLE, [(axum::http::header::RETRY_AFTER, "120")], "occupé") } }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    db.writer().execute("INSERT INTO index_delegation(owner_hex, peer, label, tasks, accepted_at) VALUES (?1, 'marie', 'Marie', 'ocr', 1)", vec![ALICE.to_owned().into()]).await.unwrap();
    let worker = Worker::new(dav.clone(), Arc::new(LocalIndexer::new(&dir.join("models"))), Some(opener(vec![("marie", addr)])), ActivityBus::new(), Duration::ZERO);
    let mut lenders = Lenders::default();
    assert_eq!(worker.round(&mut lenders).await.unwrap(), 0, "rien n'a pu être lu");
    assert!(lenders.resting("marie"), "elle a dit : revenez dans 120 s");
    assert_eq!(worker.round(&mut lenders).await.unwrap(), 0);
    assert_eq!(calls.load(Ordering::SeqCst), 1, "pas de nouvelle demande avant l'heure");
    let state: i64 = db.readers().read(|c| Ok(c.query_row("SELECT count(*) FROM search_index", [], |r| r.get(0))?)).await.unwrap();
    assert_eq!(state, 0, "occupé n'est pas un échec : le fichier attend son tour");
    let _ = std::fs::remove_dir_all(dir);
}

/// Un ami qui saurait regarder les photos : compte les demandes par tâche.
struct Eyes(AtomicUsize, AtomicUsize);
impl Indexer for Eyes {
    fn tasks(&self) -> Vec<Task> {
        vec![Task::Ocr, Task::Vision]
    }
    async fn index(&self, task: Task, _: Input) -> Result<Output, IndexError> {
        let n = if task == Task::Vision { &self.0 } else { &self.1 };
        n.fetch_add(1, Ordering::SeqCst);
        Err(IndexError::Unavailable("non demandé".into()))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn photos_never_leave_the_station_even_if_the_member_asked_for_it() {
    let (dir, db, dav) = station("photos").await;
    let eyes = Arc::new(Eyes(AtomicUsize::new(0), AtomicUsize::new(0)));
    let (addr, _server) = astro_index::server::spawn_loopback(eyes.clone(), 1, Duration::from_secs(5), None).await.unwrap();
    // Un accord ancien (ou forgé) qui mentionne les photos : il ne vaut que pour le texte.
    db.writer().execute("INSERT INTO index_delegation(owner_hex, peer, label, tasks, accepted_at) VALUES (?1, 'marie', 'Marie', 'vision,ocr', 1)", vec![ALICE.to_owned().into()]).await.unwrap();
    let worker = Worker::new(dav.clone(), Arc::new(LocalIndexer::new(&dir.join("models"))), Some(opener(vec![("marie", addr)])), ActivityBus::new(), Duration::ZERO);
    worker.round(&mut Lenders::default()).await.unwrap();
    assert_eq!(eyes.0.load(Ordering::SeqCst), 0, "aucune photo envoyée pour être regardée");
    assert!(eyes.1.load(Ordering::SeqCst) >= 1, "le texte, lui, est bien confié");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_photo_vector_is_kept_here_found_by_the_image_and_purged_with_its_file() {
    let (dir, db, _dav) = station("vectors").await;
    let job = astroportd::search_index::Job::photos().unwrap();
    let unit = |i: usize| (0..768).map(|j| if j == i { 1.0 } else { 0.0 }).collect::<Vec<f32>>();
    for n in 0..20 {
        let (path, sha) = (format!("/Photos/p{n}.jpg"), format!("p{n}"));
        db.writer().execute("INSERT INTO ucloud_entries(owner_hex, path, parent, type, cid, mime, size, mtime, sha_plain, data) VALUES (?1, ?2, '/Photos', 'file', 'c', 'image/jpeg', 1, 1, ?3, '{}')", vec![ALICE.to_owned().into(), path.clone().into(), sha.clone().into()]).await.unwrap();
        let c = astroportd::search_index::Candidate { owner: ALICE.into(), path, sha, mime: "image/jpeg".into(), size: 1 };
        let out = Output { model: job.model.clone(), text: None, vector: Some(unit(n)) };
        astroportd::search_index::record(&db, &c, &job, &Ok(out.clone()), None).await.unwrap();
        if n == 3 {
            astroportd::search_index::record(&db, &c, &job, &Ok(out), None).await.unwrap(); // refait : remplacé, pas doublé
        }
    }
    let frames: i64 = db.readers().read(|c| Ok(c.query_row("SELECT count(*) FROM search_frames", [], |r| r.get(0))?)).await.unwrap();
    assert_eq!(frames, 20);
    let q = unit(3);
    let found = db.readers().read(move |c| Ok(astroportd::library::seen(c, ALICE, &q)?.strict)).await.unwrap();
    assert_eq!(found, [("p3".to_owned(), 0)]);
    db.writer().execute("DELETE FROM ucloud_entries WHERE sha_plain = 'p3'", vec![]).await.unwrap();
    astroportd::search_index::purge(&db).await.unwrap();
    let (frames, vectors): (i64, i64) = db.readers().read(|c| Ok(c.query_row("SELECT (SELECT count(*) FROM search_frames), (SELECT count(*) FROM search_vision_vectors)", [], |r| Ok((r.get(0)?, r.get(1)?)))?)).await.unwrap();
    assert_eq!((frames, vectors), (19, 19), "la photo supprimée n'a plus de vecteur");
    let _ = std::fs::remove_dir_all(dir);
}

/// Fabrique une vidéo MP4 de `secs` secondes (index à la fin du fichier, comme la plupart des téléphones).
fn mp4(dir: &std::path::Path, secs: u32) -> Option<Vec<u8>> {
    let out = dir.join("v.mp4");
    let ok = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i", &format!("testsrc=duration={secs}:size=160x120:rate=5"), "-c:v", "mpeg4", out.to_str()?])
        .status()
        .ok()?
        .success();
    ok.then(|| std::fs::read(out).ok()).flatten()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_video_is_read_in_place_as_a_few_key_frames_and_a_disguised_playlist_reads_nothing() {
    if !astro_index::video::available() {
        eprintln!("ffmpeg absent : test sauté");
        return;
    }
    let (dir, _db, dav) = station("video").await;
    let Some(video) = mp4(&dir, 30) else { return eprintln!("ffmpeg sans encodeur mpeg4 : test sauté") };
    astro_dav::ingest::ingest(&dav, ALICE, "/Vidéos", "anniv.mp4", Some("video/mp4"), &video[..]).await.unwrap();
    // Une « vidéo » qui est en fait une liste de lecture vers un fichier de la machine.
    let playlist = b"#EXTM3U\n#EXT-X-TARGETDURATION:10\n#EXTINF:10,\nfile:///etc/passwd\n#EXT-X-ENDLIST\n";
    astro_dav::ingest::ingest(&dav, ALICE, "/Vidéos", "piege.mp4", Some("video/mp4"), &playlist[..]).await.unwrap();
    let worker = Worker::new(dav.clone(), Arc::new(LocalIndexer::new(&dir.join("models"))), None, ActivityBus::new(), Duration::ZERO);
    let job = astroportd::search_index::Job { video: true, ..astroportd::search_index::Job::photos().unwrap() };
    let todo = dav.db.readers().read(move |c| Ok(astroportd::search_index::candidates(c, &job, true, 10_000_000_000, 10, None)?)).await.unwrap();
    let get = |name: &str| todo.iter().find(|c| c.path.ends_with(name)).unwrap().clone();
    let frames = worker.video_frames(&get("anniv.mp4")).await.unwrap();
    assert!((9..=astro_index::video::MAX_FRAMES).contains(&frames.len()), "30 s : une image toutes les 2,5 s au plus, 12 au plus ({})", frames.len());
    assert!(frames.iter().all(|(_, png)| png.starts_with(b"\x89PNG")) && frames.windows(2).all(|w| w[1].0 > w[0].0), "des PNG, dans l'ordre du temps");
    assert_eq!(frames[1].0, 3_000, "30 s en 12 images, mais au moins 3 s d'écart");
    assert!(worker.video_frames(&get("piege.mp4")).await.is_err(), "le format est imposé : la liste de lecture ne lit rien");
    let _ = std::fs::remove_dir_all(dir);
}

/// Chaîne complète avec les VRAIS modèles, comme dans `serve` : photos chiffrées déposées, analysées par le travailleur avec
/// SigLIP chargé ici, puis cherchées en français comme le fait l'écran. Ignoré sans les fichiers :
/// ASTRO_OCR_DIR=<modèles> ASTRO_PHOTOS_DIR=<dossier de .jpg dont beach_sunset.jpg et birthday_cake_candles.jpg>
/// cargo test -p astroportd --features onnx --test search_index real_ -- --ignored
#[cfg(feature = "onnx")]
#[tokio::test(flavor = "multi_thread")]
#[ignore = "demande les modèles SigLIP 2 (ASTRO_OCR_DIR), des photos (ASTRO_PHOTOS_DIR) et libonnxruntime"]
async fn real_photos_are_analysed_by_the_worker_and_found_by_a_french_description() {
    let models = std::path::PathBuf::from(std::env::var("ASTRO_OCR_DIR").unwrap());
    let photos = std::path::PathBuf::from(std::env::var("ASTRO_PHOTOS_DIR").unwrap());
    let (dir, db, dav) = station("real").await;
    let mut names: Vec<_> = std::fs::read_dir(&photos).unwrap().filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|x| x == "jpg")).collect();
    names.sort();
    assert!(names.len() >= astro_index::siglip::MIN_PHOTOS, "au moins {} photos", astro_index::siglip::MIN_PHOTOS);
    for p in &names {
        astro_dav::ingest::ingest(&dav, ALICE, "/Photos", p.file_name().unwrap().to_str().unwrap(), Some("image/jpeg"), &std::fs::read(p).unwrap()[..]).await.unwrap();
    }
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let lib = astro_facecloud::onnx::find_library(std::env::var("ORT_DYLIB_PATH").ok().as_deref(), home.as_deref());
    let (local, problems) = LocalIndexer::load(&models, lib.as_deref(), 0);
    assert!(problems.is_empty() && local.tasks().contains(&Task::VisionQuery), "{problems:?}");
    let local = Arc::new(local);
    let worker = Worker::new(dav.clone(), local.clone(), None, ActivityBus::new(), Duration::ZERO);
    let mut lenders = Lenders::default();
    while worker.round(&mut lenders).await.unwrap() > 0 {}
    let frames: i64 = db.readers().read(|c| Ok(c.query_row("SELECT count(*) FROM search_frames", [], |r| r.get(0))?)).await.unwrap();
    assert_eq!(frames as usize, names.len(), "une image par photo");
    let find = async |q: &str| {
        let v = local.index(Task::VisionQuery, Input { mime: "text/plain".into(), bytes: bytes::Bytes::from(q.to_owned()) }).await.unwrap().vector.unwrap();
        let hits = db.readers().read(move |c| Ok(astroportd::library::seen(c, ALICE, &v)?.strict)).await.unwrap();
        let paths = db.readers().read(move |c| {
            let shas: Vec<String> = hits.into_iter().map(|(s, _)| s).collect();
            Ok(shas.iter().map(|s| c.query_row("SELECT path FROM ucloud_entries WHERE sha_plain = ?1", [s], |r| r.get::<_, String>(0))).collect::<Result<Vec<_>, _>>()?)
        });
        paths.await.unwrap()
    };
    let gateau = find("gâteau d'anniversaire avec des bougies").await;
    assert_eq!(gateau.first().map(String::as_str), Some("/Photos/birthday_cake_candles.jpg"), "{gateau:?}");
    let plage = find("coucher de soleil sur la plage").await;
    assert_eq!(plage.first().map(String::as_str), Some("/Photos/beach_sunset.jpg"), "{plage:?}");
    assert!(find("une girafe").await.is_empty(), "rien de tel : rien");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn after_a_restore_the_index_comes_back_from_the_member_s_pack_instead_of_being_recomputed() {
    use astroportd::search_index::{Candidate, Job, candidates, record};
    use astroportd::search_pack::{PACK_DIR, PACK_NAME, sync};
    let unit = |i: usize| (0..768).map(|j| if j == i { 1.0 } else { 0.0 }).collect::<Vec<f32>>();
    let jpeg = |n: usize| format!("\u{FF}\u{D8} photo {n}").into_bytes();
    let (photos, ocr) = (Job::photos().unwrap(), Job::ocr().unwrap());
    let jobs = vec![ocr.clone(), photos.clone()];
    let entry = |db: Arc<Db>, path: String| async move {
        db.readers().read(move |c| Ok(c.query_row("SELECT sha_plain, cid FROM ucloud_entries WHERE owner_hex = ?1 AND path = ?2", [ALICE, path.as_str()], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)))?)).await.unwrap()
    };

    // Station A : 20 photos regardées et un scan lu, puis le paquet est écrit dans le tiroir du membre.
    let (dir_a, db_a, dav_a) = station("pack-a").await;
    for n in 0..20 {
        astro_dav::ingest::ingest(&dav_a, ALICE, "/Photos", &format!("p{n}.jpg"), Some("image/jpeg"), &jpeg(n)[..]).await.unwrap();
        let (sha, _) = entry(db_a.clone(), format!("/Photos/p{n}.jpg")).await;
        let c = Candidate { owner: ALICE.into(), path: format!("/Photos/p{n}.jpg"), sha, mime: "image/jpeg".into(), size: 1 };
        record(&db_a, &c, &photos, &Ok(Output { model: photos.model.clone(), text: None, vector: Some(unit(n)) }), None).await.unwrap();
    }
    let (scan_sha, _) = entry(db_a.clone(), "/Scans/a.png".into()).await;
    let scan = Candidate { owner: ALICE.into(), path: "/Scans/a.png".into(), sha: scan_sha, mime: "image/png".into(), size: 1 };
    record(&db_a, &scan, &ocr, &Ok(Output { model: ocr.model.clone(), text: Some("FACTURE N° 117 Électricité".into()), vector: None }), None).await.unwrap();
    assert_eq!(sync(&dav_a, &jobs).await.unwrap(), (0, 1), "un paquet écrit");
    assert_eq!(sync(&dav_a, &jobs).await.unwrap(), (0, 0), "rien n'a changé : rien de réécrit");
    record(&db_a, &scan, &ocr, &Ok(Output { model: ocr.model.clone(), text: Some("FACTURE N° 117 Électricité, relue".into()), vector: None }), None).await.unwrap();
    assert_eq!(sync(&dav_a, &jobs).await.unwrap(), (0, 0), "changé, mais écrit il y a moins de 20 h : on attend");
    let pack = astro_dav::carddav::read_file(&dav_a, ALICE, &format!("{PACK_DIR}/{PACK_NAME}")).await.unwrap();
    assert!(pack.len() > 20 * 4096, "les vecteurs y sont");

    // Station B, neuve : le paquet revient avec le compte, mais la photo p19 n'est pas encore arrivée.
    let (dir_b, db_b, dav_b) = station("pack-b").await;
    astro_dav::ingest::replace(&dav_b, ALICE, PACK_DIR, PACK_NAME, None, &pack[..]).await.unwrap();
    for n in 0..19 {
        astro_dav::ingest::ingest(&dav_b, ALICE, "/Photos", &format!("p{n}.jpg"), Some("image/jpeg"), &jpeg(n)[..]).await.unwrap();
    }
    let (_, pack_cid) = entry(db_b.clone(), format!("{PACK_DIR}/{PACK_NAME}")).await;
    assert_eq!(sync(&dav_b, &jobs).await.unwrap(), (1, 0), "relu, et surtout pas réécrit : il manque encore une photo");
    assert_eq!(entry(db_b.clone(), format!("{PACK_DIR}/{PACK_NAME}")).await.1, pack_cid, "le paquet sauvegardé est intact");
    let count = |db: Arc<Db>| async move { db.readers().read(|c| Ok(c.query_row("SELECT (SELECT count(*) FROM search_frames), (SELECT count(*) FROM search_text WHERE src = 'ocr')", [], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?)).await.unwrap() };
    assert_eq!(count(db_b.clone()).await, (19, 1), "19 photos et le scan repris tels quels");

    // La dernière photo arrive : reprise au repos suivant, puis le paquet peut de nouveau être écrit.
    astro_dav::ingest::ingest(&dav_b, ALICE, "/Photos", "p19.jpg", Some("image/jpeg"), &jpeg(19)[..]).await.unwrap();
    assert_eq!(sync(&dav_b, &jobs).await.unwrap(), (1, 1), "repris en entier, puis une première écriture par cette station");
    assert_eq!(sync(&dav_b, &jobs).await.unwrap(), (0, 0), "plus rien à faire");
    assert_eq!(count(db_b.clone()).await, (20, 1));
    let p = photos.clone();
    let todo = db_b.readers().read(move |c| Ok(candidates(c, &p, true, 10_000_000_000, 50, None)?)).await.unwrap();
    assert_eq!(todo.iter().map(|c| c.path.as_str()).collect::<Vec<_>>(), ["/Scans/a.png"], "rien à recalculer, sauf l'image que A n'avait que lue, pas regardée");
    let q = unit(7);
    let found = db_b.readers().read(move |c| Ok(astroportd::library::seen(c, ALICE, &q)?.strict)).await.unwrap();
    assert_eq!(found.len(), 1, "la recherche par l'image marche sur la nouvelle station");
    let _ = (std::fs::remove_dir_all(dir_a), std::fs::remove_dir_all(dir_b));
}
