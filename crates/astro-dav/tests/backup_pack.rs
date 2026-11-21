//! Paquets et index de la sauvegarde k-sur-n, hors réseau (`docs/design/SAUVEGARDE_RS.md`, lot 2).
#![allow(clippy::unwrap_used)]

use std::collections::HashMap;

use astro_crypto::{Cipher, UencKey};
use astro_dav::backup_pack::{
    BlobOut, Extractor, FragmentSink, INDEX_INFO, PAYLOAD, PACKET_SIZE, PackBuilder, PackError, PackIndex, SealedPacket, index_key,
    open_packet, refragment,
};
use astro_dav::blob::cid_raw_sha256;
use astro_dav::manifest::Manifest;
use sha2::{Digest, Sha256};

const OWNER: &str = "a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90";

/// Octets pseudo-aléatoires reproductibles (xorshift).
fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

fn cid(bytes: &[u8]) -> String {
    cid_raw_sha256(&Sha256::digest(bytes).into())
}

#[derive(Default)]
struct Keep(Vec<SealedPacket>);
impl FragmentSink for Keep {
    fn put(&mut self, sealed: SealedPacket) -> Result<(), PackError> {
        self.0.push(sealed);
        Ok(())
    }
}

#[derive(Default)]
struct Out {
    open: HashMap<String, Vec<u8>>,
    done: HashMap<String, Vec<u8>>,
    rejected: Vec<String>,
}
impl BlobOut for Out {
    fn write(&mut self, cid: &str, bytes: &[u8]) -> Result<(), PackError> {
        self.open.entry(cid.to_owned()).or_default().extend_from_slice(bytes);
        Ok(())
    }
    fn done(&mut self, cid: &str, ok: bool) -> Result<(), PackError> {
        let b = self.open.remove(cid).unwrap_or_default();
        if ok {
            self.done.insert(cid.to_owned(), b);
        } else {
            self.rejected.push(cid.to_owned());
        }
        Ok(())
    }
}

/// Construit le flux de `blobs` en 3-sur-5.
async fn build(blobs: &[Vec<u8>]) -> (PackIndex, Keep) {
    let mut b = PackBuilder::new(OWNER, 1, 3, 5, Cipher::ChaCha20Poly1305, []).unwrap();
    let mut sink = Keep::default();
    for blob in blobs {
        let mut r = blob.as_slice();
        b.push_blob(&cid(blob), &mut r, &mut sink).await.unwrap();
    }
    (b.finish(&mut sink).await.unwrap(), sink)
}

#[tokio::test]
async fn aller_retour_complet_avec_perte_de_n_moins_k_fragments() {
    let mut blobs: Vec<Vec<u8>> = (0..40).map(|i| noise(1 + i * 7_919, i as u64)).collect();
    blobs.push(noise(2 * PACKET_SIZE + 3 * (1 << 20) + 17, 1000)); // plus grand que deux paquets : s'étale sur trois
    blobs.push(noise(3 << 20, 1001));
    blobs.push(Vec::new()); // blob vide
    blobs.extend((0..10).map(|i| noise(200_000 + i, 2000 + i as u64)));
    let (index, sink) = build(&blobs).await;

    assert_eq!(index.blobs.len(), blobs.len());
    assert_eq!(sink.0.len(), index.packets.len());
    assert!(index.packets.len() >= 3, "le gros blob s'étale sur plusieurs paquets");
    for p in &index.packets[..index.packets.len() - 1] {
        assert_eq!(p.len, PACKET_SIZE as u64, "paquets pleins : 16 Mio chiffrés");
    }
    let last = index.packets.last().unwrap();
    assert_eq!(last.len % (1 << 20), 0, "dernier paquet complété au Mio supérieur");
    assert!(last.len < PACKET_SIZE as u64);
    let big = index.blobs.iter().find(|b| b.len > PACKET_SIZE as u64).unwrap();
    assert!(big.off + big.len > PAYLOAD as u64);

    // L'index passe par son chiffrement avant de servir.
    let key = index_key(&[7u8; 32]);
    let sealed = index.seal(&key, Cipher::Aes256Gcm).await.unwrap();
    let index = PackIndex::open(&sealed, &key, OWNER).await.unwrap();

    let mut ex = Extractor::new(&index);
    let mut out = Out::default();
    for sp in &sink.0 {
        let entry = &index.packets[sp.packet as usize];
        assert_eq!(sp.fragments.len(), 5);
        for (f, e) in sp.fragments.iter().zip(&entry.frags) {
            assert_eq!(cid(f), e.cid);
        }
        // Perte de n − k = 2 fragments, dont des données : décodage par la parité.
        let kept: Vec<&[u8]> = [1usize, 3, 4].iter().map(|&i| sp.fragments[i].as_slice()).collect();
        let plain = open_packet(entry, &kept).await.unwrap();
        let r = ex.feed(sp.packet, &plain, &mut out).unwrap();
        assert!(r.bad.is_empty(), "{:?}", r.bad);
    }
    assert_eq!(ex.finish(&mut out).unwrap(), Default::default());
    assert!(out.rejected.is_empty());
    for blob in &blobs {
        assert_eq!(out.done.get(&cid(blob)).map(Vec::len), Some(blob.len()));
        assert!(out.done[&cid(blob)] == *blob);
    }
}

