//! **Client des modèles d'IA**, au format d'OpenAI (`/models`, `/chat/completions`, `/embeddings`) que parlent OpenRouter,
//! Mistral, Gemini, Anthropic, vLLM et Ollama (`/v1`). Conception : `docs/design/IA.md`.
//!
//! Deux familles, jamais mélangées :
//! - **cloud** : un préréglage ([`PRESETS`]) en HTTPS, joint par [`crate::fetch`] (adresses publiques seulement, résolution
//!   vérifiée) ;
//! - **uCloud** : une adresse en **boucle locale** seulement (service de la station ou accès local au service d'un ami par un
//!   tunnel DRAGON), sans DNS ni redirection hors de la boucle locale.
//!
//! Les réponses sont bornées en taille et en temps ; une clé d'API ne figure jamais dans une erreur ni dans un résultat.

use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::io::AsyncReadExt;

use crate::fetch::{self, FetchError, FetchLimits};

/// Fournisseurs cloud connus : `(identifiant, nom montré, adresse de base de l'API au format OpenAI)`.
pub const PRESETS: [(&str, &str, &str); 5] = [
    ("openrouter", "OpenRouter", "https://openrouter.ai/api/v1"),
    ("mixroute", "MixRoute", "https://api.mixroute.ai/v1"),
    ("mistral", "Mistral", "https://api.mistral.ai/v1"),
    ("gemini", "Gemini", "https://generativelanguage.googleapis.com/v1beta/openai"),
    ("anthropic", "Anthropic", "https://api.anthropic.com/v1"),
];

/// Modèle essayé par défaut chez un fournisseur, quand le membre n'en a pas choisi : un modèle **gratuit** quand il en propose
/// (MixRoute en ouvre une quinzaine avant toute recharge), pour qu'un premier test ne coûte rien.
fn preset_default(provider: &str) -> Option<&'static str> {
    match provider {
        "mixroute" => Some("deepseek-v4-flash"),
        "openrouter" => Some("openrouter/auto"),
        _ => None,
    }
}

/// Taille maximale d'une réponse (la liste d'OpenRouter dépasse 1 Mo).
const MAX_BODY: u64 = 8 << 20;
/// Modèles gardés dans le résultat d'un test (les plus grandes listes en comptent des centaines).
const MAX_MODELS: usize = 400;

/// Famille d'un point d'accès.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    /// Fournisseur joint par Internet avec une clé.
    Cloud,
    /// Modèle de la station ou d'un ami, en boucle locale.
    UCloud,
}

impl Family {
    /// Nom stable (`cloud`, `ucloud`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cloud => "cloud",
            Self::UCloud => "ucloud",
        }
    }
}

/// Échec d'un appel, dit simplement au membre.
#[derive(Debug, PartialEq, Eq)]
pub enum AiClientError {
    /// Réglage refusé (adresse hors boucle locale, fournisseur inconnu…).
    Invalid(String),
    /// Le fournisseur ou le service a répondu une erreur, ou ne répond pas.
    Provider(String),
}

impl std::fmt::Display for AiClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(m) | Self::Provider(m) => f.write_str(m),
        }
    }
}

/// Une adresse uCloud valide : `http(s)://hôte:port` en boucle locale (`127.x`, `[::1]`, `localhost`), sans chemin ni
/// identifiants. Rend l'adresse normalisée (sans barre finale).
///
/// # Errors
/// Toute autre adresse.
pub fn loopback_url(raw: &str) -> Result<String, AiClientError> {
    let url = raw.trim().trim_end_matches('/');
    let bad = |why: &str| AiClientError::Invalid(format!("adresse locale : {why}"));
    let u = url::Url::parse(url).map_err(|_| bad("illisible (ex. http://127.0.0.1:11434)"))?;
    if !matches!(u.scheme(), "http" | "https") || !u.username().is_empty() || u.password().is_some() || u.query().is_some() || u.fragment().is_some() || !matches!(u.path(), "" | "/") {
        return Err(bad("http://hôte:port seulement, sans chemin ni identifiants"));
    }
    let loopback = match u.host() {
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    };
    if !loopback {
        return Err(bad("toujours sur cette machine (127.0.0.1 ou localhost) : le service d'un ami s'y ouvre par un tunnel"));
    }
    if u.port().is_none() {
        return Err(bad("le port est attendu (ex. 11434 pour Ollama)"));
    }
    Ok(url.to_owned())
}

