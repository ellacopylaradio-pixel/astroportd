//! Stockage de blobs adressés par contenu.
//!
//! [`FsBlobStore`] range les blobs chiffrés dans un dossier, nommés par leur CID v1 `raw` + sha2-256
//! (`bafkrei…`). Un adaptateur Kubo (`/api/v0/add`) implémentera le même trait pour publier sur IPFS.

pub use crate::kubo::{AutoBlobStore, KuboBlobStore};

use std::future::Future;
use std::io;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeek, AsyncSeekExt, AsyncWrite, AsyncWriteExt};

/// Erreurs de stockage de blobs.
#[derive(Debug, thiserror::Error)]
pub enum BlobError {
    /// Blob absent.
    #[error("blob introuvable")]
    NotFound,
    /// Identifiant qui n'est pas un CID.
    #[error("CID invalide")]
    BadCid,
    /// Erreur d'entrée/sortie.
    #[error("entrée/sortie : {0}")]
    Io(#[from] io::Error),
}

/// Blob stocké.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobMeta {
    /// CID du blob.
    pub cid: String,
    /// Taille en octets.
    pub size: u64,
}

/// Écriture en flux d'un nouveau blob : rien n'est visible avant `commit`.
pub trait BlobWriter: AsyncWrite + Unpin + Send {
    /// Termine l'écriture, calcule le CID et publie le blob.
    fn commit(self) -> impl Future<Output = Result<BlobMeta, BlobError>> + Send;
}

/// Magasin de blobs.
pub trait BlobStore: Send + Sync + 'static {
    /// Écrivain en flux.
    type Writer: BlobWriter;
    /// Lecteur à accès aléatoire (pour les requêtes `Range`).
    type Reader: AsyncRead + AsyncSeek + Unpin + Send + 'static;

    /// Ouvre un nouvel écrivain.
    fn begin(&self) -> impl Future<Output = Result<Self::Writer, BlobError>> + Send;
    /// Ouvre un blob en lecture ; retourne aussi sa taille.
    fn open(&self, cid: &str) -> impl Future<Output = Result<(Self::Reader, u64), BlobError>> + Send;
    /// Supprime un blob (sans erreur s'il est déjà absent).
    fn remove(&self, cid: &str) -> impl Future<Output = Result<(), BlobError>> + Send;
    /// Espace disque physique `(libre, total)` de la partition qui porte le stockage ; `None` si le magasin n'est pas
    /// sur le disque local (Kubo distant) ou si la mesure est impossible : aucune garde dans ce cas.
    fn disk_space(&self) -> Option<(u64, u64)> {
        None
    }

    /// Écrivain **reprenable** du blob attendu `cid` (rapatriement d'un gros fichier) : ce qui a déjà été reçu avant une
    /// coupure ou un redémarrage est repris, ramené à un multiple de `align` et à `max` octets au plus. Une erreur laisse la
    /// partie reçue en place pour la fois suivante. Par défaut (magasin sans disque local) : un écrivain neuf.
    fn begin_resumable(&self, _cid: &str, _max: u64, _align: u64) -> impl Future<Output = Result<Resumed<Self::Writer>, BlobError>> + Send {
        async { Ok(Resumed { writer: self.begin().await?, done: 0, hasher: Sha256::new(), partial: false }) }
    }

    /// Lecteur de la partie reçue de `cid` (rapatriement en cours, voir [`Resumed::partial`]) : permet de lire un film
    /// pendant qu'il arrive. `None` : pas de partie lisible.
    fn open_partial(&self, _cid: &str) -> impl Future<Output = Result<Option<Self::Reader>, BlobError>> + Send {
        async { Ok(None) }
    }

    /// Jette la partie reçue de `cid` (contenu faux : l'empreinte ne correspond pas) ; le prochain rapatriement repart de zéro.
    fn discard_partial(&self, _cid: &str) -> impl Future<Output = ()> + Send {
        async {}
    }
}