#[tokio::test]
async fn un_cid_n_entre_qu_une_fois() {
    let a = noise(5_000, 1);
    let b = noise(6_000, 2);
    let c = noise(7_000, 3);
    let mut builder = PackBuilder::new(OWNER, 1, 1, 1, Cipher::detect(), [cid(&c)]).unwrap();
    let mut sink = Keep::default();
    assert!(builder.push_blob(&cid(&a), &mut a.as_slice(), &mut sink).await.unwrap());
    assert!(!builder.push_blob(&cid(&a), &mut a.as_slice(), &mut sink).await.unwrap(), "déjà dans ce flux");
    assert!(!builder.push_blob(&cid(&c), &mut c.as_slice(), &mut sink).await.unwrap(), "déjà dans un paquet précédent");
    assert!(builder.push_blob(&cid(&b), &mut b.as_slice(), &mut sink).await.unwrap());
    let index = builder.finish(&mut sink).await.unwrap();
    let cids: Vec<&str> = index.blobs.iter().map(|b| b.cid.as_str()).collect();
    assert_eq!(cids, [cid(&a), cid(&b)]);
    assert_eq!((index.packets.len(), index.packets[0].len), (1, 1 << 20));
    assert_eq!(index.blobs[1].off, 5_000);
}

#[tokio::test]
async fn cid_faux_refuse_et_constructeur_hors_d_usage() {
    let a = noise(5_000, 1);
    let mut builder = PackBuilder::new(OWNER, 1, 2, 3, Cipher::detect(), []).unwrap();
    let mut sink = Keep::default();
    let wrong = cid(b"autre chose");
    assert!(matches!(builder.push_blob(&wrong, &mut a.as_slice(), &mut sink).await, Err(PackError::BadCid(_))));
    assert!(matches!(builder.push_blob(&cid(&a), &mut a.as_slice(), &mut sink).await, Err(PackError::Poisoned)));
    // Forme de CID non vérifiable par SHA-256 brut (racine dag-pb) : refusée d'emblée.
    let mut fresh = PackBuilder::new(OWNER, 1, 2, 3, Cipher::detect(), []).unwrap();
    assert!(matches!(fresh.push_blob("bafybeigdyrzt", &mut a.as_slice(), &mut sink).await, Err(PackError::BadCid(_))));
    assert!(PackBuilder::new(OWNER, 1, 0, 3, Cipher::detect(), []).is_err());
    assert!(PackBuilder::new(OWNER, 1, 4, 3, Cipher::detect(), []).is_err());
    assert!(PackBuilder::new(OWNER, 1, 33, 40, Cipher::detect(), []).is_err());
}

/// Lecteur qui fabrique `len` octets à la volée, sans jamais les tenir en entier.
struct Stream {
    left: usize,
    x: u64,
}
impl tokio::io::AsyncRead for Stream {
    fn poll_read(mut self: std::pin::Pin<&mut Self>, _: &mut std::task::Context<'_>, buf: &mut tokio::io::ReadBuf<'_>) -> std::task::Poll<std::io::Result<()>> {
        let n = buf.remaining().min(self.left).min(8 << 10);
        for _ in 0..n {
            self.x ^= self.x << 13;
            self.x ^= self.x >> 7;
            self.x ^= self.x << 17;
            let b = self.x as u8;
            buf.put_slice(&[b]);
        }
        self.left -= n;
        std::task::Poll::Ready(Ok(()))
    }
}

/// Garde seulement le plus gros paquet reçu (en octets), pour mesurer ce que le constructeur rend à la fois.
#[derive(Default)]
struct Measure {
    packets: usize,
    max_bytes: usize,
}
impl FragmentSink for Measure {
    fn put(&mut self, sealed: SealedPacket) -> Result<(), PackError> {
        self.packets += 1;
        self.max_bytes = self.max_bytes.max(sealed.fragments.iter().map(Vec::len).sum());
        Ok(())
    }
}

