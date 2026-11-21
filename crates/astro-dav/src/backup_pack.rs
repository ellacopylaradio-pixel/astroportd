//! Paquets et index de la sauvegarde k-sur-n entre amis (`docs/design/SAUVEGARDE_RS.md`, lot 2, hors réseau).
//!
//! - [`PackBuilder`] concatène des blobs UENC v2 privés (adressés par CID) en un flux découpé en paquets ; chaque
//!   paquet est chiffré en UENC v2 avec une clé aléatoire qui lui est propre, puis codé en fragments ARSF par
//!   [`astro_erasure::encode`]. Traitement **paquet par paquet** : la mémoire est bornée par un paquet clair, un
//!   paquet chiffré et ses fragments, quelle que soit la taille des blobs.
//! - [`PackIndex`] : l'index JSON (§ 4.3), chiffré en UENC v2 par une clé dérivée du secret du compte ([`index_key`]).
//! - [`open_packet`] + [`Extractor`] : `k` fragments → paquet chiffré → paquet clair → blobs, chacun revérifié par son
//!   CID avant d'être rendu.
//!
//! Taille : un paquet **chiffré** fait exactement [`PACKET_SIZE`] (16 Mio) ; sa charge utile est donc
//! [`PAYLOAD`] = 16 Mio − 14 − 256 × 16 octets (en-tête et étiquettes UENC). Le dernier paquet est complété (zéros
//! en clair) pour que son chiffré tombe sur le Mio supérieur. Ainsi un fragment `k = 1` tient sous
//! `astro_erasure::MAX_FRAG_LEN` sans toucher au format ARSF.
//!
//! Placé dans `astro-dav` (et non dans `astro-erasure`) : il dépend d'`astro-crypto` (UENC v2) et du calcul de CID
//! (`blob::cid_raw_sha256`), alors qu'`astro-erasure` garde le format pur (sha2, thiserror) ; `astro-erasure` ne
//! dépend de rien du projet, il n'y a donc pas de cycle. Le `Manifest` voisin porte le CID de l'index
//! (`backup_index`).

use std::collections::{BTreeMap, HashSet};

use astro_crypto::uenc::{self, HEADER_LEN as UENC_HEADER_LEN, TAG_LEN};
use astro_crypto::{Cipher, UencError, UencKey};
use astro_erasure::ErasureError;
use astro_erasure::arsf::Header;
use hkdf::Hkdf;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt};
use zeroize::Zeroizing;

use crate::blob::cid_raw_sha256;

pub use astro_erasure::arsf::{HEADER_LEN as FRAG_HEADER_LEN, Header as FragHeader};
pub use astro_erasure::{MAX_INDEX as MAX_FRAG_INDEX, MAX_K, PACKET_SIZE};

/// Taille maximale d'un fragment ARSF complet (en-tête compris) : ce qu'un gardien accepte de ranger au plus.
pub const MAX_FRAGMENT_BYTES: u64 = (FRAG_HEADER_LEN + astro_erasure::MAX_FRAG_LEN) as u64;

/// Version du format de l'index.
pub const INDEX_VERSION: u8 = 1;
/// `info` HKDF de la clé de l'index (§ 4.3). Le changer = nouveau format.
pub const INDEX_INFO: &[u8] = b"astro-backup-index-v1";
/// Un Mio : granularité du dernier paquet.
const MIB: usize = 1 << 20;
/// Taille d'un chunk UENC v2 (64 Kio).
const CHUNK: usize = 1 << 16;
/// Charge utile (clair) d'un paquet plein : son chiffré fait exactement [`PACKET_SIZE`].
pub const PAYLOAD: usize = payload_for(PACKET_SIZE / MIB);
/// Plafond de l'index chiffré accepté à la lecture (bombe mémoire).
pub const MAX_INDEX_BYTES: usize = 64 << 20;

