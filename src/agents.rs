//! Agents autorisés à demander l'accès aux comptes d'un membre, et serveur MCP avec demande d'accord.
//!
//! - **Registre** : un événement kind 31903 `d = agents`, signé de la clé du membre, dont le contenu est chiffré pour lui seul
//!   (NIP-44 vers lui-même). Il garde, par agent, un nom, l'empreinte SHA-256 du jeton (jamais le jeton), le groupe du coffre
//!   que l'agent peut atteindre, la durée maximale d'un accès et le droit de lire un code TOTP. Le relai en fait un cache
//!   rejouable (`kv`, portée `agents`) ; la station le relit et le déchiffre avec la clé du membre qu'elle héberge.
//! - **Jeton** : `ag1_<16 premiers hex du compte>_<64 hex aléatoires>`, montré une seule fois. Seule son empreinte est gardée.
//! - **MCP** (JSON-RPC 2.0 sur `POST /mcp`, jeton en `Bearer`) : `demander_acces` et `statut_acces`. Un agent ne reçoit **jamais**
//!   un secret : il reçoit « en attente », « accordé », « refusé » ou « expiré ». Le membre décide dans l'interface.
//! - **Deux modes** (`mode` de `demander_acces`) :
//!   - `accord` (défaut) : une autorisation datée, tracée dans le journal (`audit.agent`) et dans l'activité du membre ;
//!   - `navigateur` : la station lance un navigateur **jetable** ([`crate::agent_browser`]), le membre s'y connecte lui-même
//!     (état `login`), puis le **prête** à l'agent (`POST /api/v1/agents/ready`). `statut_acces` rend alors
//!     `browser_ws_endpoint` (`ws://127.0.0.1:<port>/cdp/<jeton>`), à l'agent qui a fait la demande et à lui seul. À
//!     l'échéance, sur « Reprendre la main » (`/api/v1/agents/stop`), sur `rendre_acces` ou si le navigateur s'arrête, la
//!     station tue le navigateur et efface son profil. Conception : `docs/design/SESSION_AGENT.md`.
//! - **Ce que ce module ne fait pas** : ouvrir `/_/Coffre/agents.kdbx`. Aucun identifiant ne quitte la station.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use astro_core::{ActivityBus, Audience};
use astro_crypto::{KeySource, nip44};
use astro_db::Db;
use astro_nostr_proto::{UnsignedEvent, kind};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::{mpsc, oneshot};

use astro_p2p::tunnel::{ExposeOpts, Exposed, expose, protocol_name};
use astro_p2p::{P2pHandle, PeerId};
use futures_util::StreamExt;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use tokio_util::compat::FuturesAsyncReadCompatExt;

use crate::agent_browser::pilot::Pilot;
use crate::forwards::Forwards;
use crate::agent_browser::screen::{Frame, Input, Screen};
use crate::agent_browser::{self, BrowserProc, LaunchSpec, Launcher, cdp, gate, guard::Guard};
use crate::register::PublishFn;

/// `d`-tag du registre (kind 31903).
pub const D_TAG: &str = "agents";
/// Portée `kv` du cache rejouable du registre.
pub const KV_SCOPE: &str = "agents";
const PREFIX: &str = "ag1_";
/// Agents déclarés au plus par membre.
const MAX_AGENTS: usize = 8;
/// Demandes en attente au plus par membre (au-delà : refus, pour ne pas inonder le titulaire).
const MAX_PENDING: usize = 5;
/// Demandes terminées (dans l'heure) montrées au membre sous « Récemment ».
const RECENT_SHOWN: usize = 10;
/// Demandes par agent et par heure.
const ASKS_PER_HOUR: usize = 20;
/// Une demande non traitée expire au bout de 30 minutes : le membre n'est pas toujours devant son écran (5 minutes avant 1.11).
const REQUEST_TTL_SECS: u64 = 1800;
/// Durée maximale d'un accord, quoi que dise le registre.
pub const MAX_GRANT_SECS: u64 = 3600;
/// Temps laissé au membre pour se connecter dans le navigateur jetable.
pub const LOGIN_TTL_SECS: u64 = 600;
/// Sites liés (connexion par un autre site) que le membre peut permettre en plus, au plus.
const MAX_EXTRA_HOSTS: usize = 5;
/// Navigateurs prêtés en même temps au plus, par membre.
const MAX_SESSIONS: usize = 2;
const SESSION_TOOLS: &str = "2024-11-05";
/// Service P2P par lequel la station d'un agent relaie ses appels MCP vers la station du membre (`/x/agents-mcp-<PeerID>`).
pub const REMOTE_SERVICE: &str = "agents-mcp";
/// Taille maximale d'un message relayé (une capture d'écran tient dedans).
const REMOTE_MAX: u64 = 8 << 20;
/// Appels relayés traités en même temps au plus.
const REMOTE_PARALLEL: usize = 16;

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Échec d'une opération sur les agents.
#[derive(Debug, PartialEq, Eq)]
pub enum AgentError {
    /// Saisie refusée.
    Invalid(String),
    /// Inconnu ici.
    NotFound,
    /// Trop de demandes.
    Limited(String),
    /// Jeton absent ou faux.
    Unauthorized,
    /// Panne de la station.
    Internal(String),
}

impl std::fmt::Display for AgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(m) | Self::Limited(m) | Self::Internal(m) => f.write_str(m),
            Self::NotFound => f.write_str("introuvable"),
            Self::Unauthorized => f.write_str("jeton d'agent refusé"),
        }
    }
}

/// Un agent du registre (les empreintes ne sortent jamais d'ici).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentRecord {
    /// Identifiant court (8 hex).
    pub id: String,
    /// Nom donné par le membre (« BRO », « Claude Code »…).
    pub name: String,
    /// SHA-256 du jeton, en hexadécimal.
    pub token_sha256: String,
    /// Groupe du coffre `agents.kdbx` que l'agent peut atteindre.
    pub group: String,
    /// Durée maximale d'un accord, en secondes.
    pub max_secs: u64,
    /// L'agent peut demander un code TOTP.
    pub totp: bool,
    /// Création (secondes Unix).
    pub created_at: u64,
    /// Station de la constellation où tourne l'agent (PeerID) ; absente : cette station. Seule cette station peut relayer ses
    /// appels, et c'est à elle seule que le navigateur prêté est ouvert.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub station: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct Registry {
    agents: Vec<AgentRecord>,
}

/// État d'une demande d'accès.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Le membre n'a pas répondu.
    Pending,
    /// Mode navigateur : le navigateur jetable est ouvert, le membre s'y connecte (jusqu'à `until`).
    Login,
    /// Accordée jusqu'à `until`.
    Granted,
    /// Refusée.
    Denied,
    /// Terminée avant l'échéance ou navigateur arrêté : `revoked` (le membre), `returned` (l'agent), `ended` (le navigateur
    /// s'est arrêté), `expired` (échéance). `until` = instant de la fin.
    Closed(&'static str),
}

/// Ce que l'agent demande.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Une autorisation datée, sans rien remettre.
    Accord,
    /// Un navigateur où le membre s'est connecté, prêté par la porte CDP.
    Navigateur,
}

impl Mode {
    fn parse(s: &str) -> Result<Self, AgentError> {
        match s {
            "" | "accord" => Ok(Self::Accord),
            "navigateur" => Ok(Self::Navigateur),
            _ => Err(AgentError::Invalid("mode attendu : accord ou navigateur".into())),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Accord => "accord",
            Self::Navigateur => "navigateur",
        }
    }
}

#[derive(Debug, Clone)]
struct Request {
    owner: String,
    agent_id: String,
    agent_name: String,
    site: String,
    reason: String,
    created: u64,
    state: State,
    until: u64,
    mode: Mode,
    /// Mode navigateur : durée choisie par le membre, appliquée quand il prête le navigateur.
    grant_secs: u64,
    /// Mode navigateur : sites liés permis par le membre en plus du site demandé.
    extra_hosts: Vec<String>,
    /// Station de l'agent quand elle n'est pas celle-ci : le navigateur lui est ouvert par le maillage.
    station: Option<String>,
    /// L'échéance (demande sans réponse, accord échu) est déjà inscrite au journal et dite au membre.
    lapsed: bool,
    /// Programme qui a fait la demande, tel qu'il s'est présenté (vide : il ne l'a pas dit).
    client: String,
}

impl Request {
    /// État vu de l'extérieur : une demande ignorée, une connexion jamais finie ou un accord échu devient « expiré ».
    fn label(&self, at: u64) -> &'static str {
        match self.state {
            State::Pending if at >= self.created + REQUEST_TTL_SECS => "expired",
            State::Pending => "pending",
            State::Login if at >= self.until => "expired",
            State::Login => "login",
            State::Granted if at >= self.until => "expired",
            State::Granted => "granted",
            State::Denied => "denied",
            State::Closed(r) => r,
        }
    }

    fn view(&self, id: &str, at: u64) -> Value {
        let until = matches!(self.state, State::Granted | State::Login).then_some(self.until);
        json!({ "id": id, "agent": self.agent_name, "client": (!self.client.is_empty()).then_some(&self.client), "remote": self.station.is_some(), "site": self.site, "hosts": self.extra_hosts, "reason": self.reason, "mode": self.mode.as_str(), "state": self.label(at), "asked_at": self.created, "until": until })
    }
}

/// Nom et version du programme d'après `clientInfo` (MCP `initialize`) : texte sans caractère de contrôle, 60 au plus.
fn client_label(params: &Value) -> Option<String> {
    let info = params.get("clientInfo")?;
    let name = info.get("name").and_then(Value::as_str).map(str::trim).filter(|n| !n.is_empty())?;
    let version = info.get("version").and_then(Value::as_str).map(str::trim).unwrap_or_default();
    let label = if version.is_empty() { name.to_owned() } else { format!("{name} {version}") };
    Some(label.chars().filter(|c| !c.is_control()).take(60).collect())
}

/// Comment nommer l'appelant au membre : le nom qu'il a donné à l'agent, et le programme s'il dit autre chose.
fn caller(agent: &str, client: &str) -> String {
    if client.is_empty() || client.to_lowercase().starts_with(&agent.to_lowercase()) { agent.to_owned() } else { format!("{agent} ({client})") }
}

