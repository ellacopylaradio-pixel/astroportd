//! Échange direct de blobs chiffrés entre stations : `/astro/blob/1.1.0`.
//!
//! Requêtes (une ligne terminée par `\n`) :
//! - `HAS <cid>` : présence et taille, sans corps ;
//! - `HASN <n>` suivi de `n` empreintes SHA-256 de 32 octets (4 096 au plus) : présence de chacun, en un seul flux — la
//!   mesure de réplication n'ouvre plus un flux par fichier et par station. Réponse : état, puis un bit par empreinte ;
//! - `GET <cid> <début> <longueur>` : une plage d'octets du blob.
//!
//! Réponse : 1 octet d'état (0 ok, 1 absent, 2 refusé, 3 requête invalide) puis, si ok, la taille totale du blob
//! (u64 BE) ; pour `GET`, la longueur réellement servie (u64 BE, bornée par la fin du blob) suivie de ses octets.
//!
//! Le demandeur sonde les pairs en parallèle (`HAS`), découpe le blob en segments de 4 Mio qu'il demande à plusieurs
//! pairs à la fois (fenêtre bornée : la mémoire ne dépend pas de la taille du blob), les réécrit dans l'ordre et redemande
//! à un autre pair, **au même endroit**, un segment interrompu ou trop lent. Il **vérifie l'empreinte SHA-256 avant de
//! valider** le blob : un pair malveillant ne peut rien faire entrer dans le magasin sous un CID qui n'est pas le sien.
//! Seuls les CID `raw` (`bafkrei…`) sont échangés, car ce sont les seuls vérifiables sans DAG.

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::io::{self, SeekFrom};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use astro_dav::blob::{BlobError, BlobMeta, BlobStore, BlobWriter, cid_raw_sha256, disk_has_room, is_cid};
use futures_util::StreamExt;
use futures_util::io::{AsyncReadExt as _, AsyncWriteExt as _};
use futures_util::stream::FuturesUnordered;
use libp2p::{PeerId, StreamProtocol};
use sha2::Digest;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeek, AsyncSeekExt, AsyncWriteExt, ReadBuf};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio_util::sync::ReusableBoxFuture;

use crate::{P2pError, P2pHandle};

/// Protocole d'échange de blobs.
pub const PROTOCOL: &str = "/astro/blob/1.1.0";
/// Plus gros blob échangé par défaut (16 Gio) : au-delà du plus gros fichier qu'un compte peut déposer.
pub const MAX_BLOB_BYTES: u64 = 16 << 30;
const OK: u8 = 0;
const NOT_FOUND: u8 = 1;
const REFUSED: u8 = 2;
const BAD_REQUEST: u8 = 3;
const CHUNK: usize = 64 * 1024;
const MAX_LINE: usize = 128;
/// Taille d'un segment demandé à un pair.
const SEGMENT: u64 = 4 << 20;
/// Segments en cours (en vol ou reçus en attente d'écriture) : la mémoire du rapatriement reste sous `WINDOW × SEGMENT`.
const WINDOW: u64 = 4;
/// Segments demandés en même temps à un même pair.
const PER_PEER: usize = 2;
/// Échecs au-delà desquels un pair n'est plus sollicité pour ce blob.
const PEER_STRIKES: u32 = 3;
/// Débit plancher (octets/s) : un segment plus lent est redemandé ailleurs, un pair qui égrène les octets ne retient rien.
const MIN_RATE: u64 = 32 << 10;

/// Erreurs de rapatriement.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    /// Aucun pair n'a pu fournir le blob.
    #[error("blob introuvable chez {tried} pair(s) : {reasons}")]
    Unavailable {
        /// Pairs interrogés.
        tried: usize,
        /// Raisons par pair.
        reasons: String,
    },
    /// CID non échangeable.
    #[error("CID non supporté (seuls les blocs `raw` bafkrei… sont échangés)")]
    BadCid,
    /// Le disque local n'a pas la place de recevoir le blob (réserve comprise).
    #[error("pas assez de place sur le disque pour recevoir {size} octets")]
    NoRoom {
        /// Taille annoncée du blob.
        size: u64,
    },
    /// Stockage local.
    #[error("stockage : {0}")]
    Store(#[from] BlobError),
}

/// Écluse du serveur : rend `true` si ce CID peut sortir. Un plan qui partage son dossier de blobs avec d'autres plans
/// (bassin commun) ne sert que les blobs que **sa** base référence : le chiffré d'une UPlanet privée ne sort jamais par
/// le nœud d'un plan voisin, même à qui connaîtrait son CID (étanchéité I1).
#[derive(Clone)]
pub struct CidGate(pub Arc<dyn Fn(String) -> futures_util::future::BoxFuture<'static, bool> + Send + Sync>);

impl std::fmt::Debug for CidGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CidGate")
    }
}

/// Réglages du serveur de blobs.
#[derive(Debug, Clone)]
pub struct ServeOpts {
    /// Pairs autorisés ; `None` = tout membre du réseau privé.
    pub allow: Option<HashSet<PeerId>>,
    /// Taille maximale servie.
    pub max_blob_bytes: u64,
    /// CID autorisés à sortir ; `None` = tout ce que contient le magasin (dossier propre au plan).
    pub gate: Option<CidGate>,
}

impl Default for ServeOpts {
    fn default() -> Self {
        Self { allow: None, max_blob_bytes: MAX_BLOB_BYTES, gate: None }
    }
}

/// Serveur de blobs : s'arrête à la destruction.
pub struct BlobServer {
    task: JoinHandle<()>,
}

