//! Vecteurs figés du format ARSF v1 (`docs/design/SAUVEGARDE_RS.md` § 4.5, vecteurs 1 à 5).
//! Calculés une fois, puis recalculés par une implémentation Python indépendante (multiplication bit à bit, inverse par
//! recherche exhaustive, en-tête écrit avec `struct.pack`). **Ne jamais modifier pour faire passer un test** : un changement
//! ici est un changement de format, donc une nouvelle version.
#![allow(clippy::unwrap_used)]

use astro_erasure::arsf::{HEADER_LEN, Header};
use astro_erasure::{cauchy, decode, encode, gf};
use sha2::{Digest, Sha256};

fn sha(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}

fn pattern(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i % 251) as u8).collect()
}

#[test]
fn v1_tables() {
    assert_eq!(hex::encode(&gf::EXP[..8]), "0103050f113355ff");
    assert_eq!(gf::LOG[0x03], 1);
    assert_eq!(gf::mul(0x57, 0x83), 0xC1, "exemple FIPS-197 § 4.2");
    assert_eq!(gf::inv(0x53), 0xCA, "exemple FIPS-197 § 5.1.1");
}

#[test]
fn v2_cauchy_k3() {
    let m: Vec<u8> = (3..=5u8).flat_map(|i| (0..3u8).map(move |c| cauchy(i, c))).collect();
    assert_eq!(hex::encode(m), "f68d01cb527b52cbd1");
}

const V3: [&str; 5] = [
    "5960446fe4a7a2d67477592917bc0cbb2e7b6813263372e67f14897334c393eb",
    "e96790c59e938d7868fef690e674811ffb8cb23150cf048e48ecf0934fc5d822",
    "4631dac631af6a4012162f07553fb4863be65ec5b58e722f9f1140fa92fe3bcc",
    "98c5382468afc5891861d62f6fdb9384ba2e7e121798ce08235be1d980986f1b",
    "5f41d14492cbeeec3212b84a85c66c16c46433a27d5b868a9468d807364a042e",
];

#[test]
fn v3_packet_1000_k3_n5() {
    let packet = pattern(1000);
    let frags = encode(&packet, 3, &[0, 1, 2, 3, 4]).unwrap();
    for (f, want) in frags.iter().zip(V3) {
        assert_eq!(f.len(), HEADER_LEN + 384);
        assert_eq!(sha(f), want);
    }
    let mut subsets = 0;
    for a in 0..5 {
        for b in a + 1..5 {
            for c in b + 1..5 {
                assert_eq!(decode(&[&frags[a], &frags[b], &frags[c]]).unwrap(), packet, "{a}{b}{c}");
                subsets += 1;
            }
        }
    }
    assert_eq!(subsets, 10);
}

#[test]
fn v4_header_bytes() {
    let frags = encode(&pattern(1000), 3, &[3]).unwrap();
    assert_eq!(
        hex::encode(&frags[0][..HEADER_LEN]),
        "41525346010103034e4c294b331f7a2099a379bec34b9f9f00000000000003e8000001804e4c294b331f7a2099a379bec34b9f9fc03dc46ab465d998f4d683da53487e6d00000000"
    );
    let (h, _) = Header::parse(&frags[0]).unwrap();
    assert_eq!((h.k, h.index, h.packet_len, h.frag_len), (3, 3, 1000, 384));
}

#[test]
fn v5_k1_n1_is_header_plus_padded_packet() {
    let packet = pattern(100);
    let f = &encode(&packet, 1, &[0]).unwrap()[0];
    assert_eq!(f.len(), 200);
    assert_eq!(&f[HEADER_LEN..HEADER_LEN + 100], &packet[..]);
    assert!(f[HEADER_LEN + 100..].iter().all(|b| *b == 0), "bourrage à zéro");
    assert_eq!(
        hex::encode(&f[..HEADER_LEN]),
        "4152534601010100bce0aff19cf5aa6a7469a30d61d04e43000000000000006400000080bce0aff19cf5aa6a7469a30d61d04e4376e4bbf6381052ee9e7f33925c954d5200000000"
    );
    assert_eq!(sha(f), "85eafbe8ebed12384da4cf08c68f7fe731477c01bd5272bccdab15d71038b7ce");
    assert_eq!(decode(&[f]).unwrap(), packet);
}
