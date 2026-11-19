//! Maillage P2P entre stations : identité IPFS, réseau privé, tunnels DRAGON, échange de blobs, circuit relais.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use astro_dav::blob::{BlobStore, BlobWriter, FsBlobStore, cid_raw_sha256};
use astro_p2p::blob::{FetchError, P2pBlobStore, ServeOpts, fetch_blob, fetch_from, serve_blobs};
use astro_p2p::tunnel::{ExposeOpts, expose, forward, protocol_name};
use astro_p2p::{Multiaddr, P2pConfig, P2pNode, generate_swarm_key, parse_swarm_key};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!("astro-p2p-{tag}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn sample(n: usize, seed: u64) -> Vec<u8> {
    let mut s = seed | 1;
    (0..n)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 24) as u8
        })
        .collect()
}

async fn node(seed: u8, psk: Option<&str>, relay_server: bool) -> (P2pNode, Multiaddr) {
    let mut cfg = P2pConfig::local([seed; 32]);
    cfg.psk = psk.map(|k| parse_swarm_key(k).unwrap());
    cfg.relay_server = relay_server;
    let node = P2pNode::spawn(cfg).await.unwrap();
    let tcp: Multiaddr = node.handle.listen_addrs().into_iter().find(|a| a.to_string().contains("/tcp/")).unwrap();
    let full: Multiaddr = format!("{tcp}/p2p/{}", node.handle.peer_id()).parse().unwrap();
    (node, full)
}

async fn put_blob(store: &FsBlobStore, data: &[u8]) -> String {
    let mut w = store.begin().await.unwrap();
    w.write_all(data).await.unwrap();
    w.commit().await.unwrap().cid
}

const FETCH: Duration = Duration::from_secs(20);

#[tokio::test]
async fn the_peer_id_is_the_ipfs_peer_id_of_the_station_key() {
    let seed = astro_core::kdf::duniter_seed("salt-test", "pepper-test").unwrap();
    let mut cfg = P2pConfig::local(*seed.ed25519_secret_seed());
    cfg.listen.clear();
    let node = P2pNode::spawn(cfg).await.unwrap();
    assert_eq!(node.handle.peer_id().to_string(), seed.ipfs_peer_id());
    assert_eq!(node.handle.peer_id().to_string(), "12D3KooWFSoxH5cZqhpcywX4P4ZKgaU1yTzcopYEvVhSQfHdhwwJ", "valeur produite par tools/keygen -t ipfs");
    node.shutdown().await;
}

#[tokio::test]
async fn only_nodes_sharing_the_swarm_key_can_join() {
    let key = generate_swarm_key().unwrap();
    let other = generate_swarm_key().unwrap();
    let (a, a_addr) = node(1, Some(&key), false).await;
    let (b, _) = node(2, Some(&key), false).await;
    assert_eq!(b.handle.dial(a_addr.clone()).await.unwrap(), a.handle.peer_id());
    for _ in 0..100 {
        if a.handle.connected_peers().contains(&b.handle.peer_id()) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await; // l'événement entrant d'A arrive juste après la fin du dial de B
    }
    assert_eq!(a.handle.connected_peers(), vec![b.handle.peer_id()]);
    let (c, _) = node(3, Some(&other), false).await;
    assert!(c.handle.dial(a_addr.clone()).await.is_err(), "swarm.key différente");
    let (d, _) = node(4, None, false).await;
    assert!(d.handle.dial(a_addr).await.is_err(), "pas de swarm.key");
    assert!(!a.handle.connected_peers().contains(&c.handle.peer_id()) && !a.handle.connected_peers().contains(&d.handle.peer_id()));
}

async fn echo_server() -> std::net::SocketAddr {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = l.accept().await else { return };
            tokio::spawn(async move {
                let (mut r, mut w) = s.split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
            });
        }
    });
    addr
}

#[tokio::test]
async fn a_service_is_exposed_and_joined_through_the_mesh_without_opening_ports() {
    let key = generate_swarm_key().unwrap();
    let (a, a_addr) = node(1, Some(&key), false).await;
    let (b, _) = node(2, Some(&key), false).await;
    let (c, _) = node(3, Some(&key), false).await;
    b.handle.dial(a_addr.clone()).await.unwrap();
    c.handle.dial(a_addr).await.unwrap();
    let echo = echo_server().await;
    let allow = HashSet::from([b.handle.peer_id()]);
    let exposed = expose(&a.handle, "echo", echo.to_string(), ExposeOpts { allow: Some(allow), max_streams: 8, ..Default::default() }).unwrap();
    assert_eq!(exposed.protocol, format!("/x/echo-{}", a.handle.peer_id()));

    let fwd = forward(&b.handle, "127.0.0.1:0".parse().unwrap(), a.handle.peer_id(), "echo").await.unwrap();
    let payload = sample(1 << 20, 9);
    let mut conns = Vec::new();
    for i in 0..3u8 {
        let (addr, payload) = (fwd.local_addr, payload.clone());
        conns.push(tokio::spawn(async move {
            let mut s = TcpStream::connect(addr).await.unwrap();
            let data: Vec<u8> = payload.iter().map(|b| b.wrapping_add(i)).collect();
            let (mut r, mut w) = s.split();
            let write = async {
                w.write_all(&data).await.unwrap();
                w.shutdown().await.unwrap();
            };
            let read = async {
                let mut back = Vec::new();
                r.read_to_end(&mut back).await.unwrap();
                back
            };
            let ((), back) = tokio::join!(write, read);
            assert!(back == data, "1 Mo identique à l'aller-retour, connexion {i}");
        }));
    }
    for c in conns {
        c.await.unwrap();
    }

    let fwd_c = forward(&c.handle, "127.0.0.1:0".parse().unwrap(), a.handle.peer_id(), "echo").await.unwrap();
    let mut s = TcpStream::connect(fwd_c.local_addr).await.unwrap();
    let _ = s.write_all(b"coucou").await;
    let mut back = Vec::new();
    let got = tokio::time::timeout(Duration::from_secs(5), s.read_to_end(&mut back)).await;
    assert!(got.is_ok() && back.is_empty(), "aucun octet ne doit revenir : {got:?} {back:?}");

    let id = a.handle.peer_id();
    assert!(protocol_name("ssh", &id).is_ok() && protocol_name("brain-gpu_1.2", &id).is_ok());
    for bad in ["", "SSH", "a b", "a/b", &"x".repeat(33)] {
        assert!(protocol_name(bad, &id).is_err(), "{bad:?}");
    }
    drop(exposed);
}