/// Ordres au superviseur d'une session de navigateur.
enum Ctl {
    /// Le membre prête le navigateur jusqu'à `until` : ouvrir la porte, rendre son adresse.
    Ready { until: u64, reply: oneshot::Sender<Result<String, String>> },
    /// Tout arrêter ; répondre une fois le navigateur tué et le profil effacé.
    Stop { reason: &'static str, reply: oneshot::Sender<()> },
}

/// Session de navigateur vue du registre (le navigateur et la porte appartiennent à son superviseur).
struct SessionCtl {
    ctl: mpsc::UnboundedSender<Ctl>,
    /// Adresse de la porte, une fois le navigateur prêté. Ne sort que par `statut_acces`, vers l'agent demandeur.
    endpoint: Option<String>,
    /// Écran diffusé au membre (absent si le navigateur n'a pas pu le donner).
    screen: Option<Arc<Screen>>,
    /// Outils de navigation MCP (la station pilote pour l'agent).
    pilot: Option<Arc<Pilot>>,
    /// Agent d'une autre station : où joindre la porte par le maillage (`{ p2p: { service, peer }, path }`), à la place de l'adresse locale.
    remote: Option<Value>,
}

/// Navigateur jetable de la station.
struct BrowserCfg {
    launcher: Arc<dyn Launcher>,
    /// `<données>/agent-browsers`.
    dir: PathBuf,
    /// La station a un écran : le navigateur y ouvre aussi une fenêtre (sinon il tourne sans, et le membre le voit dans
    /// l'application).
    screen: bool,
}

type Requests = Arc<Mutex<HashMap<String, Request>>>;
type Sessions = Arc<Mutex<HashMap<String, SessionCtl>>>;

fn locked<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Un nom d'hôte seul : pas de schéma, de port, de chemin ni d'identifiants. Rend l'hôte en minuscules.
///
/// # Errors
/// [`AgentError::Invalid`] si `site` n'est pas un nom d'hôte DNS.
pub fn valid_host(site: &str) -> Result<String, AgentError> {
    let host = site.trim().trim_end_matches('.').to_ascii_lowercase();
    let label_ok = |l: &str| !l.is_empty() && l.len() <= 63 && !l.starts_with('-') && !l.ends_with('-') && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    if host.len() > 253 || !host.contains('.') || !host.split('.').all(label_ok) || host.split('.').all(|l| l.bytes().all(|b| b.is_ascii_digit())) {
        return Err(AgentError::Invalid("nom de site attendu seul (exemple : example.org), sans http://, sans chemin ni port".into()));
    }
    Ok(host)
}

fn sha_hex(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

/// Registre des agents et file des demandes d'accord.
pub struct Agents {
    db: Arc<Db>,
    keys: Arc<dyn KeySource>,
    publish: PublishFn,
    activity: Option<ActivityBus>,
    requests: Requests,
    asks: Mutex<HashMap<String, Vec<u64>>>,
    sessions: Sessions,
    browser: Option<BrowserCfg>,
    p2p: Option<(P2pHandle, Arc<Forwards>)>,
    /// Programme qui parle avec le jeton de chaque agent (`<membre>:<agent>`), tel qu'il s'est présenté à `initialize`
    /// (`clientInfo` du protocole MCP) : déclaré par le programme lui-même, montré au membre comme tel.
    clients: Mutex<HashMap<String, String>>,
}

async fn audit_to(db: &Db, data: Value) {
    let _ = db.writer().execute("INSERT INTO journal(stream, at, data) VALUES ('audit.agent', ?1, ?2)", vec![i64::try_from(now()).unwrap_or(0).into(), data.to_string().into()]).await;
}

impl Agents {
    /// Service de registre sur `db`, signant avec les clés de `keys` et publiant par `publish`.
    #[must_use]
    pub fn new(db: Arc<Db>, keys: Arc<dyn KeySource>, publish: PublishFn) -> Self {
        Self { db, keys, publish, activity: None, requests: Arc::default(), asks: Mutex::new(HashMap::new()), sessions: Arc::default(), browser: None, p2p: None, clients: Mutex::new(HashMap::new()) }
    }

    /// Dit les demandes sur le bus d'activité du titulaire.
    #[must_use]
    pub fn with_activity(mut self, bus: ActivityBus) -> Self {
        self.activity = Some(bus);
        self
    }

    /// Permet le mode `navigateur` : profils jetables dans `dir` (`<données>/agent-browsers`), lancés par `launcher`.
    /// `screen` : la station a un écran où ouvrir aussi une fenêtre ; sans lui, le navigateur tourne sans fenêtre et le
    /// membre s'y connecte depuis l'application (écran diffusé).
    #[must_use]
    pub fn with_browser(mut self, launcher: Arc<dyn Launcher>, dir: PathBuf, screen: bool) -> Self {
        self.browser = Some(BrowserCfg { launcher, dir, screen });
        self
    }

    /// Agents des autres stations de la constellation : relais de leurs appels (service [`REMOTE_SERVICE`]) et navigateur prêté
    /// par le maillage ; accès locaux vers les stations des membres pour les agents qui tournent ici (`forwards`).
    #[must_use]
    pub fn with_p2p(mut self, handle: P2pHandle, forwards: Arc<Forwards>) -> Self {
        self.p2p = Some((handle, forwards));
        self
    }

    fn here(&self) -> Option<String> {
        self.p2p.as_ref().map(|(h, _)| h.peer_id().to_string())
    }

    async fn audit(&self, data: Value) {
        audit_to(&self.db, data).await;
    }

    /// Registre du membre et horodatage de sa dernière version (0 : jamais écrit).
    async fn load_stamped(&self, owner: &str) -> Result<(Registry, u64), AgentError> {
        let o = owner.to_owned();
        let stored: Option<(String, i64)> = self
            .db
            .readers()
            .read(move |c| Ok(rusqlite::OptionalExtension::optional(c.query_row("SELECT value, updated_at FROM kv WHERE scope = ?1 AND key = ?2", rusqlite::params![KV_SCOPE, o], |r| Ok((r.get(0)?, r.get(1)?))))?))
            .await
            .map_err(|e| AgentError::Internal(e.to_string()))?;
        let Some((payload, stamp)) = stored else { return Ok((Registry::default(), 0)) };
        let secret = self.keys.secret(owner).ok_or_else(|| AgentError::Internal("clé du compte absente de cette station".into()))?;
        let plain = nip44::decrypt_from_self(&secret, owner, &payload).map_err(|e| AgentError::Internal(format!("registre illisible : {e}")))?;
        let reg = serde_json::from_str(&plain).map_err(|e| AgentError::Internal(format!("registre illisible : {e}")))?;
        Ok((reg, u64::try_from(stamp).unwrap_or(0)))
    }

    async fn load(&self, owner: &str) -> Result<Registry, AgentError> {
        self.load_stamped(owner).await.map(|(r, _)| r)
    }

    /// Signe, publie puis range le registre. L'horodatage dépasse toujours celui de la version précédente : un relai garde le
    /// plus récent d'un événement remplaçable, et deux enregistrements dans la même seconde ne doivent pas se départager au hasard.
    async fn save(&self, owner: &str, reg: &Registry, previous: u64) -> Result<(), AgentError> {
        let secret = self.keys.secret(owner).ok_or_else(|| AgentError::Internal("clé du compte absente de cette station".into()))?;
        let plain = serde_json::to_string(reg).map_err(|e| AgentError::Internal(e.to_string()))?;
        let content = nip44::encrypt_to_self(&secret, owner, &plain).map_err(|e| AgentError::Internal(e.to_string()))?;
        let at = now().max(previous + 1);
        let event = UnsignedEvent { created_at: at, kind: kind::APP_PARAMS, tags: vec![vec!["d".into(), D_TAG.into()]], content: content.clone() }.sign(&secret).map_err(|e| AgentError::Internal(e.to_string()))?;
        (self.publish)(event).await.map_err(|e| AgentError::Internal(format!("registre non publié : {e}")))?;
        // Le projecteur remplit le même cache à son rythme (et le reconstruit au `replay`) ; on l'écrit aussi ici pour qu'une
        // lecture juste après l'enregistrement voie déjà la nouvelle version.
        self.db
            .writer()
            .execute(
                "INSERT INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at WHERE excluded.updated_at >= kv.updated_at",
                vec![KV_SCOPE.to_owned().into(), owner.to_owned().into(), content.into(), i64::try_from(at).unwrap_or(0).into()],
            )
            .await
            .map(|_| ())
            .map_err(|e| AgentError::Internal(e.to_string()))
    }

    /// Agents du membre, sans empreinte.
    ///
    /// # Errors
    /// Base ou clé indisponibles.
    pub async fn list(&self, owner: &str) -> Result<Value, AgentError> {
        let reg = self.load(owner).await?;
        Ok(json!({ "here": self.here(), "agents": reg.agents.iter().map(|a| json!({ "id": a.id, "name": a.name, "group": a.group, "max_secs": a.max_secs, "totp": a.totp, "created_at": a.created_at, "station": a.station })).collect::<Vec<_>>() }))
    }

    /// Déclare un agent et rend son jeton, **montré cette seule fois**.
    ///
    /// # Errors
    /// Saisie invalide, trop d'agents, ou registre non publié.
    pub async fn enroll(&self, owner: &str, name: &str, group: &str, max_secs: u64, totp: bool) -> Result<Value, AgentError> {
        self.enroll_on(owner, name, group, max_secs, totp, None).await
    }

    /// Comme [`Agents::enroll`], pour un agent qui tourne sur la station `station` (PeerID) de la constellation.
    ///
    /// # Errors
    /// Comme [`Agents::enroll`], et station invalide.
    pub async fn enroll_on(&self, owner: &str, name: &str, group: &str, max_secs: u64, totp: bool, station: Option<&str>) -> Result<Value, AgentError> {
        let station = match station.map(str::trim).filter(|s| !s.is_empty()) {
            None => None,
            Some(s) => {
                let peer: PeerId = s.parse().map_err(|_| AgentError::Invalid("station inconnue".into()))?;
                // Déclarer cette station-ci revient à ne rien déclarer.
                (Some(peer.to_string()) != self.here()).then(|| peer.to_string())
            }
        };
        let name = name.trim();
        let group = group.trim();
        if name.is_empty() || name.chars().count() > 40 || name.chars().any(char::is_control) {
            return Err(AgentError::Invalid("nom de 1 à 40 caractères attendu".into()));
        }
        if group.is_empty() || group.chars().count() > 60 || group.chars().any(char::is_control) {
            return Err(AgentError::Invalid("groupe du coffre de 1 à 60 caractères attendu".into()));
        }
        let (mut reg, stamp) = self.load_stamped(owner).await?;
        if reg.agents.len() >= MAX_AGENTS {
            return Err(AgentError::Limited(format!("{MAX_AGENTS} agents au plus : en retirer un d'abord")));
        }
        let mut raw = [0u8; 36];
        getrandom::fill(&mut raw).map_err(|e| AgentError::Internal(format!("aléa du système indisponible : {e}")))?;
        let id = hex::encode(&raw[..4]);
        let token = format!("{PREFIX}{}_{}", owner.get(..16).unwrap_or(owner), hex::encode(&raw[4..]));
        reg.agents.push(AgentRecord { id: id.clone(), name: name.to_owned(), token_sha256: sha_hex(&token), group: group.to_owned(), max_secs: max_secs.clamp(30, MAX_GRANT_SECS), totp, created_at: now(), station: station.clone() });
        self.save(owner, &reg, stamp).await?;
        self.audit(json!({ "event": "enroll", "hex": owner, "agent": id, "name": name })).await;
        Ok(json!({ "id": id, "name": name, "token": token, "note": "Ce jeton ne sera plus jamais affiché : copiez-le maintenant dans la configuration de l'agent." }))
    }

    /// Retire un agent : son jeton cesse de fonctionner et ses demandes en cours tombent.
    ///
    /// # Errors
    /// Agent inconnu ou registre non publié.
    pub async fn revoke(&self, owner: &str, id: &str) -> Result<Value, AgentError> {
        let (mut reg, stamp) = self.load_stamped(owner).await?;
        let before = reg.agents.len();
        reg.agents.retain(|a| a.id != id);
        if reg.agents.len() == before {
            return Err(AgentError::NotFound);
        }
        self.save(owner, &reg, stamp).await?;
        // Ses navigateurs prêtés sont arrêtés avant que ses demandes ne tombent.
        let live: Vec<String> = locked(&self.requests).iter().filter(|(_, r)| r.owner == owner && r.agent_id == id).map(|(k, _)| k.clone()).collect();
        for rid in live {
            self.stop_session(&rid, "revoked").await;
        }
        locked(&self.requests).retain(|_, r| !(r.owner == owner && r.agent_id == id));
        self.audit(json!({ "event": "revoke", "hex": owner, "agent": id })).await;
        Ok(json!({ "revoked": id }))
    }

    /// Identifie un agent d'après son jeton.
    async fn authenticate(&self, token: &str) -> Result<(String, AgentRecord), AgentError> {
        let rest = token.strip_prefix(PREFIX).ok_or(AgentError::Unauthorized)?;
        let (prefix, secret) = rest.split_once('_').ok_or(AgentError::Unauthorized)?;
        if prefix.len() != 16 || secret.len() != 64 || !prefix.bytes().chain(secret.bytes()).all(|b| b.is_ascii_hexdigit()) {
            return Err(AgentError::Unauthorized);
        }
        let like = format!("{prefix}%");
        let owners: Vec<String> = self
            .db
            .readers()
            .read(move |c| Ok(c.prepare("SELECT key FROM kv WHERE scope = ?1 AND key LIKE ?2 LIMIT 8")?.query_map(rusqlite::params![KV_SCOPE, like], |r| r.get(0))?.collect::<Result<_, _>>()?))
            .await
            .map_err(|e| AgentError::Internal(e.to_string()))?;
        let digest = sha_hex(token);
        for owner in owners {
            if let Some(a) = self.load(&owner).await?.agents.into_iter().find(|a| a.token_sha256 == digest) {
                return Ok((owner, a));
            }
        }
        Err(AgentError::Unauthorized)
    }

    fn sweep(map: &mut HashMap<String, Request>, at: u64) {
        map.retain(|_, r| match r.state {
            State::Pending | State::Denied => at < r.created + REQUEST_TTL_SECS + 3600,
            State::Login | State::Granted | State::Closed(_) => at < r.until + 3600,
        });
    }

    /// Demandes arrivées à échéance sans que rien ne le dise : une demande restée sans réponse, ou un accord (mode accord)
    /// échu. Le mode navigateur a son superviseur, qui conclut lui-même. Chacune est inscrite au journal d'audit et dite au
    /// membre une seule fois, pour que le journal raconte chaque demande jusqu'au bout.
    pub async fn note_lapses(&self) -> usize {
        let at = now();
        let due: Vec<(String, String, String, String, bool)> = {
            let mut map = locked(&self.requests);
            map.iter_mut()
                .filter(|(_, r)| !r.lapsed && r.label(at) == "expired" && (r.state == State::Pending || (r.state == State::Granted && r.mode == Mode::Accord)))
                .map(|(id, r)| {
                    r.lapsed = true;
                    (id.clone(), r.owner.clone(), r.agent_name.clone(), r.site.clone(), r.state == State::Pending)
                })
                .collect()
        };
        for (id, owner, who, site, unanswered) in &due {
            self.audit(json!({ "event": "expired", "hex": owner, "request": id, "site": site, "was": if *unanswered { "pending" } else { "granted" } })).await;
            if let Some(bus) = &self.activity {
                let what = if *unanswered { format!("La demande de {who} pour {site} a expiré sans réponse") } else { format!("Temps écoulé : {who} n'a plus accès à {site}") };
                bus.publish_to(Audience::Owner(owner.clone()), "agent", "agents", what, Some(json!({ "request": id, "site": site, "agent": who })));
            }
        }
        due.len()
    }

    /// Tâche de fond : relève les échéances toutes les `every`, jusqu'à l'arrêt de la station.
    pub async fn run_lapses(self: Arc<Self>, every: std::time::Duration, mut stop: tokio::sync::watch::Receiver<bool>) {
        loop {
            self.note_lapses().await;
            tokio::select! {
                () = tokio::time::sleep(every) => {}
                changed = stop.changed() => {
                    if changed.is_err() || *stop.borrow() {
                        return;
                    }
                }
            }
        }
    }

    /// Un agent demande l'accès à `site` (`mode` : `accord` par défaut, ou `navigateur`). Rend l'identifiant de la demande ;
    /// le membre est prévenu.
    ///
    /// # Errors
    /// Site ou mode invalide, navigateur impossible ici, trop de demandes (par agent ou en attente chez le membre).
    pub async fn ask(&self, owner: &str, agent: &AgentRecord, site: &str, reason: &str, mode: &str) -> Result<Value, AgentError> {
        let site = valid_host(site)?;
        let mode = Mode::parse(mode.trim())?;
        if mode == Mode::Navigateur && self.browser.is_none() {
            return Err(AgentError::Invalid("cette station ne prête pas de navigateur (Chromium absent)".into()));
        }
        let reason: String = reason.trim().chars().filter(|c| !c.is_control()).take(200).collect();
        let at = now();
        {
            let mut asks = self.asks.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let list = asks.entry(format!("{owner}:{}", agent.id)).or_default();
            list.retain(|t| at.saturating_sub(*t) < 3600);
            if list.len() >= ASKS_PER_HOUR {
                return Err(AgentError::Limited("trop de demandes : réessayez plus tard".into()));
            }
            list.push(at);
        }
        let mut raw = [0u8; 8];
        getrandom::fill(&mut raw).map_err(|e| AgentError::Internal(format!("aléa du système indisponible : {e}")))?;
        let id = hex::encode(raw);
        let client = locked(&self.clients).get(&format!("{owner}:{}", agent.id)).cloned().unwrap_or_default();
        {
            let mut map = self.requests.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            Self::sweep(&mut map, at);
            let waiting = map.values().filter(|r| r.owner == owner && r.label(at) == "pending").count();
            if waiting >= MAX_PENDING {
                return Err(AgentError::Limited("trop de demandes en attente : le membre doit d'abord répondre".into()));
            }
            map.insert(id.clone(), Request { owner: owner.to_owned(), agent_id: agent.id.clone(), agent_name: agent.name.clone(), site: site.clone(), reason: reason.clone(), created: at, state: State::Pending, until: 0, mode, grant_secs: 0, extra_hosts: Vec::new(), station: agent.station.clone(), lapsed: false, client: client.clone() });
        }
        self.audit(json!({ "event": "ask", "hex": owner, "agent": agent.id, "client": client, "remote": agent.station, "site": site, "mode": mode.as_str(), "request": id })).await;
        if let Some(bus) = &self.activity {
            let who = caller(&agent.name, &client);
            let what = if mode == Mode::Navigateur { format!("{who} voudrait agir sur {site} pour vous") } else { format!("{who} demande l'accès à {site}") };
            bus.publish_to(Audience::Owner(owner.to_owned()), "agent", "agents", what, Some(json!({ "request": id, "site": site, "agent": agent.name, "client": client, "mode": mode.as_str() })));
        }
        Ok(json!({ "id": id, "state": "pending", "mode": mode.as_str(), "expires_in": REQUEST_TTL_SECS }))
    }

    /// État d'une demande, vu par l'agent qui l'a faite. En mode navigateur accordé, porte l'adresse de la porte CDP
    /// (`browser_ws_endpoint`) : c'est le seul endroit d'où elle sort.
    ///
    /// # Errors
    /// Demande inconnue ou d'un autre agent.
    pub fn status(&self, owner: &str, agent: &AgentRecord, id: &str) -> Result<Value, AgentError> {
        let at = now();
        let mut view = {
            let map = locked(&self.requests);
            let r = map.get(id).filter(|r| r.owner == owner && r.agent_id == agent.id).ok_or(AgentError::NotFound)?;
            r.view(id, at)
        };
        if view["state"] == "granted" {
            let sessions = locked(&self.sessions);
            match sessions.get(id).map(|s| (s.remote.clone(), s.endpoint.clone())) {
                Some((Some(remote), _)) => view["browser"] = remote,
                Some((None, Some(e))) => view["browser_ws_endpoint"] = Value::String(e),
                _ => {}
            }
        }
        Ok(view)
    }

    /// Arrête la session de navigateur de la demande `id` (sans effet s'il n'y en a pas) et attend que tout soit détruit.
    async fn stop_session(&self, id: &str, reason: &'static str) -> bool {
        let ctl = locked(&self.sessions).get(id).map(|s| s.ctl.clone());
        let Some(ctl) = ctl else { return false };
        let (tx, rx) = oneshot::channel();
        if ctl.send(Ctl::Stop { reason, reply: tx }).is_ok() {
            let _ = rx.await;
        }
        true
    }

    /// Termine une demande accordée ou en cours (`revoked` par le membre, `returned` par l'agent).
    async fn finish(&self, owner: &str, id: &str, agent_id: Option<&str>, reason: &'static str) -> Result<Value, AgentError> {
        let at = now();
        let (who, site, was_pending) = {
            let mut map = locked(&self.requests);
            let r = map.get_mut(id).filter(|r| r.owner == owner && agent_id.is_none_or(|a| r.agent_id == a)).ok_or(AgentError::NotFound)?;
            let label = r.label(at);
            match label {
                "pending" if agent_id.is_none() => r.state = State::Denied,
                "pending" => {
                    r.state = State::Closed(reason);
                    r.until = at;
                }
                "login" | "granted" => {
                    // Mode accord : rien à détruire, l'accord prend fin ici. Mode navigateur : c'est le superviseur qui conclut.
                    if r.mode == Mode::Accord {
                        r.state = State::Closed(reason);
                        r.until = at;
                    }
                }
                _ => return Err(AgentError::Invalid("rien à arrêter : cette demande n'est plus en cours".into())),
            }
            (r.agent_name.clone(), r.site.clone(), label == "pending")
        };
        if !self.stop_session(id, reason).await {
            self.audit(json!({ "event": reason, "hex": owner, "request": id })).await;
            // Sans navigateur, aucun superviseur ne prévient le membre : une demande retirée ou un accord rendu par l'agent
            // se dit ici, sinon une demande de quelques secondes passerait sans laisser de trace à l'écran.
            if let (Some(bus), Some(_)) = (&self.activity, agent_id) {
                let what = if was_pending { format!("{who} a retiré sa demande d'accès à {site}") } else { format!("{who} a rendu l'accès à {site}") };
                bus.publish_to(Audience::Owner(owner.to_owned()), "agent", "agents", what, Some(json!({ "request": id, "site": site, "agent": who })));
            }
        }
        let map = locked(&self.requests);
        Ok(map.get(id).map_or_else(|| json!({ "id": id, "state": reason }), |r| r.view(id, now())))
    }

    /// Le membre reprend la main : refuse une demande en attente, ou arrête tout de suite un accord ou un navigateur prêté.
    ///
    /// # Errors
    /// Demande inconnue ou déjà terminée.
    pub async fn stop(&self, owner: &str, id: &str) -> Result<Value, AgentError> {
        self.finish(owner, id, None, "revoked").await
    }

    /// L'agent a fini : la station arrête tout de suite son accord (et son navigateur).
    ///
    /// # Errors
    /// Demande inconnue, d'un autre agent, ou déjà terminée.
    pub async fn give_back(&self, owner: &str, agent: &AgentRecord, id: &str) -> Result<Value, AgentError> {
        self.finish(owner, id, Some(&agent.id), "returned").await
    }

    /// Mode navigateur : le membre s'est connecté et **prête** le navigateur à l'agent pour la durée choisie à l'accord.
    ///
    /// # Errors
    /// Demande inconnue, pas en cours de connexion, ou navigateur arrêté entre-temps.
    pub async fn ready(&self, owner: &str, id: &str) -> Result<Value, AgentError> {
        let until = {
            let map = locked(&self.requests);
            let r = map.get(id).filter(|r| r.owner == owner).ok_or(AgentError::NotFound)?;
            if r.label(now()) != "login" {
                return Err(AgentError::Invalid("ce navigateur n'attend pas d'être prêté".into()));
            }
            now() + r.grant_secs
        };
        let ctl = locked(&self.sessions).get(id).map(|s| s.ctl.clone()).ok_or_else(|| AgentError::Invalid("le navigateur s'est arrêté".into()))?;
        let (tx, rx) = oneshot::channel();
        ctl.send(Ctl::Ready { until, reply: tx }).map_err(|_| AgentError::Invalid("le navigateur s'est arrêté".into()))?;
        rx.await.map_err(|_| AgentError::Invalid("le navigateur s'est arrêté".into()))?.map_err(AgentError::Internal)?;
        let map = locked(&self.requests);
        Ok(map.get(id).map_or(Value::Null, |r| r.view(id, now())))
    }

    /// Pilote d'un navigateur prêté à cet agent (demande accordée en mode navigateur).
    fn pilot_of(&self, owner: &str, agent: &AgentRecord, id: &str) -> Result<Arc<Pilot>, AgentError> {
        let (label, mode) = locked(&self.requests).get(id).filter(|r| r.owner == owner && r.agent_id == agent.id).map(|r| (r.label(now()), r.mode)).ok_or(AgentError::NotFound)?;
        if mode != Mode::Navigateur || label != "granted" {
            return Err(AgentError::Invalid("aucun navigateur ne vous est prêté pour cette demande (statut_acces doit dire granted, en mode navigateur)".into()));
        }
        locked(&self.sessions).get(id).and_then(|s| s.pilot.clone()).ok_or_else(|| AgentError::Invalid("le navigateur s'est arrêté".into()))
    }

    /// Un outil de navigation MCP ; rend le contenu MCP (texte, ou image pour `capture`).
    async fn browse_tool(&self, owner: &str, agent: &AgentRecord, name: &str, args: &Value) -> Result<Vec<Value>, AgentError> {
        let id = args.get("id").and_then(Value::as_str).unwrap_or_default();
        let pilot = self.pilot_of(owner, agent, id)?;
        let n = || args.get("element").and_then(Value::as_u64).ok_or_else(|| AgentError::Invalid("element (numéro rendu par lire_page) attendu".into()));
        let text = |v: Value| vec![json!({ "type": "text", "text": v.to_string() })];
        let r = match name {
            "naviguer" => pilot.navigate(args.get("url").and_then(Value::as_str).unwrap_or_default()).await.map(text),
            "lire_page" => pilot.read().await.map(text),
            "cliquer" => pilot.click(n()?).await.map(text),
            "saisir" => pilot.type_text(n()?, args.get("texte").and_then(Value::as_str).unwrap_or_default(), args.get("entree").and_then(Value::as_bool).unwrap_or(false)).await.map(text),
            _ => pilot.capture().await.map(|data| vec![json!({ "type": "image", "data": data, "mimeType": "image/jpeg" })]),
        };
        r.map_err(AgentError::Invalid)
    }

    /// Écran d'une session du membre, pendant la connexion ou le prêt.
    fn screen_of(&self, owner: &str, id: &str) -> Result<Arc<Screen>, AgentError> {
        let label = locked(&self.requests).get(id).filter(|r| r.owner == owner).map(|r| r.label(now())).ok_or(AgentError::NotFound)?;
        if !matches!(label, "login" | "granted") {
            return Err(AgentError::Invalid("ce navigateur n'est plus ouvert".into()));
        }
        locked(&self.sessions).get(id).and_then(|s| s.screen.clone()).ok_or_else(|| AgentError::Invalid("l'écran de ce navigateur n'est pas disponible".into()))
    }

    /// Regarder le navigateur d'une session (images JSON `{ seq, w, h, jpeg }`) : le membre seul.
    ///
    /// # Errors
    /// Demande inconnue, d'un autre compte, terminée, ou écran indisponible.
    pub async fn screen_watch(&self, owner: &str, id: &str) -> Result<tokio::sync::watch::Receiver<Option<Frame>>, AgentError> {
        self.screen_of(owner, id)?.watch().await.map_err(AgentError::Internal)
    }

    /// Rejoue un geste du membre (`{ type: click|scroll|key|text, … }`) dans le navigateur d'une session.
    ///
    /// # Errors
    /// Geste invalide, demande inconnue ou terminée, navigateur indisponible.
    pub async fn screen_input(&self, owner: &str, id: &str, gesture: &Value) -> Result<Value, AgentError> {
        let input = Input::parse(gesture).map_err(AgentError::Invalid)?;
        self.screen_of(owner, id)?.input(&input).await.map_err(AgentError::Invalid)?;
        Ok(json!({ "ok": true }))
    }

    /// Lance le navigateur jetable d'une demande que le membre vient d'accepter (état `Login` déjà posé).
    fn open_browser(&self, owner: &str, id: &str, req: &Request) -> Result<(), AgentError> {
        let cfg = self.browser.as_ref().ok_or_else(|| AgentError::Invalid("cette station ne prête pas de navigateur".into()))?;
        let profile = cfg.dir.join(id);
        let browser = cfg.launcher.launch(&LaunchSpec { profile: profile.clone(), url: format!("https://{}/", req.site), headless: !cfg.screen }).map_err(AgentError::Internal)?;
        let (tx, rx) = mpsc::unbounded_channel();
        locked(&self.sessions).insert(id.to_owned(), SessionCtl { ctl: tx, endpoint: None, screen: None, pilot: None, remote: None });
        let sup = Supervisor {
            id: id.to_owned(),
            owner: owner.to_owned(),
            agent_id: req.agent_id.clone(),
            agent_name: req.agent_name.clone(),
            policy: Arc::new(cdp::Policy::new(&req.site, req.extra_hosts.clone(), profile.join("Téléchargements"))),
            requests: self.requests.clone(),
            sessions: self.sessions.clone(),
            db: self.db.clone(),
            activity: self.activity.clone(),
            remote: req.station.as_ref().and_then(|s| s.parse::<PeerId>().ok()).zip(self.p2p.as_ref().map(|(h, _)| h.clone())),
        };
        tokio::spawn(sup.run(browser, rx, req.until));
        Ok(())
    }

    /// Demandes en cours du membre (en attente ou accordées), et `recent` : celles terminées dans l'heure (refusées, expirées,
    /// rendues ou reprises), les plus récentes d'abord, pour qu'aucune demande ne passe sans laisser de trace à son écran.
    #[must_use]
    pub fn requests(&self, owner: &str) -> Value {
        let at = now();
        let mut map = self.requests.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::sweep(&mut map, at);
        let mut live: Vec<(u64, Value)> = Vec::new();
        let mut done: Vec<(u64, Value)> = Vec::new();
        for (id, r) in map.iter().filter(|(_, r)| r.owner == owner) {
            if matches!(r.label(at), "pending" | "login" | "granted") {
                live.push((r.created, r.view(id, at)));
            } else {
                done.push((r.created, r.view(id, at)));
            }
        }
        live.sort_by_key(|(t, _)| std::cmp::Reverse(*t));
        done.sort_by_key(|(t, _)| std::cmp::Reverse(*t));
        done.truncate(RECENT_SHOWN);
        let rows = |v: Vec<(u64, Value)>| v.into_iter().map(|(_, v)| v).collect::<Vec<_>>();
        json!({ "requests": rows(live), "recent": rows(done) })
    }

    /// Le membre accorde (`secs`, borné par l'agent et par [`MAX_GRANT_SECS`]) ou refuse. En mode navigateur, accorder
    /// ouvre le navigateur jetable où le membre se connecte (état `login`) ; la durée court à partir de [`Agents::ready`].
    ///
    /// # Errors
    /// Demande inconnue, expirée ou déjà tranchée ; navigateur impossible à lancer (la demande reste alors en attente).
    pub async fn decide(&self, owner: &str, id: &str, approve: bool, secs: Option<u64>) -> Result<Value, AgentError> {
        self.decide_hosts(owner, id, approve, secs, &[]).await
    }

    /// Comme [`Agents::decide`], en permettant aussi, en mode navigateur, des sites liés (`hosts`, noms d'hôte seuls, 5 au plus :
    /// connexion par un autre site, par exemple). Le site demandé et ses sous-domaines sont toujours permis.
    ///
    /// # Errors
    /// Comme [`Agents::decide`], et site lié invalide ou trop nombreux.
    pub async fn decide_hosts(&self, owner: &str, id: &str, approve: bool, secs: Option<u64>, hosts: &[String]) -> Result<Value, AgentError> {
        if hosts.len() > MAX_EXTRA_HOSTS {
            return Err(AgentError::Invalid(format!("{MAX_EXTRA_HOSTS} sites liés au plus")));
        }
        let hosts = hosts.iter().map(|h| valid_host(h)).collect::<Result<Vec<_>, _>>()?;
        let cap = self.load(owner).await.map(|r| r.agents).unwrap_or_default();
        let at = now();
        let (view, agent_id, site, launch) = {
            let mut map = locked(&self.requests);
            let browsing = map.values().filter(|r| r.owner == owner && matches!(r.label(at), "login" | "granted") && r.mode == Mode::Navigateur).count();
            let r = map.get_mut(id).filter(|r| r.owner == owner).ok_or(AgentError::NotFound)?;
            if r.label(at) != "pending" {
                return Err(AgentError::Invalid("cette demande n'est plus en attente".into()));
            }
            let agent_cap = cap.iter().find(|a| a.id == r.agent_id).map_or(MAX_GRANT_SECS, |a| a.max_secs);
            let secs = secs.unwrap_or(agent_cap).clamp(30, agent_cap.min(MAX_GRANT_SECS));
            let launch = approve && r.mode == Mode::Navigateur;
            if launch && browsing >= MAX_SESSIONS {
                return Err(AgentError::Limited(format!("{MAX_SESSIONS} navigateurs prêtés au plus en même temps : reprenez d'abord la main sur un autre")));
            }
            match (approve, r.mode) {
                (false, _) => r.state = State::Denied,
                (true, Mode::Accord) => {
                    r.state = State::Granted;
                    r.until = at + secs;
                }
                // Posé avant le lancement : une seconde décision simultanée trouve la demande déjà prise.
                (true, Mode::Navigateur) => {
                    r.state = State::Login;
                    r.until = at + LOGIN_TTL_SECS;
                    r.grant_secs = secs;
                    r.extra_hosts = hosts;
                }
            }
            (r.view(id, at), r.agent_id.clone(), r.site.clone(), launch.then(|| r.clone()))
        };
        if let Some(req) = launch {
            if let Err(e) = self.open_browser(owner, id, &req) {
                if let Some(r) = locked(&self.requests).get_mut(id) {
                    r.state = State::Pending;
                    r.until = 0;
                }
                return Err(e);
            }
            self.audit(json!({ "event": "session_open", "hex": owner, "agent": agent_id, "site": site, "request": id })).await;
            return Ok(view);
        }
        self.audit(json!({ "event": if approve { "grant" } else { "deny" }, "hex": owner, "agent": agent_id, "site": site, "request": id })).await;
        Ok(view)
    }

    fn tools() -> Value {
        json!([
            { "name": "demander_acces", "description": "Demande au membre l'accès à un de ses comptes. Rend l'identifiant d'une demande ; le membre est prévenu et répond lui-même. Aucun mot de passe n'est jamais renvoyé à l'agent. En mode navigateur, le membre se connecte lui-même dans un navigateur jetable que la station prête ensuite à l'agent.",
              "inputSchema": { "type": "object", "properties": {
                  "site": { "type": "string", "description": "Nom du site seul, par exemple example.org" },
                  "raison": { "type": "string", "description": "Pourquoi (200 caractères au plus), montrée au membre" },
                  "mode": { "type": "string", "enum": ["accord", "navigateur"], "description": "accord (défaut) : autorisation datée ; navigateur : session de navigateur connectée, pilotable par CDP" } }, "required": ["site"] } },
            { "name": "statut_acces", "description": "État d'une demande : pending, login (le membre se connecte), granted, denied, expired, revoked, returned ou ended. En mode navigateur accordé : browser_ws_endpoint, à passer à Playwright connectOverCDP ou Puppeteer connect ; valable jusqu'à until.",
              "inputSchema": { "type": "object", "properties": { "id": { "type": "string" } }, "required": ["id"] } },
            { "name": "rendre_acces", "description": "L'agent a fini : la station arrête tout de suite l'accord et détruit le navigateur prêté.",
              "inputSchema": { "type": "object", "properties": { "id": { "type": "string" } }, "required": ["id"] } },
            { "name": "naviguer", "description": "Navigateur prêté (sans CDP) : va à une adresse du site accordé et rend la page comme lire_page.",
              "inputSchema": { "type": "object", "properties": { "id": { "type": "string" }, "url": { "type": "string" } }, "required": ["id", "url"] } },
            { "name": "lire_page", "description": "Navigateur prêté : adresse, titre, texte visible et éléments numérotés (boutons, liens, champs) à passer à cliquer ou saisir.",
              "inputSchema": { "type": "object", "properties": { "id": { "type": "string" } }, "required": ["id"] } },
            { "name": "cliquer", "description": "Navigateur prêté : clique sur l'élément numéroté par le dernier lire_page.",
              "inputSchema": { "type": "object", "properties": { "id": { "type": "string" }, "element": { "type": "integer" } }, "required": ["id", "element"] } },
            { "name": "saisir", "description": "Navigateur prêté : tape un texte dans l'élément numéroté (champ), puis Entrée si entree vaut true.",
              "inputSchema": { "type": "object", "properties": { "id": { "type": "string" }, "element": { "type": "integer" }, "texte": { "type": "string" }, "entree": { "type": "boolean" } }, "required": ["id", "element", "texte"] } },
            { "name": "capture", "description": "Navigateur prêté : image de la page (JPEG).",
              "inputSchema": { "type": "object", "properties": { "id": { "type": "string" } }, "required": ["id"] } }
        ])
    }

    /// Un message JSON-RPC 2.0 du serveur MCP. `None` : notification, rien à répondre.
    ///
    /// # Errors
    /// [`AgentError::Unauthorized`] si le jeton est absent ou faux (aucun traitement avant cette vérification).
    pub async fn mcp(&self, bearer: Option<&str>, body: &Value) -> Result<Option<Value>, AgentError> {
        self.mcp_as(None, bearer, body).await
    }

    /// Comme [`Agents::mcp`], pour un agent qui tourne sur une autre station : si le compte du jeton n'est pas hébergé ici et
    /// que `station` (PeerID de la station du membre) est donnée, l'appel lui est relayé par le maillage, et l'adresse du
    /// navigateur prêté est rendue en boucle locale **ici** (accès local ouvert vers la station du membre).
    ///
    /// # Errors
    /// Comme [`Agents::mcp`] ; station injoignable.
    pub async fn mcp_via(&self, bearer: Option<&str>, station: Option<&str>, body: &Value) -> Result<Option<Value>, AgentError> {
        match self.mcp_as(None, bearer, body).await {
            Err(AgentError::Unauthorized) if station.is_some() && self.p2p.is_some() => self.relay(station.unwrap_or_default(), bearer.ok_or(AgentError::Unauthorized)?, body).await,
            r => r,
        }
    }

    /// Un appel MCP ; `from` : la station qui le relaie (reconnue par le maillage, jamais par ce qu'elle déclare).
    async fn mcp_as(&self, from: Option<PeerId>, bearer: Option<&str>, body: &Value) -> Result<Option<Value>, AgentError> {
        let (owner, agent) = self.authenticate(bearer.ok_or(AgentError::Unauthorized)?).await?;
        // Un agent déclaré sur une autre station ne parle que par elle ; un agent d'ici ne passe jamais par une autre.
        let allowed = match (&agent.station, from) {
            (None, None) => true,
            (Some(s), Some(p)) => *s == p.to_string(),
            _ => false,
        };
        if !allowed {
            return Err(AgentError::Unauthorized);
        }
        let id = body.get("id").cloned();
        let method = body.get("method").and_then(Value::as_str).unwrap_or_default();
        let reply = |result: Value| id.clone().map(|id| json!({ "jsonrpc": "2.0", "id": id, "result": result }));
        let fail = |code: i64, message: &str| id.clone().map(|id| json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }));
        Ok(match method {
            "initialize" => {
                if let Some(c) = body.get("params").and_then(client_label) {
                    locked(&self.clients).insert(format!("{owner}:{}", agent.id), c);
                }
                reply(json!({ "protocolVersion": SESSION_TOOLS, "capabilities": { "tools": {} }, "serverInfo": { "name": "astroportd", "version": env!("CARGO_PKG_VERSION") },
                "instructions": "Ce serveur ne remet jamais de mot de passe. demander_acces prévient le membre, qui accorde ou refuse ; interrogez ensuite statut_acces. En mode navigateur, attendez l'état granted puis connectez-vous à browser_ws_endpoint (CDP) ; appelez rendre_acces quand vous avez fini." }))
            }
            "ping" => reply(json!({})),
            "tools/list" => reply(json!({ "tools": Self::tools() })),
            "tools/call" => {
                let params = body.get("params").cloned().unwrap_or(Value::Null);
                let args = params.get("arguments").cloned().unwrap_or(Value::Null);
                let text = |k: &str| args.get(k).and_then(Value::as_str).unwrap_or_default().to_owned();
                let as_text = |r: Result<Value, AgentError>| r.map(|v| vec![json!({ "type": "text", "text": v.to_string() })]);
                let outcome = match params.get("name").and_then(Value::as_str) {
                    Some("demander_acces") => as_text(self.ask(&owner, &agent, &text("site"), &text("raison"), &text("mode")).await),
                    Some("statut_acces") => as_text(self.status(&owner, &agent, &text("id"))),
                    Some("rendre_acces") => as_text(self.give_back(&owner, &agent, &text("id")).await),
                    Some(t @ ("naviguer" | "lire_page" | "cliquer" | "saisir" | "capture")) => self.browse_tool(&owner, &agent, t, &args).await,
                    _ => return Ok(fail(-32602, "outil inconnu")),
                };
                reply(match outcome {
                    Ok(content) => json!({ "content": content, "isError": false }),
                    Err(AgentError::Internal(e)) => {
                        eprintln!("agents : {e}");
                        json!({ "content": [{ "type": "text", "text": "erreur interne de la station" }], "isError": true })
                    }
                    Err(e) => json!({ "content": [{ "type": "text", "text": e.to_string() }], "isError": true }),
                })
            }
            m if m.starts_with("notifications/") => None,
            _ => fail(-32601, "méthode inconnue"),
        })
    }
}

