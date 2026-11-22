//! Écran du navigateur prêté, diffusé au membre dans l'application (lot 2 de `docs/design/SESSION_AGENT.md`).
//!
//! La station ouvre **sa** session sur la page (par le multiplexeur : l'agent ne la voit pas), la diffuse en JPEG
//! (`Page.startScreencast`) **seulement tant que quelqu'un regarde**, et rejoue les gestes du membre (`Input.*`). Le membre
//! se connecte ainsi au site depuis son téléphone, sur une station sans écran, puis regarde l'agent travailler.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::{Value, json};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use super::mux::Mux;

/// Largeur et hauteur maximales d'une image diffusée.
const MAX_SIDE: u32 = 1280;
/// Texte tapé d'un coup au plus.
const MAX_TEXT: usize = 1000;

/// Une image : JSON prêt à envoyer (`{ seq, w, h, jpeg }`, `w`/`h` en pixels CSS de la page).
pub type Frame = Arc<str>;

/// Un geste du membre ; `x`, `y` en fraction (0 à 1) de l'image affichée.
#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    /// Clic gauche.
    Click {
        /// Abscisse (0 à 1).
        x: f64,
        /// Ordonnée (0 à 1).
        y: f64,
    },
    /// Molette, `dy` en pixels.
    Scroll {
        /// Abscisse (0 à 1).
        x: f64,
        /// Ordonnée (0 à 1).
        y: f64,
        /// Défilement vertical.
        dy: f64,
    },
    /// Touche spéciale (Enter, Backspace, Tab, Escape, flèches, Delete).
    Key(String),
    /// Texte tapé.
    Text(String),
}

impl Input {
    /// Lit un geste envoyé par l'application.
    ///
    /// # Errors
    /// Geste inconnu ou valeurs hors bornes.
    pub fn parse(v: &Value) -> Result<Self, String> {
        let frac = |k: &str| v.get(k).and_then(Value::as_f64).filter(|f| (0.0..=1.0).contains(f)).ok_or_else(|| format!("{k} attendu entre 0 et 1"));
        match v.get("type").and_then(Value::as_str).unwrap_or_default() {
            "click" => Ok(Self::Click { x: frac("x")?, y: frac("y")? }),
            "scroll" => Ok(Self::Scroll { x: frac("x")?, y: frac("y")?, dy: v.get("dy").and_then(Value::as_f64).unwrap_or(0.0).clamp(-5000.0, 5000.0) }),
            "key" => {
                let k = v.get("key").and_then(Value::as_str).unwrap_or_default();
                key_code(k).map(|_| Self::Key(k.to_owned())).ok_or_else(|| "touche non prise en charge".to_owned())
            }
            "text" => {
                let t: String = v.get("text").and_then(Value::as_str).unwrap_or_default().chars().filter(|c| !c.is_control()).take(MAX_TEXT).collect();
                if t.is_empty() { Err("texte vide".into()) } else { Ok(Self::Text(t)) }
            }
            _ => Err("geste inconnu".into()),
        }
    }
}

/// Code clavier Windows d'une touche spéciale (ce qu'attend `Input.dispatchKeyEvent`).
fn key_code(key: &str) -> Option<(i64, &'static str)> {
    Some(match key {
        "Enter" => (13, "Enter"),
        "Backspace" => (8, "Backspace"),
        "Tab" => (9, "Tab"),
        "Escape" => (27, "Escape"),
        "Delete" => (46, "Delete"),
        "ArrowLeft" => (37, "ArrowLeft"),
        "ArrowUp" => (38, "ArrowUp"),
        "ArrowRight" => (39, "ArrowRight"),
        "ArrowDown" => (40, "ArrowDown"),
        _ => return None,
    })
}

#[derive(Default)]
struct Attached {
    target: String,
    session: String,
    /// Taille de la page en pixels CSS (d'après la dernière image).
    size: (f64, f64),
}