impl Drop for BlobServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Sert les blobs de `store` aux pairs du maillage.
pub fn serve_blobs<B: BlobStore>(handle: &P2pHandle, store: Arc<B>, opts: ServeOpts) -> Result<BlobServer, P2pError> {
    let mut incoming = handle.control().accept(StreamProtocol::new(PROTOCOL)).map_err(|e| P2pError::Stream(e.to_string()))?;
    let task = tokio::spawn(async move {
        while let Some((peer, stream)) = incoming.next().await {
            if opts.allow.as_ref().is_some_and(|a| !a.contains(&peer)) {
                continue;
            }
            let (store, max, gate) = (store.clone(), opts.max_blob_bytes, opts.gate.clone());
            tokio::spawn(async move {
                let _ = serve_one(stream, store.as_ref(), max, gate).await;
            });
        }
    });
    Ok(BlobServer { task })
}

/// Empreintes au plus dans une sonde groupée `HASN`.
pub const MAX_HAS_MANY: usize = 4096;

/// Requête d'un pair : une plage du blob (`GET`), sa présence (`HAS`, sans corps) ou celle de plusieurs (`HASN`).
#[derive(Debug, PartialEq, Eq)]
enum Request {
    Get { cid: String, offset: u64, len: u64 },
    Has(String),
    HasMany(usize),
}

fn parse_request(text: &str) -> Option<Request> {
    let mut words = text.split(' ');
    let request = match (words.next()?, words.next()?) {
        ("HAS", cid) => Request::Has(cid.to_owned()),
        ("HASN", n) => Request::HasMany(n.parse::<usize>().ok().filter(|n| (1..=MAX_HAS_MANY).contains(n))?),
        ("GET", cid) => Request::Get { cid: cid.to_owned(), offset: words.next()?.parse().ok()?, len: words.next()?.parse().ok()? },
        _ => return None,
    };
    words.next().is_none().then_some(request)
}

async fn read_request(stream: &mut libp2p::Stream) -> std::io::Result<Option<Request>> {
    let mut line = Vec::with_capacity(MAX_LINE);
    let mut byte = [0u8; 1];
    while line.len() < MAX_LINE {
        stream.read_exact(&mut byte).await?;
        if byte[0] == b'\n' {
            return Ok(String::from_utf8(line).ok().as_deref().and_then(parse_request));
        }
        line.push(byte[0]);
    }
    Ok(None)
}

async fn answer(stream: &mut libp2p::Stream, status: u8) -> std::io::Result<()> {
    stream.write_all(&[status]).await?;
    stream.close().await
}

async fn serve_one<B: BlobStore>(mut stream: libp2p::Stream, store: &B, max: u64, gate: Option<CidGate>) -> std::io::Result<()> {
    let (cid, range) = match read_request(&mut stream).await? {
        Some(Request::Get { cid, offset, len }) => (cid, Some((offset, len))),
        Some(Request::Has(cid)) => (cid, None),
        Some(Request::HasMany(n)) => return serve_many(stream, store, max, gate, n).await,
        None => return answer(&mut stream, BAD_REQUEST).await,
    };
    if !exchangeable(&cid) {
        return answer(&mut stream, BAD_REQUEST).await;
    }
    // Hors écluse : « absent », comme un blob inconnu (rien ne révèle qu'un plan voisin le détient).
    if let Some(CidGate(allowed)) = &gate {
        if !allowed(cid.clone()).await {
            return answer(&mut stream, NOT_FOUND).await;
        }
    }
    let Ok((mut reader, total)) = store.open(&cid).await else {
        return answer(&mut stream, NOT_FOUND).await;
    };
    if total > max {
        return answer(&mut stream, REFUSED).await;
    }
    let Some((offset, len)) = range else {
        stream.write_all(&[OK]).await?;
        stream.write_all(&total.to_be_bytes()).await?;
        return stream.close().await;
    };
    if offset > total {
        return answer(&mut stream, BAD_REQUEST).await;
    }
    let served = len.min(total - offset);
    reader.seek(SeekFrom::Start(offset)).await?;
    stream.write_all(&[OK]).await?;
    stream.write_all(&total.to_be_bytes()).await?;
    stream.write_all(&served.to_be_bytes()).await?;
    let (mut remaining, mut buf) = (served, vec![0u8; CHUNK]);
    while remaining > 0 {
        let want = usize::try_from(remaining.min(CHUNK as u64)).unwrap_or(CHUNK);
        let n = reader.read(&mut buf[..want]).await?;
        if n == 0 {
            break; // blob tronqué sur le disque : le demandeur verra une plage incomplète
        }
        stream.write_all(&buf[..n]).await?;
        remaining -= n as u64;
    }
    stream.close().await
}

/// `HASN` : `n` empreintes lues, un bit par blob présent ici **et** autorisé à sortir (même écluse que `HAS` : un blob hors
/// écluse est « absent »).
async fn serve_many<B: BlobStore>(mut stream: libp2p::Stream, store: &B, max: u64, gate: Option<CidGate>, n: usize) -> std::io::Result<()> {
    let mut digests = vec![0u8; n * 32];
    stream.read_exact(&mut digests).await?;
    let mut bits = vec![0u8; n.div_ceil(8)];
    for (i, d) in digests.chunks_exact(32).enumerate() {
        let mut digest = [0u8; 32];
        digest.copy_from_slice(d);
        let cid = cid_raw_sha256(&digest);
        if let Some(CidGate(allowed)) = &gate {
            if !allowed(cid.clone()).await {
                continue;
            }
        }
        if store.open(&cid).await.is_ok_and(|(_, len)| len <= max) {
            bits[i / 8] |= 1 << (i % 8);
        }
    }
    stream.write_all(&[OK]).await?;
    stream.write_all(&bits).await?;
    stream.close().await
}

