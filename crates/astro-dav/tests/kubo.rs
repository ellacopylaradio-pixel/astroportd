//! Adaptateur Kubo contre un faux démon IPFS (API RPC) : écriture en flux, lecture avec Range, désépinglage, repli local.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use astro_dav::blob::{AutoBlobStore, BlobError, BlobStore, BlobWriter, FsBlobStore, KuboBlobStore, cid_raw_sha256, is_cid};
use astro_dav::kubo::AddMode;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use common::*;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

#[derive(Default)]
struct Mock {
    blobs: HashMap<String, Vec<u8>>,
    pinned: HashSet<String>,
    add_queries: Vec<String>,
    cat_queries: Vec<String>,
    calls: Vec<String>,
    fail_add: bool,
    /// `Experimental.FilestoreEnabled`.
    filestore: bool,
    /// Références au disque (`nocopy`) : CID → chemin absolu reçu dans `Abspath`.
    referenced: HashMap<String, String>,
}

type Shared = Arc<Mutex<Mock>>;

fn query_param(q: &str, name: &str) -> Option<String> {
    q.split('&').find_map(|kv| kv.strip_prefix(&format!("{name}=")).map(str::to_owned))
}

fn json_error(msg: &str) -> Response {
    (StatusCode::INTERNAL_SERVER_ERROR, format!("{{\"Message\":\"{msg}\",\"Code\":0,\"Type\":\"error\"}}")).into_response()
}