/// Hôte et port d'une adresse (`127.0.0.1:11434`), pour comparer une adresse à une offre de la station.
#[must_use]
pub fn host_port(url: &str) -> Option<(String, u16)> {
    let u = url::Url::parse(url).ok()?;
    let host = match u.host()? {
        url::Host::Domain(d) if d.eq_ignore_ascii_case("localhost") => "127.0.0.1".to_owned(),
        h => h.to_string(),
    };
    Some((host, u.port_or_known_default()?))
}

/// Un point d'accès à des modèles.
pub struct Endpoint {
    family: Family,
    provider: String,
    base: String,
    key: Option<String>,
    timeout: Duration,
    /// Tests seulement : un faux fournisseur « cloud » en boucle locale.
    #[cfg(test)]
    test_local_cloud: bool,
}

impl Endpoint {
    /// Fournisseur cloud `provider` (un identifiant de [`PRESETS`]) avec sa clé.
    ///
    /// # Errors
    /// Fournisseur inconnu.
    pub fn cloud(provider: &str, key: &str) -> Result<Self, AiClientError> {
        let base = PRESETS.iter().find(|p| p.0 == provider).map(|p| p.2).ok_or_else(|| AiClientError::Invalid(format!("fournisseur inconnu : {provider}")))?;
        Ok(Self::new(Family::Cloud, provider, base.to_owned(), Some(key.to_owned())))
    }

    /// Service uCloud à l'adresse `url` (boucle locale seulement) : son API au format OpenAI est sous `/v1`.
    ///
    /// # Errors
    /// Adresse hors boucle locale.
    pub fn local(url: &str) -> Result<Self, AiClientError> {
        let url = loopback_url(url)?;
        Ok(Self::new(Family::UCloud, "local", format!("{url}/v1"), None))
    }

    fn new(family: Family, provider: &str, base: String, key: Option<String>) -> Self {
        Self {
            family,
            provider: provider.to_owned(),
            base,
            key,
            timeout: Duration::from_secs(30),
            #[cfg(test)]
            test_local_cloud: false,
        }
    }

    /// Famille du point d'accès.
    #[must_use]
    pub fn family(&self) -> Family {
        self.family
    }

    fn limits(&self) -> FetchLimits {
        let mut l = FetchLimits { timeout: self.timeout, max_redirects: 0, ..FetchLimits::new(MAX_BODY) };
        l.loopback_only = self.family == Family::UCloud;
        #[cfg(test)]
        if self.test_local_cloud {
            l.loopback_only = true;
        }
        l
    }

    fn headers(&self) -> Vec<(&'static str, String)> {
        let mut h = Vec::new();
        if let Some(k) = &self.key {
            h.push(("authorization", format!("Bearer {k}")));
            if self.provider == "anthropic" {
                h.push(("x-api-key", k.clone()));
                h.push(("anthropic-version", "2023-06-01".to_owned()));
            }
        }
        if self.provider == "openrouter" {
            // Recommandé par OpenRouter pour reconnaître l'application (aucune donnée du membre).
            h.push(("x-title", "Astroport".to_owned()));
        }
        h
    }

    fn explain(&self, e: &FetchError) -> AiClientError {
        let who = if self.family == Family::UCloud { "le service local" } else { "le fournisseur" };
        AiClientError::Provider(match e {
            FetchError::Status(401 | 403) => "clé d'API refusée : vérifiez-la, ou ses droits".to_owned(),
            FetchError::Status(402) => "crédit épuisé chez le fournisseur".to_owned(),
            FetchError::Status(404) => format!("{who} ne connaît pas cette demande (modèle ou adresse au format OpenAI ?)"),
            FetchError::Status(429) => "trop de demandes : le fournisseur demande d'attendre".to_owned(),
            FetchError::Status(s) if *s >= 500 => format!("{who} est en panne (erreur {s})"),
            FetchError::Connect(_) | FetchError::Dns(_) => format!("{who} ne répond pas"),
            FetchError::Blocked(_) => "adresse refusée : boucle locale pour l'uCloud, Internet public pour le cloud".to_owned(),
            FetchError::TooLarge(_) => format!("{who} répond une réponse trop grosse"),
            other => format!("{who} : {other}"),
        })
    }