/// Empreinte SHA-256 d'un CID `raw` (`bafkrei…`) : l'inverse de [`cid_raw_sha256`].
pub fn digest_of(cid: &str) -> Option<[u8; 32]> {
    if !exchangeable(cid) {
        return None;
    }
    let (mut acc, mut bits, mut out) = (0u32, 0u32, Vec::with_capacity(36));
    for c in cid[1..].bytes() {
        let v = match c {
            b'a'..=b'z' => c - b'a',
            b'2'..=b'7' => c - b'2' + 26,
            _ => return None,
        };
        acc = (acc << 5) | u32::from(v);
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push(((acc >> bits) & 0xff) as u8);
        }
    }
    (out.len() >= 36 && out[..4] == [0x01, 0x55, 0x12, 0x20]).then(|| {
        let mut d = [0u8; 32];
        d.copy_from_slice(&out[4..36]);
        d
    })
}

/// Lesquels de `cids` ce pair a-t-il ? Une seule sonde (`HASN`) pour tous, au lieu d'un `HAS` par fichier. Les CID non
/// échangeables par le maillage (`bafybei…`) sont rendus absents ; au plus [`MAX_HAS_MANY`] par appel.
pub async fn has_many(handle: &P2pHandle, peer: PeerId, cids: &[String], timeout: Duration) -> Result<Vec<bool>, String> {
    if cids.len() > MAX_HAS_MANY {
        return Err(format!("{MAX_HAS_MANY} fichiers au plus par sonde"));
    }
    let digests: Vec<Option<[u8; 32]>> = cids.iter().map(|c| digest_of(c)).collect();
    let asked: Vec<usize> = (0..cids.len()).filter(|i| digests[*i].is_some()).collect();
    let mut out = vec![false; cids.len()];
    if asked.is_empty() {
        return Ok(out);
    }
    let work = async {
        let mut control = handle.control();
        let mut stream = control.open_stream(peer, StreamProtocol::new(PROTOCOL)).await.map_err(|e| format!("ouverture du flux : {e}"))?;
        let mut body = format!("HASN {}\n", asked.len()).into_bytes();
        for i in &asked {
            body.extend(digests[*i].unwrap_or_default());
        }
        stream.write_all(&body).await.map_err(|e| e.to_string())?;
        let mut status = [0u8; 1];
        stream.read_exact(&mut status).await.map_err(|e| e.to_string())?;
        if status[0] != OK {
            return Err("sonde groupée refusée par le pair (version trop ancienne ?)".to_owned());
        }
        let mut bits = vec![0u8; asked.len().div_ceil(8)];
        stream.read_exact(&mut bits).await.map_err(|e| e.to_string())?;
        Ok::<_, String>(bits)
    };
    let bits = tokio::time::timeout(timeout, work).await.unwrap_or_else(|_| Err("délai dépassé".to_owned()))?;
    for (k, i) in asked.iter().enumerate() {
        out[*i] = bits[k / 8] & (1 << (k % 8)) != 0;
    }
    Ok(out)
}

fn exchangeable(cid: &str) -> bool {
    cid.starts_with("bafkrei") && is_cid(cid)
}

/// Ce pair a-t-il ce blob ? Rend sa taille, ou `None` s'il ne l'a pas. Rien n'est transféré : c'est la sonde de la mesure
/// de réplication et du choix des pairs d'un rapatriement.
pub async fn has_blob(handle: &P2pHandle, peer: PeerId, cid: &str, timeout: Duration) -> Result<Option<u64>, String> {
    if !exchangeable(cid) {
        return Err("CID non échangeable (seuls les blocs `raw` bafkrei… le sont)".into());
    }
    let work = async {
        let mut control = handle.control();
        let mut stream = control.open_stream(peer, StreamProtocol::new(PROTOCOL)).await.map_err(|e| format!("ouverture du flux : {e}"))?;
        stream.write_all(format!("HAS {cid}\n").as_bytes()).await.map_err(|e| e.to_string())?;
        let mut status = [0u8; 1];
        stream.read_exact(&mut status).await.map_err(|e| e.to_string())?;
        match status[0] {
            OK => {
                let mut len = [0u8; 8];
                stream.read_exact(&mut len).await.map_err(|e| e.to_string())?;
                Ok(Some(u64::from_be_bytes(len)))
            }
            NOT_FOUND => Ok(None),
            REFUSED => Err("le pair refuse de le servir (trop gros ou non autorisé)".to_owned()),
            _ => Err("requête non comprise par le pair (version trop ancienne ?)".to_owned()),
        }
    };
    tokio::time::timeout(timeout, work).await.unwrap_or_else(|_| Err("délai dépassé".to_owned()))
}

/// Demande `cid` à `peer` et l'enregistre dans `store` après vérification de l'empreinte.
pub async fn fetch_from<B: BlobStore>(handle: &P2pHandle, peer: PeerId, cid: &str, store: &B, max_bytes: u64, idle: Duration) -> Result<BlobMeta, FetchError> {
    fetch_blob(handle, &[peer], cid, store, max_bytes, idle).await
}

/// Échec d'un segment : sa raison, et s'il disqualifie le pair pour tout le blob (absent, refus, taille différente).
struct SegmentError {
    why: String,
    fatal: bool,
}

impl SegmentError {
    fn transient(why: impl Into<String>) -> Self {
        Self { why: why.into(), fatal: false }
    }
    fn fatal(why: impl Into<String>) -> Self {
        Self { why: why.into(), fatal: true }
    }
}

