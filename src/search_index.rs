//! Indexation multimédia en tâche de fond, deux tâches ([`Job`]) : **lire** le texte des images et des PDF (OCR, `ocr`, rangé
//! dans `search_text`) et **regarder** les photos (`vision` : un vecteur SigLIP par photo, dans `search_frames` et
//! `search_vision_vectors`). La recherche qui suit se fait toujours dans la base locale : elle ne dépend jamais du réseau.
//! Une vidéo est regardée comme quelques photos : jusqu'à 12 images clés réparties sur sa durée, chacune avec son instant
//! (`search_frames.t_ms`). ffmpeg la lit à une adresse en boucle locale, à jeton unique, servie en déchiffrant à la volée
//! ([`Worker`]) : aucun clair sur le disque.
//!
//! Le travailleur choisit en SQL les fichiers (comptes hébergés, images JPEG/PNG/WebP/GIF, et PDF pour la lecture, hors
//! compagnons d'une fiche de la Médiathèque), les déchiffre un par un **en mémoire** et range le texte dans `search_text`
//! (`src` = la tâche), l'état dans `search_index`. Un fichier se reconnaît à son empreinte (`sha_plain`) : renommé ou copié, il n'est pas relu ;
//! le modèle change, il l'est.
//!
//! **Où se fait le calcul** : la lecture, ici si la station a le moteur, sinon chez le Capitaine ami que le **membre** a accepté
//! (`index_delegation`, tâche `ocr`), par le tunnel `ai-index` ; sans moteur ni accord, ses fichiers attendent : rien ne part.
//! Les photos, **seulement ici** : chercher par l'image demande de transformer la requête en vecteur au moment de chercher,
//! avec le même modèle ; une station sans ce modèle n'a donc pas de recherche par l'image (pas de vecteurs pour rien).
//!
//! Rythme : un fichier, puis une pause (la station reste disponible) ; file vide, une ronde toutes les 5 minutes, qui purge
//! aussi le texte des fichiers disparus. Un échec est retenté au plus 3 fois, à une heure d'écart.

use std::collections::HashMap;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use astro_core::{ActivityBus, Audience};
use astro_dav::{BlobStore, DavState};
use astro_db::Db;
use astro_index::catalog::Model;
use astro_db::VectorTable;
use astro_index::{IndexError, Indexer, Input, LocalIndexer, ModelTag, Output, RemoteIndexer, Task};
use bytes::Bytes;
use rusqlite::params;

/// Essais au plus par fichier.
pub const MAX_TRIES: i64 = 3;
/// Délai avant de retenter un échec.
const RETRY_SECS: i64 = 3600;
/// Fichiers choisis par ronde.
const BATCH: i64 = 16;
/// Durée maximale d'une indexation (un long PDF scanné chez un ami lent).
const TIMEOUT: Duration = Duration::from_secs(600);

/// Ouvre (ou rend, s'il l'est déjà) l'accès local au service `ai-index` d'une station amie : son adresse 127.0.0.1.
pub type Opener = Arc<dyn Fn(String) -> Pin<Box<dyn Future<Output = Result<SocketAddr, String>> + Send>> + Send + Sync>;

/// Une tâche d'indexation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    /// Nom en base (`search_index.task`, `search_text.src`) et dans l'accord du membre : `ocr` ou `vision`.
    pub db: &'static str,
    /// Ce que l'on demande à l'indexeur.
    pub task: Task,
    /// Les PDF sont-ils concernés ?
    pub pdf: bool,
    /// Modèle dont dépend le résultat (le changer fait refaire la tâche).
    pub model: ModelTag,
    /// Peut-on la confier à une station amie ?
    pub share: bool,
    /// Les vidéos sont-elles concernées (images clés, ffmpeg présent) ?
    pub video: bool,
}

impl Job {
    /// Lire le texte des images et des PDF.
    pub fn ocr() -> Option<Self> {
        Some(Self { db: "ocr", task: Task::Ocr, pdf: true, model: Model::find("ppocrv6-small")?.tag()?, share: true, video: false })
    }