/// Clair d'un paquet dont le chiffré fait `mib` Mio : `mib · 2²⁰ − 14 − 16 · (16 · mib)` (16 chunks de 64 Kio par
/// Mio, chacun suivi de son étiquette ; valable tant que `256 · mib + 14 < 64 Kio`, soit jusqu'à 255 Mio).
const fn payload_for(mib: usize) -> usize {
    mib * MIB - UENC_HEADER_LEN - TAG_LEN * (mib * MIB / CHUNK)
}

/// Charge utile (clair) d'un paquet dont le chiffré fait `packet_len` octets (multiple de 1 Mio, ≤ [`PACKET_SIZE`]) :
/// sert à savoir sur quels paquets s'étale un blob. `None` hors de ces bornes.
pub fn payload_of(packet_len: u64) -> Option<u64> {
    let mib = usize::try_from(packet_len).ok()?;
    if mib == 0 || mib > PACKET_SIZE || mib % MIB != 0 {
        return None;
    }
    Some(payload_for(mib / MIB) as u64)
}

/// Erreurs du paquetage.
#[derive(Debug, thiserror::Error)]
pub enum PackError {
    /// Code d'effacement ou en-tête ARSF.
    #[error("fragments : {0}")]
    Erasure(#[from] ErasureError),
    /// Chiffrement UENC.
    #[error("chiffrement : {0}")]
    Uenc(#[from] UencError),
    /// Entrée/sortie en lisant un blob.
    #[error("lecture : {0}")]
    Io(#[from] std::io::Error),
    /// Index illisible (JSON).
    #[error("index illisible : {0}")]
    Json(#[from] serde_json::Error),
    /// Index incohérent (version, propriétaire, bornes).
    #[error("index invalide : {0}")]
    BadIndex(&'static str),
    /// CID absent, d'une autre forme que `bafkrei…`, ou ne correspondant pas aux octets lus.
    #[error("CID incorrect : {0}")]
    BadCid(String),
    /// Fragment dont le CID ou l'indice n'est pas celui que l'index annonce pour ce paquet.
    #[error("fragment étranger au paquet")]
    ForeignFragment,
    /// Paramètres `k`/`n` refusés.
    #[error("paramètres invalides : {0}")]
    BadParams(&'static str),
    /// Le constructeur a déjà échoué : ses paquets en cours ne sont plus sûrs.
    #[error("constructeur hors d'usage après une erreur")]
    Poisoned,
    /// Erreur rendue par le destinataire des fragments ou des blobs.
    #[error("destination : {0}")]
    Sink(String),
}

/// Clé de l'index des paquets d'un compte : HKDF-SHA256 (sans sel, `info` = [`INDEX_INFO`]) de la **clé privée
/// Nostr** du MULTIPASS.
///
/// Pourquoi ce secret : c'est `sha256(graine)` où la graine vient de scrypt sur la formule **isolée**
/// `(SALT, PEPPER_<sha256(UPLANETNAME)[:16]>)` (`astro_core::formula::KeyFormula::Isolated`,
/// `astro_core::kdf::Seed::nostr_keys`). Donc :
/// - la station qui héberge le compte l'a déjà (elle signe pour lui) ;
/// - une station qui restaure depuis la carte secrète (SALT/PEPPER) la recalcule, sans le manifeste (pas de cercle) ;
/// - elle est isolée par constellation, comme toutes les clés du MULTIPASS ;
/// - un gardien qui tient l'index chiffré doit passer par scrypt pour éprouver un SALT/PEPPER faible, ce qui ne
///   serait pas le cas si l'on dérivait directement de SALT/PEPPER par HKDF (rapide).
pub fn index_key(nostr_secret: &[u8; 32]) -> UencKey {
    let hk = Hkdf::<Sha256>::new(None, nostr_secret);
    let mut okm = Zeroizing::new([0u8; 32]);
    // 32 octets ≤ 255 × 32 : `expand` ne peut pas échouer ; on garde une clé nulle plutôt que paniquer si jamais.
    if hk.expand(INDEX_INFO, okm.as_mut()).is_err() {
        okm.fill(0);
    }
    UencKey::from_bytes(*okm)
}

/// Un fragment produit pour un paquet.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FragEntry {
    /// Indice ARSF (`< k` = donnée).
    pub i: u8,
    /// CID du fragment (en-tête compris).
    pub cid: String,
    /// Gardien qui le tient (rempli à l'envoi, lot 3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
}

/// Un paquet de l'index.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PacketEntry {
    /// `packet_id` ARSF en hex (16 premiers octets du SHA-256 du paquet chiffré).
    pub id: String,
    /// Clé UENC du paquet (64 hex).
    pub key: String,
    /// Longueur du paquet **chiffré** (= `packet_len` ARSF), multiple de 1 Mio.
    pub len: u64,
    /// `k` du paquet.
    pub k: u8,
    /// Fragments produits.
    pub frags: Vec<FragEntry>,
}

/// Un blob dans le flux.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BlobEntry {
    /// CID du blob (`bafkrei…`).
    pub cid: String,
    /// Premier paquet (indice dans `packets`).
    pub packet: u32,
    /// Décalage dans le clair de ce paquet.
    pub off: u64,
    /// Longueur totale ; au-delà de la fin du paquet, le blob continue au début des paquets suivants.
    pub len: u64,
    /// Gardiens qui tiennent déjà ce blob en entier (§ 2.4).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub held_whole: Vec<String>,
}

/// Index des paquets (§ 4.3).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PackIndex {
    /// Version ([`INDEX_VERSION`]).
    pub v: u8,
    /// Propriétaire (64 hex).
    pub owner: String,
    /// Révision : on garde la plus haute qui se déchiffre.
    pub rev: u64,
    /// Taille d'un paquet chiffré plein.
    pub packet_size: u64,
    /// Paquets.
    pub packets: Vec<PacketEntry>,
    /// Blobs, dans l'ordre du flux.
    pub blobs: Vec<BlobEntry>,
    /// Copies complètes d'avant le codage : gardien → CID.
    #[serde(default)]
    pub legacy: BTreeMap<String, Vec<String>>,
}

impl PackIndex {
    /// Index vide.
    pub fn new(owner: &str, rev: u64) -> Self {
        Self {
            v: INDEX_VERSION,
            owner: owner.to_owned(),
            rev,
            packet_size: PACKET_SIZE as u64,
            packets: Vec::new(),
            blobs: Vec::new(),
            legacy: BTreeMap::new(),
        }
    }

    /// Sérialise et chiffre l'index (UENC v2).
    pub async fn seal(&self, key: &UencKey, cipher: Cipher) -> Result<Vec<u8>, PackError> {
        let json = Zeroizing::new(serde_json::to_vec(self)?);
        Ok(uenc::encrypt_to_vec(key, cipher, &json).await?)
    }

    /// Déchiffre et valide un index rendu par un gardien. Refusé s'il ne se déchiffre pas (forgé ou autre compte),
    /// s'il n'est pas à `owner`, ou si ses bornes sont incohérentes.
    pub async fn open(blob: &[u8], key: &UencKey, owner: &str) -> Result<Self, PackError> {
        if blob.len() > MAX_INDEX_BYTES {
            return Err(PackError::BadIndex("index trop grand"));
        }
        let plain = Zeroizing::new(uenc::decrypt_to_vec(key, blob).await?);
        let index: Self = serde_json::from_slice(&plain)?;
        index.validate(owner)?;
        Ok(index)
    }

    /// Choisit, parmi les index rendus par les gardiens, celui de `rev` le plus haut **qui se déchiffre** (§ 8.3).
    /// Les autres (forgés, abîmés, d'un autre compte) sont ignorés.
    pub async fn best(candidates: &[&[u8]], key: &UencKey, owner: &str) -> Option<Self> {
        let mut best: Option<Self> = None;
        for blob in candidates {
            if let Ok(i) = Self::open(blob, key, owner).await
                && best.as_ref().is_none_or(|b| i.rev > b.rev)
            {
                best = Some(i);
            }
        }
        best
    }

    fn validate(&self, owner: &str) -> Result<(), PackError> {
        if self.v != INDEX_VERSION {
            return Err(PackError::BadIndex("version inconnue"));
        }
        if self.owner != owner {
            return Err(PackError::BadIndex("autre propriétaire"));
        }
        if self.packet_size != PACKET_SIZE as u64 {
            return Err(PackError::BadIndex("taille de paquet inconnue"));
        }
        for p in &self.packets {
            if p.len == 0 || p.len > PACKET_SIZE as u64 || p.len % MIB as u64 != 0 {
                return Err(PackError::BadIndex("longueur de paquet"));
            }
        }
        for b in &self.blobs {
            let p = self.packets.get(b.packet as usize).ok_or(PackError::BadIndex("paquet inexistant"))?;
            if b.off >= payload_for((p.len / MIB as u64) as usize) as u64 && b.len > 0 {
                return Err(PackError::BadIndex("décalage hors du paquet"));
            }
        }
        Ok(())
    }
}

/// Les fragments d'un paquet, rendus au destinataire dès que le paquet est scellé.
#[derive(Debug)]
pub struct SealedPacket {
    /// Indice du paquet dans l'index.
    pub packet: u32,
    /// Fragments ARSF complets, dans l'ordre des indices `0..n`.
    pub fragments: Vec<Vec<u8>>,
    /// En-tête UENC v2 du paquet chiffré (préfixe de nonce compris) : à garder **en local** pour recalculer plus tard
    /// n'importe quel fragment depuis les blobs ([`refragment`]), sans garder le paquet. Jamais dans l'index.
    pub head: [u8; UENC_HEADER_LEN],
}

/// Destinataire des paquets scellés (magasin local, envoi…) : le constructeur ne garde rien après l'appel.
pub trait FragmentSink {
    /// Reçoit les fragments d'un paquet.
    fn put(&mut self, sealed: SealedPacket) -> Result<(), PackError>;
}

/// Construit le flux de paquets, blob par blob, en mémoire bornée.
pub struct PackBuilder {
    index: PackIndex,
    cipher: Cipher,
    k: u8,
    n: u8,
    buf: Vec<u8>,
    seen: HashSet<String>,
    poisoned: bool,
}

impl PackBuilder {
    /// Nouveau flux pour `owner`, codé en `k`-sur-`n` (indices `0..n`). `already` : CID déjà dans des paquets
    /// (ils ne repasseront pas). L'index commence vide à la révision `rev`.
    pub fn new(owner: &str, rev: u64, k: u8, n: u8, cipher: Cipher, already: impl IntoIterator<Item = String>) -> Result<Self, PackError> {
        if k == 0 || k > astro_erasure::MAX_K {
            return Err(PackError::BadParams("k hors de 1..=32"));
        }
        if n < k || n > astro_erasure::MAX_INDEX {
            return Err(PackError::BadParams("n hors de k..=254"));
        }
        Ok(Self {
            index: PackIndex::new(owner, rev),
            cipher,
            k,
            n,
            buf: Vec::with_capacity(PAYLOAD),
            seen: already.into_iter().collect(),
            poisoned: false,
        })
    }

    /// Capacité du tampon de paquet (mémoire bornée : jamais plus que [`PAYLOAD`]).
    pub fn buffer_capacity(&self) -> usize {
        self.buf.capacity()
    }

    /// Ajoute un blob lu depuis `reader`, en vérifiant au passage que son SHA-256 est bien `cid`. `Ok(false)` si
    /// ce CID est déjà dans le flux (un CID n'entre qu'une fois). Les paquets remplis sont scellés et rendus à `sink`.
    ///
    /// En cas d'erreur (CID faux, lecture coupée), des octets du blob sont peut-être déjà dans un paquet scellé :
    /// le constructeur refuse alors toute suite ([`PackError::Poisoned`]) ; on recommence le flux.
    pub async fn push_blob<R, S>(&mut self, cid: &str, reader: &mut R, sink: &mut S) -> Result<bool, PackError>
    where
        R: AsyncRead + Unpin,
        S: FragmentSink,
    {
        if self.poisoned {
            return Err(PackError::Poisoned);
        }
        if !cid.starts_with("bafkrei") {
            return Err(PackError::BadCid(cid.to_owned()));
        }
        if self.seen.contains(cid) {
            return Ok(false);
        }
        self.poisoned = true;
        if self.buf.len() == PAYLOAD {
            self.seal(sink).await?;
        }
        let entry = BlobEntry {
            cid: cid.to_owned(),
            packet: self.packet_no()?,
            off: self.buf.len() as u64,
            len: 0,
            held_whole: Vec::new(),
        };
        let mut hasher = Sha256::new();
        let mut len = 0u64;
        let mut scratch = vec![0u8; CHUNK];
        loop {
            let room = (PAYLOAD - self.buf.len()).min(CHUNK);
            let got = reader.read(&mut scratch[..room]).await?;
            if got == 0 {
                break;
            }
            self.buf.extend_from_slice(&scratch[..got]);
            hasher.update(&scratch[..got]);
            len += got as u64;
            if self.buf.len() == PAYLOAD {
                self.seal(sink).await?;
            }
        }
        let digest: [u8; 32] = hasher.finalize().into();
        if cid_raw_sha256(&digest) != cid {
            return Err(PackError::BadCid(cid.to_owned()));
        }
        self.index.blobs.push(BlobEntry { len, ..entry });
        self.seen.insert(cid.to_owned());
        self.poisoned = false;
        Ok(true)
    }

    /// Scelle le dernier paquet (complété au Mio supérieur) et rend l'index.
    pub async fn finish<S: FragmentSink>(mut self, sink: &mut S) -> Result<PackIndex, PackError> {
        if self.poisoned {
            return Err(PackError::Poisoned);
        }
        let used = self.buf.len();
        let pending = self.index.blobs.last().is_some_and(|b| b.packet as usize == self.index.packets.len());
        if used > 0 || pending {
            self.seal(sink).await?;
        }
        Ok(self.index)
    }

    fn packet_no(&self) -> Result<u32, PackError> {
        u32::try_from(self.index.packets.len()).map_err(|_| PackError::BadParams("trop de paquets"))
    }

    async fn seal<S: FragmentSink>(&mut self, sink: &mut S) -> Result<(), PackError> {
        let mib = (1..=PACKET_SIZE / MIB).find(|&m| payload_for(m) >= self.buf.len()).unwrap_or(PACKET_SIZE / MIB);
        self.buf.resize(payload_for(mib), 0);
        let key = UencKey::generate()?;
        let sealed = uenc::encrypt_to_vec(&key, self.cipher, &self.buf).await?;
        self.buf.clear();
        let indices: Vec<u8> = (0..self.n).collect();
        let fragments = astro_erasure::encode(&sealed, self.k, &indices)?;
        let digest: [u8; 32] = Sha256::digest(&sealed).into();
        let mut head = [0u8; UENC_HEADER_LEN];
        head.copy_from_slice(sealed.get(..UENC_HEADER_LEN).ok_or(PackError::BadParams("paquet chiffré trop court"))?);
        drop(sealed);
        let frags = fragments
            .iter()
            .zip(&indices)
            .map(|(f, &i)| FragEntry { i, cid: cid_raw_sha256(&Sha256::digest(f).into()), host: None })
            .collect();
        let packet = self.packet_no()?;
        self.index.packets.push(PacketEntry {
            id: hex::encode(&digest[..16]),
            key: key.to_hex(),
            len: (mib * MIB) as u64,
            k: self.k,
            frags,
        });
        sink.put(SealedPacket { packet, fragments, head })
    }
}

/// Reconstitue le **clair** d'un paquet depuis au moins `k` de ses fragments. Chaque fragment doit avoir le CID et
/// l'indice que l'index annonce (un fragment d'un autre paquet ou forgé est refusé avant décodage) ; le paquet
/// décodé doit avoir l'identifiant de l'index, puis s'authentifier en UENC v2 avec la clé du paquet.
pub async fn open_packet(entry: &PacketEntry, fragments: &[&[u8]]) -> Result<Vec<u8>, PackError> {
    for f in fragments {
        let cid = cid_raw_sha256(&Sha256::digest(f).into());
        let (h, _) = Header::parse(f)?;
        if !entry.frags.iter().any(|e| e.cid == cid && e.i == h.index) {
            return Err(PackError::ForeignFragment);
        }
        if h.k != entry.k || hex::encode(h.packet_id()) != entry.id || h.packet_len != entry.len {
            return Err(PackError::ForeignFragment);
        }
    }
    let sealed = astro_erasure::decode(fragments)?;
    let key = UencKey::from_hex(&entry.key)?;
    Ok(uenc::decrypt_to_vec(&key, &sealed).await?)
}

/// Recalcule les fragments d'indices `indices` d'un paquet déjà émis, depuis son **clair** (charge utile complétée de
/// zéros, longueur [`payload_of`]`(entry.len)`) et l'en-tête UENC gardé au scellement : réparation sans rassembler `k`
/// fragments sur le réseau, et contrôle de possession. Le chiffré recalculé doit avoir l'identifiant de l'index, sinon
/// rien ne sort ([`PackError::ForeignFragment`]) : un clair différent sous le même nonce ne quitte jamais la fonction.
pub async fn refragment(entry: &PacketEntry, head: &[u8; UENC_HEADER_LEN], plain: &[u8], indices: &[u8]) -> Result<Vec<Vec<u8>>, PackError> {
    if payload_of(entry.len) != Some(plain.len() as u64) {
        return Err(PackError::BadParams("clair de paquet de mauvaise longueur"));
    }
    let key = UencKey::from_hex(&entry.key)?;
    let sealed = Zeroizing::new(uenc::reseal_to_vec(&key, head, plain).await?);
    let digest = Sha256::digest(sealed.as_slice());
    if sealed.len() as u64 != entry.len || hex::encode(&digest[..16]) != entry.id {
        return Err(PackError::ForeignFragment);
    }
    Ok(astro_erasure::encode(&sealed, entry.k, indices)?)
}

/// Destinataire des blobs extraits. Les octets arrivent dans l'ordre ; `done` dit si le CID est juste : sur
/// `false`, ce qui a été écrit pour ce CID doit être jeté.
pub trait BlobOut {
    /// Octets suivants du blob `cid`.
    fn write(&mut self, cid: &str, bytes: &[u8]) -> Result<(), PackError>;
    /// Fin du blob : `ok` = SHA-256 conforme au CID.
    fn done(&mut self, cid: &str, ok: bool) -> Result<(), PackError>;
}

/// Bilan d'un paquet extrait.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Extracted {
    /// Blobs complets et vérifiés.
    pub ok: Vec<String>,
    /// Blobs rejetés (CID faux, ou suite manquante : paquet suivant absent ou donné dans le désordre).
    pub bad: Vec<String>,
}

/// Extrait les blobs des paquets clairs, un paquet à la fois. Un blob qui s'étale sur plusieurs paquets exige
/// qu'on lui donne ses paquets dans l'ordre et sans trou ; sinon il est rejeté (les autres blobs ne sont pas touchés).
pub struct Extractor<'a> {
    index: &'a PackIndex,
    by_packet: BTreeMap<u32, Vec<usize>>,
    /// Blob en cours : (indice dans `blobs`, empreinte partielle, octets restants, paquet attendu).
    carry: Option<(usize, Sha256, u64, u32)>,
}