    async fn get(&self, path: &str) -> Result<Value, AiClientError> {
        let got = fetch::fetch_with(&format!("{}{path}", self.base), &self.headers(), &self.limits()).await.map_err(|e| self.explain(&e))?;
        let mut buf = Vec::new();
        let mut body = got.body;
        body.read_to_end(&mut buf).await.map_err(|e| self.explain(&FetchError::Connect(e.to_string())))?;
        if buf.len() as u64 > MAX_BODY {
            return Err(self.explain(&FetchError::TooLarge(buf.len() as u64)));
        }
        serde_json::from_slice(&buf).map_err(|_| AiClientError::Provider("réponse illisible : est-ce bien une API au format OpenAI ?".into()))
    }

    async fn post(&self, path: &str, body: &Value) -> Result<Value, AiClientError> {
        let raw = fetch::post_json_with(&format!("{}{path}", self.base), &body.to_string(), &self.headers(), &self.limits()).await.map_err(|e| self.explain(&e))?;
        serde_json::from_slice(&raw).map_err(|_| AiClientError::Provider("réponse illisible : est-ce bien une API au format OpenAI ?".into()))
    }

    /// Modèles proposés, avec ce que le fournisseur en dit (OpenRouter : contexte, modalités, outils, prix).
    ///
    /// # Errors
    /// Fournisseur injoignable, clé refusée, réponse illisible.
    pub async fn models(&self) -> Result<Vec<ModelInfo>, AiClientError> {
        let v = self.get("/models").await?;
        let list = v.get("data").or_else(|| v.get("models")).and_then(Value::as_array).ok_or_else(|| AiClientError::Provider("liste de modèles absente de la réponse".into()))?;
        let mut out: Vec<ModelInfo> = list.iter().filter_map(ModelInfo::from_json).collect();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out.dedup_by(|a, b| a.id == b.id);
        Ok(out)
    }

    /// Une réponse de `model` à `messages` (au format OpenAI), `max_tokens` au plus.
    ///
    /// # Errors
    /// Fournisseur injoignable, modèle inconnu, réponse sans texte.
    pub async fn chat(&self, model: &str, messages: &[Value], max_tokens: u32) -> Result<Reply, AiClientError> {
        let v = self.post("/chat/completions", &json!({ "model": model, "messages": messages, "max_tokens": max_tokens, "stream": false })).await?;
        let text = v.pointer("/choices/0/message/content").and_then(Value::as_str).ok_or_else(|| AiClientError::Provider("le modèle n'a rien répondu".into()))?.to_owned();
        let n = |p: &str| v.pointer(p).and_then(Value::as_u64).unwrap_or(0);
        Ok(Reply { text, tokens_in: n("/usage/prompt_tokens"), tokens_out: n("/usage/completion_tokens") })
    }

