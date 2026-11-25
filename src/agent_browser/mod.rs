//! Navigateur jetable prêté à un agent (`docs/design/SESSION_AGENT.md`).
//!
//! - [`Chromium`] lance un navigateur dans un **profil vierge** (`<données>/agent-browsers/<demande>/`, `0700`), relié à la
//!   station par un **tube** CDP (`--remote-debugging-pipe`, descripteurs 3 et 4) : aucun port de débogage n'est ouvert, aucun
//!   autre compte de la machine ne peut le piloter.
//! - [`gate`] ouvre la seule entrée vers ce tube, filtrée par [`cdp`].
//! - Abandonner le [`BrowserProc`] tue **le groupe de processus** entier (les processus de rendu avec) et efface le profil.
//!   Au démarrage, [`sweep`] fait de même pour ce qu'un arrêt brutal du démon aurait laissé.

pub mod cdp;
pub mod gate;
pub mod guard;
pub mod mux;
pub mod pilot;
pub mod screen;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

pub use mux::Mux;

/// Sous-dossier des profils jetables, dans le dossier de données de la station.
pub const PROFILES_DIR: &str = "agent-browsers";
/// Messages en transit au plus dans chaque sens.
const QUEUE: usize = 256;

/// Ce qu'il faut pour lancer un navigateur.
#[derive(Debug, Clone)]
pub struct LaunchSpec {
    /// Profil jetable (créé ici, effacé avec le navigateur).
    pub profile: PathBuf,
    /// Page ouverte au lancement.
    pub url: String,
    /// Sans fenêtre (station sans écran).
    pub headless: bool,
}

/// Un navigateur vivant. Sa destruction l'arrête et efface son profil.
pub struct BrowserProc {
    mux: Arc<Mux>,
    exited: watch::Receiver<bool>,
    pgid: Option<i32>,
    profile: Option<PathBuf>,
    tasks: Vec<JoinHandle<()>>,
}

impl BrowserProc {
    /// Les dernières lignes que le navigateur a écrites en s'arrêtant (« Missing X server », bac à sable refusé…) : sans elles,
    /// un navigateur qui ne démarre pas sur un serveur ne laisse aucune trace.
    #[must_use]
    pub fn log_tail(&self) -> String {
        let raw = self.profile.as_ref().and_then(|p| std::fs::read_to_string(p.join("chromium.log")).ok()).unwrap_or_default();
        let lines: Vec<&str> = raw.lines().filter(|l| !l.contains("dbus/bus.cc")).collect();
        lines[lines.len().saturating_sub(4)..].join(" | ").chars().take(400).collect()
    }

    /// Le multiplexeur du tube CDP.
    #[must_use]
    pub fn mux(&self) -> Arc<Mux> {
        self.mux.clone()
    }

    /// Se résout quand le navigateur s'arrête de lui-même (fermé, planté).
    pub async fn exited(&self) {
        let mut rx = self.exited.clone();
        let _ = rx.wait_for(|e| *e).await;
    }
}

impl Drop for BrowserProc {
    fn drop(&mut self) {
        // Pas de signal à un groupe déjà mort : son numéro pourrait avoir été redonné à d'autres processus.
        if let (Some(pgid), false) = (self.pgid, *self.exited.borrow()) {
            if let Some(pid) = rustix::process::Pid::from_raw(pgid) {
                let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
            }
        }
        self.tasks.iter().for_each(JoinHandle::abort);
        if let Some(p) = &self.profile {
            let _ = std::fs::remove_dir_all(p);
        }
    }
}

/// Lanceur de navigateur (Chromium en production, simulé dans les tests).
pub trait Launcher: Send + Sync {
    /// Lance un navigateur selon `spec`.
    ///
    /// # Errors
    /// Profil impossible à créer ou navigateur qui ne démarre pas.
    fn launch(&self, spec: &LaunchSpec) -> Result<BrowserProc, String>;
}

/// Crée `dir` en `0700` (et ses parents), vide.
fn private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    let _ = std::fs::remove_dir_all(dir);
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)
}

