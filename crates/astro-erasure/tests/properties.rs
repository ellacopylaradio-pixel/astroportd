//! Propriétés du code (toutes les combinaisons de k fragments parmi n) et décodeur face à des fragments hostiles (§ 8.3).
#![allow(clippy::unwrap_used)]

use astro_erasure::arsf::HEADER_LEN;
use astro_erasure::{ErasureError, MAX_FRAG_LEN, PACKET_SIZE, decode, encode};

/// Paquet pseudo-aléatoire déterministe (xorshift).
fn noise(n: usize, seed: u64) -> Vec<u8> {
    let mut s = seed | 1;
    (0..n)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s as u8
        })
        .collect()
}

/// Appelle `f` sur chaque sous-ensemble de taille `k` de `0..n`.
fn combinations(n: usize, k: usize, f: &mut impl FnMut(&[usize])) {
    fn go(start: usize, n: usize, k: usize, cur: &mut Vec<usize>, f: &mut impl FnMut(&[usize])) {
        if cur.len() == k {
            return f(cur);
        }
        for i in start..n {
            cur.push(i);
            go(i + 1, n, k, cur, f);
            cur.pop();
        }
    }
    go(0, n, k, &mut Vec::new(), f);
}

#[test]
fn every_k_subset_decodes_for_k_up_to_6_and_n_up_to_10() {
    for size in [0usize, 1, 63, 64, 65, 1000] {
        for k in 1..=6u8 {
            let packet = noise(size, size as u64 * 31 + u64::from(k));
            for n in k..=10u8 {
                let indices: Vec<u8> = (0..n).collect();
                let frags = encode(&packet, k, &indices).unwrap();
                combinations(usize::from(n), usize::from(k), &mut |set| {
                    // Ordre inversé : le décodeur ne doit pas dépendre de l'ordre de présentation.
                    let pick: Vec<&[u8]> = set.iter().rev().map(|&i| frags[i].as_slice()).collect();
                    assert_eq!(decode(&pick).unwrap(), packet, "taille {size}, {k}-sur-{n}, {set:?}");
                });
            }
        }
    }
}

#[test]
fn high_parity_indices_and_k32_decode() {
    let packet = noise(5000, 7);
    let frags = encode(&packet, 32, &(223..=254).collect::<Vec<u8>>()).unwrap(); // que de la parité
    let all: Vec<&[u8]> = frags.iter().map(Vec::as_slice).collect();
    assert_eq!(decode(&all).unwrap(), packet);
}

/// 16 Mio en 4-sur-6, sans les deux premières données. Lent en debug (≈ quelques secondes) : gardé actif.
#[test]
fn full_packet_16_mib() {
    let packet = noise(PACKET_SIZE, 42);
    let frags = encode(&packet, 4, &[0, 1, 2, 3, 4, 5]).unwrap();
    assert_eq!(frags[0].len(), HEADER_LEN + PACKET_SIZE / 4);
    assert_eq!(decode(&[&frags[5], &frags[2], &frags[4], &frags[3]]).unwrap(), packet);
}

/// Débit d'encodage, à lancer en release : `cargo test -p astro-erasure --release --test properties -- --ignored --nocapture`.
#[test]
#[ignore]
fn throughput() {
    let packet = noise(PACKET_SIZE, 1);
    for (k, n) in [(3u8, 5u8), (4, 6), (3, 6)] {
        let indices: Vec<u8> = (0..n).collect();
        let t = std::time::Instant::now();
        let reps = 5;
        for _ in 0..reps {
            std::hint::black_box(encode(&packet, k, &indices).unwrap());
        }
        let s = t.elapsed().as_secs_f64() / f64::from(reps);
        println!("encode {k}-sur-{n} : {:.0} Mio/s de paquet ({:.0} ms / 16 Mio)", 16.0 / s, s * 1000.0);
        let frags = encode(&packet, k, &indices).unwrap();
        let pick: Vec<&[u8]> = frags[usize::from(n - k)..].iter().map(Vec::as_slice).collect();
        let t = std::time::Instant::now();
        std::hint::black_box(decode(&pick).unwrap());
        println!("decode {k}-sur-{n} sans données de tête : {:.0} ms", t.elapsed().as_secs_f64() * 1000.0);
    }
}

// ───────────────────────────── Hostile ─────────────────────────────

fn sample() -> (Vec<u8>, Vec<Vec<u8>>) {
    let p = noise(1000, 3);
    let f = encode(&p, 3, &[0, 1, 2, 3, 4]).unwrap();
    (p, f)
}

#[test]
fn encode_refuses_bad_parameters() {
    assert_eq!(encode(b"x", 0, &[0]), Err(ErasureError::BadK(0)));
    assert_eq!(encode(b"x", 33, &[0]), Err(ErasureError::BadK(33)));
    assert_eq!(encode(b"x", 2, &[255]), Err(ErasureError::BadIndex(255)));
    assert_eq!(encode(b"x", 2, &[3, 3]), Err(ErasureError::Duplicate(3)));
}

#[test]
fn encode_refuses_a_packet_beyond_the_fragment_cap() {
    // k = 1 : un paquet d'un octet de plus que 16 Mio dépasserait MAX_FRAG_LEN (aucune allocation de fragment n'a lieu).
    let big = vec![0u8; MAX_FRAG_LEN + 1];
    assert_eq!(encode(&big, 1, &[0]), Err(ErasureError::TooLarge));
}