    /// Regarder les photos (un vecteur par photo) et les vidéos (un par image clé, si ffmpeg est là), ici seulement.
    pub fn photos() -> Option<Self> {
        Some(Self { db: "vision", task: Task::Vision, pdf: false, model: Model::find("siglip2-base")?.tag()?, share: false, video: astro_index::video::available() })
    }

    /// Toutes les tâches connues.
    pub fn all() -> Vec<Self> {
        [Self::ocr(), Self::photos()].into_iter().flatten().collect()
    }
}

/// Un fichier à lire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// Propriétaire.
    pub owner: String,
    /// Chemin (un des chemins de ce contenu).
    pub path: String,
    /// Empreinte du clair.
    pub sha: String,
    /// Type MIME (déduit de l'extension pour un PDF sans type).
    pub mime: String,
    /// Taille.
    pub size: i64,
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

/// Fragment SQL (entrée aliasée `e`) : la lecture est-elle permise pour ce compte ? Le Capitaine décide : interrupteur de la
/// station (`kv` options/search_index `enabled`), réglage du compte (`index_members`), sinon le réglage par défaut (`default`).
/// Tout est vrai tant que le Capitaine n'a rien réglé. Le membre, lui, peut mettre l'analyse de ses fichiers en pause
/// (`kv` scope `search_pause`, [`crate::delegation`]).
#[macro_export]
macro_rules! allowed_sql {
    () => {
        "COALESCE((SELECT json_extract(value, '$.enabled') FROM kv WHERE scope = 'options' AND key = 'search_index'), 1) = 1 \
         AND COALESCE((SELECT m.enabled FROM index_members m WHERE m.owner_hex = e.owner_hex), \
           (SELECT json_extract(value, '$.default') FROM kv WHERE scope = 'options' AND key = 'search_index'), 1) = 1 \
         AND NOT EXISTS (SELECT 1 FROM kv WHERE scope = 'search_pause' AND key = e.owner_hex)"
    };
}

/// Fichiers à traiter pour `job` : `local` = la station a le moteur (sinon, seulement les comptes qui ont accepté un Capitaine
/// ami pour cette tâche) ; `owner` : un seul compte. Un contenu présent sous plusieurs chemins n'est rendu qu'une fois.
pub fn candidates(c: &rusqlite::Connection, job: &Job, local: bool, at: i64, limit: i64, owner: Option<&str>) -> rusqlite::Result<Vec<Candidate>> {
    let sql = concat!(
        "SELECT e.owner_hex, min(e.path), e.sha_plain, CASE WHEN lower(e.path) GLOB '*.pdf' THEN 'application/pdf' ELSE COALESCE(e.mime, '') END, max(COALESCE(e.size, 0)) \
         FROM ucloud_entries e JOIN identities i ON i.hex = e.owner_hex AND i.hosted = 1 \
         WHERE e.type = 'file' AND e.cid IS NOT NULL AND e.sha_plain IS NOT NULL AND NOT (e.path >= '/_/' AND e.path < '/_0') \
         AND (e.mime IN ('image/jpeg', 'image/png', 'image/webp', 'image/gif') OR (?8 = 1 AND (e.mime = 'application/pdf' OR lower(e.path) GLOB '*.pdf')) \
           OR (?9 = 1 AND e.mime IN ('video/mp4', 'video/quicktime', 'video/x-m4v', 'video/3gpp', 'video/webm', 'video/x-matroska'))) \
         AND NOT (?8 = 0 AND lower(e.path) GLOB '*.pdf') AND ",
        crate::not_companion_sql!(),
        " AND (?6 IS NULL OR e.owner_hex = ?6) AND ",
        allowed_sql!(),
        " AND (?2 = 1 OR EXISTS (SELECT 1 FROM index_delegation d WHERE d.owner_hex = e.owner_hex AND (',' || d.tasks || ',') LIKE '%,' || ?7 || ',%')) \
         AND NOT EXISTS (SELECT 1 FROM search_index s WHERE s.owner_hex = e.owner_hex AND s.sha = e.sha_plain AND s.task = ?7 \
           AND ((s.state IN ('done', 'skipped') AND s.model_sha = ?1) OR (s.state = 'failed' AND (s.tries >= ?3 OR s.updated_at > ?4)))) \
         GROUP BY e.owner_hex, e.sha_plain ORDER BY max(e.mtime) DESC LIMIT ?5"
    );
    let mut stmt = c.prepare_cached(sql)?;
    let rows = stmt.query_map(params![job.model.sha256, i64::from(local), MAX_TRIES, at - RETRY_SECS, limit, owner, job.db, i64::from(job.pdf), i64::from(job.video)], |r| Ok(Candidate { owner: r.get(0)?, path: r.get(1)?, sha: r.get(2)?, mime: r.get(3)?, size: r.get(4)? }))?;
    rows.collect()
}

