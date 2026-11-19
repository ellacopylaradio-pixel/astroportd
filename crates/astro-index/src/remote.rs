//! Indexeur distant : la station d'un Capitaine ami, jointe par l'adresse **locale** d'un `forward` ouvert sur son service
//! `ai-index` (le tunnel DRAGON chiffre le transport, sa politique décide qui entre).
//!
//! Un résultat n'est accepté que s'il vient du **même modèle** que celui qu'utilise cette station (même identifiant, même
//! SHA-256) : sinon ses vecteurs ne seraient pas comparables aux nôtres. Une tâche sans modèle attendu n'est pas envoyée du tout.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::{Request, StatusCode, header};
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;

use crate::sched::Load;
use crate::{IndexError, Indexer, Input, ModelTag, Output, Task};

/// Réponse lue au plus.
const MAX_REPLY: usize = 8 << 20;

/// Indexeur chez un ami.
#[derive(Clone)]
pub struct RemoteIndexer {
    base: String,
    peer: String,
    timeout: Duration,
    expected: BTreeMap<Task, ModelTag>,
    client: Client<HttpConnector, Full<Bytes>>,
}

impl RemoteIndexer {
    /// `local` : adresse du `forward` vers le service `ai-index` de `peer` ; **boucle locale seulement**.
    pub fn new(local: SocketAddr, peer: &str, timeout: Duration) -> Result<Self, IndexError> {
        if !local.ip().is_loopback() {
            return Err(IndexError::Invalid(format!("{local} : un indexeur ami se joint par son tunnel local (127.0.0.1)")));
        }
        Ok(Self { base: format!("http://{local}/v1/"), peer: peer.to_owned(), timeout, expected: BTreeMap::new(), client: Client::builder(TokioExecutor::new()).build_http() })
    }

    /// Accepte les résultats de `task` venant de ce modèle (celui de cette station).
    #[must_use]
    pub fn expect(mut self, task: Task, model: ModelTag) -> Self {
        self.expected.insert(task, model);
        self
    }

    /// Charge annoncée par la station amie (pour choisir la moins chargée).
    pub async fn load(&self) -> Result<Load, IndexError> {
        let req = Request::get(self.base.trim_end_matches('/')).body(Full::new(Bytes::new())).map_err(|e| IndexError::Backend(e.to_string()))?;
        let call = async {
            let resp = self.client.request(req).await.map_err(|e| IndexError::Backend(format!("station amie injoignable : {e}")))?;
            let body = Limited::new(resp.into_body(), 64 << 10).collect().await.map_err(|e| IndexError::Backend(e.to_string()))?.to_bytes();
            let v: serde_json::Value = serde_json::from_slice(&body).map_err(|e| IndexError::Invalid(e.to_string()))?;
            serde_json::from_value::<Load>(v["load"].clone()).map_err(|e| IndexError::Invalid(format!("charge illisible : {e}")))
        };
        tokio::time::timeout(Duration::from_secs(10), call).await.map_err(|_| IndexError::Backend("délai dépassé".into()))?
    }

    /// PeerID de la station amie (noté avec chaque résultat : « calculé chez … »).
    pub fn peer(&self) -> &str {
        &self.peer
    }

    async fn call(&self, task: Task, input: Input) -> Result<Output, IndexError> {
        let req = Request::post(format!("{}{}", self.base, task.name()))
            .header(header::CONTENT_TYPE, &input.mime)
            .body(Full::new(input.bytes))
            .map_err(|e| IndexError::Backend(e.to_string()))?;
        let resp = self.client.request(req).await.map_err(|e| IndexError::Backend(format!("station amie injoignable : {e}")))?;
        let status = resp.status();
        let retry = resp.headers().get(header::RETRY_AFTER).and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<u32>().ok());
        let body = Limited::new(resp.into_body(), MAX_REPLY).collect().await.map_err(|e| IndexError::Backend(format!("réponse : {e}")))?.to_bytes();
        let why = || String::from_utf8_lossy(&body[..body.len().min(200)]).into_owned();
        match status {
            StatusCode::OK => serde_json::from_slice(&body).map_err(|e| IndexError::Invalid(format!("réponse illisible : {e}"))),
            StatusCode::SERVICE_UNAVAILABLE => Err(IndexError::Busy(retry.unwrap_or(30).clamp(5, 600))),
            StatusCode::PAYLOAD_TOO_LARGE => Err(IndexError::TooLarge(0)),
            StatusCode::NOT_FOUND | StatusCode::NOT_IMPLEMENTED => Err(IndexError::Unavailable(why())),
            s => Err(IndexError::Backend(format!("{s} : {}", why()))),
        }
    }
}

impl Indexer for RemoteIndexer {
    fn tasks(&self) -> Vec<Task> {
        self.expected.keys().copied().collect()
    }

    async fn index(&self, task: Task, input: Input) -> Result<Output, IndexError> {
        let Some(want) = self.expected.get(&task) else {
            return Err(IndexError::Unavailable(format!("{} : aucun modèle attendu, rien n'est envoyé", task.name())));
        };
        input.check(task)?;
        let out = tokio::time::timeout(self.timeout, self.call(task, input)).await.map_err(|_| IndexError::Backend("délai dépassé".into()))??;
        out.check(task)?;
        if &out.model != want {
            return Err(IndexError::Invalid(format!("modèle {} ≠ {} : vecteurs incomparables", out.model.id, want.id)));
        }
        Ok(out)
    }
}
