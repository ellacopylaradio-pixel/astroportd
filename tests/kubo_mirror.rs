//! Miroir Kubo d'ORIGIN (« le disque d'abord, Kubo en miroir ») contre un faux Kubo : publication par référence au fichier,
//! rapatriement vérifié des blobs d'avant, désépinglage de ce qui n'est plus référencé, repli sur la copie quand Kubo ne
//! relit pas le disque, Kubo arrêté.

#![allow(clippy::unwrap_used)]

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use astro_db::Db;
use astro_dav::blob::{BlobStore, BlobWriter, FsBlobStore, KuboBlobStore, cid_raw_sha256};
use astroportd::kubo_mirror::Mirror;
use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

#[derive(Default)]
struct Mock {
    /// Blocs copiés dans Kubo.
    blobs: HashMap<String, Vec<u8>>,
    /// Références au disque (filestore) : CID → chemin.
    referenced: HashMap<String, String>,
    pinned: HashSet<String>,
    filestore: bool,
    /// Kubo n'a pas le droit de relire les fichiers du disque.
    unreadable: bool,
    adds: Vec<String>,
}

type Shared = Arc<Mutex<Mock>>;

fn param(q: &str, name: &str) -> Option<String> {
    q.split('&').find_map(|kv| kv.strip_prefix(&format!("{name}=")).map(str::to_owned))
}

fn err(msg: &str) -> Response {
    (StatusCode::INTERNAL_SERVER_ERROR, format!("{{\"Message\":\"{msg}\",\"Code\":0,\"Type\":\"error\"}}")).into_response()
}