/// Une plage `[offset, offset + len)` du blob chez `peer`. `idle` : silence toléré entre deux lectures ; le segment entier
/// doit en outre arriver au débit plancher [`MIN_RATE`].
async fn fetch_segment(handle: &P2pHandle, peer: PeerId, cid: &str, offset: u64, len: u64, total: u64, idle: Duration) -> Result<Vec<u8>, SegmentError> {
    let quiet = |e: tokio::time::error::Elapsed| SegmentError::transient(format!("aucun octet depuis {} s ({e})", idle.as_secs()));
    let work = async {
        let mut control = handle.control();
        let mut stream = tokio::time::timeout(idle, control.open_stream(peer, StreamProtocol::new(PROTOCOL)))
            .await
            .map_err(quiet)?
            .map_err(|e| SegmentError::transient(format!("ouverture du flux : {e}")))?;
        stream.write_all(format!("GET {cid} {offset} {len}\n").as_bytes()).await.map_err(|e| SegmentError::transient(e.to_string()))?;
        let mut head = [0u8; 1];
        tokio::time::timeout(idle, stream.read_exact(&mut head)).await.map_err(quiet)?.map_err(|e| SegmentError::transient(e.to_string()))?;
        match head[0] {
            OK => {}
            NOT_FOUND => return Err(SegmentError::fatal("le pair n'a pas ce blob")),
            REFUSED => return Err(SegmentError::fatal("le pair refuse de le servir (trop gros ou non autorisé)")),
            _ => return Err(SegmentError::fatal("requête refusée par le pair (version trop ancienne ?)")),
        }
        let mut sizes = [0u8; 16];
        tokio::time::timeout(idle, stream.read_exact(&mut sizes)).await.map_err(quiet)?.map_err(|e| SegmentError::transient(e.to_string()))?;
        let (mut announced, mut served) = ([0u8; 8], [0u8; 8]);
        announced.copy_from_slice(&sizes[..8]);
        served.copy_from_slice(&sizes[8..]);
        if u64::from_be_bytes(announced) != total {
            return Err(SegmentError::fatal("le pair annonce une autre taille pour ce blob"));
        }
        if u64::from_be_bytes(served) != len {
            return Err(SegmentError::fatal("plage incomplète"));
        }
        let size = usize::try_from(len).map_err(|_| SegmentError::fatal("segment trop grand"))?;
        let (mut buf, mut pos) = (vec![0u8; size], 0usize);
        while pos < size {
            let end = (pos + CHUNK).min(size);
            let n = tokio::time::timeout(idle, stream.read(&mut buf[pos..end])).await.map_err(quiet)?.map_err(|e| SegmentError::transient(e.to_string()))?;
            if n == 0 {
                return Err(SegmentError::transient("flux interrompu avant la fin du segment"));
            }
            pos += n;
        }
        Ok(buf)
    };
    let deadline = idle + Duration::from_secs(len / MIN_RATE + 1);
    tokio::time::timeout(deadline, work).await.unwrap_or_else(|_| Err(SegmentError::transient("débit trop faible")))
}

/// Pair qui détient le blob, avec sa charge et ses échecs.
struct Holder {
    peer: PeerId,
    busy: usize,
    strikes: u32,
}

/// Pair le moins chargé encore utilisable ; à égalité, le premier dans l'ordre de préférence.
fn pick(holders: &[Holder]) -> Option<usize> {
    holders.iter().enumerate().filter(|(_, h)| h.strikes < PEER_STRIKES && h.busy < PER_PEER).min_by_key(|(i, h)| (h.busy, h.strikes, *i)).map(|(i, _)| i)
}

/// Sonde les `candidates` en parallèle. Rend ceux qui ont le blob et la taille retenue : celle qu'annonce le plus grand
/// nombre de pairs (à égalité, le premier dans l'ordre) ; un pair qui annonce une autre taille est écarté.
async fn locate(handle: &P2pHandle, candidates: &[PeerId], cid: &str, idle: Duration, reasons: &mut Vec<String>) -> Option<(Vec<Holder>, u64)> {
    let probes = futures_util::future::join_all(candidates.iter().map(|p| has_blob(handle, *p, cid, idle))).await;
    let mut found: Vec<(PeerId, u64)> = Vec::new();
    for (peer, probe) in candidates.iter().zip(probes) {
        match probe {
            Ok(Some(size)) => found.push((*peer, size)),
            Ok(None) => reasons.push(format!("{peer} : le pair n'a pas ce blob")),
            Err(why) => reasons.push(format!("{peer} : {why}")),
        }
    }
    let size = found.iter().map(|(_, s)| *s).max_by_key(|s| (found.iter().filter(|(_, t)| t == s).count(), std::cmp::Reverse(found.iter().position(|(_, t)| t == s))))?;
    let mut holders = Vec::new();
    for (peer, s) in found {
        if s == size {
            holders.push(Holder { peer, busy: 0, strikes: 0 });
        } else {
            reasons.push(format!("{peer} : annonce {s} octets, les autres pairs {size}"));
        }
    }
    Some((holders, size))
}

/// Demande `cid` aux `peers` (dans l'ordre de préférence), ou à tous les pairs connectés si `peers` est vide, et
/// l'enregistre dans `store` après vérification de l'empreinte. Les segments sont répartis entre les pairs qui ont le
/// blob ; un segment interrompu est redemandé ailleurs, au même endroit. `idle` : silence toléré d'un pair.
pub async fn fetch_blob<B: BlobStore>(handle: &P2pHandle, peers: &[PeerId], cid: &str, store: &B, max_bytes: u64, idle: Duration) -> Result<BlobMeta, FetchError> {
    fetch_with_progress(handle, peers, cid, store, max_bytes, idle, None).await
}

/// Segments reçus en avance gardés en mémoire pour les lecteurs, au plus (8 × 4 Mio = 32 Mio par blob).
const AHEAD_CACHE: usize = 8;

/// Lecture pendant l'arrivée, **là où l'on lit** : les lecteurs disent quels segments ils attendent (un saut loin dans le
/// film) ; le rapatriement les demande avant ceux qui suivent l'ordre du fichier, et garde en mémoire ceux qui arrivent
/// avant leur tour. Le fichier sur le disque reste écrit dans l'ordre : empreinte et reprise n'en dépendent pas.
#[derive(Default)]
pub struct Ahead {
    state: std::sync::Mutex<AheadState>,
    wake: tokio::sync::Notify,
}

#[derive(Default)]
struct AheadState {
    wanted: std::collections::BTreeSet<u64>,
    cache: BTreeMap<u64, Arc<Vec<u8>>>,
    order: VecDeque<u64>,
}