    /// Un tour de conversation avec des outils (`tools`, au format OpenAI) : le texte rendu, ou les outils que le modèle veut
    /// appeler (`id`, nom, arguments JSON).
    ///
    /// # Errors
    /// Fournisseur injoignable, modèle inconnu ou qui ne sait pas appeler d'outils (le demandeur réessaie alors sans).
    pub async fn turn(&self, model: &str, messages: &[Value], tools: &[Value], max_tokens: u32) -> Result<Turn, AiClientError> {
        let mut body = json!({ "model": model, "messages": messages, "max_tokens": max_tokens, "stream": false });
        if !tools.is_empty() {
            body["tools"] = Value::Array(tools.to_vec());
        }
        let v = self.post("/chat/completions", &body).await?;
        let msg = v.pointer("/choices/0/message").ok_or_else(|| AiClientError::Provider("le modèle n'a rien répondu".into()))?;
        let calls = msg
            .get("tool_calls")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|c| {
                        let name = c.pointer("/function/name")?.as_str()?.to_owned();
                        let args = c.pointer("/function/arguments").and_then(Value::as_str).and_then(|a| serde_json::from_str(a).ok()).unwrap_or(Value::Null);
                        Some(ToolCall { id: c.get("id").and_then(Value::as_str).unwrap_or("appel").to_owned(), name, args })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let text = msg.get("content").and_then(Value::as_str).map(str::to_owned).filter(|t| !t.trim().is_empty());
        if text.is_none() && calls.is_empty() {
            return Err(AiClientError::Provider("le modèle n'a rien répondu".into()));
        }
        let n = |p: &str| v.pointer(p).and_then(Value::as_u64).unwrap_or(0);
        Ok(Turn { text, calls, tokens_in: n("/usage/prompt_tokens"), tokens_out: n("/usage/completion_tokens") })
    }

    /// Le modèle à employer : `wanted` s'il est donné, sinon celui que l'on choisirait au test ([`default_model`], le modèle
    /// gratuit du fournisseur quand la liste manque).
    ///
    /// # Errors
    /// Aucun modèle connu.
    pub async fn pick_model(&self, wanted: Option<&str>) -> Result<String, AiClientError> {
        if let Some(m) = wanted {
            return Ok(m.to_owned());
        }
        match self.models().await {
            Ok(list) => default_model(&self.provider, &list),
            Err(_) => preset_default(&self.provider).map(str::to_owned),
        }
        .ok_or_else(|| AiClientError::Provider("aucun modèle disponible : choisissez-en un".into()))
    }

    /// Fournisseur (`openrouter`…, `local` pour l'uCloud).
    #[must_use]
    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// `model` rend-il des vecteurs (`/embeddings`) ?
    pub async fn embeds(&self, model: &str) -> bool {
        self.post("/embeddings", &json!({ "model": model, "input": "essai" })).await.is_ok_and(|v| v.pointer("/data/0/embedding").and_then(Value::as_array).is_some_and(|e| !e.is_empty()))
    }

    /// OpenRouter : crédit restant et dépensé de la clé (en dollars, tels que donnés).
    async fn credit(&self) -> Option<Value> {
        if self.provider != "openrouter" {
            return None;
        }
        let v = self.get("/key").await.ok()?;
        let d = v.get("data")?;
        Some(json!({ "usage": d.get("usage"), "limit": d.get("limit"), "remaining": d.get("limit_remaining"), "free_tier": d.get("is_free_tier") }))
    }

    /// Teste le point d'accès : liste des modèles, une réponse courte de `model` (ou du premier modèle), et ce que l'on sait de
    /// ses capacités. Ne lève jamais d'erreur : un échec est dit dans le résultat (`ok: false`, `error`).
    pub async fn probe(&self, model: Option<&str>) -> Value {
        let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let head = json!({ "family": self.family.as_str(), "provider": self.provider, "at": at });
        // Sans liste de modèles (tous ne l'offrent pas), un modèle donné par le membre suffit pour essayer une réponse.
        let model = model.or_else(|| preset_default(&self.provider));
        let models = match (self.models().await, model) {
            (Ok(m), _) => m,
            (Err(_), Some(m)) => vec![ModelInfo { id: m.to_owned(), context: None, vision: None, tools: None, image_out: None, price_in: None, price_out: None }],
            (Err(e), None) => {
                let e = e.to_string();
                let hint = if e.contains("ne connaît pas") { format!("{e} ; si ce fournisseur ne donne pas sa liste, indiquez le modèle") } else { e };
                return merge(head, json!({ "ok": false, "error": hint, "caps": Caps::default().view() }));
            }
        };
        let chosen = model.filter(|m| models.iter().any(|x| x.id == *m)).map(str::to_owned).or_else(|| default_model(&self.provider, &models));
        let mut caps = Caps::default();
        let mut error = None;
        let mut latency = None;
        if let Some(m) = &chosen {
            let started = Instant::now();
            // 64 jetons : un modèle qui « réfléchit » avant de répondre (deepseek-v4…) les dépense en partie à réfléchir.
            match self.chat(m, &[json!({ "role": "user", "content": "Réponds seulement : OK" })], 64).await {
                Ok(_) => {
                    caps.chat = Some(true);
                    latency = Some(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX));
                }
                Err(e) => {
                    caps.chat = Some(false);
                    error = Some(e.to_string());
                }
            }
            if let Some(info) = models.iter().find(|x| &x.id == m) {
                caps.tools = info.tools;
                caps.vision = info.vision;
            }
        }
        caps.images = models.iter().any(|x| x.image_out == Some(true)).then_some(true);
        // Vecteurs : seulement un modèle qui se dit fait pour cela (un essai coûte un appel).
        if let Some(e) = models.iter().find(|x| is_embedding(&x.id)) {
            caps.embeddings = Some(self.embeds(&e.id).await);
        }
        let shown: Vec<Value> = models.iter().take(MAX_MODELS).map(ModelInfo::view).collect();
        merge(
            head,
            json!({ "ok": caps.chat == Some(true), "error": error, "model": chosen, "latency_ms": latency, "caps": caps.view(), "models": shown, "models_total": models.len(), "credit": self.credit().await }),
        )
    }
}

