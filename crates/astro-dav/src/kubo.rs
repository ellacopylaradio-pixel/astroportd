//! Adaptateur IPFS Kubo : [`BlobStore`] sur l'API RPC `http://127.0.0.1:5001/api/v0/`.
//!
//! - écriture : `POST /add?cid-version=1&raw-leaves=true&pin=true` en multipart **en flux** (jamais le fichier entier en mémoire) ;
//! - lecture : `POST /cat?arg=<cid>&offset=…&length=…`, avec accès aléatoire pour les requêtes `Range` ;
//! - suppression : `POST /pin/rm?arg=<cid>` (le ramasse-miettes de Kubo libère l'espace) ;
//! - publication d'un fichier du disque ([`KuboBlobStore::add_file`]) : copie, ou simple référence au fichier (*filestore*,
//!   `nocopy`) quand Kubo l'autorise : rien n'est écrit deux fois.
//!
//! [`AutoBlobStore`] : **le disque d'abord**. Tout s'écrit sur le disque local, qui reste la vérité de la station ; Kubo n'y
//! est qu'un **miroir de publication** (ORIGIN seulement), tenu à part par la station. Kubo arrêté ne rend donc rien illisible.
//! Il reste lu en dernier recours, pour les blobs d'avant ce choix qui n'ont pas encore été rapatriés sur le disque.

use std::future::Future;
use std::io::{self, SeekFrom};
use std::pin::Pin;
use std::sync::Mutex;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures_util::StreamExt;
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::{BodyExt, Empty, StreamBody};
use hyper::body::Frame;
use hyper::{Request, Response, header};
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use tokio::io::{AsyncRead, AsyncSeek, AsyncWrite, ReadBuf};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::io::StreamReader;
use tokio_util::sync::PollSender;

use crate::blob::{BlobError, BlobMeta, BlobStore, BlobWriter, FsBlobStore, FsBlobWriter, is_cid};

type ReqBody = UnsyncBoxBody<Bytes, io::Error>;
type HttpClient = Client<HttpConnector, ReqBody>;

/// Taille maximale d'un morceau envoyé à Kubo.
const CHUNK: usize = 64 * 1024;

fn empty() -> ReqBody {
    Empty::<Bytes>::new().map_err(|never| match never {}).boxed_unsync()
}

fn io_err(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}

/// Magasin de blobs sur un démon Kubo.
#[derive(Clone)]
pub struct KuboBlobStore {
    client: HttpClient,
    /// `http://hôte:port` sans barre finale.
    base: String,
}

impl KuboBlobStore {
    /// `api` : adresse de l'API RPC, par exemple `http://127.0.0.1:5001`.
    pub fn new(api: &str) -> Self {
        Self { client: Client::builder(TokioExecutor::new()).build_http(), base: api.trim_end_matches('/').to_owned() }
    }

    async fn post(&self, path_and_query: &str, body: ReqBody) -> Result<Response<hyper::body::Incoming>, BlobError> {
        let req = Request::post(format!("{}{path_and_query}", self.base)).body(body).map_err(|e| BlobError::Io(io_err(e)))?;
        let resp = self.client.request(req).await.map_err(|e| BlobError::Io(io_err(e)))?;
        check_status(resp).await
    }

    /// `cid` est-il épinglé dans ce Kubo (`/api/v0/pin/ls`, tous types) ? Une erreur ou un CID mal formé vaut « non ».
    pub async fn is_pinned(&self, cid: &str) -> bool {
        is_cid(cid) && self.json(&format!("/api/v0/pin/ls?arg={cid}&type=all&quiet=true")).await.is_ok()
    }

    async fn json(&self, path_and_query: &str) -> Result<serde_json::Value, BlobError> {
        let resp = self.post(path_and_query, empty()).await?;
        let body = resp.into_body().collect().await.map_err(|e| BlobError::Io(io_err(e)))?.to_bytes();
        serde_json::from_slice(&body).map_err(|e| BlobError::Io(io_err(e)))
    }