impl Ahead {
    fn lock(&self) -> std::sync::MutexGuard<'_, AheadState> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Un lecteur attend ces segments : le rapatriement est réveillé s'il y a du nouveau.
    fn want(&self, segs: &[u64]) {
        let mut st = self.lock();
        let mut new = false;
        for s in segs {
            if !st.cache.contains_key(s) {
                new |= st.wanted.insert(*s);
            }
        }
        drop(st);
        if new {
            self.wake.notify_one();
        }
    }

    fn cached(&self, seg: u64) -> Option<Arc<Vec<u8>>> {
        self.lock().cache.get(&seg).cloned()
    }

    fn wanted(&self, seg: u64) -> bool {
        self.lock().wanted.contains(&seg)
    }

    /// Garde un segment reçu en avance (le plus ancien gardé part au-delà de [`AHEAD_CACHE`]).
    fn keep(&self, seg: u64, bytes: Arc<Vec<u8>>) {
        let mut st = self.lock();
        st.wanted.remove(&seg);
        if st.cache.insert(seg, bytes).is_none() {
            st.order.push_back(seg);
        }
        while st.order.len() > AHEAD_CACHE {
            if let Some(old) = st.order.pop_front() {
                st.cache.remove(&old);
            }
        }
    }
}

/// Avancement d'un rapatriement, pour lire un blob pendant qu'il arrive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progress {
    /// Recherche des pairs qui l'ont.
    Locating,
    /// Les `ready` premiers octets (sur `size`) sont sur le disque, dans la partie lisible du magasin.
    Streaming {
        /// Taille du blob.
        size: u64,
        /// Octets lisibles depuis le début.
        ready: u64,
    },
    /// Blob complet, vérifié et validé.
    Done,
    /// Échec (raison).
    Failed(String),
}

/// [`fetch_blob`] qui dit son avancement sur `progress` : [`Progress::Streaming`] seulement si l'écrivain écrit dans la partie
/// lisible du magasin ([`astro_dav::blob::Resumed::partial`]).
async fn fetch_with_progress<B: BlobStore>(handle: &P2pHandle, peers: &[PeerId], cid: &str, store: &B, max_bytes: u64, idle: Duration, progress: Option<(&watch::Sender<Progress>, &Ahead)>) -> Result<BlobMeta, FetchError> {
    if !exchangeable(cid) {
        return Err(FetchError::BadCid);
    }
    let candidates: Vec<PeerId> = if peers.is_empty() { handle.connected_peers() } else { peers.to_vec() };
    let tried = candidates.len();
    let unavailable = |reasons: &[String]| FetchError::Unavailable { tried, reasons: reasons.join(" ; ") };
    let mut reasons = Vec::new();
    let Some((mut holders, size)) = locate(handle, &candidates, cid, idle, &mut reasons).await else {
        return Err(unavailable(&reasons));
    };
    if size > max_bytes {
        return Err(unavailable(&[format!("annonce {size} octets, au-delà de la limite de {max_bytes}")]));
    }
    // Ce qui a déjà été reçu avant une coupure ou un redémarrage est repris : seuls les segments manquants sont demandés.
    let resumed = store.begin_resumable(cid, size, SEGMENT).await?;
    if !disk_has_room(store, size - resumed.done) {
        return Err(FetchError::NoRoom { size });
    }
    let mut writer = resumed.writer;
    // Lecture pendant l'arrivée : seulement si les octets reçus sont lisibles par `open_partial`.
    let quiet = Ahead::default();
    let (progress, ahead) = match progress.filter(|_| resumed.partial) {
        Some((p, a)) => (Some(p), a),
        None => (None, &quiet),
    };
    if let Some(p) = progress {
        p.send_replace(Progress::Streaming { size, ready: resumed.done });
    }
    let segments = size.div_ceil(SEGMENT);
    let first = resumed.done / SEGMENT;
    let (mut hasher, mut written, mut launched) = (resumed.hasher, first, first);
    let (mut retry, mut ready) = (VecDeque::<u64>::new(), BTreeMap::<u64, Vec<u8>>::new());
    let mut flying = HashSet::<u64>::new();
    let mut flight = FuturesUnordered::new();
    while written < segments {
        // Ce que les lecteurs attendent : reçu hors d'ordre → gardé pour eux ; au-delà de la fenêtre → demandé en premier.
        let (mut priority, mut handed) = (VecDeque::new(), false);
        {
            let mut st = ahead.lock();
            let wanted: Vec<u64> = st.wanted.iter().copied().collect();
            for w in wanted {
                if w < written || w >= segments || st.cache.contains_key(&w) {
                    st.wanted.remove(&w);
                } else if let Some(bytes) = ready.get(&w) {
                    drop(st);
                    ahead.keep(w, Arc::new(bytes.clone()));
                    handed = true;
                    st = ahead.lock();
                } else if w >= launched && !flying.contains(&w) && !retry.contains(&w) {
                    priority.push_back(w);
                }
            }
        }
        if let (true, Some(p)) = (handed, progress) {
            p.send_modify(|_| {});
        }
        // Lancer tout ce que la fenêtre et la charge des pairs permettent : reprises, puis demandes des lecteurs, puis l'ordre du fichier.
        loop {
            let (seg, sequential) = match (retry.front(), priority.front()) {
                (Some(&seg), _) => (seg, false),
                (None, Some(&seg)) => (seg, false),
                (None, None) if launched < segments && launched < written + WINDOW => (launched, true),
                (None, None) => break,
            };
            if sequential {
                // Déjà reçu en avance, ou déjà demandé par un lecteur : pas de second envoi.
                if let Some(bytes) = ahead.cached(seg) {
                    launched += 1;
                    ready.insert(seg, bytes.as_ref().clone());
                    continue;
                }
                if flying.contains(&seg) {
                    launched += 1;
                    continue;
                }
            }
            let Some(h) = pick(&holders) else { break };
            if retry.front() == Some(&seg) {
                retry.pop_front();
            } else if priority.front() == Some(&seg) {
                priority.pop_front();
            } else {
                launched += 1;
            }
            flying.insert(seg);
            holders[h].busy += 1;
            let (peer, offset) = (holders[h].peer, seg * SEGMENT);
            let len = SEGMENT.min(size - offset);
            flight.push(async move { (seg, h, fetch_segment(handle, peer, cid, offset, len, size, idle).await) });
        }
        // Des segments repris de la mémoire peuvent prolonger l'écriture sans rien attendre du réseau.
        if !ready.contains_key(&written) {
            // Un segment arrive, ou un lecteur demande autre chose. Rien en vol et rien de lançable : plus aucun pair utilisable.
            let next = tokio::select! {
                next = flight.next() => next,
                () = ahead.wake.notified(), if !flight.is_empty() => continue,
            };
            let Some((seg, h, result)) = next else {
                // Ce qui a été reçu est mis sur le disque avant de rendre la main : la prochaine fois reprend à cet endroit.
                let _ = writer.flush().await;
                return Err(unavailable(&reasons));
            };
            flying.remove(&seg);
            holders[h].busy -= 1;
            match result {
                Ok(bytes) => {
                    // En avance sur l'écriture et attendu par un lecteur (ou hors de la fenêtre) : gardé en mémoire pour lui.
                    let early = seg >= launched || (seg > written && ahead.wanted(seg));
                    if early {
                        ahead.keep(seg, Arc::new(bytes.clone()));
                        if let Some(p) = progress {
                            p.send_modify(|_| {});
                        }
                    }
                    if seg < launched {
                        ready.insert(seg, bytes);
                    }
                }
                Err(e) => {
                    holders[h].strikes = if e.fatal { PEER_STRIKES } else { holders[h].strikes + 1 };
                    reasons.push(format!("{} : {}", holders[h].peer, e.why));
                    retry.push_back(seg);
                }
            }
        }
        let before = written;
        while let Some(bytes) = ready.remove(&written) {
            hasher.update(&bytes);
            writer.write_all(&bytes).await.map_err(BlobError::Io)?;
            written += 1;
        }
        if let Some(p) = progress.filter(|_| written > before) {
            // Sur le disque avant d'être annoncés : un lecteur de la partie voit ce qu'on lui dit prêt.
            writer.flush().await.map_err(BlobError::Io)?;
            p.send_replace(Progress::Streaming { size, ready: (written * SEGMENT).min(size) });
        }
    }
    // Vérification AVANT validation : un blob falsifié est jeté avec son fichier temporaire (partie reprise comprise).
    if cid_raw_sha256(&hasher.finalize().into()) != cid {
        drop(writer);
        store.discard_partial(cid).await;
        let from: Vec<String> = holders.iter().map(|h| h.peer.to_string()).collect();
        return Err(unavailable(&[format!("{} : empreinte SHA-256 différente du CID demandé : blob rejeté", from.join(", "))]));
    }
    Ok(writer.commit().await?)
}

