//! Outils de navigation MCP (lot 5 de `docs/design/SESSION_AGENT.md`) : la station pilote elle-même le navigateur prêté pour un
//! agent qui ne peut pas s'y brancher (agent hébergé dans le nuage de son éditeur : ChatGPT, claude.ai…).
//!
//! L'agent ne touche jamais au navigateur : il appelle `naviguer`, `lire_page`, `cliquer`, `saisir`, `capture`, et la station
//! exécute par **sa** session (multiplexeur), sous les mêmes règles que la porte CDP : navigation bornée au site accordé, garde
//! de navigation armée, aucune commande arbitraire, pas d'exécution de JavaScript fourni par l'agent.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::{Value, json};

use super::cdp::Policy;
use super::mux::Mux;

/// Éléments numérotés rendus au plus par `lire_page`.
const MAX_ELEMENTS: usize = 200;
/// Texte de la page rendu au plus (caractères).
const MAX_TEXT: usize = 6000;
/// Texte saisi d'un coup au plus.
const MAX_INPUT: usize = 2000;
/// Rôles d'accessibilité retenus : ce sur quoi on agit, et les titres pour se repérer.
const ROLES: &[&str] = &["button", "link", "textbox", "searchbox", "combobox", "checkbox", "radio", "menuitem", "tab", "option", "switch", "heading", "listbox", "spinbutton", "slider"];

/// Pilote d'un navigateur prêté.
pub struct Pilot {
    mux: Arc<Mux>,
    policy: Arc<Policy>,
    session: tokio::sync::Mutex<Option<String>>,
    /// Numéro rendu par `lire_page` → nœud du document.
    elements: Mutex<HashMap<u64, i64>>,
}

impl Pilot {
    /// Pilote du navigateur derrière `mux`, selon `policy`.
    #[must_use]
    pub fn new(mux: Arc<Mux>, policy: Arc<Policy>) -> Self {
        Self { mux, policy, session: tokio::sync::Mutex::new(None), elements: Mutex::default() }
    }

    /// Session sur la première page (ouverte au premier usage, rouverte si la page a changé).
    async fn session(&self, fresh: bool) -> Result<String, String> {
        let mut s = self.session.lock().await;
        if let (Some(id), false) = (s.as_ref(), fresh) {
            return Ok(id.clone());
        }
        let targets = self.mux.call(None, "Target.getTargets", json!({})).await?;
        let page = targets["targetInfos"].as_array().into_iter().flatten().find(|t| t["type"] == "page").and_then(|t| t["targetId"].as_str()).map(str::to_owned).ok_or("aucune page ouverte")?;
        let id = self.mux.attach(&page).await?;
        *s = Some(id.clone());
        Ok(id)
    }