    /// Pairs qui détiennent `cid` (`/api/v0/routing/findprovs`), au plus `max`, en `within` au plus.
    ///
    /// La réponse est un flux : ce qui est arrivé à l'échéance est conservé (une recherche lente rend des
    /// fournisseurs partiels, pas une erreur). Seuls les `PeerID` sont rendus.
    pub async fn providers(&self, cid: &str, max: usize, within: Duration) -> Result<Vec<String>, BlobError> {
        if !is_cid(cid) {
            return Err(BlobError::Io(io_err(format!("CID invalide : {cid}"))));
        }
        let deadline = tokio::time::Instant::now() + within;
        let query = format!("/api/v0/routing/findprovs?arg={cid}&num-providers={}", max.max(1));
        let resp = tokio::time::timeout_at(deadline, self.post(&query, empty())).await.map_err(|_| BlobError::Io(io_err("findprovs : délai dépassé")))??;
        let mut body = resp.into_body();
        let mut text = String::new();
        loop {
            match tokio::time::timeout_at(deadline, body.frame()).await {
                Ok(Some(Ok(frame))) => {
                    if let Some(data) = frame.data_ref() {
                        text.push_str(&String::from_utf8_lossy(data));
                    }
                }
                Ok(Some(Err(e))) => return Err(BlobError::Io(io_err(e))),
                _ => break, // fin du flux ou échéance
            }
        }
        Ok(parse_providers(&text))
    }

    /// Le démon répond-il ? (`/api/v0/id`, délai de 1,5 s)
    pub async fn ping(&self) -> bool {
        tokio::time::timeout(Duration::from_millis(1500), self.json("/api/v0/id")).await.is_ok_and(|r| r.is_ok())
    }

    /// Kubo accepte-t-il les références à des fichiers du disque (`Experimental.FilestoreEnabled`) ? Une erreur vaut « non ».
    pub async fn filestore_enabled(&self) -> bool {
        self.json("/api/v0/config?arg=Experimental.FilestoreEnabled").await.is_ok_and(|v| v["Value"].as_bool() == Some(true))
    }

    /// Kubo sait-il **vraiment** relire `cid` (un octet, par `cat`) ? Pour un ajout par référence : Kubo lit le fichier du
    /// disque au moment de servir, et un fichier qu'il n'a pas le droit d'ouvrir ne se voit qu'ici.
    pub async fn readable(&self, cid: &str) -> bool {
        if !is_cid(cid) {
            return false;
        }
        match self.post(&format!("/api/v0/cat?arg={cid}&offset=0&length=1"), empty()).await {
            Ok(r) => r.into_body().collect().await.is_ok_and(|b| b.to_bytes().len() == 1),
            Err(_) => false,
        }
    }

    /// Épingle `cid`, déjà connu de Kubo.
    pub async fn pin(&self, cid: &str) -> Result<(), BlobError> {
        if !is_cid(cid) {
            return Err(BlobError::BadCid);
        }
        let r = self.post(&format!("/api/v0/pin/add?arg={cid}"), empty()).await?;
        let _ = r.into_body().collect().await;
        Ok(())
    }

