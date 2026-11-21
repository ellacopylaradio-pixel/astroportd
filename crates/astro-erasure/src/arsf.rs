//! En-tête de fragment `ARSF` v1 (72 octets, entiers gros-boutistes) ; voir `docs/design/SAUVEGARDE_RS.md` § 4.1.
//!
//! | Décalage | Taille | Champ |
//! |---|---|---|
//! | 0 | 4 | `"ARSF"` |
//! | 4 | 1 | version `0x01` |
//! | 5 | 1 | code `0x01` (Cauchy sur GF(2⁸) `0x11B`) |
//! | 6 | 1 | `k` (1..=32) |
//! | 7 | 1 | indice (0..=254) |
//! | 8 | 16 | `packet_id` = SHA-256 du paquet\[..16\] |
//! | 24 | 8 | `packet_len` |
//! | 32 | 4 | `frag_len` (multiple de 64) |
//! | 36 | 32 | SHA-256 du paquet |
//! | 68 | 4 | réservé = 0 |
//! | 72 | `frag_len` | données |

use crate::{ErasureError, MAX_FRAG_LEN, MAX_INDEX, MAX_K};

/// Magie de début de fragment.
pub const MAGIC: [u8; 4] = *b"ARSF";
/// Version de l'en-tête.
pub const VERSION: u8 = 1;
/// Identifiant du code : Cauchy systématique sur GF(2⁸), polynôme `0x11B`.
pub const CODE_CAUCHY_GF256: u8 = 1;
/// Taille de l'en-tête.
pub const HEADER_LEN: usize = 72;
/// Alignement de `frag_len`.
pub const ALIGN: usize = 64;

/// En-tête d'un fragment, validé.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    /// Nombre de fragments nécessaires.
    pub k: u8,
    /// Indice du fragment (`< k` : donnée, sinon parité).
    pub index: u8,
    /// Longueur du paquet avant bourrage.
    pub packet_len: u64,
    /// Longueur des données du fragment.
    pub frag_len: u32,
    /// SHA-256 du paquet complet (les 16 premiers octets forment `packet_id`).
    pub packet_sha256: [u8; 32],
}

impl Header {
    /// `packet_id` : les 16 premiers octets du SHA-256 du paquet.
    pub fn packet_id(&self) -> [u8; 16] {
        let mut id = [0u8; 16];
        id.copy_from_slice(&self.packet_sha256[..16]);
        id
    }

    /// Longueur de fragment imposée par `packet_len` et `k` : le plus petit multiple de 64 tel que `k · frag_len ≥ packet_len`.
    pub fn frag_len_for(packet_len: u64, k: u8) -> Option<u64> {
        let stripe = u64::from(k).checked_mul(ALIGN as u64)?;
        if stripe == 0 {
            return None;
        }
        packet_len.div_ceil(stripe).checked_mul(ALIGN as u64)
    }

    /// Vérifie la cohérence interne (mêmes règles en écriture et en lecture).
    pub fn validate(&self) -> Result<(), ErasureError> {
        if self.k == 0 || self.k > MAX_K {
            return Err(ErasureError::BadK(self.k));
        }
        if self.index > MAX_INDEX {
            return Err(ErasureError::BadIndex(self.index));
        }
        if self.frag_len as usize > MAX_FRAG_LEN {
            return Err(ErasureError::TooLarge);
        }
        // `k · frag_len ≥ packet_len` et `< packet_len + k·64` ⇔ `frag_len` est exactement la longueur imposée.
        if Self::frag_len_for(self.packet_len, self.k) != Some(u64::from(self.frag_len)) {
            return Err(ErasureError::BadLength);
        }
        Ok(())
    }

    /// Écrit l'en-tête (72 octets).
    pub fn to_bytes(&self) -> Result<[u8; HEADER_LEN], ErasureError> {
        self.validate()?;
        let mut h = [0u8; HEADER_LEN];
        h[0..4].copy_from_slice(&MAGIC);
        h[4] = VERSION;
        h[5] = CODE_CAUCHY_GF256;
        h[6] = self.k;
        h[7] = self.index;
        h[8..24].copy_from_slice(&self.packet_sha256[..16]);
        h[24..32].copy_from_slice(&self.packet_len.to_be_bytes());
        h[32..36].copy_from_slice(&self.frag_len.to_be_bytes());
        h[36..68].copy_from_slice(&self.packet_sha256);
        Ok(h) // 68..72 : réservé, zéros
    }

    /// Lit et valide l'en-tête d'un fragment complet ; renvoie l'en-tête et les données.
    /// La taille du blob doit être exactement `72 + frag_len` : aucune allocation ne dépend d'un champ non vérifié.
    pub fn parse(blob: &[u8]) -> Result<(Self, &[u8]), ErasureError> {
        let Some((h, data)) = blob.split_first_chunk::<HEADER_LEN>() else { return Err(ErasureError::Truncated) };
        if h[0..4] != MAGIC {
            return Err(ErasureError::BadMagic);
        }
        if h[4] != VERSION {
            return Err(ErasureError::UnknownVersion(h[4]));
        }
        if h[5] != CODE_CAUCHY_GF256 {
            return Err(ErasureError::UnknownCode(h[5]));
        }
        if h[68..72] != [0; 4] {
            return Err(ErasureError::Reserved);
        }
        let mut sha = [0u8; 32];
        sha.copy_from_slice(&h[36..68]);
        if h[8..24] != sha[..16] {
            return Err(ErasureError::Discordant);
        }
        let mut len8 = [0u8; 8];
        len8.copy_from_slice(&h[24..32]);
        let mut len4 = [0u8; 4];
        len4.copy_from_slice(&h[32..36]);
        let header = Self { k: h[6], index: h[7], packet_len: u64::from_be_bytes(len8), frag_len: u32::from_be_bytes(len4), packet_sha256: sha };
        header.validate()?;
        if data.len() != header.frag_len as usize {
            return Err(ErasureError::BadLength);
        }
        Ok((header, data))
    }

    /// Même paquet, même code : tout sauf l'indice.
    pub fn same_packet(&self, other: &Self) -> bool {
        self.k == other.k && self.packet_len == other.packet_len && self.frag_len == other.frag_len && self.packet_sha256 == other.packet_sha256
    }
}