/// Chromium (ou Chrome) trouvé sur la station.
pub struct Chromium {
    bin: PathBuf,
}

impl Chromium {
    /// `ASTRO_CHROMIUM` s'il est donné, sinon `chromium`, `chromium-browser`, `google-chrome`, `google-chrome-stable` dans le `PATH`.
    #[must_use]
    pub fn find() -> Option<Self> {
        if let Some(p) = std::env::var_os("ASTRO_CHROMIUM").map(PathBuf::from) {
            return p.is_file().then_some(Self { bin: p });
        }
        let path = std::env::var_os("PATH")?;
        ["chromium", "chromium-browser", "google-chrome", "google-chrome-stable"]
            .iter()
            .flat_map(|name| std::env::split_paths(&path).map(move |d| d.join(name)))
            .find(|p| p.is_file())
            .map(|bin| Self { bin })
    }

    /// Chemin du binaire.
    #[must_use]
    pub fn bin(&self) -> &Path {
        &self.bin
    }
}

impl Launcher for Chromium {
    fn launch(&self, spec: &LaunchSpec) -> Result<BrowserProc, String> {
        private_dir(&spec.profile).map_err(|e| format!("profil jetable impossible à créer : {e}"))?;
        let mut flags = vec![
            "--remote-debugging-pipe".to_owned(),
            format!("--user-data-dir={}", spec.profile.display()),
            "--no-first-run".into(),
            "--no-default-browser-check".into(),
            "--disable-sync".into(),
            "--disable-extensions".into(),
            "--password-store=basic".into(),
        ];
        if spec.headless {
            // Serveur sans écran : pas de GPU, mémoire partagée de /dev/shm souvent minuscule dans les petites machines.
            flags.extend(["--headless=new".into(), "--disable-gpu".into(), "--disable-dev-shm-usage".into()]);
        }
        // Chromium refuse de démarrer en root sans cette option (station installée en root sur un serveur).
        if rustix::process::geteuid().is_root() {
            flags.push("--no-sandbox".into());
        }
        // Le tube CDP est sur les descripteurs 3 (lecture) et 4 (écriture). `sh` y branche l'entrée et la sortie standard puis
        // devient Chromium (`exec`) : aucun code non sûr, et les arguments restent positionnels (pas d'injection).
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.arg("-c")
            .arg(r#"exec "$0" "$@" 3<&0 4>&1 </dev/null >/dev/null"#)
            .arg(&self.bin)
            .args(&flags)
            .arg(&spec.url)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::fs::File::create(spec.profile.join("chromium.log")).map_or_else(|_| std::process::Stdio::null(), std::process::Stdio::from))
            .process_group(0)
            .kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|e| {
            let _ = std::fs::remove_dir_all(&spec.profile);
            format!("navigateur non lancé : {e}")
        })?;
        let pgid = child.id().and_then(|p| i32::try_from(p).ok());
        if let Some(p) = pgid {
            let _ = std::fs::write(spec.profile.join("astroportd.pid"), p.to_string());
        }
        let (Some(mut stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            return Err("tube du navigateur indisponible".into());
        };
        let (to_tx, mut to_rx) = mpsc::channel::<String>(QUEUE);
        let mux = Mux::new(to_tx);
        let (exit_tx, exit_rx) = watch::channel(false);
        let writer = tokio::spawn(async move {
            while let Some(m) = to_rx.recv().await {
                if stdin.write_all(m.as_bytes()).await.is_err() || stdin.write_all(&[0]).await.is_err() {
                    break;
                }
            }
        });
        let m = mux.clone();
        let reader = tokio::spawn(async move {
            let mut r = BufReader::new(stdout);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match r.read_until(0, &mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        if buf.last() == Some(&0) {
                            buf.pop();
                        }
                        m.dispatch(String::from_utf8_lossy(&buf).into_owned());
                    }
                }
            }
        });
        let waiter = tokio::spawn(async move {
            let _ = child.wait().await;
            let _ = exit_tx.send(true);
        });
        Ok(BrowserProc {
            mux,
            exited: exit_rx,
            pgid,
            profile: Some(spec.profile.clone()),
            tasks: vec![writer, reader, waiter],
        })
    }
}

