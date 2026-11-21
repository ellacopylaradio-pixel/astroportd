//! Code d'effacement de la sauvegarde entre amis (`docs/design/SAUVEGARDE_RS.md`, lot 1).
//!
//! - [`gf`] : corps GF(2⁸) `0x11B` (partagé avec le partage de Shamir d'`astro-lifecycle`) ;
//! - [`encode`] / [`decode`] : code de Cauchy **systématique** — les fragments `0..k` sont des tranches du paquet,
//!   l'indice `i ≥ k` est la parité de coefficients `1 / (i ⊕ c)` (c ∈ 0..k) ;
//! - [`arsf`] : en-tête de fragment `ARSF` v1.
//!
//! Crate à part, sans `unsafe` ni dépendance lourde (sha2, thiserror) : le format doit rester décodable dans dix ans,
//! indépendamment d'`astro-dav` ou du binaire. Tout changement des vecteurs de `tests/vectors.rs` est un changement de format.
#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod arsf;
pub mod gf;

use arsf::{HEADER_LEN, Header};
use sha2::{Digest, Sha256};

/// `k` maximal (Q4).
pub const MAX_K: u8 = 32;
/// Indice de fragment maximal (Q4) : `x = 255` serait le seul point libre restant, gardé hors format.
pub const MAX_INDEX: u8 = 254;
/// Taille de paquet retenue (Q2) : 16 Mio.
pub const PACKET_SIZE: usize = 16 << 20;
/// Plafond des données d'un fragment (`k = 1` d'un paquet de 16 Mio).
pub const MAX_FRAG_LEN: usize = PACKET_SIZE;

/// Erreurs du code et du format.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ErasureError {
    /// Blob plus court que l'en-tête.
    #[error("fragment tronqué")]
    Truncated,
    /// Magie autre que `ARSF`.
    #[error("ce n'est pas un fragment ARSF")]
    BadMagic,
    /// Version d'en-tête inconnue.
    #[error("version de fragment inconnue : {0}")]
    UnknownVersion(u8),
    /// Code inconnu.
    #[error("code de fragment inconnu : {0}")]
    UnknownCode(u8),
    /// Octets réservés non nuls.
    #[error("champ réservé non nul")]
    Reserved,
    /// `k` hors de 1..=32.
    #[error("k invalide : {0}")]
    BadK(u8),
    /// Indice hors de 0..=254.
    #[error("indice de fragment invalide : {0}")]
    BadIndex(u8),
    /// `frag_len` incohérent avec la taille du blob ou avec `packet_len`.
    #[error("longueur de fragment incohérente")]
    BadLength,
    /// Paquet ou fragment au-delà des plafonds.
    #[error("paquet trop grand")]
    TooLarge,
    /// En-têtes discordants (autre paquet, autre `k`…).
    #[error("fragments de paquets différents")]
    Discordant,
    /// Deux fragments de même indice.
    #[error("indice de fragment en double : {0}")]
    Duplicate(u8),
    /// Moins de `k` fragments distincts.
    #[error("{have} fragments pour {need} nécessaires")]
    NotEnough {
        /// Fragments distincts fournis.
        have: usize,
        /// `k`.
        need: usize,
    },
    /// Sous-matrice non inversible (impossible avec des indices distincts : défense en profondeur).
    #[error("matrice non inversible")]
    Singular,
    /// SHA-256 du paquet reconstitué différent de l'en-tête.
    #[error("empreinte du paquet fausse")]
    BadDigest,
}

/// Coefficient de Cauchy de la ligne de parité `index` (≥ k) pour la colonne `c` (< k) : `1 / (index ⊕ c)`.
pub fn cauchy(index: u8, c: u8) -> u8 {
    gf::inv(index ^ c)
}

/// Ligne `index` de la matrice systématique `[I_k ; C]`.
fn row(index: u8, k: u8) -> Vec<u8> {
    (0..k).map(|c| if index < k { u8::from(c == index) } else { cauchy(index, c) }).collect()
}