#[tokio::test]
async fn memoire_bornee_paquet_par_paquet() {
    let len = 2 * PACKET_SIZE + (5 << 20);
    let mut hasher = Sha256::new();
    let mut s = Stream { left: len, x: 99 };
    let mut chunk = Vec::new();
    use tokio::io::AsyncReadExt;
    while s.left > 0 {
        chunk.clear();
        (&mut s).take(1 << 20).read_to_end(&mut chunk).await.unwrap();
        hasher.update(&chunk);
    }
    let c = cid_raw_sha256(&hasher.finalize().into());
    let mut builder = PackBuilder::new(OWNER, 1, 2, 3, Cipher::detect(), []).unwrap();
    let mut sink = Measure::default();
    builder.push_blob(&c, &mut Stream { left: len, x: 99 }, &mut sink).await.unwrap();
    assert!(builder.buffer_capacity() <= PAYLOAD, "tampon d'un seul paquet");
    assert_eq!(sink.packets, 2, "les paquets pleins partent dès qu'ils sont pleins");
    let index = builder.finish(&mut sink).await.unwrap();
    assert_eq!(sink.packets, 3);
    // Ce qui sort à la fois : n fragments d'un paquet, soit n/k × 16 Mio (+ en-têtes), jamais le flux entier.
    assert!(sink.max_bytes <= 3 * (PACKET_SIZE / 2 + 72), "{}", sink.max_bytes);
    assert_eq!(index.blobs[0].len, len as u64);
}

#[tokio::test]
async fn fragment_d_un_autre_paquet_refuse_et_suite_manquante_rejetee() {
    let big = noise(PACKET_SIZE + 10_000, 5);
    let small = noise(1_000, 6);
    let (index, sink) = build(&[big.clone(), small.clone()]).await;
    assert_eq!(index.packets.len(), 2);
    let f1: Vec<&[u8]> = sink.0[1].fragments.iter().map(Vec::as_slice).collect();
    assert!(matches!(open_packet(&index.packets[0], &f1).await, Err(PackError::ForeignFragment)));
    // Un fragment altéré n'a plus son CID : refusé avant décodage.
    let mut bad = sink.0[0].fragments[0].clone();
    *bad.last_mut().unwrap() ^= 1;
    let f0: Vec<&[u8]> = vec![&bad, &sink.0[0].fragments[1], &sink.0[0].fragments[2]];
    assert!(matches!(open_packet(&index.packets[0], &f0).await, Err(PackError::ForeignFragment)));

    // Paquet 1 seul : le gros blob, commencé au paquet 0, ne peut pas être rendu ; le petit, si.
    let p1 = open_packet(&index.packets[1], &f1[..3]).await.unwrap();
    let mut ex = Extractor::new(&index);
    let mut out = Out::default();
    let r = ex.feed(1, &p1, &mut out).unwrap();
    assert_eq!(r.ok, [cid(&small)]);
    assert!(!out.done.contains_key(&cid(&big)));
}

#[tokio::test]
async fn index_dechiffre_seulement_avec_la_bonne_cle() {
    let key = index_key(&[1u8; 32]);
    let other = index_key(&[2u8; 32]);
    let sealed = PackIndex::new(OWNER, 3).seal(&key, Cipher::detect()).await.unwrap();
    assert_eq!(PackIndex::open(&sealed, &key, OWNER).await.unwrap().rev, 3);
    assert!(PackIndex::open(&sealed, &other, OWNER).await.is_err());
    let not_owner = "f".repeat(64);
    assert!(matches!(PackIndex::open(&sealed, &key, &not_owner).await, Err(PackError::BadIndex(_))));
}

#[tokio::test]
async fn index_forge_ou_ancien_rev() {
    let key = index_key(&[1u8; 32]);
    let v2 = PackIndex::new(OWNER, 2).seal(&key, Cipher::detect()).await.unwrap();
    let v5 = PackIndex::new(OWNER, 5).seal(&key, Cipher::detect()).await.unwrap();
    // Forgé par un gardien : rev énorme, mais pas la clé.
    let forged = PackIndex::new(OWNER, 1_000).seal(&index_key(&[9u8; 32]), Cipher::detect()).await.unwrap();
    // Bien chiffré mais incohérent (blob dans un paquet qui n'existe pas).
    let mut incoherent = PackIndex::new(OWNER, 900);
    incoherent.blobs.push(astro_dav::backup_pack::BlobEntry { cid: "bafkreix".into(), packet: 4, off: 0, len: 1, held_whole: vec![] });
    let incoherent = incoherent.seal(&key, Cipher::detect()).await.unwrap();
    let mut tampered = v5.clone();
    tampered[40] ^= 1;
    let garbage = vec![0u8; 100];
    let all: Vec<&[u8]> = vec![&v2, &forged, &incoherent, &tampered, &garbage, &v5];
    assert_eq!(PackIndex::best(&all, &key, OWNER).await.unwrap().rev, 5);
    // Sans le plus récent : l'ancien reste une restauration partielle valide.
    let old: Vec<&[u8]> = vec![&forged, &v2, &tampered];
    assert_eq!(PackIndex::best(&old, &key, OWNER).await.unwrap().rev, 2);
    assert!(PackIndex::best(&[&forged], &key, OWNER).await.is_none());
}