/// Range le résultat d'un fichier : texte ou vecteur (s'il y en a) et état. `via` : PeerID de la station amie qui a calculé, `None` = ici.
pub async fn record(db: &Db, c: &Candidate, job: &Job, result: &Result<Output, IndexError>, via: Option<&str>) -> Result<(), astro_db::DbError> {
    let outcome = result.as_ref().map(|o| (o.text.clone(), o.vector.clone().map(|v| vec![(0, v)]).unwrap_or_default()));
    store(db, c, job, outcome, via).await
}

/// Range les images clés d'une vidéo, regardées ici : `(instant en ms, vecteur)`.
pub async fn record_frames(db: &Db, c: &Candidate, job: &Job, result: &Result<Vec<(i64, Vec<f32>)>, IndexError>) -> Result<(), astro_db::DbError> {
    store(db, c, job, result.as_ref().map(|f| (None, f.clone())), None).await
}

/// Ce qu'une analyse a rendu : texte éventuel et vecteurs datés (`(instant en ms, vecteur)`).
type Found = (Option<String>, Vec<(i64, Vec<f32>)>);

async fn store(db: &Db, c: &Candidate, job: &Job, outcome: Result<Found, &IndexError>, via: Option<&str>) -> Result<(), astro_db::DbError> {
    let (owner, sha, model, via, task) = (c.owner.clone(), c.sha.clone(), job.model.clone(), via.map(str::to_owned), job.db);
    let (state, text, frames) = match outcome {
        Ok((text, frames)) => match frames.iter().map(|(t, v)| VectorTable::SearchVision.encode(v).map(|b| (*t, b))).collect::<Result<Vec<_>, _>>() {
            Ok(frames) => ("done", text.filter(|t| !t.trim().is_empty()), frames),
            Err(_) => ("skipped", None, Vec::new()), // vecteur de mauvaise dimension : inutilisable
        },
        Err(IndexError::TooLarge(_) | IndexError::Invalid(_)) => ("skipped", None, Vec::new()),
        Err(_) => ("failed", None, Vec::new()),
    };
    let at = now();
    db.writer()
        .transaction(move |tx| {
            if state != "failed" {
                tx.execute("DELETE FROM search_text WHERE owner_hex = ?1 AND sha = ?2 AND src = ?3", params![owner, sha, task])?;
                if task == "vision" {
                    tx.execute("DELETE FROM search_vision_vectors WHERE vis_id IN (SELECT vis_id FROM search_frames WHERE owner_hex = ?1 AND sha = ?2)", params![owner, sha])?;
                    tx.execute("DELETE FROM search_frames WHERE owner_hex = ?1 AND sha = ?2", params![owner, sha])?;
                }
            }
            for (t_ms, blob) in &frames {
                tx.execute("INSERT INTO search_frames(owner_hex, sha, t_ms) VALUES (?1, ?2, ?3)", params![owner, sha, t_ms])?;
                tx.execute("INSERT INTO search_vision_vectors(vis_id, owner_hex, embedding) VALUES (?1, ?2, ?3)", params![tx.last_insert_rowid(), owner, blob])?;
            }
            if let Some(t) = &text {
                tx.execute("INSERT INTO search_text(body, owner_hex, sha, src, t_ms) VALUES (?1, ?2, ?3, ?4, 0)", params![t, owner, sha, task])?;
            }
            tx.execute(
                "INSERT INTO search_index(owner_hex, sha, task, state, model_id, model_sha, via, tries, updated_at) VALUES (?1, ?2, ?8, ?3, ?4, ?5, ?6, 1, ?7) \
                 ON CONFLICT(owner_hex, sha, task) DO UPDATE SET state = excluded.state, model_id = excluded.model_id, model_sha = excluded.model_sha, via = excluded.via, \
                 tries = CASE WHEN excluded.state = 'failed' THEN search_index.tries + 1 ELSE 1 END, updated_at = excluded.updated_at",
                params![owner, sha, state, model.id, model.sha256, via, at, task],
            )?;
            Ok(())
        })
        .await
}

