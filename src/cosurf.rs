//! Naviguer ensemble (co-surfing) : un membre se fait aider par un ami sur un site web (lot 6 de `docs/design/SESSION_AGENT.md`).
//!
//! La station du membre ouvre un navigateur **jetable** ([`crate::agent_browser`]) à l'adresse choisie et le montre, dans
//! l'application, au membre et aux amis qu'il invite (participants d'une conversation). Chaque invité a un niveau, que le
//! membre change à tout moment : **regarder**, **montrer** (son pointeur s'affiche chez tout le monde, sans cliquer) ou
//! **prendre la main** (clics et frappe, mots de passe compris : la personne aidée peut ne pas pouvoir taper elle-même). Un seul
//! invité a la main à la fois ; le membre la garde toujours.
//!
//! - Un ami hébergé **sur une autre station** de la constellation passe par la sienne, qui relaie par le maillage (service
//!   [`SERVICE`]) : chaque demande porte un événement NIP-98 (kind 27235) signé de **sa** clé, que la station du membre vérifie
//!   (signature, fraîcheur, usage unique, invité de cette session). Une station ne peut donc pas se faire passer pour un ami.
//! - Le navigateur ne va jamais sur le réseau local (règles ouvertes de [`cdp::Policy::open`], garde armée dès le départ).
//! - Une session dure au plus [`SESSION_SECS`] ; à l'échéance ou quand le membre termine, le navigateur est tué et son profil
//!   effacé. Rien n'en est gardé, sauf le journal `audit.cosurf` (ouverture, niveaux, fin).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Duration;

use astro_core::{ActivityBus, Audience};
use astro_crypto::KeySource;
use astro_db::Db;
use astro_nostr_proto::{Event, UnsignedEvent, kind};
use astro_p2p::tunnel::protocol_name;
use astro_p2p::{P2pHandle, PeerId};
use futures_util::StreamExt;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_util::compat::FuturesAsyncReadCompatExt;

use crate::agent_browser::guard::Guard;
use crate::agent_browser::screen::{Input, Screen};
use crate::agent_browser::{LaunchSpec, Launcher, cdp};

/// Service P2P du co-surfing (`/x/cosurf-<PeerID>`).
pub const SERVICE: &str = "cosurf";
/// Durée d'une navigation partagée.
pub const SESSION_SECS: u64 = 3600;
/// Invités au plus.
const MAX_GUESTS: usize = 8;
/// Fraîcheur exigée d'une demande signée relayée.
const FRESH_SECS: u64 = 60;
/// Taille maximale d'une demande relayée.
const REMOTE_MAX: u64 = 64 << 10;
/// Images par seconde au plus vers un invité.
const FRAME_GAP: Duration = Duration::from_millis(120);
/// Sous-dossier des profils jetables.
pub const PROFILES_DIR: &str = "cosurf-browsers";

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn locked<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Échec d'une opération de co-surfing.
#[derive(Debug, PartialEq, Eq)]
pub enum CoSurfError {
    /// Saisie refusée.
    Invalid(String),
    /// Session inconnue, ou pas pour ce compte.
    NotFound,
    /// Le niveau de l'invité ne le permet pas.
    Forbidden(String),
    /// Panne de la station.
    Internal(String),
}

impl std::fmt::Display for CoSurfError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(m) | Self::Forbidden(m) | Self::Internal(m) => f.write_str(m),
            Self::NotFound => f.write_str("navigation partagée introuvable ou terminée"),
        }
    }
}

/// Ce qu'un invité peut faire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// Voir la page.
    Watch,
    /// Voir, et montrer (pointeur affiché chez tous).
    Point,
    /// Cliquer et taper (un seul invité à la fois).
    Drive,
}

impl Level {
    fn parse(s: &str) -> Result<Self, CoSurfError> {
        match s {
            "watch" => Ok(Self::Watch),
            "point" => Ok(Self::Point),
            "drive" => Ok(Self::Drive),
            _ => Err(CoSurfError::Invalid("niveau attendu : watch, point ou drive".into())),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Watch => "watch",
            Self::Point => "point",
            Self::Drive => "drive",
        }
    }
}

#[derive(Debug, Clone)]
struct Guest {
    hex: String,
    label: String,
    level: Level,
}

/// Une navigation partagée (le navigateur et la garde appartiennent à sa tâche de surveillance).
struct Session {
    id: String,
    owner: String,
    owner_label: String,
    url: String,
    until: u64,
    guests: Mutex<Vec<Guest>>,
    screen: Arc<Screen>,
    pointers: watch::Sender<Option<Arc<str>>>,
    stop: Mutex<Option<oneshot::Sender<&'static str>>>,
}

/// Rôle d'un compte dans une session.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Role {
    Owner,
    Guest(Level, String),
}

impl Session {
    fn role(&self, hex: &str) -> Option<Role> {
        if hex == self.owner {
            return Some(Role::Owner);
        }
        locked(&self.guests).iter().find(|g| g.hex == hex).map(|g| Role::Guest(g.level, g.label.clone()))
    }

    fn view(&self, viewer: &str, here: Option<&str>) -> Value {
        let mine = viewer == self.owner;
        let level = match self.role(viewer) {
            Some(Role::Guest(l, _)) => Some(l.as_str()),
            _ => None,
        };
        let guests: Vec<Value> = locked(&self.guests).iter().map(|g| json!({ "hex": g.hex, "label": g.label, "level": g.level.as_str() })).collect();
        json!({ "id": self.id, "owner": self.owner, "owner_label": self.owner_label, "mine": mine, "level": level, "url": self.url, "until": self.until, "guests": guests, "station": here })
    }
}