impl Agents {
    /// Relaie un appel MCP vers la station `station` du membre, puis rend l'adresse du navigateur prêté en boucle locale ici.
    async fn relay(&self, station: &str, token: &str, body: &Value) -> Result<Option<Value>, AgentError> {
        let (handle, forwards) = self.p2p.as_ref().ok_or(AgentError::Unauthorized)?;
        let peer: PeerId = station.parse().map_err(|_| AgentError::Invalid("station inconnue".into()))?;
        let protocol = protocol_name(REMOTE_SERVICE, &peer).map_err(|e| AgentError::Internal(e.to_string()))?;
        let exchange = async {
            let mut stream = handle.control().open_stream(peer, protocol).await.map_err(|e| format!("station du membre injoignable : {e}"))?.compat();
            let mut line = json!({ "token": token, "body": body }).to_string();
            line.push('\n');
            stream.write_all(line.as_bytes()).await.map_err(|e| e.to_string())?;
            stream.flush().await.map_err(|e| e.to_string())?;
            let mut buf = Vec::new();
            tokio::io::BufReader::new(stream).take(REMOTE_MAX).read_until(b'\n', &mut buf).await.map_err(|e| e.to_string())?;
            serde_json::from_slice::<Value>(&buf).map_err(|_| "réponse illisible de la station du membre".to_owned())
        };
        let wire = tokio::time::timeout(std::time::Duration::from_secs(60), exchange).await.map_err(|_| AgentError::Internal("la station du membre ne répond pas".into()))?.map_err(AgentError::Internal)?;
        if wire["err"] == "unauthorized" {
            return Err(AgentError::Unauthorized);
        }
        if let Some(e) = wire["err"].as_str() {
            return Err(AgentError::Invalid(e.to_owned()));
        }
        let mut reply = wire.get("ok").cloned().filter(|v| !v.is_null());
        // Navigateur prêté par le maillage : accès local ouvert ici, adresse réécrite (même forme qu'en local pour l'agent).
        if let Some(text) = reply.as_ref().and_then(|r| r.pointer("/result/content/0/text")).and_then(Value::as_str) {
            if let Ok(mut v) = serde_json::from_str::<Value>(text) {
                if let (Some(service), Some(path)) = (v.pointer("/browser/p2p/service").and_then(Value::as_str).map(str::to_owned), v.pointer("/browser/path").and_then(Value::as_str).map(str::to_owned)) {
                    let id = v["id"].as_str().unwrap_or_default().to_owned();
                    let until = v["until"].as_i64().unwrap_or(0);
                    let now_s = i64::try_from(now()).unwrap_or(0);
                    let addr = forwards.open(&format!("agent-{id}"), station, &service, until, now_s).await.map_err(AgentError::Internal)?;
                    if let Some(o) = v.as_object_mut() {
                        o.remove("browser");
                        o.insert("browser_ws_endpoint".into(), Value::String(format!("ws://{addr}{path}")));
                    }
                    if let Some(t) = reply.as_mut().and_then(|r| r.pointer_mut("/result/content/0/text")) {
                        *t = Value::String(v.to_string());
                    }
                }
            }
        }
        Ok(reply)
    }