    /// Commande sur la page ; une session perdue (page fermée) est rouverte une fois.
    async fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        let s = self.session(false).await?;
        match self.mux.call(Some(&s), method, params.clone()).await {
            Err(e) if e.contains("ession") => {
                let s = self.session(true).await?;
                self.mux.call(Some(&s), method, params).await
            }
            r => r,
        }
    }

    async fn eval(&self, expr: &str) -> Result<Value, String> {
        let r = self.call("Runtime.evaluate", json!({ "expression": expr, "returnByValue": true })).await?;
        Ok(r.pointer("/result/value").cloned().unwrap_or(Value::Null))
    }

    /// Va à `url` (site accordé seulement) et attend que la page soit chargée (15 s au plus).
    ///
    /// # Errors
    /// Adresse hors du site, ou navigateur indisponible.
    pub async fn navigate(&self, url: &str) -> Result<Value, String> {
        if !self.policy.allows_url(url) {
            return Err("adresse hors du site accordé".into());
        }
        let r = self.call("Page.navigate", json!({ "url": url })).await?;
        if let Some(e) = r["errorText"].as_str() {
            return Err(format!("page non chargée : {e}"));
        }
        for _ in 0..60 {
            if self.eval("document.readyState").await.ok().as_ref().and_then(Value::as_str) == Some("complete") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        self.read().await
    }

    /// Adresse, titre, texte visible et éléments numérotés (à passer à `cliquer` / `saisir`).
    ///
    /// # Errors
    /// Navigateur indisponible.
    pub async fn read(&self) -> Result<Value, String> {
        let page = self.eval(&format!("({{ url: location.href, title: document.title, text: (document.body ? document.body.innerText : '').slice(0, {MAX_TEXT}) }})")).await?;
        let tree = self.call("Accessibility.getFullAXTree", json!({})).await?;
        let mut map = HashMap::new();
        let mut out = Vec::new();
        for node in tree["nodes"].as_array().into_iter().flatten() {
            if out.len() >= MAX_ELEMENTS {
                break;
            }
            let role = node.pointer("/role/value").and_then(Value::as_str).unwrap_or_default();
            let name: String = node.pointer("/name/value").and_then(Value::as_str).unwrap_or_default().chars().take(120).collect();
            let Some(backend) = node["backendDOMNodeId"].as_i64() else { continue };
            if node["ignored"] == true || !ROLES.contains(&role) || (name.trim().is_empty() && !matches!(role, "textbox" | "searchbox" | "combobox")) {
                continue;
            }
            let n = out.len() as u64 + 1;
            map.insert(n, backend);
            out.push(json!({ "n": n, "role": role, "name": name.trim() }));
        }
        *self.elements.lock().unwrap_or_else(PoisonError::into_inner) = map;
        Ok(json!({ "url": page["url"], "title": page["title"], "elements": out, "text": page["text"] }))
    }

    fn node(&self, n: u64) -> Result<i64, String> {
        self.elements.lock().unwrap_or_else(PoisonError::into_inner).get(&n).copied().ok_or_else(|| "élément inconnu : appelez d'abord lire_page".to_owned())
    }

    /// Clique au centre de l'élément `n`.
    ///
    /// # Errors
    /// Élément inconnu ou invisible, navigateur indisponible.
    pub async fn click(&self, n: u64) -> Result<Value, String> {
        let node = self.node(n)?;
        let _ = self.call("DOM.scrollIntoViewIfNeeded", json!({ "backendNodeId": node })).await;
        let model = self.call("DOM.getBoxModel", json!({ "backendNodeId": node })).await.map_err(|_| "élément invisible".to_owned())?;
        let q: Vec<f64> = model.pointer("/model/content").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_f64).collect();
        if q.len() < 8 {
            return Err("élément invisible".into());
        }
        let (x, y) = ((q[0] + q[2] + q[4] + q[6]) / 4.0, (q[1] + q[3] + q[5] + q[7]) / 4.0);
        for kind in ["mousePressed", "mouseReleased"] {
            self.call("Input.dispatchMouseEvent", json!({ "type": kind, "x": x, "y": y, "button": "left", "clickCount": 1 })).await?;
        }
        Ok(json!({ "clicked": n }))
    }

    /// Tape `text` dans l'élément `n` (puis Entrée si `enter`).
    ///
    /// # Errors
    /// Élément inconnu, texte vide, navigateur indisponible.
    pub async fn type_text(&self, n: u64, text: &str, enter: bool) -> Result<Value, String> {
        let node = self.node(n)?;
        let text: String = text.chars().filter(|c| !c.is_control()).take(MAX_INPUT).collect();
        self.call("DOM.focus", json!({ "backendNodeId": node })).await?;
        if !text.is_empty() {
            self.call("Input.insertText", json!({ "text": text })).await?;
        }
        if enter {
            self.call("Input.dispatchKeyEvent", json!({ "type": "keyDown", "key": "Enter", "code": "Enter", "windowsVirtualKeyCode": 13, "text": "\r" })).await?;
            self.call("Input.dispatchKeyEvent", json!({ "type": "keyUp", "key": "Enter", "code": "Enter", "windowsVirtualKeyCode": 13 })).await?;
        }
        Ok(json!({ "typed": n, "chars": text.chars().count(), "enter": enter }))
    }

    /// Image JPEG de la page, en base64.
    ///
    /// # Errors
    /// Navigateur indisponible.
    pub async fn capture(&self) -> Result<String, String> {
        let r = self.call("Page.captureScreenshot", json!({ "format": "jpeg", "quality": 60 })).await?;
        r["data"].as_str().map(str::to_owned).ok_or_else(|| "capture vide".into())
    }
}