/// Magasin qui rapatrie à la demande les blobs absents : `open` cherche d'abord en local, puis auprès des pairs
/// et enregistre le résultat. Branché sous WebDAV et FaceCloud, il rend Kubo facultatif pour les partages entre amis.
///
/// Un gros fichier se **lit pendant qu'il arrive** : `open` lance le rapatriement (une seule fois par blob, il continue
/// même si le lecteur s'en va), rend la main dès que la taille est connue, et le lecteur suit la partie reçue — une lecture
/// au-delà attend les octets suivants. Rien n'est servi sans contrôle : chaque bloc UENC v2 est authentifié au
/// déchiffrement, et l'empreinte complète est vérifiée avant que le blob n'entre dans le magasin.
pub struct P2pBlobStore<B: BlobStore> {
    inner: Arc<B>,
    handle: Option<P2pHandle>,
    peers: Vec<PeerId>,
    max_bytes: u64,
    idle: Duration,
    inflight: Arc<std::sync::Mutex<std::collections::HashMap<String, Following>>>,
}

/// Un rapatriement suivi : son avancement et les demandes de ses lecteurs.
type Following = (watch::Receiver<Progress>, Arc<Ahead>);

/// Silence toléré d'un pair pendant un rapatriement à la demande.
const STORE_IDLE: Duration = Duration::from_secs(30);

impl<B: BlobStore> P2pBlobStore<B> {
    /// `peers` : pairs à interroger en priorité (vide = tous les pairs connectés).
    pub fn new(inner: Arc<B>, handle: P2pHandle, peers: Vec<PeerId>) -> Self {
        Self { inner, handle: Some(handle), peers, max_bytes: MAX_BLOB_BYTES, idle: STORE_IDLE, inflight: Arc::default() }
    }

    /// Sans maillage : se comporte comme `inner` (permet de n'avoir qu'un seul type de magasin selon la configuration).
    pub fn local_only(inner: Arc<B>) -> Self {
        Self { inner, handle: None, peers: Vec::new(), max_bytes: MAX_BLOB_BYTES, idle: STORE_IDLE, inflight: Arc::default() }
    }

    /// Avancement du rapatriement de `cid`, lancé en tâche de fond s'il ne l'est pas déjà.
    fn follow(&self, handle: &P2pHandle, cid: &str) -> (watch::Receiver<Progress>, Arc<Ahead>) {
        let mut map = self.inflight.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((rx, ahead)) = map.get(cid) {
            return (rx.clone(), ahead.clone());
        }
        let (tx, rx) = watch::channel(Progress::Locating);
        let ahead = Arc::new(Ahead::default());
        map.insert(cid.to_owned(), (rx.clone(), ahead.clone()));
        let shared = ahead.clone();
        let (inner, handle, peers, cid, max, idle, map) = (self.inner.clone(), handle.clone(), self.peers.clone(), cid.to_owned(), self.max_bytes, self.idle, self.inflight.clone());
        tokio::spawn(async move {
            let end = match fetch_with_progress(&handle, &peers, &cid, inner.as_ref(), max, idle, Some((&tx, shared.as_ref()))).await {
                Ok(_) => Progress::Done,
                Err(e) => Progress::Failed(e.to_string()),
            };
            map.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&cid);
            tx.send_replace(end);
        });
        (rx, ahead)
    }
}

