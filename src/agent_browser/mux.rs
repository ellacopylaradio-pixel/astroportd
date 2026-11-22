//! Multiplexeur du tube CDP : la station et l'agent parlent au même navigateur sans se voir.
//!
//! Le tube n'a qu'une session racine. La station y ouvre ses **propres** sessions (diffusion de l'écran au membre, garde de
//! navigation, outils de navigation MCP) et numérote ses commandes à partir de [`INTERNAL_BASE`]. Le multiplexeur :
//!
//! - rend à la station ses réponses et les événements de ses sessions, à l'agent tout le reste ;
//! - refuse à l'agent les numéros réservés et toute commande qui vise une session de la station (il ne peut ni la piloter,
//!   ni la détacher, ni couper sa garde) ;
//! - ne montre pas à l'agent les `Target.attachedToTarget` / `detachedFromTarget` des sessions de la station — Chromium
//!   peut annoncer l'attachement **avant** de répondre : ces annonces sont retenues le temps de la réponse ;
//! - garde la découverte des cibles (`Target.setDiscoverTargets`) allumée : l'agent ne peut pas l'éteindre (réponse
//!   simulée), la garde de navigation en dépend.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::{broadcast, mpsc, oneshot};

/// Premier numéro de commande réservé à la station. Chromium n'accepte que des numéros sur 32 bits signés ; les agents
/// (Playwright, Puppeteer) numérotent depuis 1 et n'approchent jamais du milliard.
pub const INTERNAL_BASE: u64 = 1 << 30;
/// Délai d'une commande de la station.
const CALL_TIMEOUT: Duration = Duration::from_secs(15);
/// Messages vers l'agent en attente au plus.
const AGENT_QUEUE: usize = 256;

#[derive(Deserialize)]
struct Head {
    id: Option<u64>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    method: Option<String>,
}

fn head(raw: &str) -> Option<Head> {
    serde_json::from_str(raw).ok()
}

fn locked<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Ce que fait le multiplexeur d'un message de l'agent (déjà passé par le filtre).
#[derive(Debug, PartialEq, Eq)]
pub enum AgentSend {
    /// Vers le navigateur.
    Send,
    /// Réponse rendue à l'agent sans rien envoyer (refus ou réponse simulée).
    Reply(String),
}

/// Le multiplexeur d'un navigateur.
pub struct Mux {
    to: mpsc::Sender<String>,
    next: AtomicU64,
    pending: Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>,
    internal: Mutex<HashSet<String>>,
    attaching: Mutex<HashMap<String, Vec<String>>>,
    events: broadcast::Sender<Arc<Value>>,
    agent: Mutex<Option<mpsc::Sender<String>>>,
}

/// Place de l'agent : la libérer (destruction) débranche l'agent.
pub struct AgentSlot(Arc<Mux>);

impl Drop for AgentSlot {
    fn drop(&mut self) {
        *locked(&self.0.agent) = None;
    }
}

impl Mux {
    /// Multiplexeur qui écrit vers le navigateur par `to`.
    #[must_use]
    pub fn new(to: mpsc::Sender<String>) -> Arc<Self> {
        let (events, _) = broadcast::channel(256);
        Arc::new(Self { to, next: AtomicU64::new(INTERNAL_BASE), pending: Mutex::default(), internal: Mutex::default(), attaching: Mutex::default(), events, agent: Mutex::default() })
    }

    /// Événements pour la station : ceux de ses sessions, et les `Target.*` de la racine.
    #[must_use]
    pub fn events(&self) -> broadcast::Receiver<Arc<Value>> {
        self.events.subscribe()
    }

    /// Branche l'agent. `None` si un agent est déjà branché (un seul à la fois).
    #[must_use]
    pub fn connect_agent(self: &Arc<Self>) -> Option<(mpsc::Receiver<String>, AgentSlot)> {
        let mut slot = locked(&self.agent);
        if slot.is_some() {
            return None;
        }
        let (tx, rx) = mpsc::channel(AGENT_QUEUE);
        *slot = Some(tx);
        Some((rx, AgentSlot(self.clone())))
    }

    fn to_agent(&self, raw: String) {
        if let Some(tx) = locked(&self.agent).as_ref() {
            // Agent trop lent : le message est perdu plutôt que de bloquer la station.
            let _ = tx.try_send(raw);
        }
    }

    fn to_internal(&self, raw: &str) {
        if self.events.receiver_count() > 0 {
            if let Ok(v) = serde_json::from_str::<Value>(raw) {
                let _ = self.events.send(Arc::new(v));
            }
        }
    }