/// Écran d'un navigateur ; la diffusion s'arrête à la destruction.
pub struct Screen {
    mux: Arc<Mux>,
    on: Mutex<Attached>,
    frames: watch::Sender<Option<Frame>>,
    casting: AtomicBool,
    seq: AtomicU64,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl Drop for Screen {
    fn drop(&mut self) {
        if let Some(t) = self.task.lock().unwrap_or_else(PoisonError::into_inner).take() {
            t.abort();
        }
    }
}

impl Screen {
    /// Écran du navigateur derrière `mux` : découverte des cibles allumée, session ouverte sur la première page.
    ///
    /// # Errors
    /// Navigateur sans page ou injoignable.
    pub async fn start(mux: Arc<Mux>) -> Result<Arc<Self>, String> {
        mux.call(None, "Target.setDiscoverTargets", json!({ "discover": true })).await?;
        let (frames, _) = watch::channel(None);
        let screen = Arc::new(Self { mux: mux.clone(), on: Mutex::default(), frames, casting: AtomicBool::new(false), seq: AtomicU64::new(0), task: Mutex::new(None) });
        screen.attach_first_page().await?;
        let weak = Arc::downgrade(&screen);
        let mut events = mux.events();
        let task = tokio::spawn(async move {
            loop {
                let ev = match events.recv().await {
                    Ok(ev) => ev,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => return,
                };
                let Some(s) = weak.upgrade() else { return };
                s.on_event(&ev).await;
            }
        });
        *screen.task.lock().unwrap_or_else(PoisonError::into_inner) = Some(task);
        Ok(screen)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Attached> {
        self.on.lock().unwrap_or_else(PoisonError::into_inner)
    }

    async fn attach_first_page(&self) -> Result<(), String> {
        let targets = self.mux.call(None, "Target.getTargets", json!({})).await?;
        let page = targets["targetInfos"].as_array().into_iter().flatten().find(|t| t["type"] == "page").and_then(|t| t["targetId"].as_str()).map(str::to_owned).ok_or("aucune page ouverte")?;
        let session = self.mux.attach(&page).await?;
        *self.lock() = Attached { target: page, session, size: (0.0, 0.0) };
        self.casting.store(false, Ordering::SeqCst);
        if self.frames.receiver_count() > 0 {
            self.cast().await?;
        }
        Ok(())
    }

    async fn cast(&self) -> Result<(), String> {
        if self.casting.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let session = self.lock().session.clone();
        let r = self.mux.call(Some(&session), "Page.startScreencast", json!({ "format": "jpeg", "quality": 60, "maxWidth": MAX_SIDE, "maxHeight": MAX_SIDE })).await;
        if r.is_err() {
            self.casting.store(false, Ordering::SeqCst);
        }
        r.map(|_| ())
    }

    async fn on_event(&self, ev: &Value) {
        let method = ev["method"].as_str().unwrap_or_default();
        let (target, session) = {
            let a = self.lock();
            (a.target.clone(), a.session.clone())
        };
        match method {
            "Page.screencastFrame" if ev["sessionId"] == session.as_str() => {
                let p = &ev["params"];
                let _ = self.mux.call(Some(&session), "Page.screencastFrameAck", json!({ "sessionId": p["sessionId"] })).await;
                let (w, h) = (p["metadata"]["deviceWidth"].as_f64().unwrap_or(0.0), p["metadata"]["deviceHeight"].as_f64().unwrap_or(0.0));
                self.lock().size = (w, h);
                let seq = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
                let frame: Frame = json!({ "seq": seq, "w": w, "h": h, "jpeg": p["data"] }).to_string().into();
                self.frames.send_replace(Some(frame));
                // Plus personne ne regarde : la station cesse d'encoder.
                if self.frames.receiver_count() == 0 && self.casting.swap(false, Ordering::SeqCst) {
                    let _ = self.mux.call(Some(&session), "Page.stopScreencast", json!({})).await;
                }
            }
            // La page suivie est fermée (par l'agent ou par le site) : l'écran passe à la page suivante.
            "Target.targetDestroyed" if ev["params"]["targetId"] == target.as_str() => {
                let _ = self.attach_first_page().await;
            }
            "Target.detachedFromTarget" if ev["params"]["sessionId"] == session.as_str() => {
                let _ = self.attach_first_page().await;
            }
            _ => {}
        }
    }

    /// Regarder l'écran : la diffusion démarre si besoin ; elle s'arrête quand plus aucun récepteur ne vit.
    ///
    /// # Errors
    /// Le navigateur refuse la diffusion.
    pub async fn watch(&self) -> Result<watch::Receiver<Option<Frame>>, String> {
        let rx = self.frames.subscribe();
        self.cast().await?;
        Ok(rx)
    }

    /// Rejoue un geste du membre sur la page.
    ///
    /// # Errors
    /// Pas encore d'image (taille inconnue) ou navigateur indisponible.
    pub async fn input(&self, input: &Input) -> Result<(), String> {
        let (session, (w, h)) = {
            let a = self.lock();
            (a.session.clone(), a.size)
        };
        let at = |x: f64, y: f64| -> Result<(f64, f64), String> {
            if w <= 0.0 || h <= 0.0 { Err("l'écran n'est pas encore affiché".into()) } else { Ok(((x * w).round(), (y * h).round())) }
        };
        let s = Some(session.as_str());
        match input {
            Input::Click { x, y } => {
                let (px, py) = at(*x, *y)?;
                for kind in ["mousePressed", "mouseReleased"] {
                    self.mux.call(s, "Input.dispatchMouseEvent", json!({ "type": kind, "x": px, "y": py, "button": "left", "clickCount": 1 })).await?;
                }
            }
            Input::Scroll { x, y, dy } => {
                let (px, py) = at(*x, *y)?;
                self.mux.call(s, "Input.dispatchMouseEvent", json!({ "type": "mouseWheel", "x": px, "y": py, "deltaX": 0, "deltaY": dy })).await?;
            }
            Input::Key(k) => {
                let (code, name) = key_code(k).ok_or("touche non prise en charge")?;
                let text = if name == "Enter" { "\r" } else { "" };
                self.mux.call(s, "Input.dispatchKeyEvent", json!({ "type": "keyDown", "key": name, "code": name, "windowsVirtualKeyCode": code, "text": text })).await?;
                self.mux.call(s, "Input.dispatchKeyEvent", json!({ "type": "keyUp", "key": name, "code": name, "windowsVirtualKeyCode": code })).await?;
            }
            Input::Text(t) => {
                self.mux.call(s, "Input.insertText", json!({ "text": t })).await?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gestures_are_bounded() {
        assert_eq!(Input::parse(&json!({ "type": "click", "x": 0.5, "y": 1 })).unwrap(), Input::Click { x: 0.5, y: 1.0 });
        assert!(matches!(Input::parse(&json!({ "type": "scroll", "x": 0.1, "y": 0.1, "dy": 1e9 })).unwrap(), Input::Scroll { dy, .. } if (dy - 5000.0).abs() < f64::EPSILON));
        assert_eq!(Input::parse(&json!({ "type": "text", "text": "a\u{0}b\n" })).unwrap(), Input::Text("ab".into()));
        assert_eq!(Input::parse(&json!({ "type": "text", "text": "é".repeat(5000) })).unwrap(), Input::Text("é".repeat(MAX_TEXT)));
        for bad in [json!({ "type": "click", "x": 2, "y": 0 }), json!({ "type": "click", "x": -0.1, "y": 0 }), json!({ "type": "key", "key": "F12" }), json!({ "type": "text", "text": "" }), json!({ "type": "eval" }), json!(null)] {
            assert!(Input::parse(&bad).is_err(), "{bad}");
        }
    }
}