impl<'a> Extractor<'a> {
    /// Prépare l'extraction selon `index`.
    pub fn new(index: &'a PackIndex) -> Self {
        let mut by_packet: BTreeMap<u32, Vec<usize>> = BTreeMap::new();
        for (i, b) in index.blobs.iter().enumerate() {
            by_packet.entry(b.packet).or_default().push(i);
        }
        Self { index, by_packet, carry: None }
    }

    /// Donne le clair du paquet `packet` ; rend les blobs terminés dans ce paquet.
    pub fn feed<O: BlobOut>(&mut self, packet: u32, plain: &[u8], out: &mut O) -> Result<Extracted, PackError> {
        let mut report = Extracted::default();
        if let Some((bi, hasher, left, want)) = self.carry.take() {
            let cid = &self.index.blobs[bi].cid;
            if want == packet {
                let take = usize::try_from(left).unwrap_or(usize::MAX).min(plain.len());
                out.write(cid, &plain[..take])?;
                let mut hasher = hasher;
                hasher.update(&plain[..take]);
                let left = left - take as u64;
                if left == 0 || plain.is_empty() {
                    self.close(bi, hasher, left, out, &mut report)?;
                } else {
                    self.carry = Some((bi, hasher, left, packet + 1));
                }
            } else {
                out.done(cid, false)?;
                report.bad.push(cid.clone());
            }
        }
        let starts = self.by_packet.get(&packet).cloned().unwrap_or_default();
        for bi in starts {
            let b = &self.index.blobs[bi];
            let off = usize::try_from(b.off).unwrap_or(usize::MAX);
            if off > plain.len() {
                out.done(&b.cid, false)?;
                report.bad.push(b.cid.clone());
                continue;
            }
            let avail = plain.len() - off;
            let take = usize::try_from(b.len).unwrap_or(usize::MAX).min(avail);
            out.write(&b.cid, &plain[off..off + take])?;
            let mut hasher = Sha256::new();
            hasher.update(&plain[off..off + take]);
            let left = b.len - take as u64;
            if left == 0 {
                self.close(bi, hasher, 0, out, &mut report)?;
            } else if self.carry.is_some() {
                // Deux blobs à suivre en même temps : index incohérent.
                self.close(bi, hasher, left, out, &mut report)?;
            } else {
                self.carry = Some((bi, hasher, left, packet + 1));
            }
        }
        Ok(report)
    }