#[tokio::test]
async fn a_blob_is_pulled_from_a_friend_and_its_integrity_is_verified() {
    let key = generate_swarm_key().unwrap();
    let (a, a_addr) = node(1, Some(&key), false).await;
    let (b, _) = node(2, Some(&key), false).await;
    b.handle.dial(a_addr).await.unwrap();
    let (da, db, dc) = (TempDir::new("a"), TempDir::new("b"), TempDir::new("c"));
    let store_a = Arc::new(FsBlobStore::open(&da.0).unwrap());
    let store_b = FsBlobStore::open(&db.0).unwrap(); // station B : aucun Kubo, magasin vide
    let data = sample(3_000_000, 5);
    let cid = put_blob(&store_a, &data).await;
    let _server = serve_blobs(&a.handle, store_a.clone(), ServeOpts::default()).unwrap();

    let meta = fetch_from(&b.handle, a.handle.peer_id(), &cid, &store_b, 1 << 30, FETCH).await.unwrap();
    assert_eq!((meta.cid.as_str(), meta.size), (cid.as_str(), 3_000_000));
    let (mut r, len) = store_b.open(&cid).await.unwrap();
    let mut got = Vec::new();
    r.read_to_end(&mut got).await.unwrap();
    assert_eq!((len, got == data), (3_000_000, true));
    assert_eq!(cid_raw_sha256(&Sha256::digest(&got).into()), cid, "intégrité : le CID est l'empreinte SHA-256");

    let missing = "bafkreihdwdcefgh4dqkjv67uzcmw7ojee6xedzdetojuzjevtenxquvyku";
    let err = fetch_from(&b.handle, a.handle.peer_id(), missing, &store_b, 1 << 30, FETCH).await.unwrap_err().to_string();
    assert!(err.contains("n'a pas ce blob"), "{err}");
    assert!(matches!(fetch_from(&b.handle, a.handle.peer_id(), "bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi", &store_b, 1 << 30, FETCH).await, Err(FetchError::BadCid)));
    assert!(matches!(fetch_from(&b.handle, a.handle.peer_id(), "../../etc/passwd", &store_b, 1 << 30, FETCH).await, Err(FetchError::BadCid)));
    let small = FsBlobStore::open(&dc.0).unwrap();
    let err = fetch_from(&b.handle, a.handle.peer_id(), &cid, &small, 1000, FETCH).await.unwrap_err().to_string();
    assert!(err.contains("limite"), "{err}");
}

#[tokio::test]
async fn a_lying_peer_cannot_plant_a_blob_under_a_cid_that_is_not_its_hash() {
    let key = generate_swarm_key().unwrap();
    let (liar, liar_addr) = node(1, Some(&key), false).await;
    let (b, _) = node(2, Some(&key), false).await;
    b.handle.dial(liar_addr).await.unwrap();
    let dir = TempDir::new("liar");
    let store = Arc::new(FsBlobStore::open(&dir.0).unwrap());
    let real_cid = put_blob(&store, b"contenu authentique").await;
    let wanted = "bafkreihdwdcefgh4dqkjv67uzcmw7ojee6xedzdetojuzjevtenxquvyku"; // CID du fichier vide
    std::fs::copy(dir.0.join(&real_cid), dir.0.join(wanted)).unwrap();
    let _server = serve_blobs(&liar.handle, store, ServeOpts::default()).unwrap();
    let victim_dir = TempDir::new("victim");
    let victim = FsBlobStore::open(&victim_dir.0).unwrap();
    let err = fetch_from(&b.handle, liar.handle.peer_id(), wanted, &victim, 1 << 30, FETCH).await.unwrap_err().to_string();
    assert!(err.contains("rejeté"), "{err}");
    let stored: Vec<_> = std::fs::read_dir(&victim_dir.0).unwrap().map(|e| e.unwrap().file_name()).filter(|n| n != ".tmp").collect();
    assert!(stored.is_empty(), "aucun blob validé, aucun résidu : {stored:?}");
    assert_eq!(std::fs::read_dir(victim_dir.0.join(".tmp")).unwrap().count(), 0);
}