    /// Ajoute le fichier `path` du disque à Kubo, découpé comme [`BlobStore::begin`] (CID v1, feuilles `raw`, sha2-256), et rend
    /// la racine que Kubo lui donne. Le fichier est lu **en flux**.
    pub async fn add_file(&self, path: &std::path::Path, mode: AddMode) -> Result<String, BlobError> {
        let file = tokio::fs::File::open(path).await?;
        let mut rnd = [0u8; 12];
        getrandom::fill(&mut rnd).map_err(|e| BlobError::Io(io_err(e)))?;
        let boundary = format!("astro{}", hex::encode(rnd));
        // Le filestore exige le chemin absolu du fichier dans l'en-tête `Abspath` de la partie (relevé sur Kubo 0.40.0).
        let abspath = match mode {
            AddMode::NoCopy => {
                let abs = std::fs::canonicalize(path)?;
                let text = abs.to_str().filter(|s| !s.contains(['\r', '\n'])).ok_or_else(|| BlobError::Io(io_err("chemin du blob inutilisable par Kubo")))?.to_owned();
                format!("Abspath: {text}\r\n")
            }
            _ => String::new(),
        };
        let prefix = format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"blob\"\r\n{abspath}Content-Type: application/octet-stream\r\n\r\n");
        let suffix = format!("\r\n--{boundary}--\r\n");
        let body_stream = futures_util::stream::once(async move { Ok::<_, io::Error>(Bytes::from(prefix)) })
            .chain(tokio_util::io::ReaderStream::with_capacity(file, CHUNK))
            .chain(futures_util::stream::once(async move { Ok::<_, io::Error>(Bytes::from(suffix)) }))
            .map(|r| r.map(Frame::data));
        let body: ReqBody = StreamBody::new(body_stream).boxed_unsync();
        let options = match mode {
            AddMode::Copy => "pin=true",
            AddMode::NoCopy => "pin=true&nocopy=true",
            AddMode::OnlyHash => "pin=false&only-hash=true",
        };
        let url = format!("{}/api/v0/add?cid-version=1&raw-leaves=true&hash=sha2-256&quieter=true&progress=false&{options}", self.base);
        let req = Request::post(url).header(header::CONTENT_TYPE, format!("multipart/form-data; boundary={boundary}")).body(body).map_err(|e| BlobError::Io(io_err(e)))?;
        let resp = check_status(self.client.request(req).await.map_err(|e| BlobError::Io(io_err(e)))?).await?;
        let bytes = resp.into_body().collect().await.map_err(|e| BlobError::Io(io_err(e)))?.to_bytes();
        let last = String::from_utf8_lossy(&bytes).lines().rev().find(|l| !l.trim().is_empty()).map(str::to_owned).unwrap_or_default();
        let hash = serde_json::from_str::<serde_json::Value>(&last).ok().and_then(|v| v.get("Hash").and_then(|h| h.as_str().map(str::to_owned)));
        hash.filter(|h| is_cid(h)).ok_or_else(|| BlobError::Io(io_err(format!("réponse /add inattendue : {last}"))))
    }
}

/// Façon d'ajouter un fichier du disque à Kubo ([`KuboBlobStore::add_file`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddMode {
    /// Kubo garde sa propre copie des blocs.
    Copy,
    /// Kubo ne garde qu'une référence au fichier du disque (*filestore*) : aucune place en plus. Le fichier doit rester là.
    NoCopy,
    /// Rien n'est gardé : seule la racine est calculée (pour vérifier qu'un contenu est bien celui d'un CID de Kubo).
    OnlyHash,
}

/// Extrait les `PeerID` des fournisseurs d'un flux NDJSON de `routing/findprovs` (`Type` 4 = fournisseur trouvé).
/// Une ligne illisible (flux coupé net à l'échéance) est ignorée.
pub fn parse_providers(ndjson: &str) -> Vec<String> {
    let mut found = std::collections::BTreeSet::new();
    for line in ndjson.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        if v["Type"].as_i64() != Some(4) {
            continue;
        }
        for r in v["Responses"].as_array().into_iter().flatten() {
            if let Some(id) = r["ID"].as_str().filter(|s| !s.is_empty()) {
                found.insert(id.to_owned());
            }
        }
    }
    found.into_iter().collect()
}

/// Convertit une réponse Kubo en erreur si le statut n'est pas 2xx (le message JSON de Kubo est conservé).
async fn check_status(resp: Response<hyper::body::Incoming>) -> Result<Response<hyper::body::Incoming>, BlobError> {
    if resp.status().is_success() {
        return Ok(resp);
    }
    let status = resp.status();
    let body = resp.into_body().collect().await.map(|b| b.to_bytes()).unwrap_or_default();
    let text = String::from_utf8_lossy(&body[..body.len().min(512)]).into_owned();
    let lower = text.to_ascii_lowercase();
    if lower.contains("not found") || lower.contains("does not exist") {
        return Err(BlobError::NotFound);
    }
    Err(BlobError::Io(io::Error::other(format!("Kubo a répondu {status} : {text}"))))
}

/// Écrivain en flux vers `/api/v0/add`.
pub struct KuboWriter {
    tx: PollSender<Bytes>,
    task: Option<JoinHandle<Result<String, BlobError>>>,
    boundary: String,
    size: u64,
}