async fn kubo(State(m): State<Shared>, req: Request) -> Response {
    let (path, q) = (req.uri().path().to_owned(), req.uri().query().unwrap_or("").to_owned());
    let boundary = req.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).and_then(|v| v.split("boundary=").nth(1)).map(str::to_owned);
    match path.as_str() {
        "/api/v0/id" => "{\"ID\":\"12D3KooWMock\"}".into_response(),
        "/api/v0/config" => format!("{{\"Key\":\"Experimental.FilestoreEnabled\",\"Value\":{}}}", m.lock().unwrap().filestore).into_response(),
        "/api/v0/add" => {
            let body = axum::body::to_bytes(req.into_body(), 64 << 20).await.unwrap();
            let b = boundary.unwrap();
            let start = body.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
            let end = body.len() - format!("\r\n--{b}--\r\n").len();
            let abspath = String::from_utf8_lossy(&body[..start]).lines().find_map(|l| l.strip_prefix("Abspath: ")).map(str::to_owned);
            let content = body[start..end].to_vec();
            let cid = cid_raw_sha256(&Sha256::digest(&content).into());
            let mut g = m.lock().unwrap();
            g.adds.push(q.clone());
            let reply = format!("{{\"Name\":\"blob\",\"Hash\":\"{cid}\"}}\n");
            if param(&q, "only-hash").as_deref() == Some("true") {
                return reply.into_response();
            }
            if param(&q, "nocopy").as_deref() == Some("true") {
                let Some(p) = abspath.filter(|_| g.filestore) else { return err("missing file path or URL, can't create filestore reference") };
                g.referenced.insert(cid.clone(), p);
            } else {
                g.blobs.insert(cid.clone(), content);
            }
            g.pinned.insert(cid);
            reply.into_response()
        }
        "/api/v0/files/stat" => {
            let cid = param(&q, "arg").unwrap().trim_start_matches("/ipfs/").to_owned();
            match m.lock().unwrap().blobs.get(&cid) {
                Some(b) => format!("{{\"Size\":{}}}", b.len()).into_response(),
                None => err("file does not exist"),
            }
        }
        "/api/v0/cat" => {
            let cid = param(&q, "arg").unwrap();
            let g = m.lock().unwrap();
            let data = match (g.blobs.get(&cid), g.referenced.get(&cid)) {
                (Some(b), _) => b.clone(),
                (None, Some(_)) if g.unreadable => return err("open: permission denied"),
                (None, Some(p)) => match std::fs::read(p) {
                    Ok(b) => b,
                    Err(_) => return err("failed to fetch all nodes"),
                },
                _ => return err("merkledag: not found"),
            };
            let off: usize = param(&q, "offset").map_or(0, |o| o.parse().unwrap());
            let len: usize = param(&q, "length").map_or(data.len() - off, |l| l.parse().unwrap());
            data[off..(off + len).min(data.len())].to_vec().into_response()
        }
        "/api/v0/pin/ls" => {
            let cid = param(&q, "arg").unwrap();
            if m.lock().unwrap().pinned.contains(&cid) { "{\"Keys\":{}}".into_response() } else { err(&format!("path '{cid}' is not pinned")) }
        }
        "/api/v0/pin/rm" => {
            let cid = param(&q, "arg").unwrap();
            let mut g = m.lock().unwrap();
            if g.pinned.remove(&cid) {
                g.referenced.remove(&cid);
                "{\"Pins\":[]}".into_response()
            } else {
                err("not pinned or pinned indirectly")
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

struct Station {
    dir: std::path::PathBuf,
    db: Arc<Db>,
    fs: FsBlobStore,
}

impl Drop for Station {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn station(tag: &str) -> Station {
    let dir = std::env::temp_dir().join(format!("astroportd-kubo-mirror-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
    let fs = FsBlobStore::open(dir.join("blobs")).unwrap();
    Station { dir, db, fs }
}

async fn on_disk(fs: &FsBlobStore, bytes: &[u8]) -> String {
    let mut w = fs.begin().await.unwrap();
    w.write_all(bytes).await.unwrap();
    w.commit().await.unwrap().cid
}

async fn reference(db: &Db, path: &str, cid: &str) {
    db.writer().execute("INSERT INTO ucloud_entries(owner_hex, path, parent, type, cid, data) VALUES ('aa', ?1, '/', 'file', ?2, '{}')", vec![path.to_owned().into(), cid.to_owned().into()]).await.unwrap();
}

async fn mirrored(db: &Db) -> HashMap<String, (String, i64)> {
    db.readers()
        .read(|c| {
            let mut s = c.prepare("SELECT blob, root, nocopy FROM kubo_mirror")?;
            Ok(s.query_map([], |r| Ok((r.get::<_, String>(0)?, (r.get::<_, String>(1)?, r.get::<_, i64>(2)?))))?.collect::<Result<HashMap<_, _>, _>>()?)
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn the_mirror_publishes_by_reference_brings_back_old_blobs_and_unpins_what_is_gone() {
    let st = station("main");
    let (url, mock) = mock_kubo().await;
    mock.lock().unwrap().filestore = true;
    // Un fichier sur le disque ; un ancien, dans Kubo seulement ; un introuvable ; une racine dag-pb dont le contenu ne correspond pas.
    let disk = on_disk(&st.fs, b"photo sur le disque").await;
    let old_bytes = b"ancien fichier range dans kubo".to_vec();
    let old = cid_raw_sha256(&Sha256::digest(&old_bytes).into());
    let lost = cid_raw_sha256(&Sha256::digest(b"perdu").into());
    let fake_root = "bafybeic54ggur77oamccaa44omnsezmbhsdcjisl5l3hswcigrcecpd7oq".to_owned();
    {
        let mut g = mock.lock().unwrap();
        g.blobs.insert(old.clone(), old_bytes.clone());
        g.pinned.insert(old.clone());
        g.blobs.insert(fake_root.clone(), b"autre chose".to_vec());
    }
    for (p, c) in [("/disk", &disk), ("/old", &old), ("/lost", &lost), ("/root", &fake_root)] {
        reference(&st.db, p, c).await;
    }
    let mirror = Mirror::new(st.db.clone(), st.fs.clone(), KuboBlobStore::new(&url));
    let p = mirror.pass(100).await.unwrap();
    assert_eq!((p.by_reference, p.copied, p.brought_back, p.missing, p.unpinned), (1, 0, 1, 1, 0), "{p:?}");
    assert!(p.failures.len() == 1 && p.failures[0].contains("rapatriement refusé"), "une racine qui ne correspond pas n'entre pas sur le disque : {:?}", p.failures);
    assert!(st.fs.open(&old).await.is_ok(), "l'ancien blob vit maintenant sur le disque");
    assert!(st.fs.open(&fake_root).await.is_err());
    let rows = mirrored(&st.db).await;
    assert_eq!(rows[&disk], (disk.clone(), 1), "publié par référence au fichier");
    assert_eq!(rows[&old], (old.clone(), 0), "déjà épinglé par copie : rien à republier");
    {
        let g = mock.lock().unwrap();
        assert!(g.referenced.contains_key(&disk) && !g.blobs.contains_key(&disk), "aucun octet copié dans Kubo");
        assert!(g.pinned.contains(&disk));
    }
    assert!(p.summary().contains("1 rapatrié(s)") && p.summary().contains("par référence"));

    // Rien de neuf : rien n'est republié.
    let adds = mock.lock().unwrap().adds.len();
    let again = mirror.pass(100).await.unwrap();
    assert_eq!((again.by_reference, again.copied, again.brought_back), (0, 0, 0));
    assert_eq!(mock.lock().unwrap().adds.len(), adds + 1, "seule la racine douteuse est revérifiée");

    // Le fichier est supprimé de la station : son épingle part au passage suivant, la ligne aussi.
    st.db.writer().execute("DELETE FROM ucloud_entries WHERE path = '/disk'", vec![]).await.unwrap();
    let p = mirror.pass(100).await.unwrap();
    assert_eq!(p.unpinned, 1);
    assert!(!mirrored(&st.db).await.contains_key(&disk));
    assert!(!mock.lock().unwrap().pinned.contains(&disk));

    // Budget : ce qui dépasse attend le passage suivant.
    for n in 0..3 {
        let c = on_disk(&st.fs, format!("lot {n}").as_bytes()).await;
        reference(&st.db, &format!("/lot{n}"), &c).await;
    }
    let p = mirror.pass(2).await.unwrap();
    assert!(p.waiting >= 2, "{p:?}");

    // Kubo arrêté : rien n'est tenté, le disque reste la vérité.
    let down = Mirror::new(st.db.clone(), st.fs.clone(), KuboBlobStore::new("http://127.0.0.1:1"));
    assert!(down.pass(100).await.unwrap().kubo_down);
}

#[tokio::test]
async fn a_reference_kubo_cannot_read_is_withdrawn_and_replaced_by_a_copy() {
    let st = station("unreadable");
    let (url, mock) = mock_kubo().await;
    {
        let mut g = mock.lock().unwrap();
        g.filestore = true;
        g.unreadable = true;
    }
    let a = on_disk(&st.fs, b"premier").await;
    let b = on_disk(&st.fs, b"second").await;
    reference(&st.db, "/a", &a).await;
    reference(&st.db, "/b", &b).await;
    let mirror = Mirror::new(st.db.clone(), st.fs.clone(), KuboBlobStore::new(&url));
    let p = mirror.pass(100).await.unwrap();
    assert_eq!((p.by_reference, p.copied), (0, 2), "{p:?}");
    assert!(p.failures.iter().any(|f| f.contains("même utilisateur")), "le Capitaine sait pourquoi : {:?}", p.failures);
    let g = mock.lock().unwrap();
    assert!(g.referenced.is_empty(), "la référence illisible a été retirée");
    assert_eq!(g.adds.iter().filter(|q| q.contains("nocopy=true")).count(), 1, "une seule tentative par référence, puis des copies");
    assert!(g.blobs.contains_key(&a) && g.blobs.contains_key(&b));
}

#[tokio::test]
async fn without_filestore_the_mirror_copies() {
    let st = station("copy");
    let (url, mock) = mock_kubo().await;
    let a = on_disk(&st.fs, b"sans filestore").await;
    reference(&st.db, "/a", &a).await;
    let p = Mirror::new(st.db.clone(), st.fs.clone(), KuboBlobStore::new(&url)).pass(100).await.unwrap();
    assert_eq!((p.by_reference, p.copied, p.failures.len()), (0, 1, 0), "{p:?}");
    assert_eq!(mirrored(&st.db).await[&a].1, 0);
    assert!(mock.lock().unwrap().adds.iter().all(|q| !q.contains("nocopy")), "pas de tentative de référence sans filestore");
}