#[test]
fn ancien_manifeste_toujours_lisible() {
    let old = r#"{"v":1,"owner":"ab","rev":4,"created_at":10,"entries":[],"keyring":[]}"#;
    let m: Manifest = serde_json::from_str(old).unwrap();
    assert_eq!(m.backup_index, None);
    assert_eq!(serde_json::to_string(&m).unwrap(), old, "sans index : octets inchangés");
    let with = Manifest { backup_index: Some("bafkreiabc".into()), ..m };
    let back: Manifest = serde_json::from_str(&serde_json::to_string(&with).unwrap()).unwrap();
    assert_eq!(back.backup_index.as_deref(), Some("bafkreiabc"));
    assert_eq!(astro_dav::manifest::VERSION, 1);
}

// ---- Vecteur 6 (§ 4.5) : figé, recoupé par `tests/vector6.py` (Python, `cryptography`). ----

/// Secret de test : la clé privée Nostr 0x01..=0x20.
const V6_SECRET: [u8; 32] = {
    let mut s = [0u8; 32];
    let mut i = 0;
    while i < 32 {
        s[i] = i as u8 + 1;
        i += 1;
    }
    s
};
const V6_KEY: &str = "6849519bb7db61b7137264dda400b9af873e577e150e4801147090bdcf77acae";
const V6_JSON: &str = r#"{"v":1,"owner":"a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90","rev":42,"packet_size":16777216,"packets":[],"blobs":[],"legacy":{}}"#;
/// Fabriqué par Python (`cryptography`, préfixe de nonce 01..07, AES-256-GCM), indépendamment du code Rust.
const V6_BLOB: &str = "55454e430201100102030405060761deba40c65631cf5052e94a67d9a75f60e358fedb9fc2567f9c8a3efe73db76a7c685cbb1e6f968d6cc441b278974d67a4b8430cfce47469ef92d8ec5a84a8eb22e8f922a9155afccb921a1554c444322beee446a1bfbef4c0a7986afb3bea80d2b9f8e6394d9d3c5409720a75c086896314e6966fd27b265ac7bdd0c41dfa97bc9b70aae6f358abb6645bc1bfba7cd1cf4b9511fc0b24c35aedef74d35d9f12a1eeaec39df";

#[tokio::test]
async fn vecteur_6_index_minimal() {
    assert_eq!(INDEX_INFO, b"astro-backup-index-v1");
    let key = index_key(&V6_SECRET);
    assert_eq!(key.to_hex(), V6_KEY);
    assert_eq!(serde_json::to_string(&PackIndex::new(OWNER, 42)).unwrap(), V6_JSON);
    let blob = hex::decode(V6_BLOB).unwrap();
    let index = PackIndex::open(&blob, &key, OWNER).await.unwrap();
    assert_eq!(index, PackIndex::new(OWNER, 42));
    assert!(PackIndex::open(&blob, &UencKey::from_bytes(V6_SECRET), OWNER).await.is_err(), "le secret brut n'est pas la clé");
}

/// Lot 5 : un fragment se recalcule depuis le clair et l'en-tête UENC gardé, octet pour octet (même CID), y compris un
/// indice neuf ; un clair différent ne sort jamais (le nonce serait réutilisé).
#[tokio::test]
async fn un_fragment_se_recalcule_depuis_le_clair_et_l_en_tete() {
    let blobs: Vec<Vec<u8>> = (0..5).map(|i| noise(300_000 + i * 11, 50 + i as u64)).collect();
    let (index, sink) = build(&blobs).await;
    let entry = &index.packets[0];
    let sp = &sink.0[0];
    let plain = open_packet(entry, &[sp.fragments[0].as_slice(), sp.fragments[1].as_slice(), sp.fragments[2].as_slice()]).await.unwrap();
    let again = refragment(entry, &sp.head, &plain, &[0, 4, 7]).await.unwrap();
    assert_eq!(cid(&again[0]), entry.frags[0].cid);
    assert_eq!(cid(&again[1]), entry.frags[4].cid);
    // Indice neuf (7) : décodable avec deux anciens fragments.
    let mixed = open_packet(&astro_dav::backup_pack::PacketEntry { frags: vec![entry.frags[1].clone(), entry.frags[2].clone(), astro_dav::backup_pack::FragEntry { i: 7, cid: cid(&again[2]), host: None }], ..entry.clone() }, &[sp.fragments[1].as_slice(), sp.fragments[2].as_slice(), again[2].as_slice()]).await.unwrap();
    assert!(mixed == plain);
    let mut other = plain.clone();
    other[10] ^= 1;
    assert!(matches!(refragment(entry, &sp.head, &other, &[0]).await, Err(PackError::ForeignFragment)));
    assert!(refragment(entry, &sp.head, &plain[1..], &[0]).await.is_err());
}