/// Écrivain repris par [`BlobStore::begin_resumable`].
pub struct Resumed<W> {
    /// Écrivain, placé après la partie déjà reçue.
    pub writer: W,
    /// Octets déjà reçus (0 : départ de zéro).
    pub done: u64,
    /// Empreinte SHA-256 de ces octets, à poursuivre.
    pub hasher: Sha256,
    /// L'écrivain écrit dans la partie reprenable du blob, lisible par [`BlobStore::open_partial`] pendant l'écriture.
    pub partial: bool,
}

/// Une partie reçue que plus rien ne reprend depuis 7 jours est effacée à l'ouverture du magasin.
pub const PARTIAL_TTL: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 3600);

/// Réserve sous laquelle un dépôt est refusé : le plus grand de 500 Mo et 5 % de la partition. Un disque plein
/// corrompt le WAL SQLite et fige la station, pire qu'un téléversement refusé.
pub fn disk_reserve(total: u64) -> u64 {
    (500 << 20).max(total / 20)
}

/// Le dépôt de `incoming` octets laisserait-il au moins [`disk_reserve`] libres ?
pub fn disk_has_room<B: BlobStore>(blobs: &B, incoming: u64) -> bool {
    blobs.disk_space().is_none_or(|(free, total)| free.saturating_sub(incoming) >= disk_reserve(total))
}

const B32: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

/// CID v1, codec `raw` (0x55), multihash sha2-256 (0x12 0x20), en base32 minuscule préfixée de `b`.
pub fn cid_raw_sha256(digest: &[u8; 32]) -> String {
    let mut bytes = vec![0x01, 0x55, 0x12, 0x20];
    bytes.extend_from_slice(digest);
    let mut out = String::from("b");
    let (mut acc, mut bits) = (0u32, 0u32);
    for b in bytes {
        acc = (acc << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(char::from(B32[((acc >> bits) & 31) as usize]));
        }
    }
    if bits > 0 {
        out.push(char::from(B32[((acc << (5 - bits)) & 31) as usize]));
    }
    out
}

/// `true` si `s` a la forme d'un CID v1 sha2-256 en base32 : `bafkrei…` (bloc `raw`, [`cid_raw_sha256`])
/// ou `bafybei…` (racine `dag-pb` d'un fichier UnixFS découpé par Kubo).
pub fn is_cid(s: &str) -> bool {
    s.len() == 59 && (s.starts_with("bafkrei") || s.starts_with("bafybei")) && s.bytes().all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b))
}

/// Magasin sur disque.
#[derive(Debug, Clone)]
pub struct FsBlobStore {
    root: PathBuf,
    /// Parties en cours de reprise : deux rapatriements du même blob n'écrivent jamais dans le même fichier.
    resuming: Arc<Mutex<HashSet<String>>>,
}

impl FsBlobStore {
    /// Crée (si besoin) le dossier de stockage, et efface les fichiers temporaires abandonnés depuis [`PARTIAL_TTL`].
    pub fn open(root: impl Into<PathBuf>) -> io::Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(root.join(".tmp"))?;
        for entry in std::fs::read_dir(root.join(".tmp"))?.flatten() {
            let old = entry.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age > PARTIAL_TTL);
            if old {
                let _ = std::fs::remove_file(entry.path());
            }
        }
        Ok(Self { root, resuming: Arc::default() })
    }

    fn partial_path(&self, cid: &str) -> PathBuf {
        self.root.join(".tmp").join(format!("part-{cid}"))
    }

    /// Dossier de stockage.
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn path_of(&self, cid: &str) -> Result<PathBuf, BlobError> {
        if is_cid(cid) { Ok(self.root.join(cid)) } else { Err(BlobError::BadCid) }
    }

    /// Chemin du blob `cid` sur le disque, qu'il existe ou non (le miroir Kubo le donne au *filestore* de Kubo).
    pub fn blob_path(&self, cid: &str) -> Result<PathBuf, BlobError> {
        self.path_of(cid)
    }

    /// Écrivain qui range le blob sous le nom `cid` donné (rapatriement d'un blob connu par son CID). Un CID `raw`
    /// (`bafkrei…`) est vérifié à la validation : un contenu qui n'a pas cette empreinte est refusé. Une racine `dag-pb`
    /// (`bafybei…`) ne se recalcule pas ici : l'appelant la vérifie avant de valider (voir [`FsBlobWriter::tmp_path`]).
    pub async fn begin_named(&self, cid: &str) -> Result<FsBlobWriter, BlobError> {
        if !is_cid(cid) {
            return Err(BlobError::BadCid);
        }
        let mut w = self.begin().await?;
        w.name = Some(cid.to_owned());
        Ok(w)
    }
}

