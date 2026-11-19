//! Service `ai-index` : la station prête sa puissance de calcul à des Capitaines amis.
//!
//! Il écoute **seulement** sur la boucle locale ([`spawn_loopback`]) : on n'y entre que par le tunnel DRAGON
//! `/x/ai-index-<PeerID>`, dont la politique (fermé par défaut) décide qui passe. Jamais sur le port HTTPS public.
//!
//! - `GET /v1` → `{"tasks": […]}` ; `POST /v1/<tâche>` (corps = le contenu, `Content-Type` = son type) → [`Output`] en JSON.
//! - Le contenu reste **en mémoire** : rien n'est écrit sur disque ni journalisé.
//! - `GET /v1` rend aussi la charge ([`Load`]) : un ami qui a plusieurs stations choisit la moins chargée.
//! - Partage : file équitable entre stations ([`FairQueue`]), chacune reconnue par le carnet des pairs du tunnel ([`PeerOf`]) —
//!   jamais par ce qu'elle déclare. Refus 503 avec `Retry-After` quand sa file est pleine.
//! - Bornes : taille par tâche, délai par calcul.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{ConnectInfo, DefaultBodyLimit, Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use crate::sched::{FairQueue, Load};
use crate::{IndexError, Indexer, Input, Output, Task};

/// Qui appelle, d'après l'adresse locale de la connexion (le carnet des pairs du tunnel) : un PeerID, ou `None`.
pub type PeerOf = Arc<dyn Fn(SocketAddr) -> Option<String> + Send + Sync>;

struct Shared<I> {
    indexer: Arc<I>,
    queue: Arc<FairQueue>,
    timeout: Duration,
    peer_of: Option<PeerOf>,
}

fn json(status: StatusCode, body: String) -> Response {
    (status, [(header::CONTENT_TYPE, "application/json")], body).into_response()
}

fn fail(e: &IndexError) -> Response {
    let status = match e {
        IndexError::Unavailable(_) => StatusCode::NOT_IMPLEMENTED,
        IndexError::TooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
        IndexError::Invalid(_) => StatusCode::UNPROCESSABLE_ENTITY,
        IndexError::Busy(s) => return (StatusCode::SERVICE_UNAVAILABLE, [(header::RETRY_AFTER, s.to_string())], "occupé").into_response(),
        IndexError::Backend(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    json(status, serde_json::json!({ "error": e.to_string() }).to_string())
}

async fn tasks<I: Indexer>(State(s): State<Arc<Shared<I>>>) -> Response {
    let load: Load = s.queue.load();
    json(StatusCode::OK, serde_json::json!({ "tasks": s.indexer.tasks(), "load": load }).to_string())
}

async fn index<I: Indexer>(State(s): State<Arc<Shared<I>>>, ConnectInfo(from): ConnectInfo<SocketAddr>, Path(name): Path<String>, headers: HeaderMap, bytes: Bytes) -> Response {
    let Some(task) = Task::parse(&name) else { return json(StatusCode::NOT_FOUND, r#"{"error":"tâche inconnue"}"#.into()) };
    if !s.indexer.tasks().contains(&task) {
        return fail(&IndexError::Unavailable(format!("{name} : pas proposé ici")));
    }
    let mime = headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("").to_owned();
    let input = Input { mime, bytes };
    if let Err(e) = input.check(task) {
        return fail(&e);
    }
    // Une station inconnue du carnet (appel local direct) partage une seule file avec les autres inconnues.
    let peer = s.peer_of.as_ref().and_then(|f| f(from)).unwrap_or_else(|| "?".to_owned());
    let _ticket = match s.queue.enter(&peer).await {
        Ok(t) => t,
        Err(e) => return fail(&e),
    };
    let done: Result<Output, IndexError> = match tokio::time::timeout(s.timeout, s.indexer.index(task, input)).await {
        // Un résultat mal formé est une panne d'ici, pas une faute de l'appelant : 500, et il ne part pas.
        Ok(r) => r.and_then(|o| o.check(task).map(|()| o).map_err(|e| IndexError::Backend(e.to_string()))),
        Err(_) => Err(IndexError::Backend("délai dépassé".into())),
    };
    match done.and_then(|o| serde_json::to_string(&o).map_err(|e| IndexError::Backend(e.to_string()))) {
        Ok(body) => json(StatusCode::OK, body),
        Err(e) => fail(&e),
    }
}

/// Le routeur du service, pour `indexer`, `slots` calculs simultanés au plus, `peer_of` pour reconnaître les stations.
pub fn router<I: Indexer + 'static>(indexer: Arc<I>, slots: usize, timeout: Duration, peer_of: Option<PeerOf>) -> Router {
    let max = Task::ALL.into_iter().map(Task::max_input).max().unwrap_or(0);
    let shared = Arc::new(Shared { indexer, queue: FairQueue::new(slots), timeout, peer_of });
    Router::new().route("/v1", get(tasks::<I>)).route("/v1/{task}", post(index::<I>)).layer(DefaultBodyLimit::max(max)).with_state(shared)
}

/// Démarre le service sur `127.0.0.1` (port libre) ; rend l'adresse à exposer sous le nom `ai-index`.
pub async fn spawn_loopback<I: Indexer + 'static>(indexer: Arc<I>, slots: usize, timeout: Duration, peer_of: Option<PeerOf>) -> std::io::Result<(SocketAddr, tokio::task::JoinHandle<()>)> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let app = router(indexer, slots, timeout, peer_of).into_make_service_with_connect_info::<SocketAddr>();
    let task = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            eprintln!("ai-index : service arrêté ({e})");
        }
    });
    Ok((addr, task))
}