type Sessions = Arc<Mutex<HashMap<String, Arc<Session>>>>;

/// Ce qu'un spectateur reçoit : `("frame" | "pointer" | "level", JSON)`.
pub type Feed = mpsc::Receiver<(String, Arc<str>)>;

/// Le service de co-surfing d'une station.
pub struct CoSurf {
    db: Arc<Db>,
    keys: Arc<dyn KeySource>,
    browser: Option<(Arc<dyn Launcher>, PathBuf, bool)>,
    p2p: Option<P2pHandle>,
    activity: Option<ActivityBus>,
    sessions: Sessions,
    seen: Mutex<HashMap<String, u64>>,
}

/// Une adresse web publique ; `exemple.org` devient `https://exemple.org/`.
///
/// # Errors
/// Adresse qui n'est pas celle d'un site public (réseau local, adresse IP, autre protocole).
pub fn public_url(raw: &str) -> Result<String, CoSurfError> {
    let raw = raw.trim();
    let with = if raw.contains("://") { raw.to_owned() } else { format!("https://{raw}") };
    let u = url::Url::parse(&with).map_err(|_| CoSurfError::Invalid("adresse de site attendue (exemple : impots.gouv.fr)".into()))?;
    if !cdp::Policy::open(PathBuf::new()).allows_url(u.as_str()) || u.as_str() == "about:blank" {
        return Err(CoSurfError::Invalid("seuls les sites publics s'ouvrent ici (jamais le réseau local de la station)".into()));
    }
    Ok(u.to_string())
}

impl CoSurf {
    /// Service sur `db` ; `keys` signe les demandes relayées au nom des comptes hébergés ici.
    #[must_use]
    pub fn new(db: Arc<Db>, keys: Arc<dyn KeySource>) -> Self {
        Self { db, keys, browser: None, p2p: None, activity: None, sessions: Arc::default(), seen: Mutex::default() }
    }

    /// Navigateur de la station : profils jetables dans `dir` ; `screen` : ouvrir aussi une fenêtre sur l'écran de la station.
    #[must_use]
    pub fn with_browser(mut self, launcher: Arc<dyn Launcher>, dir: PathBuf, screen: bool) -> Self {
        self.browser = Some((launcher, dir, screen));
        self
    }

    /// Amis hébergés ailleurs dans la constellation (relais par le maillage).
    #[must_use]
    pub fn with_p2p(mut self, handle: P2pHandle) -> Self {
        self.p2p = Some(handle);
        self
    }

    /// Prévient les comptes hébergés ici (invitations, niveau changé, ami arrivé).
    #[must_use]
    pub fn with_activity(mut self, bus: ActivityBus) -> Self {
        self.activity = Some(bus);
        self
    }

    fn here(&self) -> Option<String> {
        self.station()
    }

    /// PeerID de cette station (pour les liens d'invitation : l'ami d'une autre station passe par la sienne).
    #[must_use]
    pub fn station(&self) -> Option<String> {
        self.p2p.as_ref().map(|h| h.peer_id().to_string())
    }

    fn say(&self, to: &str, message: String, data: Value) {
        if let Some(bus) = &self.activity {
            bus.publish_to(Audience::Owner(to.to_owned()), "cosurf", "cosurf", message, Some(data));
        }
    }

    async fn audit(&self, data: Value) {
        let _ = self.db.writer().execute("INSERT INTO journal(stream, at, data) VALUES ('audit.cosurf', ?1, ?2)", vec![i64::try_from(now()).unwrap_or(0).into(), data.to_string().into()]).await;
    }

    fn get(&self, id: &str) -> Result<Arc<Session>, CoSurfError> {
        locked(&self.sessions).get(id).cloned().ok_or(CoSurfError::NotFound)
    }

    fn with_role(&self, id: &str, hex: &str) -> Result<(Arc<Session>, Role), CoSurfError> {
        let s = self.get(id)?;
        let role = s.role(hex).ok_or(CoSurfError::NotFound)?;
        Ok((s, role))
    }