#[test]
fn reserved_version_code_magic_and_truncation() {
    let (_, f) = sample();
    let mut x = f[0].clone();
    x[70] = 1;
    assert_eq!(decode(&[&x]), Err(ErasureError::Reserved));
    let mut x = f[0].clone();
    x[4] = 2;
    assert_eq!(decode(&[&x]), Err(ErasureError::UnknownVersion(2)));
    let mut x = f[0].clone();
    x[5] = 2;
    assert_eq!(decode(&[&x]), Err(ErasureError::UnknownCode(2)));
    let mut x = f[0].clone();
    x[0] = b'B';
    assert_eq!(decode(&[&x]), Err(ErasureError::BadMagic));
    assert_eq!(decode(&[&f[0][..71]]), Err(ErasureError::Truncated));
    assert_eq!(decode(&[&[][..]]), Err(ErasureError::Truncated));
    assert_eq!(decode(&[]), Err(ErasureError::NotEnough { have: 0, need: 1 }));
}

#[test]
fn bad_k_and_index_in_header() {
    let (_, f) = sample();
    let mut x = f[0].clone();
    x[6] = 0;
    assert_eq!(decode(&[&x]), Err(ErasureError::BadK(0)));
    x[6] = 200;
    assert_eq!(decode(&[&x]), Err(ErasureError::BadK(200)));
    let mut x = f[0].clone();
    x[7] = 255;
    assert_eq!(decode(&[&x]), Err(ErasureError::BadIndex(255)));
}

#[test]
fn inflated_lengths_are_refused_without_allocating() {
    let (_, f) = sample();
    // frag_len gonflé (bombe mémoire) : 4 Gio annoncés, blob de 456 octets.
    let mut x = f[0].clone();
    x[32..36].copy_from_slice(&u32::MAX.to_be_bytes());
    assert_eq!(decode(&[&x]), Err(ErasureError::TooLarge));
    // frag_len plausible mais différent de la taille du blob.
    let mut x = f[0].clone();
    x[32..36].copy_from_slice(&448u32.to_be_bytes());
    assert_eq!(decode(&[&x]), Err(ErasureError::BadLength));
    // packet_len énorme.
    let mut x = f[0].clone();
    x[24..32].copy_from_slice(&u64::MAX.to_be_bytes());
    assert_eq!(decode(&[&x]), Err(ErasureError::BadLength));
    // packet_len incompatible avec frag_len (k · frag_len ≥ packet_len + k·64).
    let mut x = f[0].clone();
    x[24..32].copy_from_slice(&800u64.to_be_bytes());
    assert_eq!(decode(&[&x]), Err(ErasureError::BadLength));
    // Données plus longues que frag_len.
    let mut x = f[0].clone();
    x.push(0);
    assert_eq!(decode(&[&x]), Err(ErasureError::BadLength));
}

#[test]
fn discordant_headers_are_refused() {
    let (_, f) = sample();
    // Fragment d'un autre paquet.
    let other = encode(&noise(1000, 4), 3, &[3]).unwrap();
    assert_eq!(decode(&[&f[0], &f[1], &other[0]]), Err(ErasureError::Discordant));
    // Même paquet, autre k.
    let other_k = encode(&noise(1000, 3), 2, &[3]).unwrap();
    assert_eq!(decode(&[&f[0], &f[1], &other_k[0]]), Err(ErasureError::Discordant));
    // packet_id qui ne correspond pas au SHA-256 de l'en-tête.
    let mut x = f[0].clone();
    x[8] ^= 1;
    assert_eq!(decode(&[&x]), Err(ErasureError::Discordant));
}

#[test]
fn duplicate_indices_and_too_few_fragments() {
    let (_, f) = sample();
    assert_eq!(decode(&[&f[0], &f[0], &f[1]]), Err(ErasureError::Duplicate(0)));
    assert_eq!(decode(&[&f[0], &f[3]]), Err(ErasureError::NotEnough { have: 2, need: 3 }));
    // Une donnée recopiée en parité : l'en-tête est retouché, mais le SHA final la démasque.
    let mut fake = f[0].clone();
    fake[7] = 4;
    assert_eq!(decode(&[&f[1], &f[2], &fake]), Err(ErasureError::BadDigest));
}

#[test]
fn corrupted_data_fails_the_final_digest() {
    let (p, f) = sample();
    let mut x = f[3].clone();
    x[HEADER_LEN + 10] ^= 0x55;
    assert_eq!(decode(&[&x, &f[0], &f[1], &f[2]]), Ok(p.clone()), "la parité en trop n'est pas lue quand les données suffisent");
    assert_eq!(decode(&[&f[0], &x, &f[4]]), Err(ErasureError::BadDigest));
    // SHA final faux mais cohérent partout (packet_id compris) : seul le contrôle final peut le voir.
    let forged: Vec<Vec<u8>> = f[..3]
        .iter()
        .map(|x| {
            let mut x = x.clone();
            x[12] ^= 1;
            x[40] ^= 1;
            x
        })
        .collect();
    assert_eq!(decode(&[&forged[0], &forged[1], &forged[2]]), Err(ErasureError::BadDigest));
}

#[test]
fn random_garbage_never_panics() {
    for seed in 0..2000u64 {
        let mut blob = noise(72 + (seed as usize % 300), seed);
        if seed % 2 == 0 {
            blob[..6].copy_from_slice(b"ARSF\x01\x01");
            blob[68..72].fill(0);
        }
        let _ = decode(&[&blob]);
    }
}