/// Écrivain du magasin disque : fichier temporaire haché au fil de l'eau.
pub struct FsBlobWriter {
    file: tokio::fs::File,
    hasher: Sha256,
    size: u64,
    tmp: PathBuf,
    root: PathBuf,
    committed: bool,
    /// Partie reprenable : `(cid, registre)` ; non validée, elle reste sur le disque pour la fois suivante.
    partial: Option<(String, Arc<Mutex<HashSet<String>>>)>,
    /// Nom imposé ([`FsBlobStore::begin_named`]) ; sinon le CID `raw` du contenu.
    name: Option<String>,
}

impl FsBlobWriter {
    /// Fichier temporaire en cours d'écriture : de quoi vérifier le contenu avant [`BlobWriter::commit`].
    pub fn tmp_path(&self) -> &Path {
        &self.tmp
    }

    fn release(&mut self) {
        if let Some((cid, set)) = self.partial.take() {
            set.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&cid);
        }
    }
}

impl AsyncWrite for FsBlobWriter {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        let poll = Pin::new(&mut this.file).poll_write(cx, buf);
        if let Poll::Ready(Ok(n)) = &poll {
            this.hasher.update(&buf[..*n]);
            this.size += *n as u64;
        }
        poll
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().file).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().file).poll_shutdown(cx)
    }
}

impl BlobWriter for FsBlobWriter {
    async fn commit(mut self) -> Result<BlobMeta, BlobError> {
        self.file.flush().await?;
        self.file.sync_all().await?;
        let digest: [u8; 32] = self.hasher.clone().finalize().into();
        let raw = cid_raw_sha256(&digest);
        let cid = match self.name.take() {
            Some(n) if n.starts_with("bafkrei") && n != raw => {
                return Err(BlobError::Io(io::Error::new(io::ErrorKind::InvalidData, format!("contenu refusé : son empreinte est {raw}, pas {n}"))));
            }
            Some(n) => n,
            None => raw,
        };
        let dest = self.root.join(&cid);
        if tokio::fs::try_exists(&dest).await? {
            tokio::fs::remove_file(&self.tmp).await?; // même contenu déjà présent
        } else {
            tokio::fs::rename(&self.tmp, &dest).await?;
        }
        self.committed = true;
        self.release();
        Ok(BlobMeta { cid, size: self.size })
    }
}

impl Drop for FsBlobWriter {
    fn drop(&mut self) {
        // Une partie reprenable reçue reste pour la fois suivante ; vide, ou écrivain ordinaire, le fichier disparaît.
        if !self.committed && (self.partial.is_none() || self.size == 0) {
            let _ = std::fs::remove_file(&self.tmp);
        }
        self.release();
    }
}

impl BlobStore for FsBlobStore {
    type Writer = FsBlobWriter;
    type Reader = tokio::fs::File;

    fn disk_space(&self) -> Option<(u64, u64)> {
        let st = rustix::fs::statvfs(&self.root).ok()?;
        Some((st.f_bavail.saturating_mul(st.f_frsize), st.f_blocks.saturating_mul(st.f_frsize)))
    }