impl<B: BlobStore> BlobStore for P2pBlobStore<B> {
    type Writer = B::Writer;
    type Reader = P2pReader<B::Reader>;

    fn disk_space(&self) -> Option<(u64, u64)> {
        self.inner.disk_space()
    }

    async fn begin(&self) -> Result<B::Writer, BlobError> {
        self.inner.begin().await
    }

    async fn begin_resumable(&self, cid: &str, max: u64, align: u64) -> Result<astro_dav::blob::Resumed<B::Writer>, BlobError> {
        self.inner.begin_resumable(cid, max, align).await
    }

    async fn discard_partial(&self, cid: &str) {
        self.inner.discard_partial(cid).await;
    }

    async fn open_partial(&self, cid: &str) -> Result<Option<P2pReader<B::Reader>>, BlobError> {
        Ok(self.inner.open_partial(cid).await?.map(P2pReader::Local))
    }

    async fn open(&self, cid: &str) -> Result<(P2pReader<B::Reader>, u64), BlobError> {
        match self.inner.open(cid).await {
            Err(BlobError::NotFound) => {}
            other => return other.map(|(r, n)| (P2pReader::Local(r), n)),
        }
        let Some(handle) = &self.handle else { return Err(BlobError::NotFound) };
        if !exchangeable(cid) {
            return Err(BlobError::BadCid);
        }
        let (mut rx, ahead) = self.follow(handle, cid);
        loop {
            let state = rx.borrow_and_update().clone();
            match state {
                Progress::Locating => {}
                Progress::Streaming { size, .. } => {
                    // La partie peut avoir été validée entre-temps : le blob complet est alors lu tel quel.
                    if let Some(file) = self.inner.open_partial(cid).await? {
                        return Ok((P2pReader::Growing(Box::new(Growing::new(file, size, rx, ahead))), size));
                    }
                }
                Progress::Done => return self.inner.open(cid).await.map(|(r, n)| (P2pReader::Local(r), n)),
                Progress::Failed(_) => return Err(BlobError::NotFound),
            }
            if rx.changed().await.is_err() {
                // Tâche terminée sans dernier mot : le magasin fait foi.
                return self.inner.open(cid).await.map(|(r, n)| (P2pReader::Local(r), n));
            }
        }
    }

    async fn remove(&self, cid: &str) -> Result<(), BlobError> {
        self.inner.remove(cid).await
    }
}

/// Lecteur du magasin P2P : un blob local, ou un blob qui arrive.
pub enum P2pReader<R> {
    /// Blob déjà dans le magasin.
    Local(R),
    /// Blob en cours de rapatriement.
    Growing(Box<Growing<R>>),
}

/// Attente du prochain avancement (le récepteur revient avec le résultat).
type Wait = ReusableBoxFuture<'static, (Result<(), watch::error::RecvError>, watch::Receiver<Progress>)>;

async fn next_progress(mut rx: watch::Receiver<Progress>) -> (Result<(), watch::error::RecvError>, watch::Receiver<Progress>) {
    (rx.changed().await, rx)
}

/// Lecture d'une partie qui grandit : lit dans le fichier ce qui est annoncé prêt, dans la mémoire ce qui est arrivé en avance,
/// demande en priorité ce qui manque à l'endroit lu, et échoue si le rapatriement échoue. La fin n'est rendue qu'une fois
/// l'empreinte complète vérifiée.
pub struct Growing<R> {
    inner: R,
    size: u64,
    /// Position du lecteur.
    pos: u64,
    /// Position du fichier sous-jacent (déplacé seulement au moment d'y lire).
    inner_at: u64,
    moving: bool,
    ready: u64,
    ahead: Arc<Ahead>,
    /// Empreinte complète vérifiée et blob validé : la fin de lecture n'est rendue qu'à ce moment.
    done: bool,
    failed: Option<String>,
    wait: Wait,
}

impl<R> Growing<R> {
    fn new(inner: R, size: u64, rx: watch::Receiver<Progress>, ahead: Arc<Ahead>) -> Self {
        let mut g = Self { inner, size, pos: 0, inner_at: 0, moving: false, ready: 0, ahead, done: false, failed: None, wait: ReusableBoxFuture::new(next_progress(rx.clone())) };
        g.apply(&rx.borrow());
        g
    }

    fn apply(&mut self, p: &Progress) {
        match p {
            Progress::Streaming { ready, .. } => self.ready = self.ready.max(*ready),
            Progress::Done => {
                self.ready = self.size;
                self.done = true;
            }
            Progress::Failed(why) => self.failed = Some(why.clone()),
            Progress::Locating => {}
        }
    }
}