impl AsyncWrite for KuboWriter {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        match this.tx.poll_reserve(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(_)) => Poll::Ready(Err(io::Error::new(io::ErrorKind::BrokenPipe, "la requête vers Kubo s'est terminée"))),
            Poll::Ready(Ok(())) => {
                let n = buf.len().min(CHUNK);
                if this.tx.send_item(Bytes::copy_from_slice(&buf[..n])).is_err() {
                    return Poll::Ready(Err(io::Error::new(io::ErrorKind::BrokenPipe, "la requête vers Kubo s'est terminée")));
                }
                this.size += n as u64;
                Poll::Ready(Ok(n))
            }
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

impl BlobWriter for KuboWriter {
    async fn commit(mut self) -> Result<BlobMeta, BlobError> {
        let Some(task) = self.task.take() else { return Err(BlobError::Io(io_err("écrivain déjà terminé"))) };
        let Some(tx) = self.tx.get_ref().cloned() else { return Err(BlobError::Io(io_err("canal fermé"))) };
        let suffix = Bytes::from(format!("\r\n--{}--\r\n", self.boundary));
        let sent = tx.send(suffix).await;
        self.tx.close();
        drop(tx);
        if sent.is_err() {
            // La requête s'est arrêtée avant la fin : son erreur est la vraie cause.
            return match task.await {
                Ok(Err(e)) => Err(e),
                _ => Err(BlobError::Io(io_err("requête vers Kubo interrompue"))),
            };
        }
        let cid = task.await.map_err(|e| BlobError::Io(io_err(e)))??;
        Ok(BlobMeta { cid, size: self.size })
    }
}

impl Drop for KuboWriter {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort(); // écriture abandonnée : la requête est coupée, Kubo n'épingle rien
        }
    }
}

type BoxRead = Pin<Box<dyn AsyncRead + Send>>;
type OpenFuture = Pin<Box<dyn Future<Output = io::Result<BoxRead>> + Send>>;

enum ReadState {
    Idle,
    Opening(OpenFuture),
    Streaming(BoxRead),
}

/// Lecteur à accès aléatoire : chaque `seek` rouvre un `cat` à la bonne position.
pub struct KuboReader {
    store: KuboBlobStore,
    cid: String,
    len: u64,
    pos: u64,
    state: ReadState,
}

impl KuboReader {
    fn open_future(&self) -> OpenFuture {
        let (store, cid, pos, len) = (self.store.clone(), self.cid.clone(), self.pos, self.len);
        Box::pin(async move {
            let resp = store.post(&format!("/api/v0/cat?arg={cid}&offset={pos}&length={}", len - pos), empty()).await.map_err(io_err)?;
            let stream = resp.into_body().into_data_stream().map(|r| r.map_err(io_err));
            Ok(Box::pin(StreamReader::new(stream)) as BoxRead)
        })
    }
}

impl AsyncRead for KuboReader {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        loop {
            match &mut this.state {
                ReadState::Idle => {
                    if this.pos >= this.len {
                        return Poll::Ready(Ok(()));
                    }
                    let f = this.open_future();
                    this.state = ReadState::Opening(f);
                }
                ReadState::Opening(f) => match f.as_mut().poll(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(Err(e)) => {
                        this.state = ReadState::Idle;
                        return Poll::Ready(Err(e));
                    }
                    Poll::Ready(Ok(r)) => this.state = ReadState::Streaming(r),
                },
                ReadState::Streaming(r) => {
                    let before = buf.filled().len();
                    return match r.as_mut().poll_read(cx, buf) {
                        Poll::Ready(Ok(())) => {
                            this.pos += (buf.filled().len() - before) as u64;
                            Poll::Ready(Ok(()))
                        }
                        other => other,
                    };
                }
            }
        }
    }
}

impl AsyncSeek for KuboReader {
    fn start_seek(self: Pin<&mut Self>, position: SeekFrom) -> io::Result<()> {
        let this = self.get_mut();
        let target = match position {
            SeekFrom::Start(n) => i128::from(n),
            SeekFrom::Current(d) => i128::from(this.pos) + i128::from(d),
            SeekFrom::End(d) => i128::from(this.len) + i128::from(d),
        };
        let Ok(target) = u64::try_from(target) else { return Err(io::Error::new(io::ErrorKind::InvalidInput, "position négative")) };
        this.pos = target;
        this.state = ReadState::Idle;
        Ok(())
    }

    fn poll_complete(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<u64>> {
        Poll::Ready(Ok(self.pos))
    }
}

impl BlobStore for KuboBlobStore {
    type Writer = KuboWriter;
    type Reader = KuboReader;