fn merge(mut a: Value, b: Value) -> Value {
    if let (Some(a), Value::Object(b)) = (a.as_object_mut(), b) {
        a.extend(b);
    }
    a
}

/// Modèle essayé quand le membre n'en a pas choisi : celui du fournisseur ([`preset_default`]) s'il est dans la liste, sinon
/// le premier qui sait discuter.
fn default_model(provider: &str, models: &[ModelInfo]) -> Option<String> {
    if let Some(d) = preset_default(provider).filter(|d| models.iter().any(|m| m.id == *d)) {
        return Some(d.to_owned());
    }
    models.iter().find(|m| !is_embedding(&m.id) && m.image_out != Some(true)).or_else(|| models.first()).map(|m| m.id.clone())
}

/// Un modèle de vecteurs (il ne discute pas) : reconnu à son nom, faute de mieux dans la liste au format OpenAI.
fn is_embedding(id: &str) -> bool {
    let id = id.to_lowercase();
    let name = id.rsplit('/').next().unwrap_or(&id);
    id.contains("embed") || ["bge-", "e5-", "multilingual-e5", "gte-", "minilm", "all-minilm", "snowflake-arctic-embed", "mxbai-embed"].iter().any(|p| name.starts_with(p))
}

/// Un appel d'outil demandé par le modèle.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    /// Identifiant à rappeler dans la réponse de l'outil.
    pub id: String,
    /// Nom de l'outil.
    pub name: String,
    /// Arguments (JSON).
    pub args: Value,
}

/// Un tour de conversation : texte, ou appels d'outils.
#[derive(Debug, Clone, PartialEq)]
pub struct Turn {
    /// Texte rendu (absent quand le modèle appelle des outils).
    pub text: Option<String>,
    /// Outils à appeler.
    pub calls: Vec<ToolCall>,
    /// Jetons envoyés.
    pub tokens_in: u64,
    /// Jetons reçus.
    pub tokens_out: u64,
}

/// Réponse d'un modèle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    /// Texte rendu.
    pub text: String,
    /// Jetons envoyés (annoncés par le fournisseur, 0 s'il ne le dit pas).
    pub tokens_in: u64,
    /// Jetons reçus.
    pub tokens_out: u64,
}

/// Capacités constatées : `Some(true)` oui, `Some(false)` non, `None` inconnu.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Caps {
    /// Répond à une conversation.
    pub chat: Option<bool>,
    /// Sait appeler des outils (agir).
    pub tools: Option<bool>,
    /// Lit les images.
    pub vision: Option<bool>,
    /// Rend des vecteurs (mémoire, recherche par le sens).
    pub embeddings: Option<bool>,
    /// Crée des images.
    pub images: Option<bool>,
}

impl Caps {
    fn view(&self) -> Value {
        json!({ "chat": self.chat, "tools": self.tools, "vision": self.vision, "embeddings": self.embeddings, "images": self.images })
    }
}