/// Retire le texte, les vecteurs et l'état des contenus qu'un compte n'a plus (fichier supprimé, remplacé).
pub async fn purge(db: &Db) -> Result<usize, astro_db::DbError> {
    db.writer()
        .transaction(|tx| {
            let gone = "NOT EXISTS (SELECT 1 FROM ucloud_entries e WHERE e.owner_hex = x.owner_hex AND e.sha_plain = x.sha AND e.type = 'file')";
            let n = tx.execute(&format!("DELETE FROM search_text WHERE rowid IN (SELECT rowid FROM search_text x WHERE x.src IN ('ocr', 'vision') AND {gone})"), [])?;
            tx.execute(&format!("DELETE FROM search_vision_vectors WHERE vis_id IN (SELECT vis_id FROM search_frames x WHERE {gone})"), [])?;
            tx.execute(&format!("DELETE FROM search_frames AS x WHERE {gone}"), [])?;
            tx.execute(&format!("DELETE FROM search_index AS x WHERE {gone}"), [])?;
            Ok(n)
        })
        .await
}

/// Délai avant de réessayer une station amie injoignable.
const UNREACHABLE: Duration = Duration::from_secs(300);
/// Durée de validité de la charge annoncée par une station amie.
const LOAD_TTL: Duration = Duration::from_secs(30);

/// Ce que le travailleur sait des stations amies qui lisent pour ses membres : accès ouverts, charge annoncée, et jusqu'à quand
/// les laisser tranquilles (délai de retour qu'elles ont donné, ou station injoignable).
#[derive(Default)]
pub struct Lenders {
    remotes: HashMap<String, RemoteIndexer>,
    load: HashMap<String, (Instant, f64)>,
    until: HashMap<String, Instant>,
}

impl Lenders {
    /// La station `peer` a-t-elle demandé d'attendre (ou est-elle injoignable) ?
    pub fn resting(&self, peer: &str) -> bool {
        self.until.get(peer).is_some_and(|u| *u > Instant::now())
    }
}

/// Le travailleur.
pub struct Worker<B: BlobStore> {
    dav: Arc<DavState<B>>,
    local: Arc<LocalIndexer>,
    opener: Option<Opener>,
    activity: ActivityBus,
    pause: Duration,
    /// Lecture des vidéos par ffmpeg : adresse en boucle locale et jetons en cours (jeton → compte, chemin).
    peek: tokio::sync::OnceCell<(SocketAddr, Tokens)>,
}

type Tokens = Arc<std::sync::Mutex<HashMap<String, (String, String)>>>;

/// Retire le jeton d'une vidéo quand sa lecture est finie (ou abandonnée).
struct Forget(Tokens, String);
impl Drop for Forget {
    fn drop(&mut self) {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&self.1);
    }
}