async fn kubo(State(m): State<Shared>, req: Request) -> Response {
    let (path, query) = (req.uri().path().to_owned(), req.uri().query().unwrap_or("").to_owned());
    let boundary = req.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).and_then(|v| v.split("boundary=").nth(1)).map(str::to_owned);
    m.lock().unwrap().calls.push(path.clone());
    if req.method() != "POST" {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    match path.as_str() {
        "/api/v0/id" => "{\"ID\":\"12D3KooWMock\"}".into_response(),
        "/api/v0/add" => {
            let body = axum::body::to_bytes(req.into_body(), 64 << 20).await.unwrap();
            let mut g = m.lock().unwrap();
            g.add_queries.push(query.clone());
            if g.fail_add {
                return json_error("disque plein");
            }
            let b = boundary.unwrap();
            let start = body.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
            let end = body.len() - format!("\r\n--{b}--\r\n").len();
            assert_eq!(&body[end..], format!("\r\n--{b}--\r\n").as_bytes(), "le multipart doit se terminer correctement");
            let head = String::from_utf8_lossy(&body[..start]).into_owned();
            let abspath = head.lines().find_map(|l| l.strip_prefix("Abspath: ")).map(str::to_owned);
            let content = body[start..end].to_vec();
            let cid = cid_raw_sha256(&Sha256::digest(&content).into());
            let size = content.len();
            let reply = format!("{{\"Name\":\"blob\",\"Hash\":\"{cid}\",\"Size\":\"{size}\"}}\n");
            if query_param(&query, "only-hash").as_deref() == Some("true") {
                return reply.into_response(); // rien n'est gardé
            }
            if query_param(&query, "nocopy").as_deref() == Some("true") {
                // Comme Kubo 0.40 : refus sans filestore, ou sans chemin du fichier.
                let Some(path) = abspath.filter(|_| g.filestore) else { return json_error("missing file path or URL, can't create filestore reference") };
                g.referenced.insert(cid.clone(), path);
            } else {
                g.blobs.insert(cid.clone(), content);
            }
            g.pinned.insert(cid.clone());
            reply.into_response()
        }
        "/api/v0/config" => {
            let on = m.lock().unwrap().filestore;
            format!("{{\"Key\":\"Experimental.FilestoreEnabled\",\"Value\":{on}}}").into_response()
        }
        "/api/v0/pin/add" => {
            let cid = query_param(&query, "arg").unwrap();
            let mut g = m.lock().unwrap();
            if g.blobs.contains_key(&cid) {
                g.pinned.insert(cid.clone());
                format!("{{\"Pins\":[\"{cid}\"]}}").into_response()
            } else {
                json_error("block was not found locally (offline): ipld: could not find")
            }
        }
        "/api/v0/files/stat" => {
            let cid = query_param(&query, "arg").unwrap().trim_start_matches("/ipfs/").to_owned();
            match m.lock().unwrap().blobs.get(&cid) {
                Some(b) => format!("{{\"Size\":{},\"CumulativeSize\":{},\"Type\":\"file\"}}", b.len(), b.len()).into_response(),
                None => json_error("file does not exist"),
            }
        }
        "/api/v0/cat" => {
            let (cid, offset, length) = (query_param(&query, "arg").unwrap(), query_param(&query, "offset"), query_param(&query, "length"));
            let mut g = m.lock().unwrap();
            g.cat_queries.push(query.clone());
            let Some(b) = g.blobs.get(&cid) else { return json_error("merkledag: not found") };
            let off: usize = offset.map_or(0, |o| o.parse().unwrap());
            let len: usize = length.map_or(b.len() - off, |l| l.parse().unwrap());
            let slice = b[off..off + len].to_vec();
            let chunks: Vec<Result<Vec<u8>, std::io::Error>> = slice.chunks(10_000).map(|c| Ok(c.to_vec())).collect();
            Body::from_stream(futures_util::stream::iter(chunks)).into_response()
        }
        // Comme Kubo 0.40 : 200 si épinglé, 500 `{"Message":"path '…' is not pinned"}` sinon.
        "/api/v0/pin/ls" => {
            let cid = query_param(&query, "arg").unwrap();
            if m.lock().unwrap().pinned.contains(&cid) { format!("{{\"Keys\":{{\"{cid}\":{{\"Type\":\"recursive\"}}}}}}").into_response() } else { json_error(&format!("path '{cid}' is not pinned")) }
        }
        "/api/v0/pin/rm" => {
            let cid = query_param(&query, "arg").unwrap();
            let mut g = m.lock().unwrap();
            if g.pinned.remove(&cid) {
                g.blobs.remove(&cid); // le ramasse-miettes est supposé immédiat
                "{\"Pins\":[]}".into_response()
            } else {
                json_error("not pinned or pinned indirectly")
            }
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn mock_kubo() -> (String, Shared) {
    let state: Shared = Arc::default();
    let app = axum::Router::new().fallback(axum::routing::any(kubo)).with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, state)
}

#[tokio::test]
async fn streaming_add_returns_the_cid_and_pins_with_cid_v1_raw_leaves() {
    let (url, mock) = mock_kubo().await;
    let store = KuboBlobStore::new(&url);
    assert!(store.ping().await);
    let data = sample(300_000, 4);
    let mut w = store.begin().await.unwrap();
    for piece in data.chunks(70_001) {
        w.write_all(piece).await.unwrap();
    }
    let meta = w.commit().await.unwrap();
    assert!(is_cid(&meta.cid));
    assert_eq!(meta.size, 300_000);
    assert_eq!(meta.cid, cid_raw_sha256(&Sha256::digest(&data).into()));
    let g = mock.lock().unwrap();
    assert_eq!(g.blobs[&meta.cid], data, "le contenu arrive intact, enveloppe multipart retirée");
    assert!(g.pinned.contains(&meta.cid));
    let q = &g.add_queries[0];
    for p in ["cid-version=1", "raw-leaves=true", "pin=true"] {
        assert!(q.contains(p), "{p} absent de {q}");
    }
}

#[tokio::test]
async fn reads_support_seek_and_partial_reads_through_cat_offset() {
    let (url, mock) = mock_kubo().await;
    let store = KuboBlobStore::new(&url);
    let data = sample(100_000, 9);
    let mut w = store.begin().await.unwrap();
    w.write_all(&data).await.unwrap();
    let cid = w.commit().await.unwrap().cid;

    let (mut r, len) = store.open(&cid).await.unwrap();
    assert_eq!(len, 100_000);
    let mut all = Vec::new();
    r.read_to_end(&mut all).await.unwrap();
    assert_eq!(all, data);
    // Seek puis lecture partielle : n'appelle `cat` que pour la plage utile.
    mock.lock().unwrap().cat_queries.clear();
    r.seek(std::io::SeekFrom::Start(65_536)).await.unwrap();
    let mut part = vec![0u8; 100];
    r.read_exact(&mut part).await.unwrap();
    assert_eq!(part, data[65_536..65_636]);
    assert!(mock.lock().unwrap().cat_queries[0].contains("offset=65536"));
    r.seek(std::io::SeekFrom::End(-10)).await.unwrap();
    let mut tail = Vec::new();
    r.read_to_end(&mut tail).await.unwrap();
    assert_eq!(tail, data[99_990..]);
    assert_eq!(r.seek(std::io::SeekFrom::Current(-5)).await.unwrap(), 99_995);
    r.seek(std::io::SeekFrom::Start(200_000)).await.unwrap();
    assert_eq!(r.read(&mut part).await.unwrap(), 0, "au-delà de la fin : EOF");
    assert!(r.seek(std::io::SeekFrom::End(-200_000)).await.is_err());
    // Inconnu / CID invalide.
    assert!(matches!(store.open("bafkreihdwdcefgh4dqkjv67uzcmw7ojee6xedzdetojuzjevtenxquvyku").await, Err(BlobError::NotFound)));
    assert!(matches!(store.open("../etc").await, Err(BlobError::BadCid)));
}

#[tokio::test]
async fn remove_unpins_and_is_idempotent() {
    let (url, mock) = mock_kubo().await;
    let store = KuboBlobStore::new(&url);
    let mut w = store.begin().await.unwrap();
    w.write_all(b"a supprimer").await.unwrap();
    let cid = w.commit().await.unwrap().cid;
    store.remove(&cid).await.unwrap();
    assert!(!mock.lock().unwrap().pinned.contains(&cid));
    store.remove(&cid).await.unwrap();
    assert!(mock.lock().unwrap().calls.iter().filter(|c| c.ends_with("pin/rm")).count() == 2);
    assert!(matches!(store.remove("zzz").await, Err(BlobError::BadCid)));
}

#[tokio::test]
async fn kubo_errors_surface_and_abandoned_writes_pin_nothing() {
    let (url, mock) = mock_kubo().await;
    let store = KuboBlobStore::new(&url);
    mock.lock().unwrap().fail_add = true;
    let mut w = store.begin().await.unwrap();
    w.write_all(b"x").await.unwrap();
    let err = w.commit().await.unwrap_err().to_string();
    assert!(err.contains("disque plein"), "{err}");
    mock.lock().unwrap().fail_add = false;
    {
        let mut w = store.begin().await.unwrap();
        w.write_all(b"abandonne").await.unwrap();
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(mock.lock().unwrap().pinned.is_empty(), "une écriture non validée n'épingle rien");
    // Démon injoignable.
    let dead = KuboBlobStore::new("http://127.0.0.1:1");
    assert!(!dead.ping().await);
    let mut w = dead.begin().await.unwrap();
    let _ = w.write_all(b"x").await;
    assert!(w.commit().await.is_err());
}

#[tokio::test]
async fn auto_store_writes_to_disk_first_even_when_kubo_answers_and_reads_old_kubo_blobs_last() {
    let dir = TempDir::new("auto");
    let fs = FsBlobStore::open(dir.0.join("blobs")).unwrap();
    let (url, mock) = mock_kubo().await;
    let kubo = KuboBlobStore::new(&url);
    // Un blob d'avant « le disque d'abord » : rangé dans Kubo seulement.
    let mut w = kubo.begin().await.unwrap();
    w.write_all(b"ancien").await.unwrap();
    let old = w.commit().await.unwrap();

    // Kubo répond : l'écriture va quand même sur le disque, et rien n'est envoyé à Kubo.
    let auto = AutoBlobStore::new(Some(kubo.clone()), fs.clone()).with_health_ttl(Duration::from_millis(1));
    let adds = mock.lock().unwrap().add_queries.len();
    let mut w = auto.begin().await.unwrap();
    w.write_all(b"nouveau").await.unwrap();
    let new = w.commit().await.unwrap();
    assert!(fs.open(&new.cid).await.is_ok(), "le disque d'abord");
    assert_eq!(mock.lock().unwrap().add_queries.len(), adds, "la publication sur Kubo est l'affaire du miroir, pas de l'écriture");

    // Lecture : le disque, puis Kubo en dernier recours (avec accès aléatoire).
    let (mut r, len) = auto.open(&old.cid).await.unwrap();
    assert_eq!(len, 6);
    r.seek(std::io::SeekFrom::Start(2)).await.unwrap();
    let mut rest = String::new();
    r.read_to_string(&mut rest).await.unwrap();
    assert_eq!(rest, "cien");

    // Kubo arrêté : ce qui est sur le disque reste lisible ; seul l'ancien blob non rapatrié manque.
    let down = AutoBlobStore::new(Some(KuboBlobStore::new("http://127.0.0.1:1")), fs.clone()).with_health_ttl(Duration::from_millis(1));
    let (mut r, _) = down.open(&new.cid).await.unwrap();
    let mut got = String::new();
    r.read_to_string(&mut got).await.unwrap();
    assert_eq!(got, "nouveau");
    assert!(matches!(down.open(&old.cid).await, Err(BlobError::NotFound)));
    let mut w = down.begin().await.unwrap();
    w.write_all(b"sans kubo").await.unwrap();
    assert!(fs.open(&w.commit().await.unwrap().cid).await.is_ok());

    // Suppression : le disque, et l'épingle de même nom dans Kubo.
    auto.remove(&new.cid).await.unwrap();
    auto.remove(&old.cid).await.unwrap();
    assert!(matches!(auto.open(&new.cid).await, Err(BlobError::NotFound)));
    assert!(matches!(auto.open(&old.cid).await, Err(BlobError::NotFound)));
}

#[tokio::test]
async fn a_disk_file_is_published_by_reference_when_kubo_allows_it_otherwise_copied() {
    let dir = TempDir::new("addfile");
    let fs = FsBlobStore::open(dir.0.join("blobs")).unwrap();
    let mut w = fs.begin().await.unwrap();
    let data = sample(150_000, 3);
    w.write_all(&data).await.unwrap();
    let meta = w.commit().await.unwrap();
    let path = fs.blob_path(&meta.cid).unwrap();
    let (url, mock) = mock_kubo().await;
    let kubo = KuboBlobStore::new(&url);

    // Seulement l'empreinte : rien n'est gardé.
    assert_eq!(kubo.add_file(&path, AddMode::OnlyHash).await.unwrap(), meta.cid);
    assert!(mock.lock().unwrap().pinned.is_empty());
    // Sans filestore, Kubo refuse la référence : l'erreur remonte (la station se rabat alors sur une copie).
    assert!(!kubo.filestore_enabled().await);
    assert!(kubo.add_file(&path, AddMode::NoCopy).await.unwrap_err().to_string().contains("filestore"));
    // Avec filestore : une référence au fichier, par son chemin absolu, sans copie des octets.
    mock.lock().unwrap().filestore = true;
    assert!(kubo.filestore_enabled().await);
    assert_eq!(kubo.add_file(&path, AddMode::NoCopy).await.unwrap(), meta.cid);
    {
        let g = mock.lock().unwrap();
        assert_eq!(g.referenced[&meta.cid], std::fs::canonicalize(&path).unwrap().to_str().unwrap());
        assert!(!g.blobs.contains_key(&meta.cid) && g.pinned.contains(&meta.cid), "épinglé, sans copie");
        assert!(g.add_queries.last().unwrap().contains("nocopy=true"));
    }
    // Copie : Kubo garde les octets.
    assert_eq!(kubo.add_file(&path, AddMode::Copy).await.unwrap(), meta.cid);
    assert_eq!(mock.lock().unwrap().blobs[&meta.cid], data);
    kubo.pin(&meta.cid).await.unwrap();
    assert!(matches!(kubo.pin("../x").await, Err(BlobError::BadCid)));
}

#[tokio::test]
async fn a_named_blob_is_refused_when_its_raw_content_does_not_match() {
    let dir = TempDir::new("named");
    let fs = FsBlobStore::open(dir.0.join("blobs")).unwrap();
    let good = cid_raw_sha256(&Sha256::digest(b"juste").into());
    let mut w = fs.begin_named(&good).await.unwrap();
    w.write_all(b"juste").await.unwrap();
    assert_eq!(w.commit().await.unwrap().cid, good);
    let mut w = fs.begin_named(&cid_raw_sha256(&Sha256::digest(b"autre").into())).await.unwrap();
    w.write_all(b"faux").await.unwrap();
    let tmp = w.tmp_path().to_owned();
    assert!(w.commit().await.unwrap_err().to_string().contains("empreinte"));
    assert!(!tmp.exists(), "le contenu refusé ne reste pas sur le disque");
    // Une racine dag-pb ne se recalcule pas ici : l'appelant l'a vérifiée (par Kubo), le blob prend ce nom.
    let root = "bafybeic54ggur77oamccaa44omnsezmbhsdcjisl5l3hswcigrcecpd7oq";
    let mut w = fs.begin_named(root).await.unwrap();
    w.write_all(b"verifie par kubo").await.unwrap();
    assert_eq!(w.commit().await.unwrap().cid, root);
    assert!(fs.open(root).await.is_ok());
    assert!(matches!(fs.begin_named("../etc").await, Err(BlobError::BadCid)));
}

#[tokio::test]
async fn webdav_runs_on_kubo_with_range_reads_and_a_replayable_manifest() {
    let (url, mock) = mock_kubo().await;
    let dir = TempDir::new("davkubo");
    let store = Arc::new(KuboBlobStore::new(&url));
    let (db, app, publisher) = serve_on(&dir, store.clone(), |_| {}).await;
    let a = alice();
    let plain = sample(200_000, 21);
    assert_eq!(call(&app, "PUT", "/dav/photo.jpg", Some(&a), &[], streamed(plain.clone(), 30_000)).await.status(), 201);
    // Le blob chiffré (pas le clair) est épinglé dans Kubo.
    {
        let g = mock.lock().unwrap();
        assert_eq!(g.pinned.len(), 1);
        let blob = g.blobs.values().next().unwrap();
        assert_eq!(&blob[..5], b"UENC\x02");
        assert_eq!(blob.len(), 14 + 200_000 + 4 * 16);
    }
    let (status, _, body) = bytes(call(&app, "GET", "/dav/photo.jpg", Some(&a), &[], Body::empty()).await).await;
    assert_eq!((status, body == plain), (200, true));
    mock.lock().unwrap().cat_queries.clear();
    let (status, headers, part) = bytes(call(&app, "GET", "/dav/photo.jpg", Some(&a), &[("range", "bytes=131072-131081")], Body::empty()).await).await;
    assert_eq!((status, part.as_slice(), headers["content-range"].to_str().unwrap()), (206, &plain[131_072..131_082], "bytes 131072-131081/200000"));
    let q = mock.lock().unwrap().cat_queries.clone();
    assert!(q.iter().any(|c| c.contains("offset=") && !c.contains("offset=0&")), "seul le chunk utile est relu : {q:?}");
    // Manifeste publié sur Kubo, puis rejouable après purge de la base.
    assert!(publisher.flush().await.is_empty());
    assert_eq!(mock.lock().unwrap().pinned.len(), 2, "fichier + manifeste");
    let owner = hex(&ALICE);
    let ev = db
        .readers()
        .read(move |c| {
            let f = astro_nostr_proto::Filter { kinds: Some(vec![30_078]), authors: Some(vec![owner]), ..Default::default() };
            Ok(astro_db::events::query_events(c, &f, u64::MAX / 2, 5)?.pop().unwrap())
        })
        .await
        .unwrap();
    db.writer().batch(vec![astro_db::Statement::new("DELETE FROM ucloud_entries", vec![]), astro_db::Statement::new("DELETE FROM ucloud_keyring", vec![])]).await.unwrap();
    astro_dav::manifest::restore_from_event(&db, store.as_ref(), keys().as_ref(), &ev).await.unwrap();
    let (_, _, again) = bytes(call(&app, "GET", "/dav/photo.jpg", Some(&a), &[], Body::empty()).await).await;
    assert_eq!(again, plain);
    // Suppression : le blob est désépinglé.
    assert_eq!(call(&app, "DELETE", "/dav/photo.jpg", Some(&a), &[], Body::empty()).await.status(), 204);
    assert_eq!(mock.lock().unwrap().pinned.len(), 1, "il ne reste que le manifeste");
}

#[test]
fn providers_are_read_from_the_findprovs_stream() {
    let ndjson = concat!(
        r#"{"ID":"","Extra":"","Type":6,"Responses":null}"#, "\n",
        r#"{"ID":"","Extra":"","Type":4,"Responses":[{"ID":"12D3KooWA","Addrs":[]},{"ID":"12D3KooWB","Addrs":[]}]}"#, "\n",
        r#"{"ID":"","Extra":"","Type":4,"Responses":[{"ID":"12D3KooWA","Addrs":[]}]}"#, "\n",
        r#"{"ID":"12D3KooWZ","Extra":"","Type":1,"Responses":null}"#, "\n",
        r#"{"ID":"","Extra":"","Type":4,"Resp"#, // flux coupé à l'échéance
    );
    assert_eq!(astro_dav::kubo::parse_providers(ndjson), ["12D3KooWA", "12D3KooWB"], "dédoublonné, trié, requêtes DHT et ligne tronquée ignorées");
}

#[tokio::test]
async fn providers_keeps_what_arrived_before_the_deadline() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let cid = cid_raw_sha256(&Sha256::digest(b"fournisseurs").into());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let expected = format!("POST /api/v0/routing/findprovs?arg={cid}&num-providers=3");
    tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 2048];
        let n = s.read(&mut buf).await.unwrap();
        let head = String::from_utf8_lossy(&buf[..n]).into_owned();
        assert!(head.starts_with(&expected), "{head}");
        let line = r#"{"ID":"","Extra":"","Type":4,"Responses":[{"ID":"12D3KooWA","Addrs":[]}]}"#;
        s.write_all(format!("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{line}\n\r\n", line.len() + 1).as_bytes()).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_secs(5)).await; // la recherche dure : jamais de fin de flux
    });
    let store = astro_dav::kubo::KuboBlobStore::new(&format!("http://{addr}"));
    let got = store.providers(&cid, 3, std::time::Duration::from_millis(400)).await.unwrap();
    assert_eq!(got, ["12D3KooWA"]);
    assert!(store.providers("../x", 3, std::time::Duration::from_millis(100)).await.is_err(), "CID invalide refusé avant toute requête");
}