    /// Ouvre une navigation partagée sur `url` avec `guests` (`(clé, nom)`), chacun au niveau `level`.
    ///
    /// # Errors
    /// Adresse ou invités invalides, déjà une navigation en cours, navigateur absent ou qui ne démarre pas.
    pub async fn create(&self, owner: &str, owner_label: &str, url: &str, guests: &[(String, String)], level: &str) -> Result<Value, CoSurfError> {
        let url = public_url(url)?;
        let level = Level::parse(level)?;
        let (launcher, dir, screen) = self.browser.as_ref().ok_or_else(|| CoSurfError::Invalid("cette station n'a pas de navigateur à partager (Chromium absent)".into()))?;
        let mut list: Vec<Guest> = Vec::new();
        for (hex, label) in guests {
            let hex = hex.trim().to_ascii_lowercase();
            if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) || hex == owner {
                return Err(CoSurfError::Invalid("invité inconnu".into()));
            }
            if !list.iter().any(|g| g.hex == hex) {
                let label: String = label.trim().chars().filter(|c| !c.is_control()).take(60).collect();
                list.push(Guest { label: if label.is_empty() { format!("…{}", &hex[56..]) } else { label }, hex, level: if level == Level::Drive { Level::Point } else { level } });
            }
        }
        if list.is_empty() || list.len() > MAX_GUESTS {
            return Err(CoSurfError::Invalid(format!("de 1 à {MAX_GUESTS} amis à inviter")));
        }
        // « Prendre la main » : un seul invité à la fois, le premier.
        if level == Level::Drive {
            list[0].level = Level::Drive;
        }
        if locked(&self.sessions).values().any(|s| s.owner == owner) {
            return Err(CoSurfError::Invalid("une navigation partagée est déjà en cours : terminez-la d'abord".into()));
        }
        let mut raw = [0u8; 8];
        getrandom::fill(&mut raw).map_err(|e| CoSurfError::Internal(format!("aléa du système indisponible : {e}")))?;
        let id = hex::encode(raw);
        let profile = dir.join(&id);
        let browser = launcher.launch(&LaunchSpec { profile: profile.clone(), url: url.clone(), headless: !*screen }).map_err(CoSurfError::Internal)?;
        let screen = Screen::start(browser.mux()).await.map_err(|e| CoSurfError::Internal(format!("écran du navigateur indisponible : {e}")))?;
        let stats = Arc::new(Mutex::new(cdp::Stats::default()));
        let guard = Guard::start(browser.mux(), Arc::new(cdp::Policy::open(profile.join("Téléchargements"))), stats.clone()).await.map_err(|e| CoSurfError::Internal(format!("garde de navigation indisponible : {e}")))?;
        guard.arm();
        let (stop_tx, stop_rx) = oneshot::channel();
        let until = now() + SESSION_SECS;
        let (pointers, _) = watch::channel(None);
        let session = Arc::new(Session { id: id.clone(), owner: owner.to_owned(), owner_label: owner_label.to_owned(), url: url.clone(), until, guests: Mutex::new(list.clone()), screen, pointers, stop: Mutex::new(Some(stop_tx)) });
        locked(&self.sessions).insert(id.clone(), session.clone());
        // Surveillance : échéance, fin demandée ou navigateur arrêté → tout est détruit.
        let (sessions, db, bus, sid, who) = (self.sessions.clone(), self.db.clone(), self.activity.clone(), id.clone(), owner.to_owned());
        tokio::spawn(async move {
            let reason = tokio::select! {
                () = browser.exited() => "ended",
                () = tokio::time::sleep(Duration::from_secs(SESSION_SECS)) => "expired",
                r = stop_rx => r.unwrap_or("ended"),
            };
            let gone = locked(&sessions).remove(&sid);
            drop((guard, browser));
            let denied = locked(&stats).denied;
            let _ = db.writer().execute("INSERT INTO journal(stream, at, data) VALUES ('audit.cosurf', ?1, ?2)", vec![i64::try_from(now()).unwrap_or(0).into(), json!({ "event": "end", "hex": who, "session": sid, "reason": reason, "blocked": denied }).to_string().into()]).await;
            if let (Some(bus), Some(s)) = (bus, gone) {
                for g in locked(&s.guests).iter() {
                    bus.publish_to(Audience::Owner(g.hex.clone()), "cosurf", "cosurf", format!("La navigation partagée de {} est terminée", s.owner_label), Some(json!({ "cosurf": sid, "ended": true })));
                }
            }
        });
        for g in &list {
            self.say(&g.hex, format!("{owner_label} vous invite à naviguer ensemble"), json!({ "cosurf": id, "station": self.here() }));
        }
        self.audit(json!({ "event": "open", "hex": owner, "session": id, "guests": list.iter().map(|g| (&g.hex, g.level.as_str())).collect::<Vec<_>>(), "host": url::Url::parse(&url).ok().and_then(|u| u.host_str().map(str::to_owned)) })).await;
        Ok(session.view(owner, self.here().as_deref()))
    }

    /// Navigations partagées du compte (les siennes et celles où il est invité, sur cette station).
    #[must_use]
    pub fn list(&self, hex: &str) -> Value {
        let here = self.here();
        let rows: Vec<Value> = locked(&self.sessions).values().filter(|s| s.role(hex).is_some()).map(|s| s.view(hex, here.as_deref())).collect();
        json!({ "sessions": rows, "station": here, "browser": self.browser.is_some() })
    }

    /// Une session vue par `hex` (propriétaire ou invité).
    ///
    /// # Errors
    /// Session inconnue ou pas pour ce compte.
    pub fn view(&self, hex: &str, id: &str) -> Result<Value, CoSurfError> {
        let (s, _) = self.with_role(id, hex)?;
        Ok(s.view(hex, self.here().as_deref()))
    }

    /// Le propriétaire change le niveau d'un invité ; donner la main la retire à l'invité qui l'avait.
    ///
    /// # Errors
    /// Pas le propriétaire, invité inconnu, niveau invalide.
    pub async fn set_level(&self, owner: &str, id: &str, guest: &str, level: &str) -> Result<Value, CoSurfError> {
        let level = Level::parse(level)?;
        let s = self.get(id)?;
        if s.owner != owner {
            return Err(CoSurfError::NotFound);
        }
        {
            let mut gs = locked(&s.guests);
            if !gs.iter().any(|g| g.hex == guest) {
                return Err(CoSurfError::NotFound);
            }
            for g in gs.iter_mut() {
                if g.hex == guest {
                    g.level = level;
                } else if level == Level::Drive && g.level == Level::Drive {
                    g.level = Level::Point;
                }
            }
        }
        let what = match level {
            Level::Watch => "Vous pouvez regarder",
            Level::Point => "Vous pouvez montrer avec votre pointeur",
            Level::Drive => "Vous avez la main : vous pouvez cliquer et taper",
        };
        self.say(guest, format!("{what} ({})", s.owner_label), json!({ "cosurf": id, "level": level.as_str() }));
        self.audit(json!({ "event": "level", "hex": owner, "session": id, "guest": guest, "level": level.as_str() })).await;
        Ok(s.view(owner, self.here().as_deref()))
    }

    /// Le propriétaire termine (navigateur détruit) ; un invité s'en va (il est retiré de la session).
    ///
    /// # Errors
    /// Session inconnue ou pas pour ce compte.
    pub async fn stop(&self, hex: &str, id: &str) -> Result<Value, CoSurfError> {
        let (s, role) = self.with_role(id, hex)?;
        if role == Role::Owner {
            if let Some(tx) = locked(&s.stop).take() {
                let _ = tx.send("closed");
            }
            for _ in 0..50 {
                if !locked(&self.sessions).contains_key(id) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            return Ok(json!({ "id": id, "ended": true }));
        }
        locked(&s.guests).retain(|g| g.hex != hex);
        if let Role::Guest(_, label) = role {
            self.say(&s.owner, format!("{label} a quitté la navigation partagée"), json!({ "cosurf": id }));
        }
        Ok(json!({ "id": id, "left": true }))
    }

    /// Regarder la session : images de la page, pointeurs, et fin. Le flux se ferme si l'invité est retiré ou quand la session
    /// finit.
    ///
    /// # Errors
    /// Session inconnue ou pas pour ce compte ; écran indisponible.
    pub async fn watch(&self, hex: &str, id: &str) -> Result<Feed, CoSurfError> {
        let (s, role) = self.with_role(id, hex)?;
        let mut frames = s.screen.watch().await.map_err(CoSurfError::Internal)?;
        let mut pointers = s.pointers.subscribe();
        if let Role::Guest(_, label) = &role {
            self.say(&s.owner, format!("{label} vous a rejoint"), json!({ "cosurf": id }));
        }
        let (tx, rx) = mpsc::channel(4);
        let (weak, who): (Weak<Session>, String) = (Arc::downgrade(&s), hex.to_owned());
        drop(s);
        tokio::spawn(async move {
            let mut check = tokio::time::interval(Duration::from_secs(2));
            let mut last_level = String::new();
            loop {
                tokio::select! {
                    changed = frames.changed() => {
                        if changed.is_err() { break; }
                        let f = frames.borrow_and_update().clone();
                        if let Some(f) = f {
                            if tx.send(("frame".to_owned(), f)).await.is_err() { break; }
                        }
                        tokio::time::sleep(FRAME_GAP).await;
                    }
                    changed = pointers.changed() => {
                        if changed.is_err() { break; }
                        let p = pointers.borrow_and_update().clone();
                        if let Some(p) = p {
                            if tx.send(("pointer".to_owned(), p)).await.is_err() { break; }
                        }
                    }
                    _ = check.tick() => {
                        // Retiré, ou session finie : le flux se ferme. Niveau changé : on le dit.
                        let Some(s) = weak.upgrade() else { break };
                        let level = match s.role(&who) { None => break, Some(Role::Owner) => "owner".to_owned(), Some(Role::Guest(l, _)) => l.as_str().to_owned() };
                        if level != last_level {
                            let data: Arc<str> = json!({ "level": level }).to_string().into();
                            if tx.send(("level".to_owned(), data)).await.is_err() { break; }
                            last_level = level;
                        }
                    }
                }
            }
        });
        Ok(rx)
    }

    /// Un geste dans la page : le propriétaire et l'invité qui a la main agissent ; « montrer » affiche un pointeur.
    ///
    /// # Errors
    /// Niveau insuffisant, geste invalide, session inconnue, navigateur indisponible.
    pub async fn input(&self, hex: &str, id: &str, gesture: &Value) -> Result<Value, CoSurfError> {
        let (s, role) = self.with_role(id, hex)?;
        let label = match &role {
            Role::Owner => s.owner_label.clone(),
            Role::Guest(_, l) => l.clone(),
        };
        if gesture["type"] == "point" {
            if role == Role::Guest(Level::Watch, label.clone()) {
                return Err(CoSurfError::Forbidden("vous pouvez seulement regarder".into()));
            }
            let frac = |k: &str| gesture[k].as_f64().filter(|f| (0.0..=1.0).contains(f)).ok_or_else(|| CoSurfError::Invalid(format!("{k} attendu entre 0 et 1")));
            let p: Arc<str> = json!({ "who": label, "hex": hex, "x": frac("x")?, "y": frac("y")?, "at": now() }).to_string().into();
            s.pointers.send_replace(Some(p));
            return Ok(json!({ "ok": true }));
        }
        if matches!(role, Role::Guest(Level::Watch | Level::Point, _)) {
            return Err(CoSurfError::Forbidden("demandez la main pour cliquer ou taper".into()));
        }
        let input = Input::parse(gesture).map_err(CoSurfError::Invalid)?;
        s.screen.input(&input).await.map_err(CoSurfError::Invalid)?;
        Ok(json!({ "ok": true }))
    }
}

