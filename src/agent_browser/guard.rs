//! Garde de navigation du navigateur prêté (lot 4 de `docs/design/SESSION_AGENT.md`).
//!
//! Le filtre CDP ([`super::cdp`]) ne voit que les navigations **demandées** (`Page.navigate`). Une page peut aussi changer
//! d'adresse par un clic, un formulaire ou du JavaScript (`location = …`). La garde ouvre donc sa propre session sur chaque
//! page et y suspend les requêtes de document (`Fetch.enable`, étape `Request`) : une fois le navigateur **prêté**, la page
//! principale ne peut charger que le site accordé et les sites liés choisis par le membre — le reste échoue avant d'être
//! envoyé. Les cadres intérieurs (captcha, vidéo intégrée) passent : ce ne sont pas des changements de site. Une fenêtre qui
//! se serait ouverte sur un autre site avant que la garde ne s'y attache est fermée.
//!
//! Pendant la connexion du membre, la garde laisse tout passer : il est aux commandes (connexion par un autre site, par exemple).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::{Value, json};
use tokio::task::JoinHandle;

use super::cdp::{Policy, Stats};
use super::mux::Mux;

/// Garde d'un navigateur ; s'arrête à la destruction (les requêtes suspendues ne le restent pas : la session de la station
/// meurt avec le navigateur).
pub struct Guard {
    armed: Arc<AtomicBool>,
    task: JoinHandle<()>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct Inner {
    mux: Arc<Mux>,
    policy: Arc<Policy>,
    stats: Arc<Mutex<Stats>>,
    armed: Arc<AtomicBool>,
    /// Session de la garde → cible (page) qu'elle surveille.
    pages: Mutex<HashMap<String, String>>,
}

impl Inner {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, String>> {
        self.pages.lock().unwrap_or_else(PoisonError::into_inner)
    }

    async fn watch_page(&self, target: &str) {
        if self.lock().values().any(|t| t == target) {
            return;
        }
        let Ok(session) = self.mux.attach(target).await else { return };
        self.lock().insert(session.clone(), target.to_owned());
        let _ = self.mux.call(Some(&session), "Fetch.enable", json!({ "patterns": [{ "urlPattern": "*", "resourceType": "Document", "requestStage": "Request" }] })).await;
    }

    async fn on_event(&self, ev: &Value) {
        let p = &ev["params"];
        match ev["method"].as_str().unwrap_or_default() {
            "Fetch.requestPaused" => {
                let Some(session) = ev["sessionId"].as_str() else { return };
                let Some(target) = self.lock().get(session).cloned() else { return };
                let url = p.pointer("/request/url").and_then(Value::as_str).unwrap_or_default();
                // La page principale d'une cible a pour cadre l'identifiant de la cible.
                let main = p["frameId"].as_str() == Some(target.as_str());
                let blocked = main && self.armed.load(Ordering::SeqCst) && !self.policy.allows_url(url);
                let request = p["requestId"].clone();
                if blocked {
                    self.note(url);
                    let _ = self.mux.call(Some(session), "Fetch.failRequest", json!({ "requestId": request, "errorReason": "BlockedByClient" })).await;
                } else {
                    let _ = self.mux.call(Some(session), "Fetch.continueRequest", json!({ "requestId": request })).await;
                }
            }
            "Target.targetCreated" | "Target.targetInfoChanged" if p.pointer("/targetInfo/type").and_then(Value::as_str) == Some("page") => {
                let target = p.pointer("/targetInfo/targetId").and_then(Value::as_str).unwrap_or_default().to_owned();
                let url = p.pointer("/targetInfo/url").and_then(Value::as_str).unwrap_or_default();
                if self.armed.load(Ordering::SeqCst) && !url.is_empty() && !self.policy.allows_url(url) {
                    self.note(url);
                    let _ = self.mux.call(None, "Target.closeTarget", json!({ "targetId": target })).await;
                    return;
                }
                self.watch_page(&target).await;
            }
            "Target.detachedFromTarget" => {
                if let Some(s) = p["sessionId"].as_str() {
                    self.lock().remove(s);
                }
            }
            _ => {}
        }
    }

    fn note(&self, url: &str) {
        let mut s = self.stats.lock().unwrap_or_else(PoisonError::into_inner);
        s.denied += 1;
        if let Some(h) = url::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_ascii_lowercase)) {
            if s.hosts.len() < 50 {
                s.hosts.insert(h);
            }
        }
    }
}