/// Encode `packet` en fragments ARSF complets (en-tête + données), un par indice demandé.
/// Les indices doivent être distincts et ≤ 254 ; `k` ∈ 1..=32.
pub fn encode(packet: &[u8], k: u8, indices: &[u8]) -> Result<Vec<Vec<u8>>, ErasureError> {
    if k == 0 || k > MAX_K {
        return Err(ErasureError::BadK(k));
    }
    for (n, &i) in indices.iter().enumerate() {
        if i > MAX_INDEX {
            return Err(ErasureError::BadIndex(i));
        }
        if indices[..n].contains(&i) {
            return Err(ErasureError::Duplicate(i));
        }
    }
    let packet_len = packet.len() as u64;
    let frag_len = Header::frag_len_for(packet_len, k).ok_or(ErasureError::TooLarge)?;
    let frag_len = usize::try_from(frag_len).ok().filter(|f| *f <= MAX_FRAG_LEN).ok_or(ErasureError::TooLarge)?;
    let sha: [u8; 32] = Sha256::digest(packet).into();
    let header = Header { k, index: 0, packet_len, frag_len: frag_len as u32, packet_sha256: sha };
    header.validate()?;
    // Tranche `c` du paquet complété par des zéros (seule la dernière tranche non vide est partielle).
    let slice = |c: usize| -> &[u8] {
        let start = (c * frag_len).min(packet.len());
        &packet[start..((c + 1) * frag_len).min(packet.len())]
    };
    indices
        .iter()
        .map(|&index| {
            let mut out = Vec::with_capacity(HEADER_LEN + frag_len);
            out.extend_from_slice(&Header { index, ..header }.to_bytes()?);
            out.resize(HEADER_LEN + frag_len, 0);
            let data = &mut out[HEADER_LEN..];
            if index < k {
                let s = slice(usize::from(index));
                data[..s.len()].copy_from_slice(s);
            } else {
                for c in 0..k {
                    gf::mul_add(cauchy(index, c), slice(usize::from(c)), data);
                }
            }
            Ok(out)
        })
        .collect()
}

/// Inverse d'une matrice carrée `k × k` (lignes concaténées) par Gauss-Jordan dans GF(2⁸).
fn invert(mut m: Vec<u8>, k: usize) -> Result<Vec<u8>, ErasureError> {
    let mut r = vec![0u8; k * k];
    for i in 0..k {
        r[i * k + i] = 1;
    }
    for col in 0..k {
        let pivot = (col..k).find(|&p| m[p * k + col] != 0).ok_or(ErasureError::Singular)?;
        for j in 0..k {
            m.swap(pivot * k + j, col * k + j);
            r.swap(pivot * k + j, col * k + j);
        }
        let f = gf::inv(m[col * k + col]);
        for j in 0..k {
            m[col * k + j] = gf::mul(m[col * k + j], f);
            r[col * k + j] = gf::mul(r[col * k + j], f);
        }
        for p in (0..k).filter(|&p| p != col) {
            let g = m[p * k + col];
            if g != 0 {
                for j in 0..k {
                    m[p * k + j] ^= gf::mul(g, m[col * k + j]);
                    r[p * k + j] ^= gf::mul(g, r[col * k + j]);
                }
            }
        }
    }
    Ok(r)
}

/// Reconstitue le paquet à partir de fragments ARSF complets (au moins `k` d'indices distincts, même paquet).
/// Chaque fragment est validé ([`Header::parse`]) ; les données sont préférées aux parités ; l'empreinte finale est vérifiée.
/// La mémoire allouée est bornée par la taille des fragments fournis (`k · frag_len`).
pub fn decode(fragments: &[&[u8]]) -> Result<Vec<u8>, ErasureError> {
    let mut parsed: Vec<(Header, &[u8])> = Vec::with_capacity(fragments.len().min(usize::from(MAX_INDEX) + 1));
    for blob in fragments {
        let (h, data) = Header::parse(blob)?;
        if let Some((first, _)) = parsed.first()
            && !first.same_packet(&h)
        {
            return Err(ErasureError::Discordant);
        }
        if parsed.iter().any(|(p, _)| p.index == h.index) {
            return Err(ErasureError::Duplicate(h.index));
        }
        parsed.push((h, data));
    }
    let Some((header, _)) = parsed.first().copied() else { return Err(ErasureError::NotEnough { have: 0, need: 1 }) };
    let k = usize::from(header.k);
    if parsed.len() < k {
        return Err(ErasureError::NotEnough { have: parsed.len(), need: k });
    }
    parsed.sort_by_key(|(h, _)| h.index); // données d'abord
    parsed.truncate(k);
    let frag_len = header.frag_len as usize;
    let mut packet = vec![0u8; k * frag_len];
    if parsed.iter().all(|(h, _)| usize::from(h.index) < k) {
        for (c, (_, data)) in parsed.iter().enumerate() {
            packet[c * frag_len..(c + 1) * frag_len].copy_from_slice(data);
        }
    } else {
        let m: Vec<u8> = parsed.iter().flat_map(|(h, _)| row(h.index, header.k)).collect();
        let inverse = invert(m, k)?;
        for c in 0..k {
            let out = &mut packet[c * frag_len..(c + 1) * frag_len];
            for (j, (_, data)) in parsed.iter().enumerate() {
                gf::mul_add(inverse[c * k + j], data, out);
            }
        }
    }
    let len = usize::try_from(header.packet_len).map_err(|_| ErasureError::TooLarge)?;
    packet.truncate(len);
    if Sha256::digest(&packet).as_slice() != header.packet_sha256 {
        return Err(ErasureError::BadDigest);
    }
    Ok(packet)
}