// ── Amis hébergés ailleurs : relais par le maillage ──

impl CoSurf {
    fn signed(&self, hex: &str, id: &str, op: &str) -> Result<Event, CoSurfError> {
        let secret = self.keys.secret(hex).ok_or_else(|| CoSurfError::Internal("clé du compte absente de cette station".into()))?;
        let mut nonce = [0u8; 8];
        getrandom::fill(&mut nonce).map_err(|e| CoSurfError::Internal(e.to_string()))?;
        UnsignedEvent { created_at: now(), kind: kind::HTTP_AUTH, tags: vec![vec!["u".into(), format!("cosurf:{id}:{op}")], vec!["nonce".into(), hex::encode(nonce)]], content: String::new() }
            .sign(&secret)
            .map_err(|e| CoSurfError::Internal(e.to_string()))
    }

    async fn open(&self, station: &str, hex: &str, id: &str, op: &str, body: &Value) -> Result<Box<dyn tokio::io::AsyncBufRead + Unpin + Send>, CoSurfError> {
        let handle = self.p2p.as_ref().ok_or_else(|| CoSurfError::Invalid("maillage inactif sur cette station".into()))?;
        let peer: PeerId = station.parse().map_err(|_| CoSurfError::Invalid("station inconnue".into()))?;
        let protocol = protocol_name(SERVICE, &peer).map_err(|e| CoSurfError::Internal(e.to_string()))?;
        let ev = self.signed(hex, id, op)?;
        let mut stream = tokio::time::timeout(Duration::from_secs(15), handle.control().open_stream(peer, protocol))
            .await
            .map_err(|_| CoSurfError::Internal("la station de votre ami ne répond pas".into()))?
            .map_err(|e| CoSurfError::Internal(format!("station de votre ami injoignable : {e}")))?
            .compat();
        let mut line = json!({ "op": op, "id": id, "ev": ev, "body": body }).to_string();
        line.push('\n');
        stream.write_all(line.as_bytes()).await.map_err(|e| CoSurfError::Internal(e.to_string()))?;
        stream.flush().await.map_err(|e| CoSurfError::Internal(e.to_string()))?;
        Ok(Box::new(tokio::io::BufReader::new(stream)))
    }