impl Guard {
    /// Garde du navigateur derrière `mux`, selon `policy` ; les refus sont comptés dans `stats`. Désarmée au départ.
    ///
    /// # Errors
    /// Navigateur injoignable.
    pub async fn start(mux: Arc<Mux>, policy: Arc<Policy>, stats: Arc<Mutex<Stats>>) -> Result<Self, String> {
        let armed = Arc::new(AtomicBool::new(false));
        let inner = Arc::new(Inner { mux: mux.clone(), policy, stats, armed: armed.clone(), pages: Mutex::default() });
        let mut events = mux.events();
        mux.call(None, "Target.setDiscoverTargets", json!({ "discover": true })).await?;
        let targets = mux.call(None, "Target.getTargets", json!({})).await?;
        for t in targets["targetInfos"].as_array().into_iter().flatten().filter(|t| t["type"] == "page") {
            if let Some(id) = t["targetId"].as_str() {
                inner.watch_page(id).await;
            }
        }
        let task = tokio::spawn(async move {
            loop {
                match events.recv().await {
                    // Chaque requête suspendue est traitée à part : une réponse lente ne retient pas les autres.
                    Ok(ev) => {
                        let inner = inner.clone();
                        tokio::spawn(async move { inner.on_event(&ev).await });
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(_) => return,
                }
            }
        });
        Ok(Self { armed, task })
    }

    /// Le navigateur est prêté : la garde bloque désormais ce qui sort du site.
    pub fn arm(&self) {
        self.armed.store(true, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tokio::sync::mpsc;

    /// Navigateur scripté : une page P1 ; rend les réponses, garde la trace des décisions de la garde.
    fn browser() -> (Arc<Mux>, Arc<Mutex<Vec<Value>>>) {
        let (tx, mut rx) = mpsc::channel::<String>(64);
        let mux = Mux::new(tx);
        let seen: Arc<Mutex<Vec<Value>>> = Arc::default();
        let (m, log) = (mux.clone(), seen.clone());
        tokio::spawn(async move {
            while let Some(raw) = rx.recv().await {
                let v: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
                let result = match v["method"].as_str().unwrap_or_default() {
                    "Target.getTargets" => json!({ "targetInfos": [{ "targetId": "P1", "type": "page", "url": "https://linkedin.com/" }] }),
                    "Target.attachToTarget" => json!({ "sessionId": format!("G-{}", v["params"]["targetId"].as_str().unwrap_or_default()) }),
                    _ => json!({}),
                };
                log.lock().unwrap_or_else(PoisonError::into_inner).push(v.clone());
                m.dispatch(json!({ "id": v["id"], "result": result }).to_string());
            }
        });
        (mux, seen)
    }

    async fn decision(seen: &Arc<Mutex<Vec<Value>>>, request: &str) -> String {
        for _ in 0..100 {
            if let Some(v) = seen.lock().unwrap_or_else(PoisonError::into_inner).iter().find(|v| v["params"]["requestId"] == request) {
                return v["method"].as_str().unwrap_or_default().to_owned();
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        "rien".into()
    }

    fn paused(mux: &Mux, request: &str, frame: &str, url: &str) {
        mux.dispatch(json!({ "method": "Fetch.requestPaused", "sessionId": "G-P1", "params": { "requestId": request, "frameId": frame, "resourceType": "Document", "request": { "url": url } } }).to_string());
    }

    #[tokio::test]
    async fn once_lent_the_main_page_cannot_leave_the_site() {
        let (mux, seen) = browser();
        let stats = Arc::new(Mutex::new(Stats::default()));
        let policy = Arc::new(Policy::new("linkedin.com", vec!["accounts.google.com".into()], PathBuf::from("/p")));
        let guard = Guard::start(mux.clone(), policy, stats.clone()).await.unwrap();
        assert!(seen.lock().unwrap().iter().any(|v| v["method"] == "Fetch.enable" && v["sessionId"] == "G-P1"), "la page est surveillée");
        // Pendant la connexion du membre : tout passe.
        paused(&mux, "r1", "P1", "https://evil.example/");
        assert_eq!(decision(&seen, "r1").await, "Fetch.continueRequest");
        guard.arm();
        paused(&mux, "r2", "P1", "https://evil.example/");
        paused(&mux, "r3", "P1", "https://www.linkedin.com/feed");
        paused(&mux, "r4", "P1", "https://accounts.google.com/o");
        paused(&mux, "r5", "IFRAME", "https://captcha.example/");
        paused(&mux, "r6", "P1", "javascript:alert(1)");
        assert_eq!(decision(&seen, "r2").await, "Fetch.failRequest", "autre site refusé");
        assert_eq!(decision(&seen, "r3").await, "Fetch.continueRequest", "le site accordé passe");
        assert_eq!(decision(&seen, "r4").await, "Fetch.continueRequest", "un site lié choisi par le membre passe");
        assert_eq!(decision(&seen, "r5").await, "Fetch.continueRequest", "un cadre intérieur n'est pas un changement de site");
        assert_eq!(decision(&seen, "r6").await, "Fetch.failRequest");
        // Une nouvelle fenêtre sur un autre site est fermée ; sur le site, elle est surveillée à son tour.
        mux.dispatch(json!({ "method": "Target.targetCreated", "params": { "targetInfo": { "targetId": "P2", "type": "page", "url": "https://evil.example/" } } }).to_string());
        mux.dispatch(json!({ "method": "Target.targetCreated", "params": { "targetInfo": { "targetId": "P3", "type": "page", "url": "https://linkedin.com/x" } } }).to_string());
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let log = seen.lock().unwrap().clone();
        assert!(log.iter().any(|v| v["method"] == "Target.closeTarget" && v["params"]["targetId"] == "P2"));
        assert!(log.iter().any(|v| v["method"] == "Fetch.enable" && v["sessionId"] == "G-P3"));
        let s = stats.lock().unwrap().clone();
        assert!(s.denied >= 3 && s.hosts.contains("evil.example"), "{s:?}");
    }
}