    /// Reçoit les appels MCP relayés par les stations des agents (service [`REMOTE_SERVICE`]). La station qui appelle est celle
    /// du flux du maillage ; elle doit être la station déclarée de l'agent du jeton.
    pub fn serve_remote(self: &Arc<Self>) -> Option<tokio::task::JoinHandle<()>> {
        let (handle, _) = self.p2p.as_ref()?;
        let protocol = protocol_name(REMOTE_SERVICE, &handle.peer_id()).ok()?;
        let mut incoming = match handle.control().accept(protocol) {
            Ok(i) => i,
            Err(e) => {
                eprintln!("agents : relais MCP indisponible : {e}");
                return None;
            }
        };
        let me = Arc::downgrade(self);
        let limit = Arc::new(tokio::sync::Semaphore::new(REMOTE_PARALLEL));
        Some(tokio::spawn(async move {
            while let Some((peer, stream)) = incoming.next().await {
                let Ok(permit) = limit.clone().try_acquire_owned() else { continue };
                let Some(agents) = me.upgrade() else { return };
                tokio::spawn(async move {
                    let _permit = permit;
                    let mut stream = stream.compat();
                    let mut buf = Vec::new();
                    let read = tokio::time::timeout(std::time::Duration::from_secs(20), tokio::io::BufReader::new(&mut stream).take(REMOTE_MAX).read_until(b'\n', &mut buf)).await;
                    if !matches!(read, Ok(Ok(n)) if n > 0) {
                        return;
                    }
                    let msg: Value = serde_json::from_slice(&buf).unwrap_or(Value::Null);
                    let token = msg["token"].as_str().map(str::to_owned);
                    let wire = match agents.mcp_as(Some(peer), token.as_deref(), &msg["body"]).await {
                        Ok(v) => json!({ "ok": v }),
                        Err(AgentError::Unauthorized) => json!({ "err": "unauthorized" }),
                        Err(AgentError::Internal(e)) => {
                            eprintln!("agents : {e}");
                            json!({ "err": "erreur interne de la station" })
                        }
                        Err(e) => json!({ "err": e.to_string() }),
                    };
                    let mut out = wire.to_string();
                    out.push('\n');
                    let _ = stream.write_all(out.as_bytes()).await;
                    let _ = stream.flush().await;
                });
            }
        }))
    }
}