    /// Une demande (`view`, `input`, `stop`) à une session tenue par la station `station`, au nom de `hex`.
    ///
    /// # Errors
    /// Station injoignable, ou refus de la station de la session.
    pub async fn remote(&self, station: &str, hex: &str, id: &str, op: &str, body: &Value) -> Result<Value, CoSurfError> {
        let mut r = self.open(station, hex, id, op, body).await?;
        let mut buf = Vec::new();
        tokio::time::timeout(Duration::from_secs(20), (&mut r).take(REMOTE_MAX * 16).read_until(b'\n', &mut buf))
            .await
            .map_err(|_| CoSurfError::Internal("la station de votre ami ne répond pas".into()))?
            .map_err(|e| CoSurfError::Internal(e.to_string()))?;
        let v: Value = serde_json::from_slice(&buf).map_err(|_| CoSurfError::Internal("réponse illisible".into()))?;
        match (v.get("ok"), v["err"].as_str(), v["code"].as_str()) {
            (Some(ok), _, _) => Ok(ok.clone()),
            (_, Some(e), Some("forbidden")) => Err(CoSurfError::Forbidden(e.to_owned())),
            (_, _, Some("not_found")) => Err(CoSurfError::NotFound),
            (_, Some(e), _) => Err(CoSurfError::Invalid(e.to_owned())),
            _ => Err(CoSurfError::Internal("réponse illisible".into())),
        }
    }