impl<R: AsyncRead + AsyncSeek + Unpin> AsyncRead for Growing<R> {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        loop {
            if buf.remaining() == 0 || (this.pos >= this.size && this.done) {
                return Poll::Ready(Ok(()));
            }
            if this.pos < this.ready && this.pos < this.size {
                // Le fichier est amené à la position lue (après un saut, ou une lecture servie depuis la mémoire).
                while this.inner_at != this.pos || this.moving {
                    if !this.moving {
                        Pin::new(&mut this.inner).start_seek(SeekFrom::Start(this.pos))?;
                        this.moving = true;
                    }
                    let at = std::task::ready!(Pin::new(&mut this.inner).poll_complete(cx))?;
                    this.moving = false;
                    this.inner_at = at;
                }
                let max = usize::try_from((this.ready - this.pos).min(buf.remaining() as u64)).unwrap_or(buf.remaining());
                let n = {
                    let mut sub = ReadBuf::new(buf.initialize_unfilled_to(max));
                    std::task::ready!(Pin::new(&mut this.inner).poll_read(cx, &mut sub))?;
                    sub.filled().len()
                };
                if n == 0 {
                    return Poll::Ready(Err(io::Error::new(io::ErrorKind::UnexpectedEof, "partie reçue plus courte qu'annoncé")));
                }
                buf.advance(n);
                this.pos += n as u64;
                this.inner_at += n as u64;
                return Poll::Ready(Ok(()));
            }
            if let Some(why) = &this.failed {
                return Poll::Ready(Err(io::Error::other(format!("rapatriement interrompu : {why}"))));
            }
            if this.pos < this.size {
                // Au-delà de ce qui est écrit (un saut dans le film) : le segment reçu en avance, sinon demandé en priorité.
                let (seg, last) = (this.pos / SEGMENT, (this.size - 1) / SEGMENT);
                let next: Vec<u64> = (seg + 1..=last.min(seg + 1)).filter(|s| s * SEGMENT >= this.ready).collect();
                if let Some(bytes) = this.ahead.cached(seg) {
                    let from = usize::try_from(this.pos - seg * SEGMENT).unwrap_or(usize::MAX);
                    let chunk = bytes.get(from..).unwrap_or_default();
                    let n = chunk.len().min(buf.remaining());
                    if n == 0 {
                        return Poll::Ready(Err(io::Error::new(io::ErrorKind::UnexpectedEof, "segment reçu plus court qu'annoncé")));
                    }
                    buf.put_slice(&chunk[..n]);
                    this.pos += n as u64;
                    // Lecture suivie : le segment d'après est demandé d'avance.
                    this.ahead.want(&next);
                    return Poll::Ready(Ok(()));
                }
                let mut asked = vec![seg];
                asked.extend(next);
                this.ahead.want(&asked);
            }
            let (changed, rx) = std::task::ready!(this.wait.poll(cx));
            let state = rx.borrow().clone();
            this.apply(&state);
            if changed.is_err() && !this.done && this.failed.is_none() {
                this.failed = Some("rapatriement arrêté".to_owned());
            }
            this.wait.set(next_progress(rx));
        }
    }
}

impl<R: Unpin> AsyncSeek for Growing<R> {
    fn start_seek(self: Pin<&mut Self>, position: SeekFrom) -> io::Result<()> {
        let this = self.get_mut();
        this.pos = match position {
            SeekFrom::Start(n) => Some(n),
            SeekFrom::Current(d) => this.pos.checked_add_signed(d),
            SeekFrom::End(d) => this.size.checked_add_signed(d),
        }
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "position avant le début"))?;
        Ok(())
    }

    fn poll_complete(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<u64>> {
        Poll::Ready(Ok(self.pos))
    }
}

impl<R: AsyncRead + AsyncSeek + Unpin> AsyncRead for P2pReader<R> {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Local(r) => Pin::new(r).poll_read(cx, buf),
            Self::Growing(g) => Pin::new(g.as_mut()).poll_read(cx, buf),
        }
    }
}

impl<R: AsyncSeek + Unpin> AsyncSeek for P2pReader<R> {
    fn start_seek(self: Pin<&mut Self>, position: SeekFrom) -> io::Result<()> {
        match self.get_mut() {
            Self::Local(r) => Pin::new(r).start_seek(position),
            Self::Growing(g) => Pin::new(g.as_mut()).start_seek(position),
        }
    }

    fn poll_complete(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<u64>> {
        match self.get_mut() {
            Self::Local(r) => Pin::new(r).poll_complete(cx),
            Self::Growing(g) => Pin::new(g.as_mut()).poll_complete(cx),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_are_parsed_strictly() {
        let cid = "bafkreihdwdcefgh4dqkjv67uzcmw7ojee6xedzdetojuzjevtenxquvyku";
        assert_eq!(parse_request(&format!("HAS {cid}")), Some(Request::Has(cid.into())));
        assert_eq!(parse_request(&format!("GET {cid} 0 4194304")), Some(Request::Get { cid: cid.into(), offset: 0, len: 4 << 20 }));
        assert_eq!(parse_request("HASN 3"), Some(Request::HasMany(3)));
        assert_eq!(parse_request("HASN 0"), None);
        assert_eq!(parse_request(&format!("HASN {}", MAX_HAS_MANY + 1)), None);
        let d = [7u8; 32];
        assert_eq!(digest_of(&cid_raw_sha256(&d)), Some(d), "CID ↔ empreinte");
        assert_eq!(digest_of("bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi"), None);
        for bad in [format!("GET {cid}"), format!("GET {cid} 1"), format!("GET {cid} -1 5"), format!("GET {cid} 0 5 7"), format!("HAS {cid} 0"), "PUT x".into(), String::new()] {
            assert_eq!(parse_request(&bad), None, "{bad:?}");
        }
        assert!(format!("GET {cid} {} {}\n", u64::MAX, u64::MAX).len() <= MAX_LINE, "la plus longue requête tient dans la ligne");
    }

    #[test]
    fn the_least_busy_usable_peer_is_chosen_in_order_of_preference() {
        let p = || PeerId::random();
        let mut hs = vec![Holder { peer: p(), busy: 1, strikes: 0 }, Holder { peer: p(), busy: 0, strikes: 0 }, Holder { peer: p(), busy: 0, strikes: 0 }];
        assert_eq!(pick(&hs), Some(1));
        hs[1].strikes = PEER_STRIKES;
        assert_eq!(pick(&hs), Some(2), "un pair disqualifié n'est plus sollicité");
        hs[2].busy = PER_PEER;
        hs[0].busy = PER_PEER;
        assert_eq!(pick(&hs), None, "tous occupés ou disqualifiés");
    }
}