/// Un modèle tel que la liste du fournisseur le décrit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelInfo {
    /// Identifiant à donner au fournisseur.
    pub id: String,
    /// Taille du contexte, en jetons.
    pub context: Option<u64>,
    /// Lit les images.
    pub vision: Option<bool>,
    /// Sait appeler des outils.
    pub tools: Option<bool>,
    /// Rend des images.
    pub image_out: Option<bool>,
    /// Prix des jetons envoyés, en **millièmes de dollar par million de jetons** (entier : jamais de flottant pour un prix).
    pub price_in: Option<u64>,
    /// Prix des jetons reçus, même unité.
    pub price_out: Option<u64>,
}

impl ModelInfo {
    fn from_json(v: &Value) -> Option<Self> {
        let id: String = v.get("id").or_else(|| v.get("name")).and_then(Value::as_str)?.chars().filter(|c| !c.is_control()).take(120).collect();
        if id.is_empty() {
            return None;
        }
        let modal = |dir: &str| v.pointer(&format!("/architecture/{dir}")).and_then(Value::as_array).map(|a| a.iter().any(|m| m == "image"));
        let tools = v.get("supported_parameters").and_then(Value::as_array).map(|a| a.iter().any(|p| p == "tools"));
        let price = |k: &str| v.pointer(&format!("/pricing/{k}")).and_then(Value::as_str).and_then(per_million_milli);
        Some(Self { id, context: v.get("context_length").and_then(Value::as_u64), vision: modal("input_modalities"), tools, image_out: modal("output_modalities"), price_in: price("prompt"), price_out: price("completion") })
    }

    fn view(&self) -> Value {
        json!({ "id": self.id, "context": self.context, "vision": self.vision, "tools": self.tools, "image_out": self.image_out, "price_in": self.price_in, "price_out": self.price_out })
    }
}