/// Arrête et efface ce qu'un arrêt brutal du démon a laissé dans `dir` (`<données>/agent-browsers`). Un groupe n'est tué que
/// si son processus est encore un navigateur lancé sur ce profil (le numéro a pu être redonné). Rend le nombre de profils effacés.
pub fn sweep(dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    let mut n = 0;
    for e in entries.flatten() {
        let profile = e.path();
        if let Some(pid) = std::fs::read_to_string(profile.join("astroportd.pid")).ok().and_then(|s| s.trim().parse::<i32>().ok()) {
            let ours = std::fs::read(format!("/proc/{pid}/cmdline")).is_ok_and(|c| String::from_utf8_lossy(&c).contains(&*profile.to_string_lossy()));
            if let (true, Some(p)) = (ours, rustix::process::Pid::from_raw(pid)) {
                let _ = rustix::process::kill_process_group(p, rustix::process::Signal::KILL);
            }
        }
        if std::fs::remove_dir_all(&profile).is_ok() {
            n += 1;
        }
    }
    n
}

/// Navigateur simulé pour les tests : renvoie chaque commande en écho, et à `Test.cookieEvent` émet un événement réseau qui porte
/// un cookie. `kill` simule un navigateur qui s'arrête de lui-même.
#[cfg(test)]
pub mod fake {
    use super::*;
    use serde_json::{Value, json};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Lanceur simulé ; compte les navigateurs vivants.
    #[derive(Default)]
    pub struct FakeLauncher {
        /// Navigateurs vivants.
        pub alive: Arc<AtomicUsize>,
        /// Interrupteurs pour simuler un arrêt de lui-même, dans l'ordre des lancements.
        pub kills: std::sync::Mutex<Vec<watch::Sender<bool>>>,
        /// Commandes reçues, tous navigateurs confondus.
        pub seen: Arc<std::sync::Mutex<Vec<Value>>>,
    }

    struct Alive(Arc<AtomicUsize>);
    impl Drop for Alive {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }

    impl Launcher for FakeLauncher {
        fn launch(&self, spec: &LaunchSpec) -> Result<BrowserProc, String> {
            private_dir(&spec.profile).map_err(|e| e.to_string())?;
            self.alive.fetch_add(1, Ordering::SeqCst);
            let alive = Alive(self.alive.clone());
            let (to_tx, mut to_rx) = mpsc::channel::<String>(QUEUE);
            let mux = Mux::new(to_tx);
            let (exit_tx, exit_rx) = watch::channel(false);
            self.kills.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(exit_tx);
            let (m, seen) = (mux.clone(), self.seen.clone());
            let task = tokio::spawn(async move {
                let _alive = alive;
                let mut n = 0;
                while let Some(raw) = to_rx.recv().await {
                    let Ok(v) = serde_json::from_str::<Value>(&raw) else { continue };
                    seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(v.clone());
                    if v["method"] == "Test.cookieEvent" {
                        m.dispatch(json!({ "method": "Network.requestWillBeSentExtraInfo", "params": { "headers": { "Cookie": "li_at=SECRET", "accept": "*/*" } } }).to_string());
                    }
                    let mut result = json!({ "echo": v["method"], "params": v["params"] });
                    match v["method"].as_str().unwrap_or_default() {
                        "Runtime.evaluate" if v["params"]["expression"] == "document.readyState" => result = json!({ "result": { "value": "complete" } }),
                        "Runtime.evaluate" => result = json!({ "result": { "value": { "url": "https://linkedin.com/feed", "title": "Fil", "text": "Bonjour" } } }),
                        "Accessibility.getFullAXTree" => result = json!({ "nodes": [
                            { "role": { "value": "button" }, "name": { "value": "Publier" }, "backendDOMNodeId": 41 },
                            { "role": { "value": "generic" }, "name": { "value": "décor" }, "backendDOMNodeId": 42 },
                            { "role": { "value": "textbox" }, "name": { "value": "" }, "backendDOMNodeId": 43 } ] }),
                        "DOM.getBoxModel" => result = json!({ "model": { "content": [10.0, 20.0, 30.0, 20.0, 30.0, 40.0, 10.0, 40.0] } }),
                        "Page.captureScreenshot" => result = json!({ "data": "SlBFRw==" }),
                        _ => {}
                    }
                    if v["method"] == "Target.getTargets" {
                        result = json!({ "targetInfos": [{ "targetId": "P1", "type": "page", "url": "about:blank" }] });
                    }
                    if v["method"] == "Page.startScreencast" {
                        m.dispatch(json!({ "method": "Page.screencastFrame", "sessionId": v["sessionId"], "params": { "data": "SlBFRw==", "sessionId": 1, "metadata": { "deviceWidth": 800, "deviceHeight": 600 } } }).to_string());
                    }
                    if v["method"] == "Target.attachToTarget" {
                        n += 1;
                        let sid = format!("S{n}");
                        m.dispatch(json!({ "method": "Target.attachedToTarget", "params": { "sessionId": sid, "targetInfo": { "targetId": v["params"]["targetId"], "type": "page" } } }).to_string());
                        result = json!({ "sessionId": sid });
                    }
                    let mut reply = json!({ "id": v["id"], "result": result });
                    if let Some(s) = v.get("sessionId") {
                        reply["sessionId"] = s.clone();
                    }
                    m.dispatch(reply.to_string());
                }
            });
            Ok(BrowserProc { mux, exited: exit_rx, pgid: None, profile: Some(spec.profile.clone()), tasks: vec![task] })
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::panic)]
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use serde_json::{Value, json};
    use std::sync::Mutex;
    use tokio_tungstenite::tungstenite::Message;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("astroportd-agent-browser-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    async fn rig(tag: &str) -> (PathBuf, BrowserProc, gate::Gate, Arc<Mutex<cdp::Stats>>) {
        let root = tmp(tag);
        let launcher = fake::FakeLauncher::default();
        let profile = root.join("p1");
        let b = launcher.launch(&LaunchSpec { profile: profile.clone(), url: "https://linkedin.com".into(), headless: true }).unwrap();
        let stats = Arc::new(Mutex::new(cdp::Stats::default()));
        let policy = Arc::new(cdp::Policy::new("linkedin.com", vec![], profile.join("dl")));
        let g = gate::open(b.mux(), policy, stats.clone()).await.unwrap();
        (root, b, g, stats)
    }