/// Propriétaire du navigateur et de la porte d'une session. Il est le seul à faire avancer son état (`Login` → `Granted` →
/// `Closed`) : l'échéance, l'arrêt du navigateur, « Reprendre la main » et `rendre_acces` ne peuvent pas se croiser.
struct Supervisor {
    id: String,
    owner: String,
    agent_id: String,
    agent_name: String,
    policy: Arc<cdp::Policy>,
    requests: Requests,
    sessions: Sessions,
    db: Arc<Db>,
    activity: Option<ActivityBus>,
    /// Agent d'une autre station : son PeerID, et le maillage pour lui ouvrir la porte.
    remote: Option<(PeerId, P2pHandle)>,
}

impl Supervisor {
    async fn run(self, browser: BrowserProc, mut rx: mpsc::UnboundedReceiver<Ctl>, mut deadline: u64) {
        let stats = Arc::new(Mutex::new(cdp::Stats::default()));
        if let Some(c) = locked(&self.sessions).get_mut(&self.id) {
            c.pilot = Some(Arc::new(Pilot::new(browser.mux(), self.policy.clone())));
        }
        // L'écran que le membre verra dans l'application ; sans lui, la session continue (fenêtre sur la station).
        match Screen::start(browser.mux()).await {
            Ok(s) => {
                if let Some(c) = locked(&self.sessions).get_mut(&self.id) {
                    c.screen = Some(s);
                }
            }
            Err(e) => eprintln!("agents : écran du navigateur indisponible : {e}"),
        }
        // Garde de navigation : désarmée pendant la connexion du membre, armée au prêt. Sans elle, pas de prêt.
        let guard = match Guard::start(browser.mux(), self.policy.clone(), stats.clone()).await {
            Ok(g) => Some(g),
            Err(e) => {
                eprintln!("agents : garde de navigation indisponible : {e}");
                None
            }
        };
        let mut door: Option<gate::Gate> = None;
        // Porte ouverte par le maillage à la seule station de l'agent (tunnel DRAGON `/x/cdp-<demande>-<PeerID>`).
        let mut shared: Option<Exposed> = None;
        let (reason, ack) = loop {
            let wait = std::time::Duration::from_secs(deadline.saturating_sub(now()));
            tokio::select! {
                () = browser.exited() => {
                    let why = browser.log_tail();
                    if !why.is_empty() {
                        eprintln!("agents : le navigateur s'est arrêté : {why}");
                    }
                    break ("ended", None);
                }
                () = tokio::time::sleep(wait) => break ("expired", None),
                c = rx.recv() => match c {
                    None => break ("ended", None),
                    Some(Ctl::Stop { reason, reply }) => break (reason, Some(reply)),
                    Some(Ctl::Ready { until, reply }) => {
                        if door.is_some() {
                            let _ = reply.send(Err("navigateur déjà prêté".into()));
                            continue;
                        }
                        let Some(g) = &guard else {
                            let _ = reply.send(Err("la garde de navigation n'a pas pu démarrer : le navigateur ne sera pas prêté".into()));
                            continue;
                        };
                        g.arm();
                        match gate::open(browser.mux(), self.policy.clone(), stats.clone()).await {
                            Ok(g) => {
                                let endpoint = g.endpoint();
                                let mut remote = None;
                                if let Some((peer, handle)) = &self.remote {
                                    let service = format!("cdp-{}", self.id.get(..8).unwrap_or(&self.id));
                                    let opts = ExposeOpts { allow: Some(std::iter::once(*peer).collect()), max_streams: 4, ..ExposeOpts::default() };
                                    match expose(handle, &service, g.addr().to_string(), opts) {
                                        Ok(x) => {
                                            shared = Some(x);
                                            let path = endpoint.find("/cdp/").map_or("", |i| &endpoint[i..]).to_owned();
                                            remote = Some(json!({ "p2p": { "service": service, "peer": handle.peer_id().to_string() }, "path": path }));
                                        }
                                        Err(e) => {
                                            let _ = reply.send(Err(format!("navigateur impossible à ouvrir à la station de l'agent : {e}")));
                                            continue;
                                        }
                                    }
                                }
                                door = Some(g);
                                deadline = until;
                                if let Some(r) = locked(&self.requests).get_mut(&self.id) {
                                    r.state = State::Granted;
                                    r.until = until;
                                }
                                if let Some(s) = locked(&self.sessions).get_mut(&self.id) {
                                    s.endpoint = Some(endpoint);
                                    s.remote = remote;
                                }
                                audit_to(&self.db, json!({ "event": "session_ready", "hex": self.owner, "agent": self.agent_id, "request": self.id, "until": until })).await;
                                self.say(format!("{} agit pour vous jusqu'à l'échéance choisie", self.agent_name));
                                let _ = reply.send(Ok(String::new()));
                            }
                            Err(e) => { let _ = reply.send(Err(format!("porte du navigateur impossible à ouvrir : {e}"))); }
                        }
                    }
                }
            }
        };
        // Coupe l'agent, puis tue le navigateur et efface son profil, puis seulement dit que c'est fini.
        drop(shared);
        drop(door);
        drop(guard);
        drop(browser);
        locked(&self.sessions).remove(&self.id);
        if let Some(r) = locked(&self.requests).get_mut(&self.id) {
            r.state = State::Closed(reason);
            r.until = now();
        }
        let summary = locked(&stats).summary();
        audit_to(&self.db, json!({ "event": "session_end", "hex": self.owner, "agent": self.agent_id, "request": self.id, "reason": reason, "stats": summary })).await;
        self.say(match reason {
            "returned" => format!("{} a rendu la main ; le navigateur prêté a été détruit", self.agent_name),
            "revoked" => "Vous avez repris la main ; le navigateur prêté a été détruit".to_owned(),
            "expired" => format!("Temps écoulé : {} n'a plus accès, le navigateur prêté a été détruit", self.agent_name),
            _ => "Le navigateur prêté s'est fermé".to_owned(),
        });
        if let Some(a) = ack {
            let _ = a.send(());
        }
    }