    async fn begin(&self) -> Result<KuboWriter, BlobError> {
        let mut rnd = [0u8; 12];
        getrandom::fill(&mut rnd).map_err(|e| BlobError::Io(io_err(e)))?;
        let boundary = format!("astro{}", hex::encode(rnd));
        let (tx, rx) = mpsc::channel::<Bytes>(8);
        let prefix = format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"blob\"\r\nContent-Type: application/octet-stream\r\n\r\n");
        tx.send(Bytes::from(prefix)).await.map_err(|_| BlobError::Io(io_err("canal fermé")))?;
        let frames = futures_util::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|c| (Ok::<_, io::Error>(Frame::data(c)), rx)) });
        let body: ReqBody = StreamBody::new(frames).boxed_unsync();
        let url = format!("{}/api/v0/add?cid-version=1&raw-leaves=true&pin=true&hash=sha2-256&quieter=true&progress=false", self.base);
        let req = Request::post(url)
            .header(header::CONTENT_TYPE, format!("multipart/form-data; boundary={boundary}"))
            .body(body)
            .map_err(|e| BlobError::Io(io_err(e)))?;
        let client = self.client.clone();
        let task = tokio::spawn(async move {
            let resp = client.request(req).await.map_err(|e| BlobError::Io(io_err(e)))?;
            let resp = check_status(resp).await?;
            let body = resp.into_body().collect().await.map_err(|e| BlobError::Io(io_err(e)))?.to_bytes();
            // NDJSON : la dernière ligne est la racine du fichier.
            let last = String::from_utf8_lossy(&body).lines().rev().find(|l| !l.trim().is_empty()).map(str::to_owned).unwrap_or_default();
            let hash = serde_json::from_str::<serde_json::Value>(&last).ok().and_then(|v| v.get("Hash").and_then(|h| h.as_str().map(str::to_owned)));
            hash.filter(|h| is_cid(h)).ok_or_else(|| BlobError::Io(io_err(format!("réponse /add inattendue : {last}"))))
        });
        Ok(KuboWriter { tx: PollSender::new(tx), task: Some(task), boundary, size: 0 })
    }

    async fn open(&self, cid: &str) -> Result<(KuboReader, u64), BlobError> {
        if !is_cid(cid) {
            return Err(BlobError::BadCid);
        }
        let stat = self.json(&format!("/api/v0/files/stat?arg=/ipfs/{cid}")).await?;
        let len = stat.get("Size").and_then(serde_json::Value::as_u64).ok_or_else(|| BlobError::Io(io_err("taille absente de files/stat")))?;
        Ok((KuboReader { store: self.clone(), cid: cid.to_owned(), len, pos: 0, state: ReadState::Idle }, len))
    }

    async fn remove(&self, cid: &str) -> Result<(), BlobError> {
        if !is_cid(cid) {
            return Err(BlobError::BadCid);
        }
        match self.post(&format!("/api/v0/pin/rm?arg={cid}"), empty()).await {
            Ok(r) => {
                let _ = r.into_body().collect().await;
                Ok(())
            }
            // Déjà désépinglé ou jamais présent : l'état voulu est atteint.
            Err(BlobError::NotFound) => Ok(()),
            Err(BlobError::Io(e)) if e.to_string().contains("not pinned") => Ok(()),
            Err(e) => Err(e),
        }
    }
}

/// **Le disque d'abord** : toutes les écritures vont au dossier local, qui garde chaque blob sous son CID `raw` sha2-256
/// (vérifiable, et échangeable par le maillage). Kubo, s'il est donné, n'est lu qu'en dernier recours (blobs rangés là avant
/// ce choix, en attente de rapatriement) ; la publication sur Kubo est l'affaire du miroir de la station, pas de ce magasin.
pub struct AutoBlobStore {
    kubo: Option<KuboBlobStore>,
    fs: FsBlobStore,
    health_ttl: Duration,
    health: Mutex<Option<(Instant, bool)>>,
    /// Bases des autres plans qui partagent ce dossier de blobs (voir [`crate::pool`]).
    peers: Vec<std::path::PathBuf>,
}

/// Lecteur de [`AutoBlobStore`].
pub enum AutoReader {
    /// Depuis Kubo.
    Kubo(Box<KuboReader>),
    /// Depuis le disque local.
    Fs(tokio::fs::File),
}