    /// Regarder une session tenue par la station `station`.
    ///
    /// # Errors
    /// Station injoignable ou refus.
    pub async fn remote_watch(&self, station: &str, hex: &str, id: &str) -> Result<Feed, CoSurfError> {
        let mut r = self.open(station, hex, id, "watch", &Value::Null).await?;
        let (tx, rx) = mpsc::channel(4);
        // Première ligne : accord ou refus.
        let mut first = Vec::new();
        tokio::time::timeout(Duration::from_secs(20), (&mut r).take(REMOTE_MAX).read_until(b'\n', &mut first)).await.map_err(|_| CoSurfError::Internal("la station de votre ami ne répond pas".into()))?.map_err(|e| CoSurfError::Internal(e.to_string()))?;
        let head: Value = serde_json::from_slice(&first).unwrap_or(Value::Null);
        if head.get("ok").is_none() {
            return Err(if head["code"] == "not_found" { CoSurfError::NotFound } else { CoSurfError::Invalid(head["err"].as_str().unwrap_or("refusé").to_owned()) });
        }
        tokio::spawn(async move {
            loop {
                let mut buf = Vec::new();
                match (&mut r).take(16 << 20).read_until(b'\n', &mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                let Ok(v) = serde_json::from_slice::<Value>(&buf) else { break };
                let (Some(k), Some(d)) = (v["k"].as_str(), v["d"].as_str()) else { break };
                if tx.send((k.to_owned(), Arc::from(d))).await.is_err() {
                    break;
                }
            }
        });
        Ok(rx)
    }

    /// Vérifie une demande relayée : signature, type, fraîcheur, usage unique, usage annoncé. Rend la clé de l'ami.
    fn check(&self, ev: &Value, id: &str, op: &str) -> Result<String, CoSurfError> {
        let ev: Event = serde_json::from_value(ev.clone()).map_err(|_| CoSurfError::NotFound)?;
        let at = now();
        let fresh = ev.created_at + FRESH_SECS >= at && ev.created_at <= at + FRESH_SECS;
        if ev.verify().is_err() || ev.kind != kind::HTTP_AUTH || !fresh || ev.tag_value("u") != Some(&format!("cosurf:{id}:{op}")) {
            return Err(CoSurfError::NotFound);
        }
        let mut seen = locked(&self.seen);
        seen.retain(|_, t| at < *t + 2 * FRESH_SECS);
        if seen.insert(ev.id.clone(), at).is_some() {
            return Err(CoSurfError::NotFound);
        }
        Ok(ev.pubkey)
    }

    /// Sert les amis des autres stations (service [`SERVICE`]).
    pub fn serve_remote(self: &Arc<Self>) -> Option<tokio::task::JoinHandle<()>> {
        let handle = self.p2p.as_ref()?;
        let protocol = protocol_name(SERVICE, &handle.peer_id()).ok()?;
        let mut incoming = match handle.control().accept(protocol) {
            Ok(i) => i,
            Err(e) => {
                eprintln!("co-surfing : relais indisponible : {e}");
                return None;
            }
        };
        let me = Arc::downgrade(self);
        let limit = Arc::new(tokio::sync::Semaphore::new(64));
        Some(tokio::spawn(async move {
            while let Some((_peer, stream)) = incoming.next().await {
                let Ok(permit) = limit.clone().try_acquire_owned() else { continue };
                let Some(me) = me.upgrade() else { return };
                tokio::spawn(async move {
                    let _permit = permit;
                    let mut stream = stream.compat();
                    let mut buf = Vec::new();
                    let read = tokio::time::timeout(Duration::from_secs(20), tokio::io::BufReader::new(&mut stream).take(REMOTE_MAX).read_until(b'\n', &mut buf)).await;
                    if !matches!(read, Ok(Ok(n)) if n > 0) {
                        return;
                    }
                    let msg: Value = serde_json::from_slice(&buf).unwrap_or(Value::Null);
                    let (op, id) = (msg["op"].as_str().unwrap_or_default().to_owned(), msg["id"].as_str().unwrap_or_default().to_owned());
                    let wire = |r: Result<Value, CoSurfError>| match r {
                        Ok(v) => json!({ "ok": v }),
                        Err(CoSurfError::NotFound) => json!({ "err": "introuvable", "code": "not_found" }),
                        Err(CoSurfError::Forbidden(e)) => json!({ "err": e, "code": "forbidden" }),
                        Err(CoSurfError::Internal(e)) => {
                            eprintln!("co-surfing : {e}");
                            json!({ "err": "erreur interne de la station" })
                        }
                        Err(CoSurfError::Invalid(e)) => json!({ "err": e }),
                    };
                    let line = |v: Value| {
                        let mut s = v.to_string();
                        s.push('\n');
                        s
                    };
                    let hex = match me.check(&msg["ev"], &id, &op) {
                        Ok(h) => h,
                        Err(e) => {
                            let _ = stream.write_all(line(wire(Err(e))).as_bytes()).await;
                            return;
                        }
                    };
                    match op.as_str() {
                        "watch" => match me.watch(&hex, &id).await {
                            Err(e) => {
                                let _ = stream.write_all(line(wire(Err(e))).as_bytes()).await;
                            }
                            Ok(mut feed) => {
                                if stream.write_all(line(json!({ "ok": true })).as_bytes()).await.is_err() {
                                    return;
                                }
                                while let Some((k, d)) = feed.recv().await {
                                    if stream.write_all(line(json!({ "k": k, "d": &*d })).as_bytes()).await.is_err() || stream.flush().await.is_err() {
                                        break;
                                    }
                                }
                            }
                        },
                        "view" => {
                            let _ = stream.write_all(line(wire(me.view(&hex, &id))).as_bytes()).await;
                        }
                        "input" => {
                            let r = me.input(&hex, &id, &msg["body"]).await;
                            let _ = stream.write_all(line(wire(r)).as_bytes()).await;
                        }
                        "stop" => {
                            let r = me.stop(&hex, &id).await;
                            let _ = stream.write_all(line(wire(r)).as_bytes()).await;
                        }
                        _ => {
                            let _ = stream.write_all(line(json!({ "err": "opération inconnue" })).as_bytes()).await;
                        }
                    }
                    let _ = stream.flush().await;
                });
            }
        }))
    }

    /// Dossier des profils jetables sous `data_dir`, vidé de ce qu'un arrêt brutal y a laissé.
    #[must_use]
    pub fn profiles_dir(data_dir: &std::path::Path) -> PathBuf {
        let dir = data_dir.join(PROFILES_DIR);
        crate::agent_browser::sweep(&dir);
        dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_browser::fake::FakeLauncher;
    use astro_crypto::StaticKeySource;
    use std::sync::atomic::Ordering;

    const OWNER: [u8; 32] = [11; 32];
    const FRIEND: [u8; 32] = [12; 32];
    const OTHER: [u8; 32] = [13; 32];

    fn hex_of(s: &[u8; 32]) -> String {
        astro_nostr_proto::pubkey_of(s).unwrap()
    }

    fn rig(tag: &str, keys: &[[u8; 32]], browser: bool) -> (PathBuf, CoSurf, Arc<FakeLauncher>) {
        let dir = std::env::temp_dir().join(format!("astroportd-cosurf-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
        let mut ks = StaticKeySource::new();
        for k in keys {
            ks = ks.with(*k);
        }
        let launcher = Arc::new(FakeLauncher::default());
        let mut c = CoSurf::new(db, Arc::new(ks));
        if browser {
            c = c.with_browser(launcher.clone(), dir.join("b"), false);
        }
        (dir, c, launcher)
    }

    async fn first_frame(feed: &mut Feed) -> (String, String) {
        loop {
            let (k, d) = tokio::time::timeout(Duration::from_secs(5), feed.recv()).await.unwrap().unwrap();
            if k == "frame" {
                return (k, d.to_string());
            }
        }
    }

    #[tokio::test]
    async fn only_public_sites_and_real_invitations_open_a_session() {
        let (dir, c, _) = rig("create", &[OWNER], true);
        let (owner, friend) = (hex_of(&OWNER), hex_of(&FRIEND));
        let one = [(friend.clone(), "Paul".to_owned())];
        for bad in ["http://localhost:12345", "http://192.168.1.1", "box.lan", "file:///etc/passwd", "javascript:alert(1)", ""] {
            assert!(matches!(c.create(&owner, "Alice", bad, &one, "point").await, Err(CoSurfError::Invalid(_))), "{bad}");
        }
        assert!(c.create(&owner, "Alice", "impots.gouv.fr", &[], "point").await.is_err(), "au moins un ami");
        assert!(c.create(&owner, "Alice", "impots.gouv.fr", &[(owner.clone(), "moi".into())], "point").await.is_err(), "pas soi-même");
        let many: Vec<_> = (0..9u8).map(|i| (hex::encode([i + 100; 32]), format!("a{i}"))).collect();
        assert!(c.create(&owner, "Alice", "impots.gouv.fr", &many, "point").await.is_err(), "8 amis au plus");
        assert!(c.create(&owner, "Alice", "impots.gouv.fr", &one, "tout").await.is_err(), "niveau inconnu");
        let v = c.create(&owner, "Alice", "impots.gouv.fr", &one, "drive").await.unwrap();
        assert_eq!((v["url"].as_str(), v["guests"][0]["level"].as_str(), v["mine"].as_bool()), (Some("https://impots.gouv.fr/"), Some("drive"), Some(true)));
        assert!(c.create(&owner, "Alice", "example.org", &one, "point").await.is_err(), "une seule à la fois");
        let (_, nob, _) = rig("nobrowser", &[OWNER], false);
        assert!(nob.create(&owner, "Alice", "example.org", &one, "point").await.unwrap_err().to_string().contains("Chromium"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn each_guest_does_what_the_owner_allows_and_only_one_drives() {
        let (dir, c, launcher) = rig("levels", &[OWNER], true);
        let (owner, friend, other) = (hex_of(&OWNER), hex_of(&FRIEND), hex_of(&OTHER));
        let v = c.create(&owner, "Alice", "example.org", &[(friend.clone(), "Paul".into()), (other.clone(), "Léa".into())], "watch").await.unwrap();
        let id = v["id"].as_str().unwrap().to_owned();
        let click = json!({ "type": "click", "x": 0.5, "y": 0.5 });
        let point = json!({ "type": "point", "x": 0.2, "y": 0.3 });
        // Regarder : rien d'autre.
        assert!(matches!(c.input(&friend, &id, &point).await, Err(CoSurfError::Forbidden(_))));
        assert!(matches!(c.input(&friend, &id, &click).await, Err(CoSurfError::Forbidden(_))));
        // La page est vue, même par un invité qui ne fait que regarder ; le propriétaire agit toujours.
        let mut feed = c.watch(&friend, &id).await.unwrap();
        assert!(first_frame(&mut feed).await.1.contains("SlBFRw=="));
        assert_eq!(c.input(&owner, &id, &click).await.unwrap()["ok"], true);
        // Montrer : le pointeur arrive chez tous ; cliquer reste refusé.
        c.set_level(&owner, &id, &friend, "point").await.unwrap();
        c.input(&friend, &id, &point).await.unwrap();
        let mut got_pointer = false;
        for _ in 0..10 {
            let (k, d) = tokio::time::timeout(Duration::from_secs(5), feed.recv()).await.unwrap().unwrap();
            if k == "pointer" {
                assert!(d.contains("Paul") && d.contains("0.2"), "{d}");
                got_pointer = true;
                break;
            }
        }
        assert!(got_pointer);
        assert!(matches!(c.input(&friend, &id, &click).await, Err(CoSurfError::Forbidden(_))));
        // Prendre la main : cliquer et taper, mot de passe compris ; donner la main à Léa la retire à Paul.
        c.set_level(&owner, &id, &friend, "drive").await.unwrap();
        c.input(&friend, &id, &click).await.unwrap();
        c.input(&friend, &id, &json!({ "type": "text", "text": "mon-mot-de-passe" })).await.unwrap();
        assert!(launcher.seen.lock().unwrap().iter().any(|v| v["method"] == "Input.insertText" && v["params"]["text"] == "mon-mot-de-passe"));
        let v = c.set_level(&owner, &id, &other, "drive").await.unwrap();
        let levels: Vec<_> = v["guests"].as_array().unwrap().iter().map(|g| g["level"].as_str().unwrap().to_owned()).collect();
        assert_eq!(levels, ["point", "drive"], "un seul invité a la main");
        assert!(matches!(c.input(&friend, &id, &click).await, Err(CoSurfError::Forbidden(_))));
        // Seul le propriétaire change les niveaux ; un inconnu ne voit rien.
        assert_eq!(c.set_level(&friend, &id, &friend, "drive").await.unwrap_err(), CoSurfError::NotFound);
        assert_eq!(c.view(&hex::encode([99; 32]), &id).unwrap_err(), CoSurfError::NotFound);
        assert!(c.watch(&hex::encode([99; 32]), &id).await.is_err());
        // Paul s'en va : son flux se ferme. Alice termine : navigateur détruit, profil effacé, plus de session.
        c.stop(&friend, &id).await.unwrap();
        let closed = tokio::time::timeout(Duration::from_secs(5), async { while feed.recv().await.is_some() {} }).await;
        assert!(closed.is_ok(), "le flux de l'invité parti se ferme");
        c.stop(&owner, &id).await.unwrap();
        assert_eq!(launcher.alive.load(Ordering::SeqCst), 0);
        assert!(!dir.join("b").join(&id).exists());
        assert_eq!(c.view(&owner, &id).unwrap_err(), CoSurfError::NotFound);
        assert!(c.list(&owner)["sessions"].as_array().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    async fn p2p_node(seed: u8) -> (astro_p2p::P2pNode, astro_p2p::Multiaddr) {
        let n = astro_p2p::P2pNode::spawn(astro_p2p::P2pConfig::local([seed; 32])).await.unwrap();
        let tcp = n.handle.listen_addrs().into_iter().find(|a| a.to_string().contains("/tcp/")).unwrap();
        let full: astro_p2p::Multiaddr = format!("{tcp}/p2p/{}", n.handle.peer_id()).parse().unwrap();
        (n, full)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_friend_hosted_elsewhere_helps_through_their_own_station() {
        let (na, a_addr) = p2p_node(41).await;
        let (nb, _) = p2p_node(42).await;
        nb.handle.dial(a_addr).await.unwrap();
        // A : la station d'Alice tient la session. B : la station de Paul (et d'un compte non invité).
        let (dir_a, a, _) = rig("remote-a", &[OWNER], true);
        let a = Arc::new(a.with_p2p(na.handle.clone()));
        let _served = a.serve_remote().unwrap();
        let (dir_b, b, _) = rig("remote-b", &[FRIEND, OTHER], false);
        let b = b.with_p2p(nb.handle.clone());
        let (owner, friend, other) = (hex_of(&OWNER), hex_of(&FRIEND), hex_of(&OTHER));
        let station = na.handle.peer_id().to_string();
        let v = a.create(&owner, "Alice", "example.org", &[(friend.clone(), "Paul".into())], "point").await.unwrap();
        let id = v["id"].as_str().unwrap().to_owned();
        assert_eq!(v["station"].as_str(), Some(station.as_str()), "le lien d'invitation porte la station de la session");

        let seen = b.remote(&station, &friend, &id, "view", &Value::Null).await.unwrap();
        assert_eq!((seen["owner_label"].as_str(), seen["level"].as_str(), seen["mine"].as_bool()), (Some("Alice"), Some("point"), Some(false)));
        let mut feed = b.remote_watch(&station, &friend, &id).await.unwrap();
        assert!(first_frame(&mut feed).await.1.contains("SlBFRw=="), "Paul voit la page d'Alice à travers sa station");
        let click = json!({ "type": "click", "x": 0.5, "y": 0.5 });
        assert!(matches!(b.remote(&station, &friend, &id, "input", &click).await, Err(CoSurfError::Forbidden(_))));
        a.set_level(&owner, &id, &friend, "drive").await.unwrap();
        assert_eq!(b.remote(&station, &friend, &id, "input", &click).await.unwrap()["ok"], true);
        // Un compte de la même station, non invité, n'obtient rien.
        assert_eq!(b.remote(&station, &other, &id, "view", &Value::Null).await.unwrap_err(), CoSurfError::NotFound);
        assert!(b.remote_watch(&station, &other, &id).await.is_err());

        // Demande rejouée, périmée, pour une autre opération, ou signée d'une autre clé : refusée.
        let ev = serde_json::to_value(b.signed(&friend, &id, "view").unwrap()).unwrap();
        assert_eq!(a.check(&ev, &id, "view").unwrap(), friend);
        assert_eq!(a.check(&ev, &id, "view").unwrap_err(), CoSurfError::NotFound, "usage unique");
        let ev2 = serde_json::to_value(b.signed(&friend, &id, "view").unwrap()).unwrap();
        assert!(a.check(&ev2, &id, "input").is_err(), "usage annoncé");
        let old = UnsignedEvent { created_at: now() - 600, kind: kind::HTTP_AUTH, tags: vec![vec!["u".into(), format!("cosurf:{id}:view")]], content: String::new() }.sign(&FRIEND).unwrap();
        assert!(a.check(&serde_json::to_value(old).unwrap(), &id, "view").is_err(), "périmée");
        let mut forged = serde_json::to_value(b.signed(&friend, &id, "view").unwrap()).unwrap();
        forged["pubkey"] = Value::String(owner.clone());
        assert!(a.check(&forged, &id, "view").is_err(), "signature fausse");

        // Paul s'en va par sa station ; Alice termine : le flux de Paul se ferme.
        b.remote(&station, &friend, &id, "stop", &Value::Null).await.unwrap();
        let closed = tokio::time::timeout(Duration::from_secs(10), async { while feed.recv().await.is_some() {} }).await;
        assert!(closed.is_ok());
        a.stop(&owner, &id).await.unwrap();
        for d in [dir_a, dir_b] {
            let _ = std::fs::remove_dir_all(d);
        }
    }
}