    /// Un message du navigateur.
    pub fn dispatch(&self, raw: String) {
        let Some(h) = head(&raw) else { return };
        if let Some(id) = h.id {
            if id >= INTERNAL_BASE {
                if let Some(tx) = locked(&self.pending).remove(&id) {
                    let v: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
                    let out = match v.get("error") {
                        Some(e) => Err(e.get("message").and_then(Value::as_str).unwrap_or("erreur du navigateur").to_owned()),
                        None => Ok(v.get("result").cloned().unwrap_or(Value::Null)),
                    };
                    let _ = tx.send(out);
                }
            } else {
                self.to_agent(raw);
            }
            return;
        }
        if let Some(s) = &h.session_id {
            if locked(&self.internal).contains(s) { self.to_internal(&raw) } else { self.to_agent(raw) }
            return;
        }
        let method = h.method.unwrap_or_default();
        if !method.starts_with("Target.") {
            self.to_agent(raw);
            return;
        }
        let v: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
        if matches!(method.as_str(), "Target.attachedToTarget" | "Target.detachedFromTarget") {
            let sid = v.pointer("/params/sessionId").and_then(Value::as_str).unwrap_or_default();
            if locked(&self.internal).contains(sid) {
                if method == "Target.detachedFromTarget" {
                    locked(&self.internal).remove(sid);
                }
                self.to_internal(&raw);
                return;
            }
            if method == "Target.attachedToTarget" {
                let target = v.pointer("/params/targetInfo/targetId").and_then(Value::as_str).unwrap_or_default();
                if let Some(buf) = locked(&self.attaching).get_mut(target) {
                    buf.push(raw);
                    return;
                }
            }
        }
        // Découverte des cibles : l'agent et la station la voient tous deux.
        if self.events.receiver_count() > 0 {
            let _ = self.events.send(Arc::new(v));
        }
        self.to_agent(raw);
    }

    /// Commande de la station (sur la racine si `session` est `None`).
    ///
    /// # Errors
    /// Erreur rendue par le navigateur, navigateur arrêté ou trop lent.
    pub async fn call(&self, session: Option<&str>, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        locked(&self.pending).insert(id, tx);
        let mut msg = json!({ "id": id, "method": method, "params": params });
        if let Some(s) = session {
            msg["sessionId"] = Value::String(s.to_owned());
        }
        if self.to.send(msg.to_string()).await.is_err() {
            locked(&self.pending).remove(&id);
            return Err("navigateur arrêté".into());
        }
        match tokio::time::timeout(CALL_TIMEOUT, rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => Err("navigateur arrêté".into()),
            Err(_) => {
                locked(&self.pending).remove(&id);
                Err(format!("le navigateur ne répond pas à {method}"))
            }
        }
    }

    /// Ouvre une session de la station sur la cible `target` (mode à plat). Rend son identifiant de session.
    ///
    /// # Errors
    /// Cible inconnue ou navigateur indisponible.
    pub async fn attach(&self, target: &str) -> Result<String, String> {
        locked(&self.attaching).insert(target.to_owned(), Vec::new());
        let r = self.call(None, "Target.attachToTarget", json!({ "targetId": target, "flatten": true })).await;
        let held = locked(&self.attaching).remove(target).unwrap_or_default();
        let sid = r.as_ref().ok().and_then(|v| v["sessionId"].as_str()).map(str::to_owned);
        if let Some(s) = &sid {
            locked(&self.internal).insert(s.clone());
        }
        // Les annonces retenues : la nôtre reste à la station, celles d'un attachement de l'agent lui reviennent.
        for raw in held {
            let mine = serde_json::from_str::<Value>(&raw).ok().and_then(|v| v.pointer("/params/sessionId").and_then(Value::as_str).map(str::to_owned)) == sid;
            if mine { self.to_internal(&raw) } else { self.to_agent(raw) }
        }
        sid.ok_or_else(|| r.err().unwrap_or_else(|| "attachement refusé".into()))
    }

    /// Décide d'un message de l'agent, une fois passé par le filtre.
    #[must_use]
    pub fn check_agent(&self, raw: &str) -> AgentSend {
        let v: Value = serde_json::from_str(raw).unwrap_or(Value::Null);
        let id = v.get("id").cloned().unwrap_or(Value::Null);
        let answer = |result: Option<Value>, why: &str| {
            let mut r = match result {
                Some(res) => json!({ "id": id, "result": res }),
                None => json!({ "id": id, "error": { "code": -32000, "message": format!("refusé par la station : {why}") } }),
            };
            if let Some(s) = v.get("sessionId") {
                r["sessionId"] = s.clone();
            }
            AgentSend::Reply(r.to_string())
        };
        if id.as_u64().is_none_or(|n| n >= INTERNAL_BASE) {
            return answer(None, "numéro de commande réservé");
        }
        let internal = locked(&self.internal);
        let touches = |p: &str| v.pointer(p).and_then(Value::as_str).is_some_and(|s| internal.contains(s));
        if touches("/sessionId") || touches("/params/sessionId") {
            return answer(None, "session de la station");
        }
        drop(internal);
        if v["method"] == "Target.setDiscoverTargets" && v.pointer("/params/discover") == Some(&Value::Bool(false)) {
            return answer(Some(json!({})), "");
        }
        AgentSend::Send
    }