impl AutoBlobStore {
    /// `kubo` : `None` pour rester purement local.
    pub fn new(kubo: Option<KuboBlobStore>, fs: FsBlobStore) -> Self {
        Self { kubo, fs, health_ttl: Duration::from_secs(10), health: Mutex::new(None), peers: Vec::new() }
    }

    /// Bases des autres `astroportd` qui partagent le même dossier de blobs : un blob qu'elles référencent n'est jamais supprimé.
    pub fn with_pool_peers(mut self, peers: Vec<std::path::PathBuf>) -> Self {
        self.peers = peers;
        self
    }

    /// Durée de validité de la dernière vérification de Kubo.
    pub fn with_health_ttl(mut self, ttl: Duration) -> Self {
        self.health_ttl = ttl;
        self
    }

    /// Le dossier local.
    pub fn fs(&self) -> &FsBlobStore {
        &self.fs
    }

    /// Le Kubo donné, qu'il réponde ou non.
    pub fn kubo(&self) -> Option<&KuboBlobStore> {
        self.kubo.as_ref()
    }

    async fn kubo_up(&self) -> Option<&KuboBlobStore> {
        let kubo = self.kubo.as_ref()?;
        let cached = *self.health.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, up)) = cached {
            if at.elapsed() < self.health_ttl {
                return up.then_some(kubo);
            }
        }
        let up = kubo.ping().await;
        *self.health.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), up));
        up.then_some(kubo)
    }
}

impl AsyncRead for AutoReader {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Kubo(r) => Pin::new(r).poll_read(cx, buf),
            Self::Fs(r) => Pin::new(r).poll_read(cx, buf),
        }
    }
}

impl AsyncSeek for AutoReader {
    fn start_seek(self: Pin<&mut Self>, position: SeekFrom) -> io::Result<()> {
        match self.get_mut() {
            Self::Kubo(r) => Pin::new(r).start_seek(position),
            Self::Fs(r) => Pin::new(r).start_seek(position),
        }
    }

    fn poll_complete(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<u64>> {
        match self.get_mut() {
            Self::Kubo(r) => Pin::new(r).poll_complete(cx),
            Self::Fs(r) => Pin::new(r).poll_complete(cx),
        }
    }
}

impl BlobStore for AutoBlobStore {
    type Writer = FsBlobWriter;
    type Reader = AutoReader;

    fn disk_space(&self) -> Option<(u64, u64)> {
        self.fs.disk_space()
    }

    /// Toujours sur le disque, que Kubo réponde ou non.
    async fn begin(&self) -> Result<FsBlobWriter, BlobError> {
        self.fs.begin().await
    }

    /// Un rapatriement reprenable s'écrit sur le disque local : la partie reçue y survit à un redémarrage.
    async fn begin_resumable(&self, cid: &str, max: u64, align: u64) -> Result<crate::blob::Resumed<FsBlobWriter>, BlobError> {
        self.fs.begin_resumable(cid, max, align).await
    }

    async fn open_partial(&self, cid: &str) -> Result<Option<AutoReader>, BlobError> {
        Ok(self.fs.open_partial(cid).await?.map(AutoReader::Fs))
    }

    async fn discard_partial(&self, cid: &str) {
        self.fs.discard_partial(cid).await;
    }

    async fn open(&self, cid: &str) -> Result<(AutoReader, u64), BlobError> {
        match self.fs.open(cid).await {
            Ok((r, len)) => return Ok((AutoReader::Fs(r), len)),
            Err(BlobError::NotFound) => {}
            Err(e) => return Err(e),
        }
        match self.kubo_up().await {
            Some(kubo) => kubo.open(cid).await.map(|(r, len)| (AutoReader::Kubo(Box::new(r)), len)),
            None => Err(BlobError::NotFound),
        }
    }

    async fn remove(&self, cid: &str) -> Result<(), BlobError> {
        if !self.peers.is_empty() {
            let (peers, c) = (self.peers.clone(), cid.to_owned());
            if tokio::task::spawn_blocking(move || crate::pool::referenced_by_peers(&peers, &c)).await.unwrap_or(true) {
                return Ok(()); // un autre plan s'en sert encore : le blob reste dans le bassin
            }
        }
        self.fs.remove(cid).await?;
        if let Some(kubo) = self.kubo_up().await {
            kubo.remove(cid).await?;
        }
        Ok(())
    }
}