#[tokio::test]
async fn peers_are_tried_in_turn_and_the_p2p_store_pulls_missing_blobs_on_demand() {
    let key = generate_swarm_key().unwrap();
    let (a, a_addr) = node(1, Some(&key), false).await; // possède le blob
    let (e, e_addr) = node(2, Some(&key), false).await; // ami qui ne l'a pas
    let (b, _) = node(3, Some(&key), false).await;
    b.handle.dial(e_addr).await.unwrap();
    b.handle.dial(a_addr).await.unwrap();
    let (da, de, db) = (TempDir::new("a"), TempDir::new("e"), TempDir::new("b"));
    let (store_a, store_e) = (Arc::new(FsBlobStore::open(&da.0).unwrap()), Arc::new(FsBlobStore::open(&de.0).unwrap()));
    let data = sample(500_000, 8);
    let cid = put_blob(&store_a, &data).await;
    let _sa = serve_blobs(&a.handle, store_a, ServeOpts::default()).unwrap();
    let _se = serve_blobs(&e.handle, store_e, ServeOpts::default()).unwrap();

    let store_b = Arc::new(FsBlobStore::open(&db.0).unwrap());
    let meta = fetch_blob(&b.handle, &[e.handle.peer_id(), a.handle.peer_id()], &cid, store_b.as_ref(), 1 << 30, FETCH).await.unwrap();
    assert_eq!(meta.cid, cid);
    store_b.remove(&cid).await.unwrap();
    let none = fetch_blob(&b.handle, &[e.handle.peer_id()], &cid, store_b.as_ref(), 1 << 30, FETCH).await.unwrap_err();
    assert!(matches!(none, FetchError::Unavailable { tried: 1, .. }), "{none}");
    let p2p_store = P2pBlobStore::new(store_b.clone(), b.handle.clone(), vec![]);
    let (mut r, len) = p2p_store.open(&cid).await.unwrap();
    let mut got = Vec::new();
    r.read_to_end(&mut got).await.unwrap();
    assert_eq!((len, got == data), (500_000, true));
    // Lu pendant son arrivée : il entre dans le magasin juste après ses derniers octets, une fois l'empreinte vérifiée.
    let mut local = false;
    for _ in 0..100 {
        if store_b.open(&cid).await.is_ok() {
            local = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(local, "le blob est désormais local");
    assert!(p2p_store.open("bafkreihdwdcefgh4dqkjv67uzcmw7ojee6xedzdetojuzjevtenxquvyku").await.is_err());
}

/// Pair hostile qui parle `/astro/blob/1.1.0` à la main : il annonce la bonne taille, puis pour chaque plage demandée
/// n'envoie que `cut_after` octets avant de couper le flux, ou de se taire (`stall`).
fn rogue_peer(node: &P2pNode, data: Arc<Vec<u8>>, cut_after: usize, stall: bool) -> tokio::task::JoinHandle<()> {
    use futures_util::StreamExt;
    use futures_util::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let mut incoming = node.handle.control().accept(libp2p::StreamProtocol::new(astro_p2p::blob::PROTOCOL)).unwrap();
    tokio::spawn(async move {
        while let Some((_, mut s)) = incoming.next().await {
            let data = data.clone();
            tokio::spawn(async move {
                let (mut line, mut b) = (Vec::new(), [0u8; 1]);
                while s.read_exact(&mut b).await.is_ok() && b[0] != b'\n' {
                    line.push(b[0]);
                }
                let line = String::from_utf8(line).unwrap();
                let words: Vec<&str> = line.split(' ').collect();
                let _ = s.write_all(&[0]).await;
                let _ = s.write_all(&(data.len() as u64).to_be_bytes()).await;
                if words[0] == "GET" {
                    let (offset, len): (usize, usize) = (words[2].parse().unwrap(), words[3].parse().unwrap());
                    let _ = s.write_all(&(len as u64).to_be_bytes()).await;
                    let _ = s.write_all(&data[offset..(offset + len.min(cut_after)).min(data.len())]).await;
                    if stall {
                        tokio::time::sleep(Duration::from_secs(3600)).await;
                    }
                }
                let _ = s.close().await;
            });
        }
    })
}

#[tokio::test]
async fn a_segment_cut_off_by_one_peer_is_resumed_from_another_at_the_same_offset() {
    let key = generate_swarm_key().unwrap();
    let (a, a_addr) = node(81, Some(&key), false).await; // honnête
    let (r, r_addr) = node(82, Some(&key), false).await; // coupe chaque plage après 1 Mio
    let (b, _) = node(83, Some(&key), false).await;
    b.handle.dial(r_addr).await.unwrap();
    b.handle.dial(a_addr).await.unwrap();
    let data = sample(10_000_000, 11); // trois segments de 4 Mio au plus
    let (da, db) = (TempDir::new("cut-a"), TempDir::new("cut-b"));
    let store_a = Arc::new(FsBlobStore::open(&da.0).unwrap());
    let cid = put_blob(&store_a, &data).await;
    let _sa = serve_blobs(&a.handle, store_a, ServeOpts::default()).unwrap();
    let _rogue = rogue_peer(&r, Arc::new(data.clone()), 1 << 20, false);

    let store_b = FsBlobStore::open(&db.0).unwrap();
    // Le pair qui coupe est préféré : chacun de ses segments est repris chez l'autre, au même endroit.
    let meta = fetch_blob(&b.handle, &[r.handle.peer_id(), a.handle.peer_id()], &cid, &store_b, 1 << 30, FETCH).await.unwrap();
    assert_eq!((meta.cid.as_str(), meta.size), (cid.as_str(), 10_000_000));
    let (mut rd, _) = store_b.open(&cid).await.unwrap();
    let mut got = Vec::new();
    rd.read_to_end(&mut got).await.unwrap();
    assert!(got == data, "segments réécrits dans l'ordre");

    // Seul, le pair qui coupe ne fait rien valider : abandonné après quelques échecs, sans résidu.
    store_b.remove(&cid).await.unwrap();
    let err = fetch_blob(&b.handle, &[r.handle.peer_id()], &cid, &store_b, 1 << 30, FETCH).await.unwrap_err().to_string();
    assert!(err.contains("interrompu"), "{err}");
    assert!(store_b.open(&cid).await.is_err());
    assert_eq!(std::fs::read_dir(db.0.join(".tmp")).unwrap().count(), 0);
}

/// Pair qui sert honnêtement les plages commençant avant `serve_below` (coupe les autres sans un octet) et note l'offset de
/// chaque `GET` reçu.
fn logging_peer(node: &P2pNode, data: Arc<Vec<u8>>, serve_below: usize, log: Arc<std::sync::Mutex<Vec<usize>>>) -> tokio::task::JoinHandle<()> {
    use futures_util::StreamExt;
    use futures_util::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let mut incoming = node.handle.control().accept(libp2p::StreamProtocol::new(astro_p2p::blob::PROTOCOL)).unwrap();
    tokio::spawn(async move {
        while let Some((_, mut s)) = incoming.next().await {
            let (data, log) = (data.clone(), log.clone());
            tokio::spawn(async move {
                let (mut line, mut b) = (Vec::new(), [0u8; 1]);
                while s.read_exact(&mut b).await.is_ok() && b[0] != b'\n' {
                    line.push(b[0]);
                }
                let line = String::from_utf8(line).unwrap();
                let words: Vec<&str> = line.split(' ').collect();
                let _ = s.write_all(&[0]).await;
                let _ = s.write_all(&(data.len() as u64).to_be_bytes()).await;
                if words[0] == "GET" {
                    let (offset, len): (usize, usize) = (words[2].parse().unwrap(), words[3].parse().unwrap());
                    log.lock().unwrap().push(offset);
                    let _ = s.write_all(&(len as u64).to_be_bytes()).await;
                    if offset < serve_below {
                        let _ = s.write_all(&data[offset..offset + len]).await;
                    }
                }
                let _ = s.close().await;
            });
        }
    })
}

#[tokio::test]
async fn a_transfer_cut_short_resumes_after_a_restart_with_only_the_missing_segments() {
    let key = generate_swarm_key().unwrap();
    let (p, p_addr) = node(111, Some(&key), false).await; // ne sert que les deux premiers segments, puis plus rien
    let (q, q_addr) = node(112, Some(&key), false).await; // sert tout, et note ce qu'on lui demande
    let (b, _) = node(113, Some(&key), false).await;
    b.handle.dial(p_addr).await.unwrap();
    b.handle.dial(q_addr).await.unwrap();
    let data = Arc::new(sample(10_000_000, 17)); // segments de 4 Mio : 0, 4 Mio, 8 Mio
    let cid = astro_dav::blob::cid_raw_sha256(&Sha256::digest(data.as_slice()).into());
    let (log_p, log_q) = (Arc::default(), Arc::<std::sync::Mutex<Vec<usize>>>::default());
    let _p = logging_peer(&p, data.clone(), 8 << 20, log_p);
    let _q = logging_peer(&q, data.clone(), usize::MAX, log_q.clone());
    let db = TempDir::new("resume-b");

    // Premier essai : coupé au troisième segment. Les deux premiers restent sur le disque.
    let store = FsBlobStore::open(&db.0).unwrap();
    assert!(fetch_blob(&b.handle, &[p.handle.peer_id()], &cid, &store, 1 << 30, FETCH).await.is_err());
    let part = db.0.join(".tmp").join(format!("part-{cid}"));
    assert_eq!(std::fs::metadata(&part).unwrap().len(), 8 << 20, "deux segments entiers gardés");
    // Un segment à moitié écrit au moment de la coupure n'est pas gardé.
    std::io::Write::write_all(&mut std::fs::OpenOptions::new().append(true).open(&part).unwrap(), &[0xAB; 1000]).unwrap();

    // Redémarrage : le magasin est rouvert ; seul le segment manquant est demandé.
    drop(store);
    let store = FsBlobStore::open(&db.0).unwrap();
    let meta = fetch_blob(&b.handle, &[q.handle.peer_id()], &cid, &store, 1 << 30, FETCH).await.unwrap();
    assert_eq!((meta.cid.as_str(), meta.size), (cid.as_str(), 10_000_000));
    assert_eq!(*log_q.lock().unwrap(), [8 << 20], "rien de ce qui était déjà là n'est redemandé");
    let (mut rd, _) = store.open(&cid).await.unwrap();
    let mut got = Vec::new();
    rd.read_to_end(&mut got).await.unwrap();
    assert!(got == *data);
    assert_eq!(std::fs::read_dir(db.0.join(".tmp")).unwrap().count(), 0, "la partie reprise est devenue le blob");

    // Partie falsifiée sur le disque : l'empreinte finale la rejette, elle est jetée, et l'essai suivant repart de zéro.
    store.remove(&cid).await.unwrap();
    std::fs::write(&part, vec![0u8; 8 << 20]).unwrap();
    log_q.lock().unwrap().clear();
    let err = fetch_blob(&b.handle, &[q.handle.peer_id()], &cid, &store, 1 << 30, FETCH).await.unwrap_err().to_string();
    assert!(err.contains("empreinte"), "{err}");
    assert!(!part.exists(), "partie fausse jetée");
    let meta = fetch_blob(&b.handle, &[q.handle.peer_id()], &cid, &store, 1 << 30, FETCH).await.unwrap();
    assert_eq!(meta.cid, cid);
}

#[tokio::test]
async fn a_film_is_read_while_it_arrives_and_a_read_ahead_waits_for_its_bytes() {
    use futures_util::StreamExt;
    use futures_util::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let key = generate_swarm_key().unwrap();
    let (p, p_addr) = node(121, Some(&key), false).await; // sert le premier segment, retient les suivants tant que `hold`
    let (b, _) = node(122, Some(&key), false).await;
    b.handle.dial(p_addr).await.unwrap();
    let data = Arc::new(sample(10_000_000, 19));
    let cid = cid_raw_sha256(&Sha256::digest(data.as_slice()).into());
    let hold = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let mut incoming = p.handle.control().accept(libp2p::StreamProtocol::new(astro_p2p::blob::PROTOCOL)).unwrap();
    let (d, h) = (data.clone(), hold.clone());
    let _peer = tokio::spawn(async move {
        while let Some((_, mut s)) = incoming.next().await {
            let (data, hold) = (d.clone(), h.clone());
            tokio::spawn(async move {
                let (mut line, mut c) = (Vec::new(), [0u8; 1]);
                while s.read_exact(&mut c).await.is_ok() && c[0] != b'\n' {
                    line.push(c[0]);
                }
                let line = String::from_utf8(line).unwrap();
                let w: Vec<&str> = line.split(' ').collect();
                let _ = s.write_all(&[0]).await;
                let _ = s.write_all(&(data.len() as u64).to_be_bytes()).await;
                if w[0] == "GET" {
                    let (offset, len): (usize, usize) = (w[2].parse().unwrap(), w[3].parse().unwrap());
                    while offset > 0 && hold.load(Ordering::SeqCst) {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                    let _ = s.write_all(&(len as u64).to_be_bytes()).await;
                    let _ = s.write_all(&data[offset..offset + len]).await;
                }
                let _ = s.close().await;
            });
        }
    });
    let db = TempDir::new("growing-b");
    let local = Arc::new(FsBlobStore::open(&db.0).unwrap());
    let store = P2pBlobStore::new(local.clone(), b.handle.clone(), vec![p.handle.peer_id()]);

    // Le lecteur arrive avant le film entier : les 4 premiers Mio se lisent tout de suite.
    let (mut rd, size) = tokio::time::timeout(Duration::from_secs(20), store.open(&cid)).await.unwrap().unwrap();
    assert_eq!(size, 10_000_000);
    let mut head = vec![0u8; 4 << 20];
    tokio::time::timeout(Duration::from_secs(20), rd.read_exact(&mut head)).await.unwrap().unwrap();
    assert!(head == data[..4 << 20]);
    assert!(local.open(&cid).await.is_err(), "pas encore dans le magasin : rien n'est validé avant l'empreinte");
    // Plus loin : la lecture attend (un second lecteur, positionné vers la fin, comme un saut dans le film).
    let (mut far, _) = store.open(&cid).await.unwrap();
    tokio::io::AsyncSeekExt::seek(&mut far, std::io::SeekFrom::Start(9_000_000)).await.unwrap();
    let mut tail = vec![0u8; 1_000_000];
    assert!(tokio::time::timeout(Duration::from_millis(500), far.read_exact(&mut tail)).await.is_err(), "les octets ne sont pas encore là");
    hold.store(false, Ordering::SeqCst);
    let mut tail = vec![0u8; 1_000_000];
    tokio::time::timeout(Duration::from_secs(30), far.read_exact(&mut tail)).await.unwrap().unwrap();
    assert!(tail == data[9_000_000..]);
    let mut rest = Vec::new();
    rd.read_to_end(&mut rest).await.unwrap();
    assert!(rest == data[4 << 20..], "le premier lecteur va jusqu'au bout");
    // Puis le blob vérifié entre dans le magasin ; un nouveau lecteur le lit en local.
    for _ in 0..100 {
        if local.open(&cid).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(local.open(&cid).await.unwrap().1, 10_000_000);
    assert!(matches!(store.open(&cid).await.unwrap().0, astro_p2p::blob::P2pReader::Local(_)));
}

#[tokio::test]
async fn a_jump_far_into_a_film_that_is_arriving_is_served_first() {
    use futures_util::StreamExt;
    use futures_util::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::io::AsyncSeekExt;
    let key = generate_swarm_key().unwrap();
    let (p, p_addr) = node(141, Some(&key), false).await; // retient le segment 1 : l'ordre du fichier reste bloqué
    let (b, _) = node(142, Some(&key), false).await;
    b.handle.dial(p_addr).await.unwrap();
    const SEG: usize = 4 << 20;
    let data = Arc::new(sample(5 * SEG + 1_000_000, 29)); // segments 0 à 5 ; le 5 est hors de la fenêtre d'ordre (4)
    let cid = cid_raw_sha256(&Sha256::digest(data.as_slice()).into());
    let hold = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let mut incoming = p.handle.control().accept(libp2p::StreamProtocol::new(astro_p2p::blob::PROTOCOL)).unwrap();
    let (d, h) = (data.clone(), hold.clone());
    let _peer = tokio::spawn(async move {
        while let Some((_, mut s)) = incoming.next().await {
            let (data, hold) = (d.clone(), h.clone());
            tokio::spawn(async move {
                let (mut line, mut c) = (Vec::new(), [0u8; 1]);
                while s.read_exact(&mut c).await.is_ok() && c[0] != b'\n' {
                    line.push(c[0]);
                }
                let line = String::from_utf8(line).unwrap();
                let w: Vec<&str> = line.split(' ').collect();
                let _ = s.write_all(&[0]).await;
                let _ = s.write_all(&(data.len() as u64).to_be_bytes()).await;
                if w[0] == "GET" {
                    let (offset, len): (usize, usize) = (w[2].parse().unwrap(), w[3].parse().unwrap());
                    while offset == SEG && hold.load(Ordering::SeqCst) {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                    let _ = s.write_all(&(len as u64).to_be_bytes()).await;
                    let _ = s.write_all(&data[offset..offset + len]).await;
                }
                let _ = s.close().await;
            });
        }
    });
    let db = TempDir::new("jump-b");
    let local = Arc::new(FsBlobStore::open(&db.0).unwrap());
    let store = P2pBlobStore::new(local.clone(), b.handle.clone(), vec![p.handle.peer_id()]);

    // Saut à la fin du film : servi alors que le début n'est pas encore écrit.
    let (mut end, _) = store.open(&cid).await.unwrap();
    end.seek(std::io::SeekFrom::Start((5 * SEG + 10) as u64)).await.unwrap();
    let mut got = vec![0u8; 500_000];
    tokio::time::timeout(Duration::from_secs(20), end.read_exact(&mut got)).await.expect("le saut est servi en priorité").unwrap();
    assert!(got == data[5 * SEG + 10..5 * SEG + 10 + 500_000]);
    // Saut au milieu (segment 3, reçu dans l'ordre mais pas encore écrit : le segment 1 bloque) : servi aussi.
    let (mut mid, _) = store.open(&cid).await.unwrap();
    mid.seek(std::io::SeekFrom::Start((3 * SEG + 7) as u64)).await.unwrap();
    let mut got = vec![0u8; SEG];
    tokio::time::timeout(Duration::from_secs(20), mid.read_exact(&mut got)).await.expect("segment en avance servi depuis la mémoire").unwrap();
    assert!(got == data[3 * SEG + 7..4 * SEG + 7], "à cheval sur deux segments");
    assert!(local.open(&cid).await.is_err(), "rien n'est validé tant que le segment 1 manque");

    // Le segment 1 arrive : tout s'écrit dans l'ordre, l'empreinte est vérifiée, chacun lit jusqu'au bout.
    hold.store(false, Ordering::SeqCst);
    let mut rest = Vec::new();
    tokio::time::timeout(Duration::from_secs(30), mid.read_to_end(&mut rest)).await.unwrap().unwrap();
    assert!(rest == data[4 * SEG + 7..]);
    let (mut all, _) = store.open(&cid).await.unwrap();
    let mut whole = Vec::new();
    all.read_to_end(&mut whole).await.unwrap();
    assert!(whole == *data);
    assert_eq!(local.open(&cid).await.unwrap().1, data.len() as u64);
}

#[tokio::test]
async fn a_film_read_while_it_arrives_ends_in_an_error_if_its_fingerprint_is_wrong() {
    let key = generate_swarm_key().unwrap();
    let (p, p_addr) = node(131, Some(&key), false).await; // bonne taille, autres octets
    let (b, _) = node(132, Some(&key), false).await;
    b.handle.dial(p_addr).await.unwrap();
    let real = sample(6_000_000, 23);
    let cid = cid_raw_sha256(&Sha256::digest(&real).into());
    let _liar = logging_peer(&p, Arc::new(sample(6_000_000, 24)), usize::MAX, Arc::default());
    let db = TempDir::new("growing-liar");
    let local = Arc::new(FsBlobStore::open(&db.0).unwrap());
    let store = P2pBlobStore::new(local.clone(), b.handle.clone(), vec![p.handle.peer_id()]);
    let (mut rd, size) = store.open(&cid).await.unwrap();
    assert_eq!(size, 6_000_000);
    let mut got = Vec::new();
    let err = tokio::time::timeout(Duration::from_secs(30), rd.read_to_end(&mut got)).await.unwrap().unwrap_err().to_string();
    assert!(err.contains("empreinte"), "la fin de lecture attend le verdict : {err}");
    assert!(local.open(&cid).await.is_err(), "rien n'entre dans le magasin");
    assert_eq!(std::fs::read_dir(db.0.join(".tmp")).unwrap().count(), 0, "partie fausse jetée");
}

#[tokio::test]
async fn a_peer_that_goes_silent_is_abandoned_after_the_idle_delay() {
    let key = generate_swarm_key().unwrap();
    let (a, a_addr) = node(91, Some(&key), false).await;
    let (s, s_addr) = node(92, Some(&key), false).await; // annonce le blob puis se tait au milieu de chaque plage
    let (b, _) = node(93, Some(&key), false).await;
    b.handle.dial(s_addr).await.unwrap();
    b.handle.dial(a_addr).await.unwrap();
    let data = sample(6_000_000, 13);
    let (da, db) = (TempDir::new("mute-a"), TempDir::new("mute-b"));
    let store_a = Arc::new(FsBlobStore::open(&da.0).unwrap());
    let cid = put_blob(&store_a, &data).await;
    let _rogue = rogue_peer(&s, Arc::new(data.clone()), 100_000, true);
    let store_b = FsBlobStore::open(&db.0).unwrap();
    let idle = Duration::from_secs(2);

    let started = std::time::Instant::now();
    let err = fetch_blob(&b.handle, &[s.handle.peer_id()], &cid, &store_b, 1 << 30, idle).await.unwrap_err().to_string();
    assert!(err.contains("aucun octet"), "{err}");
    assert!(started.elapsed() < Duration::from_secs(20), "pas d'attente sans fin : {:?}", started.elapsed());

    let _sa = serve_blobs(&a.handle, store_a, ServeOpts::default()).unwrap();
    let meta = fetch_blob(&b.handle, &[s.handle.peer_id(), a.handle.peer_id()], &cid, &store_b, 1 << 30, idle).await.unwrap();
    assert_eq!(meta.cid, cid, "le pair muet est abandonné, l'autre termine");
}

#[tokio::test]
async fn behind_a_box_a_station_is_reachable_through_a_relay_circuit() {
    let key = generate_swarm_key().unwrap();
    let (relay, relay_addr) = node(9, Some(&key), true).await;
    let mut public = relay_addr.clone();
    public.pop(); // sans /p2p/<PeerID>
    relay.handle.add_external_address(public).await;
    let (a, _) = node(1, Some(&key), false).await; // « derrière la box » : joint le relais, n'est joint que par lui
    let (b, _) = node(2, Some(&key), false).await;
    let circuit = a.handle.listen_via_relay(relay_addr.clone()).await.unwrap();
    assert!(circuit.to_string().contains("p2p-circuit"), "{circuit}");

    let to_a: Multiaddr = format!("{relay_addr}/p2p-circuit/p2p/{}", a.handle.peer_id()).parse().unwrap();
    assert_eq!(b.handle.dial(to_a).await.unwrap(), a.handle.peer_id());

    let da = TempDir::new("ra");
    let store_a = Arc::new(FsBlobStore::open(&da.0).unwrap());
    let data = sample(700_000, 2);
    let cid = put_blob(&store_a, &data).await;
    let _server = serve_blobs(&a.handle, store_a, ServeOpts::default()).unwrap();
    let db = TempDir::new("rb");
    let store_b = FsBlobStore::open(&db.0).unwrap();
    let meta = fetch_from(&b.handle, a.handle.peer_id(), &cid, &store_b, 1 << 30, FETCH).await.unwrap();
    assert_eq!(meta.cid, cid, "blob rapatrié à travers le circuit relais, intégrité vérifiée");

    let echo = echo_server().await;
    let _exposed = expose(&a.handle, "echo", echo.to_string(), ExposeOpts::default()).unwrap();
    let fwd = forward(&b.handle, "127.0.0.1:0".parse().unwrap(), a.handle.peer_id(), "echo").await.unwrap();
    let mut s = TcpStream::connect(fwd.local_addr).await.unwrap();
    s.write_all(b"bonjour a travers le relais").await.unwrap();
    s.shutdown().await.unwrap();
    let mut back = String::new();
    s.read_to_string(&mut back).await.unwrap();
    assert_eq!(back, "bonjour a travers le relais");
}

#[tokio::test]
async fn a_peer_can_be_asked_whether_it_holds_a_blob_without_transferring_it() {
    use astro_p2p::blob::has_blob;
    let key = generate_swarm_key().unwrap();
    let (a, a_addr) = node(41, Some(&key), false).await;
    let (b, _) = node(42, Some(&key), false).await;
    b.handle.dial(a_addr).await.unwrap();
    let dir = TempDir::new("has");
    let store = Arc::new(FsBlobStore::open(dir.0.join("a")).unwrap());
    let data = sample(300_000, 5);
    let cid = put_blob(&store, &data).await;
    let _server = serve_blobs(&a.handle, store.clone(), ServeOpts::default()).unwrap();

    assert_eq!(has_blob(&b.handle, a.handle.peer_id(), &cid, FETCH).await, Ok(Some(300_000)), "présent : la taille, aucun octet de contenu");
    let absent = cid_raw_sha256(&Sha256::digest(b"absent").into());
    assert_eq!(has_blob(&b.handle, a.handle.peer_id(), &absent, FETCH).await, Ok(None), "absent : ce n'est pas une erreur");
    assert!(has_blob(&b.handle, a.handle.peer_id(), "bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi", FETCH).await.is_err(), "CID non échangeable : refusé avant tout flux");
    // Le téléchargement complet fonctionne toujours.
    let dest = FsBlobStore::open(dir.0.join("b")).unwrap();
    assert_eq!(fetch_from(&b.handle, a.handle.peer_id(), &cid, &dest, 1 << 20, FETCH).await.unwrap().cid, cid);
}

#[tokio::test]
async fn in_a_shared_pool_a_plane_never_serves_the_blob_of_its_neighbour() {
    use astro_p2p::blob::{CidGate, has_blob};
    // ORIGIN (réseau ouvert) partage son dossier de blobs avec une UPlanet privée : le chiffré du plan privé est sur le
    // même disque. Un inconnu qui connaît son CID ne doit ni l'obtenir, ni apprendre qu'il est là.
    let (origin, origin_addr) = node(71, None, false).await;
    let (stranger, _) = node(72, None, false).await;
    stranger.handle.dial(origin_addr).await.unwrap();
    let dir = TempDir::new("pool-gate");
    let pool = Arc::new(FsBlobStore::open(dir.0.join("pool")).unwrap());
    let mine = put_blob(&pool, &sample(200_000, 7)).await;
    let neighbour = put_blob(&pool, &sample(200_000, 8)).await;
    let referenced: Arc<HashSet<String>> = Arc::new([mine.clone()].into());
    let gate = CidGate(Arc::new(move |cid: String| {
        let referenced = referenced.clone();
        Box::pin(async move { referenced.contains(&cid) })
    }));
    let _server = serve_blobs(&origin.handle, pool.clone(), ServeOpts { gate: Some(gate), ..ServeOpts::default() }).unwrap();

    let dest = FsBlobStore::open(dir.0.join("dest")).unwrap();
    assert_eq!(fetch_from(&stranger.handle, origin.handle.peer_id(), &mine, &dest, 1 << 20, FETCH).await.unwrap().cid, mine, "ce que le plan référence sort");
    let err = fetch_from(&stranger.handle, origin.handle.peer_id(), &neighbour, &dest, 1 << 20, FETCH).await.unwrap_err().to_string();
    assert!(err.contains("n'a pas ce blob"), "le blob du voisin est « absent », pas « refusé » : {err}");
    assert_eq!(has_blob(&stranger.handle, origin.handle.peer_id(), &neighbour, FETCH).await, Ok(None), "même la sonde ne révèle rien");
    assert!(matches!(dest.open(&neighbour).await, Err(astro_dav::blob::BlobError::NotFound)));
}

#[tokio::test]
async fn a_station_answers_each_asker_according_to_who_asks() {
    use astro_p2p::ask::{AnswerFn, MAX_ANSWER, answer, ask};
    let (a, a_addr) = node(61, None, false).await;
    let (b, _) = node(62, None, false).await;
    let (c, _) = node(63, None, false).await;
    b.handle.dial(a_addr.clone()).await.unwrap();
    c.handle.dial(a_addr).await.unwrap();
    let friend = b.handle.peer_id();
    let f: AnswerFn = Arc::new(move |peer| Box::pin(async move { if peer == friend { b"bureau-vnc".to_vec() } else { Vec::new() } }));
    let _answers = answer(&a.handle, "/test/ask/1", f).unwrap();
    let t = std::time::Duration::from_secs(5);
    assert_eq!(ask(&b.handle, a.handle.peer_id(), "/test/ask/1", t).await.unwrap(), b"bureau-vnc");
    assert!(ask(&c.handle, a.handle.peer_id(), "/test/ask/1", t).await.unwrap().is_empty(), "C ne voit rien");
    // Une réponse trop grande est tronquée par la station : le demandeur ne reçoit jamais plus que la borne.
    let big: AnswerFn = Arc::new(|_| Box::pin(async { vec![7u8; MAX_ANSWER * 4] }));
    let _big = answer(&a.handle, "/test/big/1", big).unwrap();
    assert_eq!(ask(&b.handle, a.handle.peer_id(), "/test/big/1", t).await.unwrap().len(), MAX_ANSWER);
    // Protocole inconnu chez la station : erreur claire, pas de blocage.
    assert!(ask(&b.handle, a.handle.peer_id(), "/test/absent/1", t).await.is_err());
}

#[tokio::test]
async fn a_reservation_is_visible_while_it_lasts_and_disappears_with_the_relay() {
    let key = generate_swarm_key().unwrap();
    let (relay, relay_addr) = node(51, Some(&key), true).await;
    let mut public = relay_addr.clone();
    public.pop();
    relay.handle.add_external_address(public).await;
    let (a, _) = node(52, Some(&key), false).await;
    assert!(a.handle.reserved_relays().is_empty());
    a.handle.listen_via_relay(relay_addr).await.unwrap();
    let relay_id = relay.handle.peer_id();
    assert_eq!(a.handle.reserved_relays(), vec![relay_id], "réservation tenue chez le relais");
    relay.shutdown().await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while !a.handle.reserved_relays().is_empty() {
        assert!(tokio::time::Instant::now() < deadline, "la réservation perdue doit disparaître des adresses d'écoute");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(a.handle.listen_addrs().iter().all(|x| !x.to_string().contains("p2p-circuit")));
}

#[tokio::test]
async fn a_station_is_reachable_by_a_domain_name_address() {
    let key = generate_swarm_key().unwrap();
    let (a, a_addr) = node(61, Some(&key), false).await;
    let (b, _) = node(62, Some(&key), false).await;
    let port = a_addr.iter().find_map(|p| if let astro_p2p::multiaddr::Protocol::Tcp(p) = p { Some(p) } else { None }).unwrap();
    let by_name: Multiaddr = format!("/dns4/localhost/tcp/{port}/p2p/{}", a.handle.peer_id()).parse().unwrap();
    assert_eq!(b.handle.dial(by_name).await.unwrap(), a.handle.peer_id());
}

#[tokio::test]
#[ignore = "demande le multicast du réseau local (mDNS) : cargo test -p astro-p2p --test p2p -- --ignored"]
async fn two_stations_of_the_same_lan_find_and_join_each_other_without_any_address() {
    let key = generate_swarm_key().unwrap();
    let other = generate_swarm_key().unwrap();
    let lan = |seed: u8, k: &str| {
        let mut cfg = P2pConfig::local([seed; 32]);
        cfg.psk = Some(parse_swarm_key(k).unwrap());
        cfg.listen = vec!["/ip4/0.0.0.0/tcp/0".parse().unwrap()];
        cfg.mdns = true;
        P2pNode::spawn(cfg)
    };
    let a = lan(71, &key).await.unwrap();
    let b = lan(72, &key).await.unwrap();
    let stranger = lan(73, &other).await.unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while !a.handle.connected_peers().contains(&b.handle.peer_id()) {
        assert!(tokio::time::Instant::now() < deadline, "les deux stations du réseau local doivent se relier seules");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(!a.handle.connected_peers().contains(&stranger.handle.peer_id()), "une autre constellation ne se relie pas");
}

#[tokio::test]
async fn one_grouped_probe_says_which_files_a_peer_holds_and_the_gate_still_hides_the_neighbour() {
    use astro_p2p::blob::{CidGate, has_many};
    let (a, a_addr) = node(51, None, false).await;
    let (b, _) = node(52, None, false).await;
    b.handle.dial(a_addr).await.unwrap();
    let dir = TempDir::new("hasn");
    let pool = Arc::new(FsBlobStore::open(dir.0.join("pool")).unwrap());
    let (mine, other, neighbour) = (put_blob(&pool, &sample(10_000, 1)).await, put_blob(&pool, &sample(10_000, 4)).await, put_blob(&pool, &sample(10_000, 6)).await); // graines impaires après `| 1` : 1, 5, 7
    let allowed: Arc<HashSet<String>> = Arc::new([mine.clone(), other.clone()].into());
    let gate = CidGate(Arc::new(move |cid: String| {
        let allowed = allowed.clone();
        Box::pin(async move { allowed.contains(&cid) })
    }));
    let _server = serve_blobs(&a.handle, pool, ServeOpts { gate: Some(gate), ..ServeOpts::default() }).unwrap();
    let absent = cid_raw_sha256(&Sha256::digest(b"absent").into());
    let multi = "bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi".to_owned();
    let asked = vec![mine, absent, neighbour, multi, other];
    assert_eq!(has_many(&b.handle, a.handle.peer_id(), &asked, FETCH).await.unwrap(), [true, false, false, false, true], "présent, absent, voisin caché, non échangeable, présent");
}