    /// Transmet au navigateur un message de l'agent accepté.
    ///
    /// # Errors
    /// Navigateur arrêté.
    pub async fn send_agent(&self, raw: String) -> Result<(), ()> {
        self.to.send(raw).await.map_err(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rig() -> (Arc<Mux>, mpsc::Receiver<String>) {
        let (tx, rx) = mpsc::channel(64);
        (Mux::new(tx), rx)
    }

    #[tokio::test]
    async fn station_and_agent_answers_go_to_their_owner() {
        let (mux, mut to_browser) = rig();
        let (mut agent, _slot) = mux.connect_agent().unwrap();
        assert!(mux.connect_agent().is_none(), "un seul agent");
        let m = mux.clone();
        let call = tokio::spawn(async move { m.call(None, "Browser.getVersion", json!({})).await });
        let sent: Value = serde_json::from_str(&to_browser.recv().await.unwrap()).unwrap();
        let id = sent["id"].as_u64().unwrap();
        assert!(id >= INTERNAL_BASE);
        mux.dispatch(json!({ "id": 3, "result": { "agent": true } }).to_string());
        mux.dispatch(json!({ "id": id, "result": { "product": "Chrome" } }).to_string());
        assert_eq!(call.await.unwrap().unwrap()["product"], "Chrome");
        assert!(agent.recv().await.unwrap().contains("\"agent\":true"));
        assert!(agent.try_recv().is_err(), "la réponse de la station ne va pas à l'agent");
    }

    #[tokio::test]
    async fn the_agent_never_sees_nor_touches_station_sessions() {
        let (mux, mut to_browser) = rig();
        let (mut agent, slot) = mux.connect_agent().unwrap();
        let mut ev = mux.events();
        let m = mux.clone();
        let attach = tokio::spawn(async move { m.attach("T1").await });
        let sent: Value = serde_json::from_str(&to_browser.recv().await.unwrap()).unwrap();
        // Chromium annonce l'attachement avant de répondre ; un attachement de l'agent sur la même cible arrive en même temps.
        mux.dispatch(json!({ "method": "Target.attachedToTarget", "params": { "sessionId": "INT", "targetInfo": { "targetId": "T1" } } }).to_string());
        mux.dispatch(json!({ "method": "Target.attachedToTarget", "params": { "sessionId": "AG", "targetInfo": { "targetId": "T1" } } }).to_string());
        mux.dispatch(json!({ "id": sent["id"], "result": { "sessionId": "INT" } }).to_string());
        assert_eq!(attach.await.unwrap().unwrap(), "INT");
        let to_agent = agent.recv().await.unwrap();
        assert!(to_agent.contains("\"AG\"") && !to_agent.contains("INT"), "{to_agent}");
        assert!(agent.try_recv().is_err());
        assert!(ev.recv().await.unwrap().to_string().contains("INT"));
        // Événements de la session de la station : pour elle seule.
        mux.dispatch(json!({ "method": "Page.screencastFrame", "sessionId": "INT", "params": {} }).to_string());
        assert!(agent.try_recv().is_err());
        // L'agent ne peut ni piloter, ni détacher la session de la station, ni usurper un numéro réservé, ni éteindre la découverte.
        for (raw, want) in [
            (json!({ "id": 5, "method": "Fetch.disable", "sessionId": "INT" }), "refusé"),
            (json!({ "id": 6, "method": "Target.detachFromTarget", "params": { "sessionId": "INT" } }), "refusé"),
            (json!({ "id": INTERNAL_BASE + 1, "method": "Browser.getVersion" }), "refusé"),
            (json!({ "id": 7, "method": "Target.setDiscoverTargets", "params": { "discover": false } }), "\"result\":{}"),
        ] {
            let AgentSend::Reply(r) = mux.check_agent(&raw.to_string()) else { unreachable!("{raw}") };
            assert!(r.contains(want), "{r}");
        }
        assert_eq!(mux.check_agent(&json!({ "id": 8, "method": "Page.navigate", "sessionId": "AG" }).to_string()), AgentSend::Send);
        // L'agent parti, sa place se libère.
        drop(slot);
        assert!(mux.connect_agent().is_some());
    }
}