    /// Termine l'extraction : un blob encore en attente de suite est rejeté.
    pub fn finish<O: BlobOut>(mut self, out: &mut O) -> Result<Extracted, PackError> {
        let mut report = Extracted::default();
        if let Some((bi, hasher, left, _)) = self.carry.take() {
            self.close(bi, hasher, left.max(1), out, &mut report)?;
        }
        Ok(report)
    }

    fn close<O: BlobOut>(&self, bi: usize, hasher: Sha256, left: u64, out: &mut O, report: &mut Extracted) -> Result<(), PackError> {
        let cid = &self.index.blobs[bi].cid;
        let ok = left == 0 && cid_raw_sha256(&hasher.finalize().into()) == *cid;
        out.done(cid, ok)?;
        if ok { report.ok.push(cid.clone()) } else { report.bad.push(cid.clone()) }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charge_utile_d_un_paquet_plein() {
        assert_eq!(PAYLOAD, 16_773_106);
        assert_eq!(payload_for(1), MIB - 14 - 256);
        // Le chiffré d'un paquet plein fait exactement 16 Mio.
        let chunks = PAYLOAD.div_ceil(CHUNK);
        assert_eq!(PAYLOAD + UENC_HEADER_LEN + TAG_LEN * chunks, PACKET_SIZE);
    }
}