    async fn begin(&self) -> Result<FsBlobWriter, BlobError> {
        let mut id = [0u8; 12];
        getrandom::fill(&mut id).map_err(|e| io::Error::other(e.to_string()))?;
        let tmp = self.root.join(".tmp").join(hex::encode(id));
        let file = tokio::fs::File::create(&tmp).await?;
        Ok(FsBlobWriter { file, hasher: Sha256::new(), size: 0, tmp, root: self.root.clone(), committed: false, partial: None, name: None })
    }

    async fn begin_resumable(&self, cid: &str, max: u64, align: u64) -> Result<Resumed<FsBlobWriter>, BlobError> {
        if !is_cid(cid) {
            return Err(BlobError::BadCid);
        }
        // Déjà en cours ailleurs (deux lecteurs du même film) : celui-ci part de zéro dans son propre fichier.
        if !self.resuming.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(cid.to_owned()) {
            return Ok(Resumed { writer: self.begin().await?, done: 0, hasher: Sha256::new(), partial: false });
        }
        let tmp = self.partial_path(cid);
        // L'écrivain existe dès maintenant : une erreur plus bas libère le registre (Drop).
        let file = match tokio::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&tmp).await {
            Ok(f) => f,
            Err(e) => {
                self.resuming.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(cid);
                return Err(e.into());
            }
        };
        let mut w = FsBlobWriter { file, hasher: Sha256::new(), size: 0, tmp, root: self.root.clone(), committed: false, partial: Some((cid.to_owned(), self.resuming.clone())), name: None };
        let len = w.file.metadata().await?.len();
        // Seuls des segments entiers sont gardés : un segment à moitié écrit (coupure pendant l'écriture) est redemandé.
        let keep = if align == 0 { 0 } else { len.min(max) / align * align };
        w.file.set_len(keep).await?;
        w.file.seek(io::SeekFrom::Start(0)).await?;
        let (mut left, mut buf) = (keep, vec![0u8; 1 << 20]);
        while left > 0 {
            let n = w.file.read(&mut buf[..usize::try_from(left.min(1 << 20)).unwrap_or(1 << 20)]).await?;
            if n == 0 {
                return Err(BlobError::Io(io::Error::new(io::ErrorKind::UnexpectedEof, "partie reçue tronquée pendant la relecture")));
            }
            w.hasher.update(&buf[..n]);
            left -= n as u64;
        }
        w.file.seek(io::SeekFrom::Start(keep)).await?;
        w.size = keep;
        let hasher = w.hasher.clone();
        Ok(Resumed { writer: w, done: keep, hasher, partial: true })
    }

    async fn open_partial(&self, cid: &str) -> Result<Option<tokio::fs::File>, BlobError> {
        if !is_cid(cid) {
            return Err(BlobError::BadCid);
        }
        match tokio::fs::File::open(self.partial_path(cid)).await {
            Ok(f) => Ok(Some(f)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    async fn discard_partial(&self, cid: &str) {
        if is_cid(cid) && !self.resuming.lock().unwrap_or_else(std::sync::PoisonError::into_inner).contains(cid) {
            let _ = tokio::fs::remove_file(self.partial_path(cid)).await;
        }
    }

    async fn open(&self, cid: &str) -> Result<(tokio::fs::File, u64), BlobError> {
        let path = self.path_of(cid)?;
        match tokio::fs::File::open(&path).await {
            Ok(f) => {
                let len = f.metadata().await?.len();
                Ok((f, len))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Err(BlobError::NotFound),
            Err(e) => Err(e.into()),
        }
    }

    async fn remove(&self, cid: &str) -> Result<(), BlobError> {
        match tokio::fs::remove_file(self.path_of(cid)?).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    /// Compatibilité CID v1 avec Kubo : pour un contenu tenant dans **un seul bloc** (≤ 262 144 octets, le découpage par défaut
    /// de `ipfs add`), le CID natif `raw` + sha2-256 est exactement celui de Kubo. Vecteurs relevés avec Kubo 0.40.0 :
    /// `ipfs add -n -Q --cid-version=1 --raw-leaves <fichier>`. Ne pas les modifier pour faire passer un test.
    #[test]
    fn the_native_cid_is_the_one_kubo_gives_to_a_single_block_file() {
        let vectors = [
            ("75965145a8b3ad78a6c32d7a77762fe435cd99122b0ea380a3854849024646c0", "bafkreidvsziulkftvv4knqznpj3xml7egxgzserlb2rybi4fjbeqersgya"), // « astroportd cid v1 compat\n »
            ("05ece9bde690bf39239aca4213a06d0a5ddb2eb6ec9ce0c2a5593bb1832f5b2a", "bafkreiaf5tu33zuqx44shgwkiij2a3iklxns5nxmttqmfjkzhoyygl23fi"), // 200 000 octets
            ("20d3effbc34432ed1794f527de40543380c513d1facea061575d93f03557c7ce", "bafkreiba2px7xq2eglwrpfhve7peavbtqdcrhup2z2qgcv25spydkv6hzy"), // 262 144 octets : le dernier bloc unique
        ];
        for (sha, kubo) in vectors {
            let digest: [u8; 32] = hex::decode(sha).unwrap().try_into().unwrap();
            assert_eq!(super::cid_raw_sha256(&digest), kubo);
            assert!(super::is_cid(kubo));
        }
    }

    /// Au-delà d'un bloc, Kubo rend la racine `dag-pb` d'un arbre de blocs (`bafybei…`), pas le CID `raw` du fichier entier :
    /// les deux ne sont **pas** égaux. Le blob natif d'astroportd garde son CID `raw` ; un blob que Kubo a lui-même découpé
    /// garde le sien (`is_cid` accepte les deux préfixes). Vecteur : 262 145 octets.
    #[test]
    fn beyond_one_block_kubo_names_the_file_by_its_dag_root_not_by_the_raw_hash() {
        let digest: [u8; 32] = hex::decode("b8bcbba8d5046866a33edf97090d8ddbb2f3dc557c39c8ac2b59e2fc256a0d71").unwrap().try_into().unwrap();
        let kubo = "bafybeic54ggur77oamccaa44omnsezmbhsdcjisl5l3hswcigrcecpd7oq";
        assert!(super::is_cid(kubo));
        assert_ne!(super::cid_raw_sha256(&digest), kubo);
        assert!(super::cid_raw_sha256(&digest).starts_with("bafkrei"));
    }

    #[test]
    fn disk_reserve_is_the_larger_of_500_mib_and_5_percent() {
        assert_eq!(super::disk_reserve(8 << 30), 500 << 20);
        assert_eq!(super::disk_reserve(100 << 30), 5 << 30);
    }

    #[test]
    fn a_real_filesystem_reports_its_space() {
        let dir = std::env::temp_dir();
        let store = super::FsBlobStore::open(dir.join(format!("astro-space-{}", std::process::id()))).unwrap();
        let (free, total) = super::BlobStore::disk_space(&store).unwrap();
        assert!(total > 0 && free <= total);
        let _ = std::fs::remove_dir_all(store.root());
    }

    use tokio::io::{AsyncReadExt, AsyncSeekExt};

    use super::*;

    fn dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("astro-blob-{tag}-{}", std::process::id()))
    }

    #[test]
    fn cid_matches_known_ipfs_values() {
        // `ipfs add --cid-version 1 --raw-leaves` d'un fichier vide et de « hello world », vérifiés avec hashlib + base32.
        assert_eq!(cid_raw_sha256(&Sha256::digest(b"").into()), "bafkreihdwdcefgh4dqkjv67uzcmw7ojee6xedzdetojuzjevtenxquvyku");
        assert_eq!(cid_raw_sha256(&Sha256::digest(b"hello world").into()), "bafkreifzjut3te2nhyekklss27nh3k72ysco7y32koao5eei66wof36n5e");
        assert!(is_cid("bafkreihdwdcefgh4dqkjv67uzcmw7ojee6xedzdetojuzjevtenxquvyku"));
        assert!(is_cid("bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi"), "racine dag-pb de Kubo");
        assert!(!is_cid("../etc/passwd") && !is_cid("Qmabc") && !is_cid("") && !is_cid(&"bafkrei".repeat(9)));
    }

    #[tokio::test]
    async fn write_commit_open_seek_remove() {
        let d = dir("rw");
        let store = FsBlobStore::open(&d).unwrap();
        let mut w = store.begin().await.unwrap();
        w.write_all(b"0123456789").await.unwrap();
        let meta = w.commit().await.unwrap();
        assert_eq!(meta.size, 10);
        assert!(is_cid(&meta.cid));
        let (mut r, len) = store.open(&meta.cid).await.unwrap();
        assert_eq!(len, 10);
        r.seek(io::SeekFrom::Start(4)).await.unwrap();
        let mut rest = String::new();
        r.read_to_string(&mut rest).await.unwrap();
        assert_eq!(rest, "456789");
        store.remove(&meta.cid).await.unwrap();
        store.remove(&meta.cid).await.unwrap();
        assert!(matches!(store.open(&meta.cid).await, Err(BlobError::NotFound)));
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[tokio::test]
    async fn uncommitted_writes_leave_nothing_and_bad_cids_are_refused() {
        let d = dir("drop");
        let store = FsBlobStore::open(&d).unwrap();
        {
            let mut w = store.begin().await.unwrap();
            w.write_all(b"abandonne").await.unwrap();
        }
        assert_eq!(std::fs::read_dir(d.join(".tmp")).unwrap().count(), 0);
        assert!(matches!(store.open("../secret").await, Err(BlobError::BadCid)));
        assert!(matches!(store.remove("x").await, Err(BlobError::BadCid)));
        // Deux écritures du même contenu donnent le même CID, un seul fichier.
        let mut ids = Vec::new();
        for _ in 0..2 {
            let mut w = store.begin().await.unwrap();
            w.write_all(b"identique").await.unwrap();
            ids.push(w.commit().await.unwrap().cid);
        }
        assert_eq!(ids[0], ids[1]);
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 2, ".tmp + 1 blob");
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[tokio::test]
    async fn a_partial_blob_is_resumed_by_one_writer_at_a_time_and_kept_only_when_something_was_received() {
        let d = std::env::temp_dir().join(format!("astro-blob-resume-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let store = FsBlobStore::open(&d).unwrap();
        let cid = cid_raw_sha256(&[3; 32]);
        assert!(matches!(store.begin_resumable("../x", 10, 4).await, Err(BlobError::BadCid)));
        let mut first = store.begin_resumable(&cid, 100, 4).await.unwrap();
        assert_eq!(first.done, 0);
        // Même blob demandé deux fois : le second écrit dans son propre fichier, jamais dans la partie du premier.
        let second = store.begin_resumable(&cid, 100, 4).await.unwrap();
        assert!(second.writer.partial.is_none() && second.writer.tmp != first.writer.tmp);
        drop(second);
        first.writer.write_all(b"abcdefghij").await.unwrap();
        first.writer.flush().await.unwrap();
        drop(first);
        // Repris : 8 octets gardés (multiple de 4), empreinte de ces 8 octets seulement.
        let again = store.begin_resumable(&cid, 100, 4).await.unwrap();
        assert_eq!(again.done, 8);
        assert_eq!(again.hasher.clone().finalize(), Sha256::digest(b"abcdefgh"));
        drop(again);
        store.discard_partial(&cid).await;
        assert_eq!(std::fs::read_dir(d.join(".tmp")).unwrap().count(), 0, "partie jetée");
        // Rien reçu : rien ne reste.
        drop(store.begin_resumable(&cid, 100, 4).await.unwrap());
        assert_eq!(std::fs::read_dir(d.join(".tmp")).unwrap().count(), 0);
        std::fs::remove_dir_all(&d).unwrap();
    }
}