    async fn next_text(ws: &mut tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>) -> Value {
        loop {
            match tokio::time::timeout(std::time::Duration::from_secs(5), ws.next()).await.unwrap() {
                Some(Ok(Message::Text(t))) => return serde_json::from_str(&t).unwrap(),
                Some(Ok(_)) => {}
                other => panic!("connexion close : {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn the_gate_needs_the_token_admits_one_client_and_filters() {
        let (root, b, g, stats) = rig("gate").await;
        let endpoint = g.endpoint();
        assert!(endpoint.starts_with("ws://127.0.0.1:") && endpoint.len() > 64);
        // Mauvais jeton, autre chemin : la poignée de main échoue.
        for bad in [format!("ws://{}/cdp/{}", g.addr(), "0".repeat(64)), format!("ws://{}/json/version", g.addr()), format!("ws://{}/", g.addr())] {
            assert!(tokio_tungstenite::connect_async(&bad).await.is_err(), "{bad}");
        }
        let (mut ws, _) = tokio_tungstenite::connect_async(&endpoint).await.unwrap();
        // Un second client, même avec le jeton, est refermé.
        let (mut second, _) = tokio_tungstenite::connect_async(&endpoint).await.unwrap();
        let closed = tokio::time::timeout(std::time::Duration::from_secs(5), second.next()).await.unwrap();
        assert!(matches!(closed, Some(Ok(Message::Close(_))) | None | Some(Err(_))), "{closed:?}");

        ws.send(Message::text(json!({ "id": 1, "method": "Browser.getVersion" }).to_string())).await.unwrap();
        assert_eq!(next_text(&mut ws).await["result"]["echo"], "Browser.getVersion");
        ws.send(Message::text(json!({ "id": 2, "method": "Storage.getCookies" }).to_string())).await.unwrap();
        let r = next_text(&mut ws).await;
        assert!(r["error"]["message"].as_str().unwrap().contains("refusé"), "{r}");
        ws.send(Message::text(json!({ "id": 3, "method": "Test.cookieEvent" }).to_string())).await.unwrap();
        let ev = next_text(&mut ws).await;
        assert!(!ev.to_string().contains("SECRET") && ev["params"]["headers"]["accept"] == "*/*", "{ev}");
        // Trame géante : la connexion est coupée, pas la station.
        let _ = ws.send(Message::text("x".repeat(gate::MAX_MESSAGE + 1))).await;
        let s = stats.lock().unwrap().clone();
        assert_eq!((s.denied, s.calls.get("Browser").copied()), (1, Some(1)));
        drop((g, b));
        assert!(!root.join("p1").exists(), "profil effacé avec le navigateur");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn dropping_the_gate_cuts_the_agent_at_once() {
        let (root, b, g, _) = rig("cut").await;
        let (mut ws, _) = tokio_tungstenite::connect_async(&g.endpoint()).await.unwrap();
        drop(g);
        let end = tokio::time::timeout(std::time::Duration::from_secs(5), ws.next()).await.unwrap();
        assert!(!matches!(end, Some(Ok(Message::Text(_)))), "{end:?}");
        drop(b);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn sweep_removes_leftover_profiles_without_killing_strangers() {
        let root = tmp("sweep");
        for (name, pid) in [("a", "1"), ("b", "pas-un-nombre")] {
            std::fs::create_dir_all(root.join(name)).unwrap();
            std::fs::write(root.join(name).join("astroportd.pid"), pid).unwrap();
        }
        // pid 1 (init) n'a pas ce profil dans sa ligne de commande : il n'est pas touché.
        assert_eq!(sweep(&root), 2);
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
        assert_eq!(sweep(&root.join("absent")), 0);
        let _ = std::fs::remove_dir_all(root);
    }

    /// Playwright réel derrière la porte : `ASTRO_PLAYWRIGHT_PY=~/.astro/bin/python cargo test -p astroportd --lib playwright_drives -- --ignored`.
    #[tokio::test]
    #[ignore = "demande Chromium et Playwright (Python) sur la machine"]
    async fn playwright_drives_the_lent_browser() {
        let (Some(c), Some(py)) = (Chromium::find(), std::env::var_os("ASTRO_PLAYWRIGHT_PY")) else { panic!("Chromium ou ASTRO_PLAYWRIGHT_PY absent") };
        let root = tmp("playwright");
        let b = c.launch(&LaunchSpec { profile: root.join("p"), url: "about:blank".into(), headless: true }).unwrap();
        let stats = Arc::new(Mutex::new(cdp::Stats::default()));
        let g = gate::open(b.mux(), Arc::new(cdp::Policy::new("example.org", vec![], root.join("p/dl"))), stats.clone()).await.unwrap();
        // La station regarde la page pendant que l'agent travaille : sa session ne doit pas gêner Playwright.
        let scr = screen::Screen::start(b.mux()).await.unwrap();
        let policy = Arc::new(cdp::Policy::new("example.org", vec![], root.join("p/dl")));
        let garde = guard::Guard::start(b.mux(), policy, stats.clone()).await.unwrap();
        garde.arm();
        let mut frames = scr.watch().await.unwrap();
        let first = tokio::time::timeout(std::time::Duration::from_secs(10), frames.wait_for(Option::is_some)).await.unwrap().unwrap().clone().unwrap();
        assert!(first.contains("\"jpeg\""), "image reçue");
        scr.input(&screen::Input::Click { x: 0.5, y: 0.5 }).await.unwrap();
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/browser/agent_cdp.py");
        let out = tokio::process::Command::new(py).arg("-I").arg(script).arg(g.endpoint()).output().await.unwrap();
        let (so, se) = (String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        assert!(out.status.success() && so.contains("AGENT_OK"), "sortie : {so}\nerreurs : {se}");
        let s = stats.lock().unwrap().clone();
        assert!(s.denied >= 3 && s.hosts.contains("evil.example"), "{s:?}");
        drop((garde, scr, g, b));
        let _ = std::fs::remove_dir_all(root);
    }

    /// Outils de navigation MCP sur le vrai Chromium : lire, saisir, cliquer, capturer.
    #[tokio::test]
    #[ignore = "demande Chromium sur la machine"]
    async fn real_chromium_is_driven_by_the_station_tools() {
        let Some(c) = Chromium::find() else { panic!("Chromium introuvable") };
        let root = tmp("pilot");
        let b = c.launch(&LaunchSpec { profile: root.join("p"), url: "about:blank".into(), headless: true }).unwrap();
        let mux = b.mux();
        let targets = mux.call(None, "Target.getTargets", json!({})).await.unwrap();
        let page = targets["targetInfos"].as_array().unwrap().iter().find(|t| t["type"] == "page").unwrap()["targetId"].as_str().unwrap().to_owned();
        let s = mux.attach(&page).await.unwrap();
        let html = r#"document.body.innerHTML = '<label>Nom <input id=q></label><button onclick="document.title = document.getElementById(\'q\').value">Envoyer</button>'"#;
        mux.call(Some(&s), "Runtime.evaluate", json!({ "expression": html })).await.unwrap();
        let pilot = pilot::Pilot::new(mux.clone(), Arc::new(cdp::Policy::new("example.org", vec![], root.join("p/dl"))));
        let read = pilot.read().await.unwrap();
        let find = |role: &str| read["elements"].as_array().unwrap().iter().find(|e| e["role"] == role).and_then(|e| e["n"].as_u64()).unwrap_or_else(|| panic!("{role} absent : {read}"));
        pilot.type_text(find("textbox"), "Fred", false).await.unwrap();
        pilot.click(find("button")).await.unwrap();
        let title = mux.call(Some(&s), "Runtime.evaluate", json!({ "expression": "document.title", "returnByValue": true })).await.unwrap();
        assert_eq!(title["result"]["value"], "Fred");
        assert!(pilot.capture().await.unwrap().len() > 100);
        assert!(pilot.navigate("https://evil.example/").await.is_err());
        drop(b);
        let _ = std::fs::remove_dir_all(root);
    }

    /// Chromium réel : `cargo test -p astroportd agent_browser::tests::real_chromium -- --ignored`.
    #[tokio::test]
    #[ignore = "demande Chromium sur la machine"]
    async fn real_chromium_answers_through_the_pipe_and_dies_with_its_group() {
        let Some(c) = Chromium::find() else { panic!("Chromium introuvable") };
        let root = tmp("real");
        let b = c.launch(&LaunchSpec { profile: root.join("p"), url: "about:blank".into(), headless: true }).unwrap();
        let stats = Arc::new(Mutex::new(cdp::Stats::default()));
        let g = gate::open(b.mux(), Arc::new(cdp::Policy::new("example.org", vec![], root.join("p/dl"))), stats).await.unwrap();
        let (mut ws, _) = tokio_tungstenite::connect_async(&g.endpoint()).await.unwrap();
        ws.send(Message::text(json!({ "id": 1, "method": "Browser.getVersion" }).to_string())).await.unwrap();
        let v = next_text(&mut ws).await;
        assert!(v["result"]["product"].as_str().unwrap_or_default().contains("Chrome"), "{v}");
        let pid = std::fs::read_to_string(root.join("p/astroportd.pid")).unwrap();
        drop((g, b));
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        assert!(!Path::new(&format!("/proc/{}", pid.trim())).exists() || std::fs::read_to_string(format!("/proc/{}/stat", pid.trim())).unwrap_or_default().contains(") Z "), "navigateur arrêté");
        assert!(!root.join("p").exists());
        let _ = std::fs::remove_dir_all(root);
    }
}