impl<B: BlobStore + 'static> Worker<B> {
    /// `opener` : accès aux stations amies (`None` sans maillage P2P) ; `pause` : repos entre deux fichiers.
    pub fn new(dav: Arc<DavState<B>>, local: Arc<LocalIndexer>, opener: Option<Opener>, activity: ActivityBus, pause: Duration) -> Self {
        Self { dav, local, opener, activity, pause, peek: tokio::sync::OnceCell::new() }
    }

    /// Le serveur de lecture des vidéos (démarré à la première) : `GET /v/<jeton>` sert le clair, avec accès aléatoire, du
    /// seul fichier lié à ce jeton, tant que son extraction dure. Écoute sur 127.0.0.1 seulement.
    async fn peek(&self) -> Result<&(SocketAddr, Tokens), IndexError> {
        self.peek
            .get_or_try_init(|| async {
                let tokens: Tokens = Arc::default();
                let (dav, t) = (self.dav.clone(), tokens.clone());
                let app = axum::Router::new().route(
                    "/v/{token}",
                    axum::routing::get(move |axum::extract::Path(token): axum::extract::Path<String>, headers: axum::http::HeaderMap| {
                        let (dav, t) = (dav.clone(), t.clone());
                        async move {
                            let file = t.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(&token).cloned();
                            match file {
                                Some((owner, path)) => astro_dav::server::get_owner(&dav, &owner, &path, &headers, false).await,
                                None => axum::response::IntoResponse::into_response(axum::http::StatusCode::NOT_FOUND),
                            }
                        }
                    }),
                );
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.map_err(|e| IndexError::Unavailable(format!("lecture des vidéos : {e}")))?;
                let addr = listener.local_addr().map_err(|e| IndexError::Unavailable(e.to_string()))?;
                tokio::spawn(async move {
                    let _ = axum::serve(listener, app).await;
                });
                Ok((addr, tokens))
            })
            .await
    }

    /// Images clés d'une vidéo du compte (`(instant en ms, PNG)`), lues par ffmpeg à travers [`Self::peek`].
    pub async fn video_frames(&self, c: &Candidate) -> Result<Vec<(i64, Vec<u8>)>, IndexError> {
        let (addr, tokens) = self.peek().await?;
        let mut raw = [0u8; 24];
        getrandom::fill(&mut raw).map_err(|e| IndexError::Backend(format!("hasard indisponible : {e}")))?;
        let token = hex::encode(raw);
        tokens.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(token.clone(), (c.owner.clone(), c.path.clone()));
        let _forget = Forget(tokens.clone(), token.clone());
        astro_index::video::frames(&format!("http://{addr}/v/{token}"), &c.mime).await
    }

    /// Regarde une vidéo ici : chaque image clé devient un vecteur.
    async fn video(&self, c: &Candidate) -> Result<Vec<(i64, Vec<f32>)>, IndexError> {
        let mut out = Vec::new();
        for (t, png) in self.video_frames(c).await? {
            let o = self.local.index(Task::Vision, Input { mime: "image/png".into(), bytes: Bytes::from(png) }).await?;
            out.extend(o.vector.map(|v| (t, v)));
        }
        if out.is_empty() {
            return Err(IndexError::Invalid("aucune image lisible dans la vidéo".into()));
        }
        Ok(out)
    }

    /// La station amie à qui confier ce fichier : parmi celles que le membre a acceptées pour l'OCR, hors celles qui ont demandé
    /// d'attendre, **la moins chargée** (charge annoncée, relue au plus toutes les 30 s).
    async fn delegate(&self, owner: &str, lenders: &mut Lenders, job: &Job) -> Option<RemoteIndexer> {
        let (o, task) = (owner.to_owned(), job.db);
        let peers: Vec<String> = self
            .dav
            .db
            .readers()
            .read(move |c| Ok(c.prepare_cached("SELECT peer FROM index_delegation WHERE owner_hex = ?1 AND (',' || tasks || ',') LIKE '%,' || ?2 || ',%'")?.query_map([o.as_str(), task], |r| r.get(0))?.collect::<Result<Vec<String>, _>>()?))
            .await
            .ok()?;
        let at = Instant::now();
        let mut best: Option<(f64, RemoteIndexer)> = None;
        let awake: Vec<String> = peers.into_iter().filter(|p| lenders.until.get(p).is_none_or(|u| *u <= at)).collect();
        for peer in awake {
            let remote = match lenders.remotes.get(&peer) {
                Some(r) => r.clone(),
                None => {
                    let addr = match (self.opener.as_ref()?)(peer.clone()).await {
                        Ok(a) => a,
                        Err(e) => {
                            self.activity.publish_to(Audience::Owner(owner.to_owned()), "search", "index", format!("station amie injoignable pour lire vos documents : {e}"), None);
                            lenders.until.insert(peer, at + UNREACHABLE);
                            continue;
                        }
                    };
                    let Ok(r) = RemoteIndexer::new(addr, &peer, TIMEOUT) else { continue };
                    // Un accès sert toutes les tâches : chacune n'accepte que le résultat du même modèle que celui d'ici.
                    let r = Job::all().into_iter().fold(r, |r, j| r.expect(j.task, j.model));
                    lenders.remotes.insert(peer.clone(), r.clone());
                    r
                }
            };
            let pressure = match lenders.load.get(&peer) {
                Some((when, p)) if at.duration_since(*when) < LOAD_TTL => *p,
                _ => match remote.load().await {
                    Ok(l) => {
                        lenders.load.insert(peer.clone(), (at, l.pressure()));
                        l.pressure()
                    }
                    Err(_) => {
                        lenders.until.insert(peer, at + UNREACHABLE);
                        lenders.remotes.remove(remote.peer());
                        continue;
                    }
                },
            };
            if best.as_ref().is_none_or(|(b, _)| pressure < *b) {
                best = Some((pressure, remote));
            }
        }
        best.map(|(_, r)| r)
    }

    /// Lit un fichier et range le résultat. `Ok(false)` : rien n'a pu être tenté (pas d'indexeur, amis occupés) : à reprendre.
    pub async fn index_one(&self, c: &Candidate, job: &Job, lenders: &mut Lenders) -> Result<bool, astro_db::DbError> {
        let local = self.local.tasks().contains(&job.task);
        let remote = if local || !job.share { None } else { self.delegate(&c.owner, lenders, job).await };
        if !local && remote.is_none() {
            return Ok(false);
        }
        if c.mime.starts_with("video/") {
            // Lue en flux par ffmpeg : pas de borne de taille, rien en mémoire au-delà des images clés.
            let result = tokio::time::timeout(TIMEOUT, self.video(c)).await.unwrap_or_else(|_| Err(IndexError::Backend("délai dépassé".into())));
            record_frames(&self.dav.db, c, job, &result).await?;
            return Ok(true);
        }
        if usize::try_from(c.size).unwrap_or(usize::MAX) > job.task.max_input() {
            record(&self.dav.db, c, job, &Err(IndexError::TooLarge(0)), None).await?;
            return Ok(true);
        }
        let Some(bytes) = astro_dav::carddav::read_file(&self.dav, &c.owner, &c.path).await else {
            record(&self.dav.db, c, job, &Err(IndexError::Backend("fichier illisible".into())), None).await?;
            return Ok(true);
        };
        let input = Input { mime: c.mime.clone(), bytes: Bytes::from(bytes) };
        let (result, via) = match &remote {
            None => (tokio::time::timeout(TIMEOUT, self.local.index(job.task, input)).await.unwrap_or_else(|_| Err(IndexError::Backend("délai dépassé".into()))), None),
            Some(r) => (r.index(job.task, input).await, Some(r.peer().to_owned())),
        };
        if let (Err(IndexError::Invalid(_) | IndexError::Unavailable(_)), Some(r)) = (&result, &remote) {
            // Résultat refusé (autre version du modèle) ou tâche absente chez l'ami : ce n'est pas la faute du
            // fichier. L'ami est laissé de côté une heure, le fichier attend (il ira chez un autre, ou ici plus tard).
            lenders.until.insert(r.peer().to_owned(), Instant::now() + Duration::from_secs(3600));
            return Ok(false);
        }
        if let (Err(IndexError::Busy(secs)), Some(r)) = (&result, &remote) {
            // L'ami a dit quand revenir : on le laisse tranquille jusque-là, sa charge sera relue au retour.
            lenders.until.insert(r.peer().to_owned(), Instant::now() + Duration::from_secs(u64::from(*secs)));
            lenders.load.remove(r.peer());
            return Ok(false);
        }
        if let (Err(IndexError::Backend(_)), Some(r)) = (&result, &remote) {
            lenders.remotes.remove(r.peer()); // tunnel peut-être fermé : rouvert à la prochaine demande
            lenders.load.remove(r.peer());
        }
        if let (Ok(_), Some(r)) = (&result, &remote) {
            // Un calcul de plus chez cet ami : sa pression estimée monte un peu, le suivant ira peut-être ailleurs.
            if let Some((_, p)) = lenders.load.get_mut(r.peer()) {
                *p += 0.25;
            }
        }
        record(&self.dav.db, c, job, &result, via.as_deref()).await?;
        Ok(true)
    }

    /// Une ronde : au plus [`BATCH`] fichiers. Rend le nombre de fichiers traités.
    pub async fn round(&self, lenders: &mut Lenders) -> Result<usize, astro_db::DbError> {
        let mut done = 0;
        for job in Job::all() {
            let local = self.local.tasks().contains(&job.task);
            if !local && !job.share {
                continue;
            }
            let j = job.clone();
            let todo = self.dav.db.readers().read(move |c| Ok(candidates(c, &j, local, now(), BATCH, None)?)).await?;
            for c in &todo {
                if self.index_one(c, &job, lenders).await? {
                    done += 1;
                }
                tokio::time::sleep(self.pause).await;
            }
        }
        Ok(done)
    }

    /// Tourne jusqu'à l'arrêt du démon.
    pub fn spawn(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut lenders = Lenders::default();
            loop {
                match self.round(&mut lenders).await {
                    Ok(0) => {
                        if let Err(e) = purge(&self.dav.db).await {
                            self.activity.publish_to(Audience::Captain, "system", "index", format!("recherche multimédia : purge impossible ({e})"), None);
                        }
                        // Paquets d'index des membres : relire ceux qui reviennent, écrire ceux qui ont changé (`search_pack`).
                        if let Err(e) = crate::search_pack::sync(&self.dav, &Job::all()).await {
                            self.activity.publish_to(Audience::Captain, "system", "index", format!("recherche multimédia : paquets d'index impossibles ({e})"), None);
                        }
                        tokio::time::sleep(Duration::from_secs(300)).await;
                    }
                    Ok(_) => {}
                    Err(e) => {
                        self.activity.publish_to(Audience::Captain, "system", "index", format!("recherche multimédia : base indisponible ({e})"), None);
                        tokio::time::sleep(Duration::from_secs(300)).await;
                    }
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> rusqlite::Connection {
        let c = astro_db::open_in_memory().unwrap();
        c.execute_batch(
            "INSERT INTO identities(hex, role, hosted) VALUES ('alice', 'multipass', 1), ('bob', 'multipass', 1), ('ext', 'multipass', 0); \
             INSERT INTO ucloud_entries(owner_hex, path, parent, type, cid, mime, size, mtime, sha_plain, data) VALUES \
             ('alice', '/Scans/f.jpg', '/Scans', 'file', 'c1', 'image/jpeg', 10, 3, 's1', '{}'), \
             ('alice', '/Copie/f.jpg', '/Copie', 'file', 'c1', 'image/jpeg', 10, 2, 's1', '{}'), \
             ('alice', '/Docs/contrat.PDF', '/Docs', 'file', 'c2', NULL, 10, 1, 's2', '{}'), \
             ('alice', '/Films/x.poster.jpg', '/Films', 'file', 'c3', 'image/jpeg', 10, 1, 's3', '{}'), \
             ('alice', '/Films/x.media.json', '/Films', 'file', 'c4', 'application/json', 10, 1, 's4', '{}'), \
             ('alice', '/_/Coffre/id.jpg', '/_/Coffre', 'file', 'c5', 'image/jpeg', 10, 1, 's5', '{}'), \
             ('alice', '/Vidéos/v.mp4', '/Vidéos', 'file', 'c6', 'video/mp4', 10, 1, 's6', '{}'), \
             ('bob', '/b.png', '/', 'file', 'c7', 'image/png', 10, 1, 's7', '{}'), \
             ('ext', '/e.png', '/', 'file', 'c8', 'image/png', 10, 1, 's8', '{}');",
        )
        .unwrap();
        c
    }

    fn job(sha: &str) -> Job {
        Job { db: "ocr", task: Task::Ocr, pdf: true, model: ModelTag { id: "ppocrv6-small".into(), sha256: sha.into() }, share: true, video: false }
    }

    fn shas(v: &[Candidate]) -> Vec<(&str, &str)> {
        v.iter().map(|c| (c.owner.as_str(), c.sha.as_str())).collect()
    }

    #[test]
    fn only_hosted_images_and_pdfs_once_each_and_never_the_drawer_or_a_fiche_s_companions() {
        let c = db();
        let all = candidates(&c, &job("m1"), true, 10_000, 50, None).unwrap();
        assert_eq!(shas(&all), [("alice", "s1"), ("alice", "s2"), ("bob", "s7")]);
        assert_eq!(all[1].mime, "application/pdf", "PDF reconnu à son extension");
        // Sans moteur local : seulement les comptes qui ont confié l'OCR à un ami.
        assert!(candidates(&c, &job("m1"), false, 10_000, 50, None).unwrap().is_empty());
        c.execute_batch("INSERT INTO index_delegation(owner_hex, peer, label, tasks, accepted_at) VALUES ('bob', '12D3KooWami', 'Marie', 'vision,ocr', 1)").unwrap();
        assert_eq!(shas(&candidates(&c, &job("m1"), false, 10_000, 50, None).unwrap()), [("bob", "s7")]);
        assert_eq!(shas(&candidates(&c, &job("m1"), true, 10_000, 50, Some("bob")).unwrap()), [("bob", "s7")], "un seul compte");
    }

    #[test]
    fn the_captain_switches_reading_for_the_station_for_newcomers_and_per_member() {
        let c = db();
        let set = |v: &str| c.execute("INSERT OR REPLACE INTO kv(scope, key, value, updated_at) VALUES ('options', 'search_index', ?1, 0)", [v]).unwrap();
        c.execute_batch("INSERT INTO index_members(owner_hex, enabled, updated_at) VALUES ('bob', 0, 0)").unwrap();
        assert_eq!(shas(&candidates(&c, &job("m1"), true, 10_000, 50, None).unwrap()), [("alice", "s1"), ("alice", "s2")], "Bob désactivé par le Capitaine");
        set(r#"{"enabled": true, "default": false}"#);
        assert!(candidates(&c, &job("m1"), true, 10_000, 50, None).unwrap().is_empty(), "par défaut désactivé, Alice n'a pas de réglage propre");
        c.execute_batch("INSERT INTO index_members(owner_hex, enabled, updated_at) VALUES ('alice', 1, 0)").unwrap();
        assert_eq!(candidates(&c, &job("m1"), true, 10_000, 50, None).unwrap().len(), 2, "activée pour Alice seulement");
        set(r#"{"enabled": false, "default": true}"#);
        assert!(candidates(&c, &job("m1"), true, 10_000, 50, None).unwrap().is_empty(), "station coupée : personne");
    }

    #[test]
    fn done_is_done_until_the_model_changes_and_failures_wait_then_stop() {
        let c = db();
        c.execute_batch(
            "INSERT INTO search_index(owner_hex, sha, task, state, model_id, model_sha, tries, updated_at) VALUES \
             ('alice', 's1', 'ocr', 'done', 'ppocrv6-small', 'm1', 1, 1), ('alice', 's2', 'ocr', 'failed', 'ppocrv6-small', 'm1', 1, 9000), \
             ('bob', 's7', 'ocr', 'failed', 'ppocrv6-small', 'm1', 3, 1)",
        )
        .unwrap();
        assert!(candidates(&c, &job("m1"), true, 10_000, 50, None).unwrap().is_empty(), "fait ; échec récent ; trois échecs");
        assert_eq!(shas(&candidates(&c, &job("m1"), true, 9000 + RETRY_SECS + 1, 50, None).unwrap()), [("alice", "s2")], "une heure après : retenté");
        assert_eq!(shas(&candidates(&c, &job("m2"), true, 10_000, 50, None).unwrap()), [("alice", "s1")], "nouveau modèle : relu");
    }
}