/// Prix décimal par jeton (`"0.000003"` dollar) → millièmes de dollar par million de jetons (`3000`), sans flottant :
/// on décale la virgule de 9 rangs. `None` pour un texte qui n'est pas un décimal positif (prix variable `-1`, etc.).
fn per_million_milli(raw: &str) -> Option<u64> {
    let raw = raw.trim();
    let (int, frac) = raw.split_once('.').unwrap_or((raw, ""));
    if (int.is_empty() && frac.is_empty()) || !int.bytes().all(|b| b.is_ascii_digit()) || !frac.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    // Au-delà de 9 décimales, les rangs restants sont des fractions de millième : arrondis vers le bas.
    let digits: String = int.chars().chain(frac.chars().chain(std::iter::repeat('0')).take(9)).collect();
    let significant = digits.trim_start_matches('0');
    if significant.is_empty() { Some(0) } else { significant.parse::<u64>().ok() }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    /// Faux fournisseur en boucle locale : rend `routes[chemin]` (statut, corps), et compte les requêtes.
    async fn mock(routes: Vec<(&'static str, u16, String)>) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = l.accept().await {
                let routes = routes.clone();
                let log = log.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 65536];
                    let n = s.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]).to_string();
                    let path = req.split_whitespace().nth(1).unwrap_or("").to_owned();
                    log.lock().unwrap().push(req.clone());
                    let (status, body) = routes.iter().find(|r| path.ends_with(r.0)).map_or((404, String::new()), |r| (r.1, r.2.clone()));
                    let resp = format!("HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len());
                    let _ = s.write_all(resp.as_bytes()).await;
                });
            }
        });
        (format!("http://{addr}"), seen)
    }

    fn openrouter_like() -> Vec<(&'static str, u16, String)> {
        let models = json!({ "data": [
            { "id": "openrouter/auto", "context_length": 2_000_000, "architecture": { "input_modalities": ["text", "image"], "output_modalities": ["text"] }, "supported_parameters": ["tools", "temperature"], "pricing": { "prompt": "-1", "completion": "-1" } },
            { "id": "mistralai/mistral-small", "context_length": 32000, "architecture": { "input_modalities": ["text"], "output_modalities": ["text"] }, "supported_parameters": ["temperature"], "pricing": { "prompt": "0.0000002", "completion": "0.0000006" } },
            { "id": "google/gemini-image", "architecture": { "input_modalities": ["text"], "output_modalities": ["image", "text"] } }
        ] });
        let chat = json!({ "choices": [{ "message": { "role": "assistant", "content": "OK" } }], "usage": { "prompt_tokens": 12, "completion_tokens": 1 } });
        vec![("/models", 200, models.to_string()), ("/chat/completions", 200, chat.to_string()), ("/key", 200, json!({ "data": { "usage": 0.5, "limit": 10, "limit_remaining": 9.5, "is_free_tier": false } }).to_string())]
    }

    #[test]
    fn a_local_address_is_on_this_machine_only() {
        for ok in ["http://127.0.0.1:11434", "http://localhost:8188/", "http://[::1]:8000", "http://127.0.0.2:24001"] {
            assert!(loopback_url(ok).is_ok(), "{ok}");
        }
        for bad in ["http://192.168.1.10:11434", "http://10.0.0.1:11434", "http://ollama.lan:11434", "http://127.0.0.1", "http://127.0.0.1:11434/api", "http://u:p@127.0.0.1:1", "ftp://127.0.0.1:21", "http://169.254.169.254:80", "http://0.0.0.0:11434", "http://127.0.0.1.nip.io:11434"] {
            assert!(loopback_url(bad).is_err(), "{bad}");
        }
        assert_eq!(host_port("http://localhost:11434"), Some(("127.0.0.1".into(), 11434)));
    }

    #[test]
    fn prices_are_whole_thousandths_of_a_dollar_per_million_tokens() {
        assert_eq!(per_million_milli("0.000003"), Some(3000));
        assert_eq!(per_million_milli("0.0000002"), Some(200));
        assert_eq!(per_million_milli("0"), Some(0));
        assert_eq!(per_million_milli("15"), Some(15_000_000_000));
        assert_eq!(per_million_milli("0.0000000001"), Some(0), "fraction de millième arrondie vers le bas");
        for bad in ["-1", "", "abc", "1e-6", "0.1.2"] {
            assert_eq!(per_million_milli(bad), None, "{bad}");
        }
    }

    #[tokio::test]
    async fn a_probe_lists_models_answers_and_says_what_it_knows() {
        let (url, seen) = mock(openrouter_like()).await;
        let mut ep = Endpoint::new(Family::Cloud, "openrouter", format!("{url}/api/v1"), Some("sk-or-TRESSECRET".into()));
        ep.test_local_cloud = true;
        let p = ep.probe(None).await;
        assert_eq!((p["ok"].as_bool(), p["model"].as_str(), p["family"].as_str()), (Some(true), Some("openrouter/auto"), Some("cloud")), "{p}");
        assert_eq!(p["caps"], json!({ "chat": true, "tools": true, "vision": true, "embeddings": null, "images": true }));
        assert_eq!(p["models_total"], 3);
        let small = p["models"].as_array().unwrap().iter().find(|m| m["id"] == "mistralai/mistral-small").unwrap();
        assert_eq!((small["price_in"].as_u64(), small["price_out"].as_u64(), small["tools"].as_bool()), (Some(200), Some(600), Some(false)));
        assert_eq!(p["credit"]["remaining"], 9.5);
        assert!(!p.to_string().contains("TRESSECRET"), "la clé n'est jamais dans le résultat");
        let reqs = seen.lock().unwrap().join("\n");
        assert!(reqs.contains("authorization: Bearer sk-or-TRESSECRET") && reqs.contains("\"max_tokens\":64"), "{reqs}");
        // Un modèle choisi qui n'existe pas : le défaut est essayé à la place.
        assert_eq!(ep.probe(Some("n-existe/pas")).await["model"], "openrouter/auto");
        assert_eq!(ep.probe(Some("mistralai/mistral-small")).await["caps"]["tools"], false);
    }

    /// Un fournisseur qui ne donne pas sa liste (MixRoute ne la documente pas) : le modèle gratuit est essayé quand même.
    #[tokio::test]
    async fn without_a_model_list_the_free_model_of_the_provider_is_tried() {
        let chat = json!({ "choices": [{ "message": { "content": "OK" } }], "usage": { "prompt_tokens": 9, "completion_tokens": 1 } });
        let (url, seen) = mock(vec![("/models", 404, "{}".into()), ("/chat/completions", 200, chat.to_string())]).await;
        let mut ep = Endpoint::new(Family::Cloud, "mixroute", format!("{url}/v1"), Some("sk-mr-1234567890".into()));
        ep.test_local_cloud = true;
        let p = ep.probe(None).await;
        assert_eq!((p["ok"].as_bool(), p["model"].as_str()), (Some(true), Some("deepseek-v4-flash")), "{p}");
        assert!(seen.lock().unwrap().iter().any(|r| r.contains("\"model\":\"deepseek-v4-flash\"")));
        // Sans modèle par défaut connu (Mistral) ni liste : on demande au membre d'en indiquer un.
        let (url, _) = mock(vec![("/models", 404, "{}".into())]).await;
        let mut ep = Endpoint::new(Family::Cloud, "mistral", format!("{url}/v1"), Some("k".repeat(12)));
        ep.test_local_cloud = true;
        assert!(ep.probe(None).await["error"].as_str().unwrap().contains("indiquez le modèle"));
    }

    #[tokio::test]
    async fn a_refused_key_a_dead_service_and_a_giant_reply_are_said_plainly() {
        let (url, _) = mock(vec![("/models", 401, "{}".into())]).await;
        let mut ep = Endpoint::new(Family::Cloud, "mistral", format!("{url}/v1"), Some("k".repeat(12)));
        ep.test_local_cloud = true;
        let p = ep.probe(None).await;
        assert_eq!((p["ok"].as_bool(), p["error"].as_str()), (Some(false), Some("clé d'API refusée : vérifiez-la, ou ses droits")));
        // Rien n'écoute : le service local ne répond pas.
        let dead = Endpoint::local("http://127.0.0.1:9").unwrap().probe(None).await;
        assert_eq!(dead["error"], "le service local ne répond pas");
        // Une réponse géante est coupée au plafond.
        let giant = format!("{{\"data\":[{{\"id\":\"{}\"}}]}}", "x".repeat(usize::try_from(MAX_BODY).unwrap() + 10));
        let (url, _) = mock(vec![("/models", 200, giant)]).await;
        let p = Endpoint::local(&url).unwrap().probe(None).await;
        assert_eq!(p["ok"], false);
        assert!(p["error"].as_str().unwrap().contains("trop grosse") || p["error"].as_str().unwrap().contains("illisible"), "{p}");
    }

    #[tokio::test]
    async fn an_ucloud_service_is_reached_on_loopback_only_and_a_cloud_one_never_on_loopback() {
        // Ollama local (API /v1) : ce qu'il sait faire, sans clé.
        let models = json!({ "object": "list", "data": [{ "id": "llama3.2:3b" }, { "id": "bge-m3:latest" }] });
        let chat = json!({ "choices": [{ "message": { "content": "OK" } }] });
        let emb = json!({ "data": [{ "embedding": [0.1, 0.2] }] });
        let (url, seen) = mock(vec![("/models", 200, models.to_string()), ("/chat/completions", 200, chat.to_string()), ("/embeddings", 200, emb.to_string())]).await;
        let p = Endpoint::local(&url).unwrap().probe(None).await;
        assert_eq!((p["ok"].as_bool(), p["model"].as_str(), p["family"].as_str()), (Some(true), Some("llama3.2:3b"), Some("ucloud")), "{p}");
        assert_eq!(p["caps"]["embeddings"], true);
        assert!(!seen.lock().unwrap().join("\n").to_lowercase().contains("authorization"), "pas de clé vers un service local");
        // Le même faux service vu comme « cloud » : refusé, il est en boucle locale.
        let ep = Endpoint::new(Family::Cloud, "mistral", format!("{url}/v1"), Some("k".repeat(12)));
        assert_eq!(ep.probe(None).await["error"], "adresse refusée : boucle locale pour l'uCloud, Internet public pour le cloud");
        // Une adresse uCloud hors de la boucle locale n'est même pas construite.
        assert!(Endpoint::local("http://192.168.1.2:11434").is_err());
        assert!(Endpoint::cloud("inconnu", "k").is_err());
    }
}