    fn say(&self, message: String) {
        if let Some(bus) = &self.activity {
            bus.publish_to(Audience::Owner(self.owner.clone()), "agent", "agents", message, Some(json!({ "request": self.id, "agent": self.agent_name })));
        }
    }
}

/// Dossier des profils jetables sous le dossier de données `data_dir`, vidé de ce qu'un arrêt brutal y a laissé.
#[must_use]
pub fn browser_profiles_dir(data_dir: &std::path::Path) -> PathBuf {
    let dir = data_dir.join(agent_browser::PROFILES_DIR);
    let n = agent_browser::sweep(&dir);
    if n > 0 {
        eprintln!("agents : {n} navigateur(s) prêté(s) laissé(s) par un arrêt brutal, arrêté(s) et effacé(s)");
    }
    dir
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use astro_crypto::StaticKeySource;

    const ALICE: [u8; 32] = [7; 32];

    fn hex_of(secret: &[u8; 32]) -> String {
        astro_nostr_proto::pubkey_of(secret).unwrap()
    }

    async fn rig(tag: &str) -> (std::path::PathBuf, Arc<Db>, Agents, Arc<Mutex<Vec<astro_nostr_proto::Event>>>) {
        let dir = std::env::temp_dir().join(format!("astroportd-agents-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
        let published: Arc<Mutex<Vec<astro_nostr_proto::Event>>> = Arc::default();
        let (sink, store) = (published.clone(), db.clone());
        // La publication de la station écrit l'événement puis le projette ; ici on rejoue la projection du registre à la main.
        let publish: PublishFn = Arc::new(move |e| {
            let (sink, store) = (sink.clone(), store.clone());
            Box::pin(async move {
                store
                    .writer()
                    .execute("INSERT INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at", vec![KV_SCOPE.to_owned().into(), e.pubkey.clone().into(), e.content.clone().into(), i64::try_from(e.created_at).unwrap_or(0).into()])
                    .await
                    .map_err(|e| e.to_string())?;
                sink.lock().unwrap().push(e);
                Ok(())
            })
        });
        let keys: Arc<dyn KeySource> = Arc::new(StaticKeySource::new().with(ALICE));
        (dir, db.clone(), Agents::new(db, keys, publish), published)
    }

    fn token_of(v: &Value) -> String {
        v["token"].as_str().unwrap().to_owned()
    }

    async fn call(a: &Agents, token: &str, method: &str, params: Value) -> Value {
        a.mcp(Some(token), &json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params })).await.unwrap().unwrap()
    }

    #[test]
    fn only_a_bare_host_name_is_accepted() {
        assert_eq!(valid_host(" Example.ORG. ").unwrap(), "example.org");
        for bad in ["", "localhost", "https://example.org", "example.org/login", "example.org:8443", "user@example.org", "-a.example.org", "a b.example.org", "10.0.0.1", "exa_mple.org", "example..org"] {
            assert!(valid_host(bad).is_err(), "{bad}");
        }
    }

    #[tokio::test]
    async fn the_registry_is_a_signed_encrypted_31903_and_keeps_only_token_hashes() {
        let (dir, _, agents, published) = rig("registry").await;
        let owner = hex_of(&ALICE);
        let made = agents.enroll(&owner, "BRO", "Agents", 600, false).await.unwrap();
        let token = token_of(&made);
        assert!(token.starts_with(&format!("ag1_{}_", &owner[..16])) && token.len() == 4 + 16 + 1 + 64);
        let events = published.lock().unwrap().clone();
        assert_eq!(events.len(), 1);
        let ev = &events[0];
        assert_eq!((ev.kind, ev.pubkey.as_str(), ev.tag_value("d")), (31903, owner.as_str(), Some("agents")));
        ev.verify().unwrap();
        assert!(!ev.content.contains("BRO") && !ev.content.contains(&token), "contenu chiffré pour le seul membre");
        let plain = nip44::decrypt_from_self(&ALICE, &owner, &ev.content).unwrap();
        assert!(plain.contains(&sha_hex(&token)) && !plain.contains(&token), "l'empreinte, jamais le jeton");
        // La liste ne montre ni jeton ni empreinte.
        let listed = agents.list(&owner).await.unwrap().to_string();
        assert!(listed.contains("BRO") && !listed.contains(&sha_hex(&token)) && !listed.contains("token"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn mcp_asks_the_owner_and_never_returns_a_secret() {
        let (dir, db, agents, _) = rig("mcp").await;
        let bus = ActivityBus::new();
        let agents = agents.with_activity(bus.clone());
        let owner = hex_of(&ALICE);
        let token = token_of(&agents.enroll(&owner, "Claude Code", "Agents", 120, false).await.unwrap());

        let init = call(&agents, &token, "initialize", json!({ "clientInfo": { "name": "OpenClaw\u{7}", "version": "0.9.1" } })).await;
        assert_eq!(init["result"]["serverInfo"]["name"], "astroportd");
        let tools = call(&agents, &token, "tools/list", json!({})).await;
        let names: Vec<&str> = tools["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["demander_acces", "statut_acces", "rendre_acces", "naviguer", "lire_page", "cliquer", "saisir", "capture"]);
        assert!(agents.mcp(Some(&token), &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })).await.unwrap().is_none());

        let asked = call(&agents, &token, "tools/call", json!({ "name": "demander_acces", "arguments": { "site": "Example.org", "raison": "relever ma facture" } })).await;
        assert_eq!(asked["result"]["isError"], false);
        let id = serde_json::from_str::<Value>(asked["result"]["content"][0]["text"].as_str().unwrap()).unwrap()["id"].as_str().unwrap().to_owned();
        let mine = agents.requests(&owner);
        assert_eq!((mine["requests"][0]["site"].as_str(), mine["requests"][0]["state"].as_str()), (Some("example.org"), Some("pending")));
        // L'appelant est nommé : le programme tel qu'il s'est présenté à initialize (caractères de contrôle retirés).
        assert_eq!((mine["requests"][0]["client"].as_str(), mine["requests"][0]["remote"].as_bool()), (Some("OpenClaw 0.9.1"), Some(false)));
        let seen: Vec<_> = bus.recent(10).iter().filter(|a| a.kind == "agent").cloned().collect();
        assert!(seen.len() == 1 && seen[0].message.contains("Claude Code (OpenClaw 0.9.1) demande l'accès à example.org"), "{seen:?}");

        let status = |t: String| {
            let (agents, id) = (&agents, id.clone());
            async move { call(agents, &t, "tools/call", json!({ "name": "statut_acces", "arguments": { "id": id } })).await }
        };
        let before = status(token.clone()).await;
        assert!(before["result"]["content"][0]["text"].as_str().unwrap().contains("\"pending\""));
        // Le membre accorde ; la durée est plafonnée par l'agent (120 s), même si 3600 est demandé.
        let granted = agents.decide(&owner, &id, true, Some(3600)).await.unwrap();
        assert_eq!(granted["state"], "granted");
        let until = granted["until"].as_u64().unwrap();
        assert!(until <= now() + 120 && until > now() + 60, "plafonné à 120 s");
        let text = status(token.clone()).await["result"]["content"][0]["text"].as_str().unwrap().to_owned();
        assert!(text.contains("\"granted\"") && !text.to_lowercase().contains("password") && !text.contains("secret"));
        assert!(agents.decide(&owner, &id, false, None).await.is_err(), "déjà tranchée");

        // Trace d'audit : qui a demandé quoi, jamais de secret.
        let audit: Vec<String> = db.readers().read(|c| Ok(c.prepare("SELECT data FROM journal WHERE stream = 'audit.agent' ORDER BY id")?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?)).await.unwrap();
        assert!(audit.iter().any(|d| d.contains("\"ask\"") && d.contains("example.org")) && audit.iter().any(|d| d.contains("\"grant\"")), "{audit:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Un agent qui demande puis rend aussitôt (cas réel : OpenClaw « essaie » le flux) : le membre en garde la trace.
    #[tokio::test]
    async fn a_request_withdrawn_at_once_still_shows_to_the_member() {
        let (dir, _, agents, _) = rig("withdrawn").await;
        let bus = ActivityBus::new();
        let agents = agents.with_activity(bus.clone());
        let owner = hex_of(&ALICE);
        let token = token_of(&agents.enroll(&owner, "BORIS", "Agents", 600, false).await.unwrap());
        let asked = call(&agents, &token, "tools/call", json!({ "name": "demander_acces", "arguments": { "site": "example.org" } })).await;
        let id = serde_json::from_str::<Value>(asked["result"]["content"][0]["text"].as_str().unwrap()).unwrap()["id"].as_str().unwrap().to_owned();
        let back = call(&agents, &token, "tools/call", json!({ "name": "rendre_acces", "arguments": { "id": id } })).await;
        assert_eq!(back["result"]["isError"], false, "{back}");

        let mine = agents.requests(&owner);
        assert!(mine["requests"].as_array().unwrap().is_empty(), "plus en cours : {mine}");
        assert_eq!((mine["recent"][0]["site"].as_str(), mine["recent"][0]["state"].as_str(), mine["recent"][0]["agent"].as_str()), (Some("example.org"), Some("returned"), Some("BORIS")), "{mine}");
        let said: Vec<String> = bus.recent(10).iter().filter(|a| a.kind == "agent").map(|a| a.message.clone()).collect();
        assert!(said.iter().any(|m| m.contains("BORIS a retiré sa demande d'accès à example.org")), "{said:?}");

        // Une demande refusée n'est plus « en cours » : elle passe dans les récentes.
        let asked = call(&agents, &token, "tools/call", json!({ "name": "demander_acces", "arguments": { "site": "example.com" } })).await;
        let id = serde_json::from_str::<Value>(asked["result"]["content"][0]["text"].as_str().unwrap()).unwrap()["id"].as_str().unwrap().to_owned();
        agents.decide(&owner, &id, false, None).await.unwrap();
        let mine = agents.requests(&owner);
        // (Même seconde que la précédente : l'ordre entre elles n'est pas fixé, on la cherche par son site.)
        let denied = mine["recent"].as_array().unwrap().iter().find(|r| r["site"] == "example.com").map(|r| r["state"].clone());
        assert!(mine["requests"].as_array().unwrap().is_empty() && denied == Some(json!("denied")), "{mine}");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Une demande restée sans réponse : à l'échéance, le journal et le membre le savent, une seule fois.
    #[tokio::test]
    async fn an_unanswered_request_lapses_into_the_journal_once() {
        let (dir, db, agents, _) = rig("lapse").await;
        let bus = ActivityBus::new();
        let agents = agents.with_activity(bus.clone());
        let owner = hex_of(&ALICE);
        let token = token_of(&agents.enroll(&owner, "BORIS", "Agents", 600, false).await.unwrap());
        let asked = call(&agents, &token, "tools/call", json!({ "name": "demander_acces", "arguments": { "site": "framagit.org" } })).await;
        let id = serde_json::from_str::<Value>(asked["result"]["content"][0]["text"].as_str().unwrap()).unwrap()["id"].as_str().unwrap().to_owned();
        assert_eq!(agents.note_lapses().await, 0, "pas encore échue");
        locked(&agents.requests).get_mut(&id).unwrap().created -= REQUEST_TTL_SECS + 1;
        assert_eq!(agents.note_lapses().await, 1);
        assert_eq!(agents.note_lapses().await, 0, "une seule fois");
        assert_eq!(agents.requests(&owner)["recent"][0]["state"], "expired");
        let audit: Vec<String> = db.readers().read(|c| Ok(c.prepare("SELECT data FROM journal WHERE stream = 'audit.agent' ORDER BY id")?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?)).await.unwrap();
        assert!(audit.iter().any(|d| d.contains("\"expired\"") && d.contains(&id) && d.contains("\"pending\"")), "{audit:?}");
        let said: Vec<String> = bus.recent(10).iter().filter(|a| a.kind == "agent").map(|a| a.message.clone()).collect();
        assert!(said.iter().any(|m| m.contains("La demande de BORIS pour framagit.org a expiré sans réponse")), "{said:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn bad_tokens_bad_sites_flooding_and_revocation_are_refused() {
        let (dir, _, agents, _) = rig("hostile").await;
        let owner = hex_of(&ALICE);
        let token = token_of(&agents.enroll(&owner, "BRO", "Agents", 600, false).await.unwrap());
        let ping = json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" });
        // Jeton absent, mal formé, bien formé mais inconnu, ou changé d'un caractère : rien n'est traité.
        let mut flipped = token.clone();
        flipped.pop();
        flipped.push(if token.ends_with('0') { '1' } else { '0' });
        for bad in [None, Some(""), Some("Bearer x"), Some("ag1_zz"), Some(flipped.as_str()), Some(&format!("ag1_{}_{}", &owner[..16], "00".repeat(32)))] {
            assert_eq!(agents.mcp(bad, &ping).await.unwrap_err(), AgentError::Unauthorized, "{bad:?}");
        }
        let a = agents.mcp(Some(&token), &ping).await.unwrap().unwrap();
        assert_eq!(a["result"], json!({}));

        // Un site invalide est une erreur d'outil, pas une demande.
        let r = call(&agents, &token, "tools/call", json!({ "name": "demander_acces", "arguments": { "site": "https://example.org/x" } })).await;
        assert_eq!(r["result"]["isError"], true);
        assert!(agents.requests(&owner)["requests"].as_array().unwrap().is_empty());
        let unknown = call(&agents, &token, "tools/call", json!({ "name": "lire_mot_de_passe", "arguments": {} })).await;
        assert_eq!(unknown["error"]["code"], -32602);

        // Au plus 5 demandes en attente.
        for n in 0..MAX_PENDING {
            let r = call(&agents, &token, "tools/call", json!({ "name": "demander_acces", "arguments": { "site": format!("s{n}.example.org") } })).await;
            assert_eq!(r["result"]["isError"], false, "{n}");
        }
        let flood = call(&agents, &token, "tools/call", json!({ "name": "demander_acces", "arguments": { "site": "de-trop.example.org" } })).await;
        assert_eq!(flood["result"]["isError"], true);

        // Retrait : le jeton ne marche plus, les demandes tombent.
        let id = agents.list(&owner).await.unwrap()["agents"][0]["id"].as_str().unwrap().to_owned();
        agents.revoke(&owner, &id).await.unwrap();
        assert_eq!(agents.mcp(Some(&token), &ping).await.unwrap_err(), AgentError::Unauthorized);
        assert!(agents.requests(&owner)["requests"].as_array().unwrap().is_empty());
        assert_eq!(agents.revoke(&owner, &id).await.unwrap_err(), AgentError::NotFound);
        let _ = std::fs::remove_dir_all(dir);
    }

    fn text_of(v: &Value) -> Value {
        serde_json::from_str(v["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
    }

    async fn browsing_rig(tag: &str) -> (std::path::PathBuf, Arc<Db>, Agents, Arc<agent_browser::fake::FakeLauncher>, String, String) {
        let (dir, db, agents, _) = rig(tag).await;
        let launcher = Arc::new(agent_browser::fake::FakeLauncher::default());
        let agents = agents.with_browser(launcher.clone(), dir.join("agent-browsers"), true).with_activity(ActivityBus::new());
        let owner = hex_of(&ALICE);
        let token = token_of(&agents.enroll(&owner, "OpenClaw", "Agents", 600, false).await.unwrap());
        (dir, db, agents, launcher, owner, token)
    }

    async fn ask_browser(agents: &Agents, token: &str) -> String {
        let r = call(agents, token, "tools/call", json!({ "name": "demander_acces", "arguments": { "site": "linkedin.com", "raison": "publier le résumé", "mode": "navigateur" } })).await;
        assert_eq!(r["result"]["isError"], false, "{r}");
        text_of(&r)["id"].as_str().unwrap().to_owned()
    }

    async fn status_of(agents: &Agents, token: &str, id: &str) -> Value {
        text_of(&call(agents, token, "tools/call", json!({ "name": "statut_acces", "arguments": { "id": id } })).await)
    }

    #[tokio::test]
    async fn the_member_logs_in_then_lends_the_browser_and_the_agent_gives_it_back() {
        use futures_util::{SinkExt, StreamExt};
        use std::sync::atomic::Ordering;
        use tokio_tungstenite::tungstenite::Message;
        let (dir, db, agents, launcher, owner, token) = browsing_rig("browse").await;
        let id = ask_browser(&agents, &token).await;
        assert_eq!(status_of(&agents, &token, &id).await["mode"], "navigateur");

        // Le membre accepte : le navigateur s'ouvre, il s'y connecte ; l'agent n'a encore rien.
        let opened = agents.decide(&owner, &id, true, Some(300)).await.unwrap();
        assert_eq!(opened["state"], "login");
        assert_eq!(launcher.alive.load(Ordering::SeqCst), 1);
        assert!(dir.join("agent-browsers").join(&id).is_dir(), "profil jetable créé");
        let waiting = status_of(&agents, &token, &id).await;
        assert!(waiting["state"] == "login" && waiting.get("browser_ws_endpoint").is_none(), "{waiting}");

        // « C'est prêt » : la porte s'ouvre pour la durée choisie.
        let lent = agents.ready(&owner, &id).await.unwrap();
        assert_eq!(lent["state"], "granted");
        let until = lent["until"].as_u64().unwrap();
        assert!(until > now() + 250 && until <= now() + 300, "la durée court depuis le prêt");
        let st = status_of(&agents, &token, &id).await;
        let endpoint = st["browser_ws_endpoint"].as_str().unwrap().to_owned();
        assert!(endpoint.starts_with("ws://127.0.0.1:") && endpoint.contains("/cdp/"));
        // L'adresse ne sort ni vers l'écran du membre, ni vers le journal, ni vers un autre agent du membre.
        assert!(!agents.requests(&owner).to_string().contains("/cdp/"));
        let other = token_of(&agents.enroll(&owner, "Autre", "Agents", 600, false).await.unwrap());
        let spy = call(&agents, &other, "tools/call", json!({ "name": "statut_acces", "arguments": { "id": id } })).await;
        assert_eq!(spy["result"]["isError"], true);
        assert!(agents.ready(&owner, &id).await.is_err(), "déjà prêté");

        // L'agent pilote, à travers le filtre.
        let (mut ws, _) = tokio_tungstenite::connect_async(&endpoint).await.unwrap();
        ws.send(Message::text(json!({ "id": 1, "method": "Network.getAllCookies" }).to_string())).await.unwrap();
        let Some(Ok(Message::Text(t))) = ws.next().await else { unreachable!() };
        assert!(t.contains("refusé"), "{t}");

        // rendre_acces : coupé, tué, effacé, tracé.
        let back = call(&agents, &token, "tools/call", json!({ "name": "rendre_acces", "arguments": { "id": id } })).await;
        assert_eq!(text_of(&back)["state"], "returned", "{back}");
        assert_eq!(launcher.alive.load(Ordering::SeqCst), 0);
        assert!(!dir.join("agent-browsers").join(&id).exists(), "profil effacé");
        let end = tokio::time::timeout(std::time::Duration::from_secs(5), ws.next()).await.unwrap();
        assert!(!matches!(end, Some(Ok(Message::Text(_)))), "agent coupé : {end:?}");
        assert!(tokio_tungstenite::connect_async(&endpoint).await.is_err(), "porte fermée");
        let st = status_of(&agents, &token, &id).await;
        assert!(st["state"] == "returned" && st.get("browser_ws_endpoint").is_none(), "{st}");
        let audit: Vec<String> = db.readers().read(|c| Ok(c.prepare("SELECT data FROM journal WHERE stream = 'audit.agent' ORDER BY id")?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?)).await.unwrap();
        let joined = audit.join("\n");
        for ev in ["session_open", "session_ready", "session_end"] {
            assert!(joined.contains(ev), "{ev} : {joined}");
        }
        assert!(joined.contains("\"denied_calls\":1") && !joined.contains("/cdp/"), "{joined}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn the_member_sees_the_browser_in_the_app_and_only_he_can_drive_it() {
        let (dir, _, agents, _, owner, token) = browsing_rig("screen").await;
        let id = ask_browser(&agents, &token).await;
        assert!(matches!(agents.screen_watch(&owner, &id).await, Err(AgentError::Invalid(_))), "pas d'écran avant l'accord");
        agents.decide(&owner, &id, true, None).await.unwrap();
        // L'écran arrive avec le navigateur (le superviseur l'ouvre juste après le lancement).
        let mut rx = None;
        for _ in 0..50 {
            if let Ok(r) = agents.screen_watch(&owner, &id).await {
                rx = Some(r);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let mut rx = rx.unwrap();
        let frame = tokio::time::timeout(std::time::Duration::from_secs(5), rx.wait_for(Option::is_some)).await.unwrap().unwrap().clone().unwrap();
        let f: Value = serde_json::from_str(&frame).unwrap();
        assert_eq!((f["w"].as_f64(), f["jpeg"].as_str()), (Some(800.0), Some("SlBFRw==")));
        assert_eq!(agents.screen_input(&owner, &id, &json!({ "type": "click", "x": 0.5, "y": 0.5 })).await.unwrap()["ok"], true);
        assert!(matches!(agents.screen_input(&owner, &id, &json!({ "type": "eval", "code": "1" })).await, Err(AgentError::Invalid(_))));
        let stranger = "ff".repeat(32);
        assert_eq!(agents.screen_watch(&stranger, &id).await.unwrap_err(), AgentError::NotFound, "l'écran est au membre seul");
        assert_eq!(agents.screen_input(&stranger, &id, &json!({ "type": "text", "text": "x" })).await.unwrap_err(), AgentError::NotFound);
        agents.stop(&owner, &id).await.unwrap();
        assert!(agents.screen_watch(&owner, &id).await.is_err(), "plus d'écran une fois repris");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_cloud_agent_browses_through_the_station_tools_only_once_lent() {
        let (dir, _, agents, launcher, owner, token) = browsing_rig("pilot").await;
        let id = ask_browser(&agents, &token).await;
        let tool = |name: &'static str, args: Value| {
            let (agents, token) = (&agents, token.clone());
            async move { call(agents, &token, "tools/call", json!({ "name": name, "arguments": args })).await["result"].clone() }
        };
        // Avant le prêt : refusé.
        assert_eq!(tool("lire_page", json!({ "id": id })).await["isError"], true);
        agents.decide(&owner, &id, true, None).await.unwrap();
        assert_eq!(tool("lire_page", json!({ "id": id })).await["isError"], true, "pendant la connexion du membre non plus");
        agents.ready(&owner, &id).await.unwrap();
        let other = token_of(&agents.enroll(&owner, "Autre", "Agents", 600, false).await.unwrap());
        let spy = call(&agents, &other, "tools/call", json!({ "name": "lire_page", "arguments": { "id": id } })).await;
        assert_eq!(spy["result"]["isError"], true, "un autre agent ne pilote pas");

        let page: Value = serde_json::from_str(tool("lire_page", json!({ "id": id })).await["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(page["title"], "Fil");
        let els = page["elements"].as_array().unwrap();
        assert_eq!(els.len(), 2, "le décor est écarté : {els:?}");
        assert_eq!((els[0]["n"].as_u64(), els[0]["role"].as_str(), els[0]["name"].as_str()), (Some(1), Some("button"), Some("Publier")));
        assert_eq!(tool("cliquer", json!({ "id": id, "element": 1 })).await["isError"], false);
        assert_eq!(tool("saisir", json!({ "id": id, "element": 2, "texte": "Bilan de la semaine", "entree": true })).await["isError"], false);
        assert_eq!(tool("cliquer", json!({ "id": id, "element": 99 })).await["isError"], true, "numéro inconnu");
        let away = tool("naviguer", json!({ "id": id, "url": "https://evil.example/" })).await;
        assert!(away["isError"] == true && away["content"][0]["text"].as_str().unwrap().contains("hors du site"), "{away}");
        assert_eq!(tool("naviguer", json!({ "id": id, "url": "https://www.linkedin.com/feed" })).await["isError"], false);
        let shot = tool("capture", json!({ "id": id })).await;
        assert_eq!((shot["content"][0]["type"].as_str(), shot["content"][0]["mimeType"].as_str()), (Some("image"), Some("image/jpeg")));

        let seen = launcher.seen.lock().unwrap().clone();
        let press = seen.iter().find(|v| v["method"] == "Input.dispatchMouseEvent" && v["params"]["type"] == "mousePressed").unwrap();
        assert_eq!((press["params"]["x"].as_f64(), press["params"]["y"].as_f64()), (Some(20.0), Some(30.0)), "clic au centre de l'élément");
        assert!(seen.iter().any(|v| v["method"] == "DOM.focus" && v["params"]["backendNodeId"] == 43));
        assert!(seen.iter().any(|v| v["method"] == "Input.insertText" && v["params"]["text"] == "Bilan de la semaine"));
        assert!(!seen.iter().any(|v| v["method"] == "Page.navigate" && v["params"]["url"] == "https://evil.example/"), "rien n'est parti vers l'autre site");
        agents.stop(&owner, &id).await.unwrap();
        assert_eq!(tool("lire_page", json!({ "id": id })).await["isError"], true, "fini une fois repris");
        let _ = std::fs::remove_dir_all(dir);
    }

    async fn p2p_node(seed: u8) -> (astro_p2p::P2pNode, astro_p2p::Multiaddr) {
        let n = astro_p2p::P2pNode::spawn(astro_p2p::P2pConfig::local([seed; 32])).await.unwrap();
        let tcp = n.handle.listen_addrs().into_iter().find(|a| a.to_string().contains("/tcp/")).unwrap();
        let full: astro_p2p::Multiaddr = format!("{tcp}/p2p/{}", n.handle.peer_id()).parse().unwrap();
        (n, full)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_agent_on_another_station_gets_the_browser_through_the_mesh_and_only_there() {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;
        let (na, a_addr) = p2p_node(31).await;
        let (nb, _) = p2p_node(32).await;
        let (nc, _) = p2p_node(33).await;
        nb.handle.dial(a_addr.clone()).await.unwrap();
        nc.handle.dial(a_addr).await.unwrap();
        let (a_peer, b_peer) = (na.handle.peer_id().to_string(), nb.handle.peer_id().to_string());
        // Station A : héberge le membre et prête le navigateur.
        let (dir_a, _, agents_a, _) = rig("remote-a").await;
        let launcher = Arc::new(agent_browser::fake::FakeLauncher::default());
        let fa = Arc::new(Forwards::new(Some(na.handle.clone())));
        let agents_a = Arc::new(agents_a.with_browser(launcher.clone(), dir_a.join("ab"), false).with_p2p(na.handle.clone(), fa));
        let _served = agents_a.serve_remote().unwrap();
        let owner = hex_of(&ALICE);
        let made = agents_a.enroll_on(&owner, "OpenClaw", "Agents", 600, false, Some(&b_peer)).await.unwrap();
        let token = token_of(&made);
        assert_eq!(agents_a.list(&owner).await.unwrap()["agents"][0]["station"], b_peer.as_str());
        // Station B : l'agent y tourne ; elle n'héberge pas le membre et relaie.
        let (dir_b, _, agents_b, _) = rig("remote-b").await;
        let agents_b = agents_b.with_p2p(nb.handle.clone(), Arc::new(Forwards::new(Some(nb.handle.clone()))));
        let (dir_c, _, agents_c, _) = rig("remote-c").await;
        let agents_c = agents_c.with_p2p(nc.handle.clone(), Arc::new(Forwards::new(Some(nc.handle.clone()))));
        let via_b = |name: &'static str, args: Value| {
            let (agents, token, station) = (&agents_b, token.clone(), a_peer.clone());
            async move { agents.mcp_via(Some(&token), Some(&station), &json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": name, "arguments": args } })).await }
        };
        let asked = via_b("demander_acces", json!({ "site": "linkedin.com", "mode": "navigateur" })).await.unwrap().unwrap();
        let id = text_of(&asked)["id"].as_str().unwrap().to_owned();

        // Ni la station C, ni un appel direct à A (sans passer par B), ne valent la station déclarée.
        let ping = json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" });
        assert_eq!(agents_c.mcp_via(Some(&token), Some(&a_peer), &ping).await.unwrap_err(), AgentError::Unauthorized, "station C refusée");
        assert_eq!(agents_a.mcp(Some(&token), &ping).await.unwrap_err(), AgentError::Unauthorized, "l'agent de B ne parle pas directement à A");
        assert!(agents_b.mcp_via(Some(&token), None, &ping).await.is_err(), "sans station du membre, B ne sait pas où relayer");

        agents_a.decide(&owner, &id, true, None).await.unwrap();
        agents_a.ready(&owner, &id).await.unwrap();
        let st = text_of(&via_b("statut_acces", json!({ "id": id })).await.unwrap().unwrap());
        assert!(st["browser_ws_endpoint"].is_string(), "{st}");
        let endpoint = st["browser_ws_endpoint"].as_str().unwrap().to_owned();
        assert!(endpoint.starts_with("ws://127.0.0.1:") && st.get("browser").is_none(), "même forme qu'en local : {st}");
        // L'agent de B pilote le navigateur de A à travers le tunnel, filtre compris.
        let (mut ws, _) = tokio_tungstenite::connect_async(&endpoint).await.unwrap();
        ws.send(Message::text(json!({ "id": 1, "method": "Browser.getVersion" }).to_string())).await.unwrap();
        let first = tokio::time::timeout(std::time::Duration::from_secs(10), ws.next()).await.unwrap();
        assert!(matches!(&first, Some(Ok(Message::Text(t))) if t.contains("Browser.getVersion")), "{first:?}");
        ws.send(Message::text(json!({ "id": 2, "method": "Storage.getCookies" }).to_string())).await.unwrap();
        let denied = tokio::time::timeout(std::time::Duration::from_secs(10), ws.next()).await.unwrap();
        assert!(matches!(&denied, Some(Ok(Message::Text(t))) if t.contains("refusé")), "{denied:?}");
        // La station C n'obtient pas le tunnel, même en connaissant son nom.
        let fc = Forwards::new(Some(nc.handle.clone()));
        let service = format!("cdp-{}", &id[..8]);
        let c_local = fc.open("x", &a_peer, &service, i64::try_from(now()).unwrap() + 60, i64::try_from(now()).unwrap()).await.unwrap();
        let c_try = tokio_tungstenite::connect_async(format!("ws://{c_local}{}", &endpoint[endpoint.find("/cdp/").unwrap()..])).await;
        assert!(c_try.is_err(), "station C refusée par le tunnel");
        // Le membre reprend la main : l'agent de B est coupé.
        agents_a.stop(&owner, &id).await.unwrap();
        let end = tokio::time::timeout(std::time::Duration::from_secs(10), ws.next()).await.unwrap();
        assert!(!matches!(end, Some(Ok(Message::Text(_)))), "{end:?}");
        for d in [dir_a, dir_b, dir_c] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    #[tokio::test]
    async fn the_member_takes_back_control_and_a_dead_browser_ends_the_session() {
        use std::sync::atomic::Ordering;
        let (dir, _, agents, launcher, owner, token) = browsing_rig("stop").await;
        // « Reprendre la main » pendant la connexion.
        let a = ask_browser(&agents, &token).await;
        agents.decide(&owner, &a, true, None).await.unwrap();
        assert_eq!(agents.stop(&owner, &a).await.unwrap()["state"], "revoked");
        assert_eq!(launcher.alive.load(Ordering::SeqCst), 0);
        assert!(agents.ready(&owner, &a).await.is_err(), "plus rien à prêter");
        assert!(agents.stop(&owner, &a).await.is_err(), "déjà arrêtée");

        // Le navigateur s'arrête de lui-même (fermé, planté) : la session se termine, l'agent le voit.
        let b = ask_browser(&agents, &token).await;
        agents.decide(&owner, &b, true, None).await.unwrap();
        agents.ready(&owner, &b).await.unwrap();
        launcher.kills.lock().unwrap().last().unwrap().send(true).unwrap();
        let mut state = Value::Null;
        for _ in 0..50 {
            state = status_of(&agents, &token, &b).await["state"].clone();
            if state == "ended" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(state, "ended");

        // Au plus deux navigateurs prêtés à la fois ; retirer l'agent arrête les siens.
        let ids = [ask_browser(&agents, &token).await, ask_browser(&agents, &token).await, ask_browser(&agents, &token).await];
        agents.decide(&owner, &ids[0], true, None).await.unwrap();
        agents.decide(&owner, &ids[1], true, None).await.unwrap();
        assert!(matches!(agents.decide(&owner, &ids[2], true, None).await, Err(AgentError::Limited(_))));
        assert_eq!(status_of(&agents, &token, &ids[2]).await["state"], "pending", "la demande reste en attente");
        let agent_id = agents.list(&owner).await.unwrap()["agents"][0]["id"].as_str().unwrap().to_owned();
        agents.revoke(&owner, &agent_id).await.unwrap();
        assert_eq!(launcher.alive.load(Ordering::SeqCst), 0, "les navigateurs de l'agent retiré sont arrêtés");
        assert_eq!(std::fs::read_dir(dir.join("agent-browsers")).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn the_browser_mode_is_refused_without_chromium_and_bad_modes_too() {
        let (dir, _, agents, _) = rig("nobrowser").await;
        let owner = hex_of(&ALICE);
        let token = token_of(&agents.enroll(&owner, "OpenClaw", "Agents", 600, false).await.unwrap());
        for mode in ["navigateur", "root", "NAVIGATEUR"] {
            let r = call(&agents, &token, "tools/call", json!({ "name": "demander_acces", "arguments": { "site": "linkedin.com", "mode": mode } })).await;
            assert_eq!(r["result"]["isError"], true, "{mode}");
        }
        // Sans écran sur la station, le mode navigateur reste possible : le membre se connecte depuis l'application.
        let headless = agents.with_browser(Arc::new(agent_browser::fake::FakeLauncher::default()), dir.join("ab"), false);
        let r = call(&headless, &token, "tools/call", json!({ "name": "demander_acces", "arguments": { "site": "linkedin.com", "mode": "navigateur" } })).await;
        assert_eq!(r["result"]["isError"], false, "{r}");
        // Le mode accord reste le défaut, et rendre_acces y met fin aussi.
        let r = call(&headless, &token, "tools/call", json!({ "name": "demander_acces", "arguments": { "site": "linkedin.com" } })).await;
        let id = text_of(&r)["id"].as_str().unwrap().to_owned();
        headless.decide(&owner, &id, true, None).await.unwrap();
        let back = call(&headless, &token, "tools/call", json!({ "name": "rendre_acces", "arguments": { "id": id } })).await;
        assert_eq!(text_of(&back)["state"], "returned");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn another_members_agent_cannot_read_or_decide_my_requests() {
        let (dir, _, agents, _) = rig("isolation").await;
        let owner = hex_of(&ALICE);
        let t1 = token_of(&agents.enroll(&owner, "A", "Agents", 600, false).await.unwrap());
        let t2 = token_of(&agents.enroll(&owner, "B", "Agents", 600, false).await.unwrap());
        let r = call(&agents, &t1, "tools/call", json!({ "name": "demander_acces", "arguments": { "site": "example.org" } })).await;
        let id = serde_json::from_str::<Value>(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap()["id"].as_str().unwrap().to_owned();
        let spy = call(&agents, &t2, "tools/call", json!({ "name": "statut_acces", "arguments": { "id": id } })).await;
        assert_eq!(spy["result"]["isError"], true, "l'agent B ne voit pas la demande de l'agent A");
        assert_eq!(agents.decide(&"ff".repeat(32), &id, true, None).await.unwrap_err(), AgentError::NotFound, "un autre compte ne tranche pas");
        let _ = std::fs::remove_dir_all(dir);
    }
}
