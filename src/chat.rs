//! Conversations privées 1 à 1 (NIP-17) : messages kind 14 emballés en NIP-59, comme pour le partage de fichiers.
//!
//! - **Envoi** : un message est emballé deux fois, pour l'ami (remis à sa station) et pour soi (copie publiée sur le relai local :
//!   l'historique se retrouve sur n'importe quelle station qui héberge le compte). Le texte est rangé tout de suite dans
//!   `chat_messages`, avec l'état `failed` si la remise a échoué : rien n'est perdu, l'erreur est dite.
//! - **Réception** : pour chaque MULTIPASS hébergé, les enveloppes nouvelles (suivies par un repère `seq`, car NIP-59 brouille
//!   les dates) sont ouvertes ; seuls comptent les messages d'un proche (suivi par le titulaire) ou d'un compte hébergé ici.
//!   Tout le reste est lu une fois et ignoré sans bruit. La signature du scellé prouve l'expéditeur.
//! - Les messages déchiffrés sont un **état local** (`chat_messages`) : les enveloppes restent dans `nostr_events`, la base se
//!   reconstruit en remettant le repère à zéro.
//! - **Appels visio** : la sonnerie et la poignée de main WebRTC (offre, réponse, raccrochage, refus, occupé) voyagent par le
//!   même chemin, en rumeur privée `kind::CALL_SIGNAL` dans une enveloppe qui **expire** (elle contient des adresses réseau).
//!   Seul un proche peut faire sonner. Un signal reçu est poussé en direct au navigateur (flux d'activité `call:<type>`) ;
//!   une sonnerie trop vieille, ou un appel raccroché sans réponse, devient « Appel manqué » dans la conversation. Le son et
//!   l'image vont ensuite de navigateur à navigateur, sans passer par la station.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use astro_core::{ActivityBus, Audience};
use astro_crypto::{KeySource, nip59};
use astro_db::Db;
use astro_db::events::row_to_event;
use astro_nostr_proto::{Event, is_hex64, kind};
use rusqlite::OptionalExtension;
use serde_json::{Value, json};
use tokio::sync::{broadcast, watch};

/// Remise d'une enveloppe au relai de `peer` (station locale ou maillage).
pub type DeliverFn = Arc<dyn Fn(String, Event) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>> + Send + Sync>;

/// Taille maximale d'un message (caractères).
pub const MAX_CHARS: usize = 4000;
/// Enveloppes lues par MULTIPASS et par passage.
const BATCH: usize = 200;
/// Messages rendus par page.
const PAGE: usize = 60;
/// Espace du repère `seq` dans `kv`.
const SEQ_SCOPE: &str = "chat_seq";
/// Types de signaux d'appel. `media` : « voix seule », le pair cesse d'envoyer sa vidéo vers l'expéditeur (`video: false`)
/// ou la reprend (`video: true`), sans renégocier l'appel.
pub const CALL_TYPES: [&str; 8] = ["offer", "answer", "hangup", "reject", "busy", "join", "roster", "media"];
/// Appel de groupe (maillage) : participants au plus, hôte compris.
pub const MAX_ROOM: usize = 4;
/// Une salle autorisée par un ami vaut ce temps (un appel de groupe ne dure pas plus).
const ROOM_SECS: u64 = 4 * 3600;
/// Description de session (SDP) d'une offre ou d'une réponse : 64 Kio au plus.
pub const MAX_SDP: usize = 65_536;
/// Au-delà, une sonnerie est un appel manqué et un autre signal est périmé (station éteinte, réseau lent).
pub const RING_SECS: i64 = 60;
/// Durée de vie d'une enveloppe d'appel sur les relais.
const CALL_TTL: u64 = 600;
/// Traces d'appel rangées dans la conversation (le texte fait foi : l'interface reconnaît le pictogramme).
pub const MISSED: &str = "📹 Appel manqué";
/// Côté appelant : l'ami n'a pas décroché.
pub const UNANSWERED: &str = "📹 Appel sans réponse";
/// Pièces jointes au plus par message.
pub const MAX_FILES: usize = 10;
/// Accusé de lecture : rumeur kind 14 de service (balise `t`), jamais montrée comme un message ; son contenu est la date du
/// dernier message lu. Elle voyage dans une enveloppe qui expire.
pub const RECEIPT_TAG: &str = "astro-read";
/// Durée de vie d'une enveloppe d'accusé de lecture sur les relais.
const RECEIPT_TTL: u64 = 7 * 86_400;
/// Résultats de recherche au plus.
const SEARCH_MAX: i64 = 50;
/// Participants d'un groupe au plus, titulaire compris.
pub const MAX_GROUP: usize = 20;
/// « En train d'écrire » : rumeur de service, jamais rangée, dans une enveloppe qui expire vite.
pub const TYPING_TAG: &str = "astro-typing";
/// Modification d'un message envoyé (balise `e` : le message ; contenu : le nouveau texte).
pub const EDIT_TAG: &str = "astro-edit";
/// Changement d'un groupe (contenu JSON `{ name, members }`, balise `g` : le groupe).
pub const GROUP_TAG: &str = "astro-group";
const TYPING_TTL: u64 = 60;

/// Conversation à laquelle appartient une rumeur reçue.
struct Conv {
    peer: String,
    /// Groupe encore inconnu du titulaire : nom et participants à enregistrer.
    new_group: Option<(Option<String>, Vec<String>)>,
}

fn has_t(rumor: &nip59::Rumor, v: &str) -> bool {
    rumor.tags.iter().any(|t| t.first().map(String::as_str) == Some("t") && t.get(1).map(String::as_str) == Some(v))
}

/// Une réaction : un emoji court (8 caractères au plus), sans caractère de contrôle ; vide = retirer.
fn emoji_of(s: &str) -> Option<String> {
    let e = clean(s);
    (e.chars().count() <= 8 && !e.contains(char::is_whitespace)).then_some(e)
}

/// Identifiant d'une conversation de groupe : empreinte des clés des participants (titulaire compris), triées. Chacun le
/// recalcule de son côté à partir des balises `p` de la rumeur et de son auteur.
pub fn group_id(members: &[String]) -> String {
    use sha2::Digest;
    let mut m: Vec<&str> = members.iter().map(String::as_str).collect();
    m.sort_unstable();
    m.dedup();
    sha2::Sha256::digest(format!("astro-group:{}", m.join(","))).iter().map(|b| format!("{b:02x}")).collect()
}

/// Une pièce jointe annoncée dans un message : le fichier lui-même voyage par le partage du uCloud (`/Partages/De_…/` chez
/// l'ami) ; le message n'en porte que le nom, le type et la taille (balise `file`), plus, côté expéditeur seulement, son chemin.
fn file_of(v: &Value) -> Option<Value> {
    let name = v["name"].as_str().map(str::trim).filter(|n| !n.is_empty() && n.chars().count() <= 200 && !n.contains('/') && !n.chars().any(char::is_control) && *n != "." && *n != "..")?;
    let mime = v["mime"].as_str().filter(|m| m.len() <= 100 && m.bytes().all(|b| b.is_ascii_graphic())).unwrap_or("application/octet-stream");
    let size = v["size"].as_u64().unwrap_or(0);
    let mut out = json!({ "name": name, "mime": mime, "size": size });
    if let Some(p) = v["path"].as_str().filter(|p| p.starts_with('/') && p.len() <= 1024 && !p.split('/').any(|s| s == "..")) {
        out["path"] = json!(p);
    }
    Some(out)
}

/// Un message à ranger.
struct Row<'a> {
    id: &'a str,
    mine: bool,
    at: i64,
    text: &'a str,
    state: &'a str,
    read: bool,
    reply: Option<&'a str>,
    /// Réponse à un message vocal : l'instant de l'original (millisecondes) où la lecture s'est arrêtée pour répondre.
    reply_at: Option<u64>,
    files: &'a [Value],
    /// Groupe : qui a écrit.
    author: Option<&'a str>,
}

/// Erreurs d'usage du chat.
#[derive(Debug, thiserror::Error)]
pub enum ChatError {
    /// Requête invalide.
    #[error("{0}")]
    Invalid(String),
    /// Clé du compte indisponible sur cette station.
    #[error("clé du compte indisponible : envoi impossible")]
    NoKey,
    /// La station de l'ami n'a pas reçu le signal.
    #[error("{0}")]
    Unreachable(String),
    /// Base de données.
    #[error("base : {0}")]
    Db(String),
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

fn db_err(e: impl std::fmt::Display) -> ChatError {
    ChatError::Db(e.to_string())
}

/// Texte d'un message : espaces de fin retirés, caractères de contrôle écartés (sauf retour à la ligne).
fn clean(text: &str) -> String {
    text.chars().filter(|c| !c.is_control() || *c == '\n').collect::<String>().trim().to_owned()
}

/// Salle d'appel de groupe autorisée par un ami : hôte, participants, échéance.
type Room = (String, Vec<String>, std::time::Instant);

/// Service de conversations.
pub struct Chat {
    db: Arc<Db>,
    keys: Arc<dyn KeySource>,
    deliver: DeliverFn,
    publish_local: DeliverFn,
    activity: ActivityBus,
    people: Option<Arc<crate::person_keys::PersonKeys>>,
    /// Relais d'appel de la station : secret des identifiants et port UDP.
    turn: Option<([u8; 32], u16)>,
    /// Salles d'appel de groupe où un ami a invité le membre : `(membre, salle) → (hôte, participants, échéance)`. Un
    /// participant qui n'est pas un proche du membre ne peut le joindre que dans une telle salle. État en mémoire.
    rooms: std::sync::Mutex<std::collections::HashMap<(String, String), Room>>,
    /// Réveil des navigateurs du membre à l'arrivée d'un message (Web Push sans contenu).
    push: Option<Arc<crate::web_push::WebPush>>,
    /// Aperçus des liens (cache en mémoire).
    previews: crate::link_preview::Previews,
}

impl Chat {
    /// `deliver` remet l'enveloppe à l'ami ; `publish_local` la publie sur le relai de cette station (copie de l'expéditeur).
    pub fn new(db: Arc<Db>, keys: Arc<dyn KeySource>, deliver: DeliverFn, publish_local: DeliverFn, activity: ActivityBus) -> Self {
        Self { db, keys, deliver, publish_local, activity, people: None, turn: None, rooms: std::sync::Mutex::default(), push: None, previews: crate::link_preview::Previews::default() }
    }

    /// Réveille les navigateurs abonnés d'un membre quand un message lui arrive.
    pub fn with_push(mut self, push: Arc<crate::web_push::WebPush>) -> Self {
        self.push = Some(push);
        self
    }

    /// Relais d'appel : une offre vers un membre de la station ou un ami réciproque emporte des identifiants pour lui seul.
    pub fn with_turn(mut self, secret: [u8; 32], port: u16) -> Self {
        self.turn = Some((secret, port));
        self
    }

    /// Comme [`Self::with_turn`], relais facultatif.
    pub fn with_turn_opt(mut self, turn: Option<([u8; 32], u16)>) -> Self {
        self.turn = turn;
        self
    }

    /// Ami N¹ : suivi par le titulaire **et** qui le suit (sa liste kind 3), ou compte hébergé par cette station.
    async fn mutual_or_member(&self, owner: &str, peer: &str) -> bool {
        let (o, p) = (owner.to_owned(), peer.to_owned());
        self.db
            .readers()
            .read(move |c| {
                let member: Option<i64> = c.query_row("SELECT 1 FROM identities WHERE hex = ?1 AND hosted = 1", [&p], |r| r.get(0)).optional()?;
                let follows: Option<i64> = c.query_row("SELECT 1 FROM social_graph WHERE owner_hex = ?1 AND pubkey = ?2 AND level = 1", [&o, &p], |r| r.get(0)).optional()?;
                let back: Option<i64> = c.query_row("SELECT 1 FROM nostr_events e JOIN nostr_tags t ON t.event_id = e.seq WHERE e.kind = 3 AND e.pubkey = ?1 AND t.name = 'p' AND t.value = ?2 LIMIT 1", [&p, &o], |r| r.get(0)).optional()?;
                Ok(member.is_some() || (follows.is_some() && back.is_some()))
            })
            .await
            .unwrap_or(false)
    }

    /// Active les conversations par **personne** : ses clés dans plusieurs UPlanet (voir [`crate::person_keys`]).
    pub fn with_people(mut self, people: Arc<crate::person_keys::PersonKeys>) -> Self {
        self.people = Some(people);
        self
    }

    /// Les clés de la personne derrière `peer` (`peer` d'abord). Sans annuaire : `peer` seul.
    async fn class(&self, peer: &str) -> Vec<crate::person_keys::PlaneKey> {
        match &self.people {
            Some(p) => p.class_of(peer).await,
            None => vec![crate::person_keys::PlaneKey { plane: String::new(), hex: peer.to_owned(), via: "fiche".into() }],
        }
    }

    /// Clé à qui écrire : la conversation se fait entre deux clés **de la même UPlanet**. Si `peer` vit dans un plan voisin, on prend la
    /// clé de la même personne dans ce plan ; à défaut, l'erreur dit où ouvrir la conversation.
    async fn channel(&self, peer: &str) -> Result<String, ChatError> {
        let Some(people) = &self.people else { return Ok(peer.to_owned()) };
        let class = self.class(peer).await;
        let here = people.plane();
        let known_elsewhere = |k: &crate::person_keys::PlaneKey| !k.plane.is_empty() && k.plane != here;
        if class.first().is_none_or(|k| !known_elsewhere(k)) {
            return Ok(peer.to_owned());
        }
        if let Some(k) = class.iter().find(|k| k.plane == here) {
            return Ok(k.hex.clone());
        }
        let planes: Vec<&str> = class.iter().map(|k| k.plane.as_str()).filter(|p| !p.is_empty()).collect();
        // Le membre ne voit ni plan ni clé : le détail va au Capitaine, qui peut aider.
        self.activity.publish_to(Audience::Captain, "alert", "chat:channel", format!("aucune clé de cette personne dans la constellation « {here} » (connue dans : {})", planes.join(", ")), None);
        Err(ChatError::Invalid("cette personne n'est pas joignable d'ici pour le moment".to_owned()))
    }

    /// Envoie `text` à `peer`. Rend le message tel qu'il est rangé (`state` : `ok` ou `failed`, avec `error`).
    pub async fn send(&self, owner: &str, peer: &str, text: &str) -> Result<Value, ChatError> {
        self.send_full(owner, peer, text, None, None, &[]).await
    }

    /// Comme [`Self::send`], en réponse au message `reply` (balise `e`, NIP-17) — à l'instant `reply_at` (ms) d'un message
    /// vocal, balise `offset` — et avec des pièces jointes déjà partagées (`{ name, mime, size, path }`, au plus
    /// [`MAX_FILES`]). Sans texte, le message dit les noms des fichiers. `peer` peut être un groupe ([`Self::create_group`]) :
    /// la rumeur porte alors une balise `p` par participant et part, emballée, vers chacun.
    pub async fn send_full(&self, owner: &str, peer: &str, text: &str, reply: Option<&str>, reply_at: Option<u64>, files: &[Value]) -> Result<Value, ChatError> {
        if !is_hex64(peer) || peer == owner {
            return Err(ChatError::Invalid("destinataire invalide".into()));
        }
        if files.len() > MAX_FILES {
            return Err(ChatError::Invalid(format!("{MAX_FILES} pièces jointes au plus par message")));
        }
        let files: Vec<Value> = files.iter().map(|f| file_of(f).ok_or_else(|| ChatError::Invalid("pièce jointe invalide".into()))).collect::<Result<_, _>>()?;
        let reply = match reply {
            Some(r) if !is_hex64(r) => return Err(ChatError::Invalid("message cité invalide".into())),
            r => r,
        };
        let reply_at = reply.and(reply_at);
        let mut text = clean(text);
        if text.is_empty() && !files.is_empty() {
            text = files.iter().map(|f| format!("📎 {}", f["name"].as_str().unwrap_or_default())).collect::<Vec<_>>().join("\n");
        }
        if text.is_empty() {
            return Err(ChatError::Invalid("message vide".into()));
        }
        if text.chars().count() > MAX_CHARS {
            return Err(ChatError::Invalid(format!("message trop long (plus de {MAX_CHARS} caractères)")));
        }
        let group = self.group(owner, peer).await;
        if group.as_ref().is_some_and(|(_, m)| !m.iter().any(|x| x == owner)) {
            return Err(ChatError::Invalid("vous avez quitté ce groupe".into()));
        }
        let peer = if group.is_some() { peer.to_owned() } else { self.channel(peer).await? };
        let peer = peer.as_str();
        let secret = self.keys.secret(owner).ok_or(ChatError::NoKey)?;
        let at = now();
        let targets: Vec<String> = match &group {
            Some((_, members)) => members.iter().filter(|m| *m != owner).cloned().collect(),
            None => vec![peer.to_owned()],
        };
        let mut tags: Vec<Vec<String>> = targets.iter().map(|t| vec!["p".into(), t.clone()]).collect();
        if let Some(name) = group.as_ref().and_then(|g| g.0.clone()) {
            tags.push(vec!["subject".into(), name]);
        }
        if group.is_some() {
            tags.push(vec!["g".into(), peer.to_owned()]);
        }
        if let Some(r) = reply {
            tags.push(vec!["e".into(), r.to_owned(), String::new(), "reply".into()]);
        }
        if let Some(ms) = reply_at {
            tags.push(vec!["offset".into(), ms.to_string()]);
        }
        // Le chemin reste ici : l'ami ne reçoit que le nom, le type et la taille.
        tags.extend(files.iter().map(|f| vec!["file".into(), f["name"].as_str().unwrap_or_default().to_owned(), f["mime"].as_str().unwrap_or_default().to_owned(), f["size"].to_string()]));
        let rumor = nip59::Rumor { pubkey: owner.to_owned(), created_at: u64::try_from(at).unwrap_or(0), kind: kind::CHAT, tags, content: text.clone() };
        let id = rumor.id().map_err(|e| ChatError::Invalid(e.to_string()))?;
        let mut errors = Vec::new();
        for t in &targets {
            let wrap = nip59::gift_wrap(&secret, t, &rumor, rumor.created_at).map_err(|e| ChatError::Invalid(e.to_string()))?;
            if let Err(e) = (self.deliver)(t.clone(), wrap).await {
                errors.push(e);
            }
        }
        // La copie personnelle ne bloque pas la remise : on dit l'échec des deux.
        let to_self = nip59::gift_wrap(&secret, owner, &rumor, rumor.created_at).map_err(|e| ChatError::Invalid(e.to_string()))?;
        let copied = (self.publish_local)(owner.to_owned(), to_self).await;
        // Groupe : « non remis » seulement si personne ne l'a reçu ; une remise partielle est dite, sans échec.
        let error = if group.is_some() && errors.len() < targets.len() { None } else { errors.first().cloned() }.or_else(|| copied.err().map(|e| format!("copie personnelle : {e}")));
        let partial = (group.is_some() && !errors.is_empty() && errors.len() < targets.len()).then(|| format!("remis à {} participant(s) sur {}", targets.len() - errors.len(), targets.len()));
        let state = if error.is_some() { "failed" } else { "ok" };
        self.store_row(owner, peer, Row { id: &id, mine: true, at, text: &text, state, read: true, reply, reply_at, files: &files, author: None }).await?;
        if let Some(e) = error.as_ref().or(partial.as_ref()) {
            self.activity.publish_to(Audience::Owner(owner.to_owned()), "alert", "chat:out", format!("message non remis : {e}"), None);
        }
        self.activity.publish_to(Audience::Owner(owner.to_owned()), "chat", "chat:msg", peer.to_owned(), None);
        Ok(json!({ "id": id, "peer": peer, "mine": true, "at": at, "text": text, "state": state, "error": error, "partial": partial, "reply": reply, "reply_at": reply_at, "files": files }))
    }

    /// Le groupe `id` du titulaire, s'il existe : `(nom, participants titulaire compris)`.
    async fn group(&self, owner: &str, id: &str) -> Option<(Option<String>, Vec<String>)> {
        let (o, i) = (owner.to_owned(), id.to_owned());
        let row: Option<(Option<String>, String)> = self.db.readers().read(move |c| Ok(c.query_row("SELECT name, members FROM chat_groups WHERE owner_hex = ?1 AND id = ?2", [&o, &i], |r| Ok((r.get(0)?, r.get(1)?))).optional()?)).await.ok().flatten();
        row.map(|(n, m)| (n, serde_json::from_str(&m).unwrap_or_default()))
    }

    /// Crée (ou retrouve) le groupe du titulaire avec `members` (2 à [`MAX_GROUP`] − 1 autres personnes) ; rend son identifiant.
    pub async fn create_group(&self, owner: &str, name: &str, members: &[String]) -> Result<Value, ChatError> {
        let mut all: Vec<String> = members.iter().filter(|m| *m != owner).cloned().collect();
        all.sort_unstable();
        all.dedup();
        if all.len() < 2 || all.len() >= MAX_GROUP || !all.iter().all(|m| is_hex64(m)) {
            return Err(ChatError::Invalid(format!("un groupe réunit de 2 à {} autres personnes", MAX_GROUP - 1)));
        }
        all.push(owner.to_owned());
        let name: String = clean(name).chars().take(80).collect();
        let name = (!name.is_empty()).then_some(name);
        let id = group_id(&all);
        self.save_group(owner, &id, name.as_deref(), &all).await?;
        Ok(json!({ "id": id, "name": name, "members": all }))
    }

    async fn save_group(&self, owner: &str, id: &str, name: Option<&str>, members: &[String]) -> Result<(), ChatError> {
        self.db
            .writer()
            .execute(
                "INSERT INTO chat_groups(owner_hex, id, name, members) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(owner_hex, id) DO UPDATE SET name = COALESCE(excluded.name, chat_groups.name)",
                vec![owner.to_owned().into(), id.to_owned().into(), name.map(str::to_owned).into(), serde_json::to_string(members).unwrap_or_default().into()],
            )
            .await
            .map_err(db_err)?;
        Ok(())
    }

    /// Aperçu riche d'un lien (voir [`crate::link_preview`]).
    pub async fn preview(&self, url: &str) -> Result<Value, ChatError> {
        self.previews.get(url).await.map_err(ChatError::Invalid)
    }

    /// Clé publique VAPID (abonnement des navigateurs aux réveils).
    pub async fn push_key(&self) -> Result<Value, ChatError> {
        let p = self.push.as_ref().ok_or_else(|| ChatError::Unreachable("notifications non disponibles sur cette station".into()))?;
        Ok(json!({ "key": p.public_key().await.map_err(ChatError::Db)? }))
    }

    /// Abonne ou désabonne un navigateur du titulaire.
    pub async fn push_subscribe(&self, owner: &str, endpoint: &str, on: bool) -> Result<Value, ChatError> {
        let p = self.push.as_ref().ok_or_else(|| ChatError::Unreachable("notifications non disponibles sur cette station".into()))?;
        p.subscribe(owner, endpoint, on).await.map_err(ChatError::Invalid)?;
        Ok(json!({ "on": on }))
    }

    #[allow(clippy::too_many_arguments)]
    async fn store(&self, owner: &str, peer: &str, id: &str, mine: bool, at: i64, text: &str, state: &str, read: bool) -> Result<bool, ChatError> {
        self.store_row(owner, peer, Row { id, mine, at, text, state, read, reply: None, reply_at: None, files: &[], author: None }).await
    }

    async fn store_row(&self, owner: &str, peer: &str, r: Row<'_>) -> Result<bool, ChatError> {
        let meta = (!r.files.is_empty() || r.reply_at.is_some()).then(|| {
            let mut m = json!({ "files": r.files });
            if let Some(ms) = r.reply_at {
                m["at_ms"] = json!(ms);
            }
            m.to_string()
        });
        let out = self
            .db
            .writer()
            .execute(
                // Une conversation effacée par le titulaire ne revient pas (relecture des enveloppes) : `kv` chat_cleared garde la date ;
                // un message effacé seul non plus (`kv` chat_deleted).
                "INSERT INTO chat_messages(owner_hex, id, peer, mine, at, text, state, read, n, reply, meta, author) \
                 SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, (SELECT COALESCE(MAX(n), 0) + 1 FROM chat_messages WHERE owner_hex = ?1), ?9, ?10, ?11 \
                 WHERE NOT EXISTS (SELECT 1 FROM kv WHERE scope = 'chat_cleared' AND key = ?1 || ':' || ?3 AND CAST(value AS INTEGER) >= ?5) \
                   AND NOT EXISTS (SELECT 1 FROM kv WHERE scope = 'chat_deleted' AND key = ?1 || ':' || ?2) \
                 ON CONFLICT(owner_hex, id) DO UPDATE SET state = excluded.state WHERE excluded.state = 'ok' AND chat_messages.state = 'failed'",
                vec![
                    owner.to_owned().into(),
                    r.id.to_owned().into(),
                    peer.to_owned().into(),
                    i64::from(r.mine).into(),
                    r.at.into(),
                    r.text.to_owned().into(),
                    r.state.to_owned().into(),
                    i64::from(r.read).into(),
                    r.reply.map(str::to_owned).into(),
                    meta.into(),
                    r.author.map(str::to_owned).into(),
                ],
            )
            .await
            .map_err(db_err)?;
        Ok(out.changes > 0)
    }

    /// Efface un message pour le titulaire seul ; il ne revient pas à la relecture des enveloppes.
    pub async fn delete(&self, owner: &str, id: &str) -> Result<Value, ChatError> {
        if !is_hex64(id) {
            return Err(ChatError::Invalid("message invalide".into()));
        }
        let (o, i, at) = (owner.to_owned(), id.to_owned(), now());
        let peer = self
            .db
            .writer()
            .transaction(move |tx| {
                let peer: Option<String> = tx.query_row("DELETE FROM chat_messages WHERE owner_hex = ?1 AND id = ?2 RETURNING peer", rusqlite::params![o, i], |r| r.get(0)).optional()?;
                tx.execute("INSERT OR REPLACE INTO kv(scope, key, value, updated_at) VALUES ('chat_deleted', ?1, '1', ?2)", rusqlite::params![format!("{o}:{i}"), at])?;
                Ok(peer)
            })
            .await
            .map_err(db_err)?;
        if let Some(p) = &peer {
            self.activity.publish_to(Audience::Owner(owner.to_owned()), "chat", "chat:seen", p.clone(), None);
        }
        Ok(json!({ "deleted": peer.is_some() }))
    }

    /// Messages du titulaire qui contiennent `q` (sans tenir compte de la casse), les plus récents d'abord.
    pub async fn search(&self, owner: &str, q: &str) -> Result<Value, ChatError> {
        let q = clean(q);
        if q.chars().count() < 2 || q.chars().count() > 100 {
            return Err(ChatError::Invalid("recherche : de 2 à 100 caractères".into()));
        }
        let o = owner.to_owned();
        let rows = self
            .db
            .readers()
            .read(move |c| {
                let mut s = c.prepare("SELECT peer, id, at, text, mine FROM chat_messages WHERE owner_hex = ?1 AND instr(lower(text), lower(?2)) > 0 ORDER BY at DESC LIMIT ?3")?;
                let out = s
                    .query_map(rusqlite::params![o, q, SEARCH_MAX], |r| Ok(json!({ "peer": r.get::<_, String>(0)?, "id": r.get::<_, String>(1)?, "at": r.get::<_, i64>(2)?, "text": r.get::<_, String>(3)?.chars().take(200).collect::<String>(), "mine": r.get::<_, i64>(4)? == 1 })))?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(out)
            })
            .await
            .map_err(db_err)?;
        Ok(json!({ "results": rows }))
    }

    /// Accusé de lecture reçu de `sender` : ses messages jusqu'à `at` sont « lus ».
    async fn seen(&self, owner: &str, sender: &str, at: i64) {
        let r = self.db.writer().execute("UPDATE chat_messages SET seen = 1 WHERE owner_hex = ?1 AND peer = ?2 AND mine = 1 AND seen = 0 AND at <= ?3", vec![owner.to_owned().into(), sender.to_owned().into(), at.into()]).await;
        if r.is_ok_and(|r| r.changes > 0) {
            self.activity.publish_to(Audience::Owner(owner.to_owned()), "chat", "chat:seen", sender.to_owned(), None);
        }
    }

    /// Envoie l'accusé de lecture à `peer` (sans effet si la remise échoue : ce n'est qu'une indication).
    async fn send_receipt(&self, owner: &str, peer: &str, at: i64) {
        let Some(secret) = self.keys.secret(owner) else { return };
        let created = u64::try_from(now()).unwrap_or(0);
        let rumor = nip59::Rumor { pubkey: owner.to_owned(), created_at: created, kind: kind::CHAT, tags: vec![vec!["p".into(), peer.to_owned()], vec!["t".into(), RECEIPT_TAG.into()]], content: at.to_string() };
        if let Ok(wrap) = nip59::gift_wrap_expiring(&secret, peer, &rumor, created, created + RECEIPT_TTL) {
            let _ = (self.deliver)(peer.to_owned(), wrap).await;
        }
    }

    /// Conversations du titulaire, la plus récente d'abord : `{ peer, label, last, at, mine, unread }`.
    pub async fn conversations(&self, owner: &str) -> Result<Value, ChatError> {
        let o = owner.to_owned();
        let rows = self
            .db
            .readers()
            .read(move |c| {
                let mut stmt = c.prepare(
                    "SELECT m.peer, m.text, m.at, m.mine, \
                            (SELECT COUNT(*) FROM chat_messages u WHERE u.owner_hex = m.owner_hex AND u.peer = m.peer AND u.mine = 0 AND u.read = 0), \
                            COALESCE((SELECT json_extract(f.fields, '$.name') FROM contact_cards f WHERE f.owner_hex = m.owner_hex AND json_extract(f.fields, '$.nostr') = m.peer LIMIT 1), \
                                     (SELECT json_extract(e.content, '$.name') FROM nostr_events e WHERE e.pubkey = m.peer AND e.kind = 0 ORDER BY e.created_at DESC LIMIT 1)), \
                            EXISTS (SELECT 1 FROM identities i WHERE i.hex = m.peer AND i.role = 'node'), \
                            (SELECT COALESCE(g.name, '') || char(31) || json_array_length(g.members) FROM chat_groups g WHERE g.owner_hex = m.owner_hex AND g.id = m.peer), \
                            COALESCE((SELECT p.pinned || p.archived || p.muted FROM chat_prefs p WHERE p.owner_hex = m.owner_hex AND p.peer = m.peer), '000'), \
                            (SELECT NOT EXISTS (SELECT 1 FROM json_each(g.members) WHERE value = m.owner_hex) FROM chat_groups g WHERE g.owner_hex = m.owner_hex AND g.id = m.peer) \
                     FROM chat_messages m WHERE m.owner_hex = ?1 \
                       AND m.at = (SELECT MAX(x.at) FROM chat_messages x WHERE x.owner_hex = m.owner_hex AND x.peer = m.peer) \
                     GROUP BY m.peer ORDER BY m.at DESC LIMIT 200",
                )?;
                let out = stmt
                    .query_map([&o], |r| {
                        let peer: String = r.get(0)?;
                        let label: Option<String> = r.get(5)?;
                        // Groupe : son nom, sinon « Groupe (n) » côté interface.
                        let group: Option<String> = r.get(7)?;
                        let (gname, gsize) = group.as_deref().and_then(|g| g.split_once('\u{1f}')).map_or((None, 0), |(n, k)| ((!n.is_empty()).then(|| n.to_owned()), k.parse::<i64>().unwrap_or(0)));
                        let prefs: String = r.get(8)?;
                        let flag = |i: usize| prefs.as_bytes().get(i) == Some(&b'1');
                        Ok(json!({ "group": group.is_some(), "size": gsize, "gname": gname, "left": r.get::<_, Option<bool>>(9)?.unwrap_or(false),
                                   "pinned": flag(0), "archived": flag(1), "muted": flag(2), "peer": peer, "last": r.get::<_, String>(1)?.chars().take(120).collect::<String>(), "at": r.get::<_, i64>(2)?, "mine": r.get::<_, i64>(3)? == 1, "unread": r.get::<_, i64>(4)?,
                                   "station": r.get::<_, bool>(6)?, "label": label.filter(|l| !l.trim().is_empty()).unwrap_or_else(|| peer.chars().take(8).collect()) }))
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(out)
            })
            .await
            .map_err(db_err)?;
        let rows = self.by_person(owner, rows).await;
        // La pastille ne compte pas les conversations en sourdine.
        let unread: i64 = rows.iter().filter(|r| r["muted"] != json!(true)).filter_map(|r| r["unread"].as_i64()).sum();
        Ok(json!({ "conversations": rows, "unread": unread, "plane": self.people.as_ref().map(|p| p.plane()) }))
    }

    /// Une conversation par **personne** : les clés liées d'une même personne se rejoignent, et les conversations des plans voisins
    /// (même email, lecture seule) s'y ajoutent avec leur UPlanet. Chaque entrée garde la liste de ses `channels`.
    async fn by_person(&self, owner: &str, own: Vec<Value>) -> Vec<Value> {
        let Some(people) = &self.people else { return own };
        let here = people.plane().to_owned();
        let mut all: Vec<Value> = own.into_iter().map(|mut r| {
            r["plane"] = json!(here);
            r
        }).collect();
        if let Some(email) = people.email_of(owner).await {
            all.extend(people.sibling_conversations(&email).await);
        }
        let mut groups: Vec<Value> = Vec::new();
        let mut sets: Vec<std::collections::BTreeSet<String>> = Vec::new();
        for r in all {
            let peer = r["peer"].as_str().unwrap_or_default().to_owned();
            let channel = json!({ "plane": r["plane"], "peer": peer, "unread": r["unread"] });
            let class: std::collections::BTreeSet<String> = people.class_of(&peer).await.into_iter().map(|k| k.hex).collect();
            let target = sets.iter().position(|g| !g.is_disjoint(&class));
            match target {
                Some(i) => {
                    sets[i].extend(class);
                    let g = &mut groups[i];
                    // Les non lus d'un plan voisin se lisent dans ce plan : ils ne gonflent ni la pastille ni ce plan.
                    if r["plane"] == json!(here) {
                        g["unread"] = json!(g["unread"].as_i64().unwrap_or(0) + r["unread"].as_i64().unwrap_or(0));
                    }
                    if r["at"].as_i64() > g["at"].as_i64() {
                        for k in ["last", "at", "mine"] {
                            g[k] = r[k].clone();
                        }
                    }
                    if let Some(a) = g["channels"].as_array_mut() {
                        a.push(channel);
                    }
                }
                None => {
                    let mut g = r;
                    if g["plane"] != json!(here) {
                        g["unread"] = json!(0);
                    }
                    g["channels"] = json!([channel]);
                    groups.push(g);
                    sets.push(class);
                }
            }
        }
        groups.sort_by_key(|g| std::cmp::Reverse(g["at"].as_i64().unwrap_or(0)));
        groups
    }

    /// Messages avec `peer`, du plus ancien au plus récent ; `before` (horodatage) pour remonter dans l'historique.
    pub async fn messages(&self, owner: &str, peer: &str, before: Option<i64>) -> Result<Value, ChatError> {
        if !is_hex64(peer) {
            return Err(ChatError::Invalid("correspondant invalide".into()));
        }
        let o = owner.to_owned();
        let before = before.unwrap_or(i64::MAX);
        // Toutes les clés de la personne : l'historique d'une ancienne clé et celui de la nouvelle ne font qu'un seul fil.
        let peers: String = serde_json::to_string(&self.class(peer).await.iter().map(|k| k.hex.clone()).collect::<Vec<_>>()).unwrap_or_else(|_| "[]".into());
        let mut rows = self
            .db
            .readers()
            .read(move |c| {
                let mut stmt = c.prepare(
                    "SELECT m.id, m.mine, m.at, m.text, m.state, m.reply, m.meta, m.seen, q.text, q.mine, m.author, m.edited FROM chat_messages m \
                     LEFT JOIN chat_messages q ON q.owner_hex = m.owner_hex AND q.id = m.reply \
                     WHERE m.owner_hex = ?1 AND m.peer IN (SELECT value FROM json_each(?2)) AND m.at < ?3 ORDER BY m.at DESC, m.n DESC LIMIT ?4",
                )?;
                // Pièce jointe reçue : le fichier arrive par le partage du uCloud, sous `/Partages/De_…/` (absent tant qu'il voyage).
                let mut find = c.prepare("SELECT path FROM ucloud_entries WHERE owner_hex = ?1 AND path LIKE '/Partages/%' AND path = parent || '/' || ?2 ORDER BY mtime DESC LIMIT 1")?;
                let rows = stmt
                    .query_map(rusqlite::params![o, peers, before, PAGE as i64 + 1], |r| {
                        let quote: Option<String> = r.get(8)?;
                        Ok((
                            json!({ "id": r.get::<_, String>(0)?, "mine": r.get::<_, i64>(1)? == 1, "at": r.get::<_, i64>(2)?, "text": r.get::<_, String>(3)?, "state": r.get::<_, String>(4)?,
                                    "reply": r.get::<_, Option<String>>(5)?, "seen": r.get::<_, i64>(7)? == 1, "author": r.get::<_, Option<String>>(10)?, "edited": r.get::<_, i64>(11)? == 1,
                                    "quote": quote.map(|t| json!({ "text": t.chars().take(160).collect::<String>(), "mine": r.get::<_, Option<i64>>(9).ok().flatten() == Some(1) })) }),
                            r.get::<_, Option<String>>(6)?,
                        ))
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                let mut out = Vec::with_capacity(rows.len());
                for (mut m, meta) in rows {
                    let meta: Value = meta.and_then(|s| serde_json::from_str::<Value>(&s).ok()).unwrap_or(Value::Null);
                    m["reply_at"] = meta["at_ms"].clone();
                    let mut files: Vec<Value> = meta["files"].as_array().cloned().unwrap_or_default();
                    if m["mine"] == json!(false) {
                        for f in &mut files {
                            let path: Option<String> = find.query_row(rusqlite::params![o, f["name"].as_str().unwrap_or_default()], |r| r.get(0)).optional()?;
                            f["path"] = json!(path);
                        }
                    }
                    m["files"] = json!(files);
                    // Réactions : par emoji, combien, et si le titulaire en est.
                    let mut st = c.prepare_cached("SELECT emoji, COUNT(*), MAX(author = ?1), group_concat(author) FROM chat_reactions WHERE owner_hex = ?1 AND msg_id = ?2 GROUP BY emoji ORDER BY MIN(at)")?;
                    let id = m["id"].as_str().unwrap_or_default().to_owned();
                    m["reactions"] = json!(st.query_map(rusqlite::params![o, id], |r| Ok(json!({ "emoji": r.get::<_, String>(0)?, "n": r.get::<_, i64>(1)?, "mine": r.get::<_, i64>(2)? == 1, "by": r.get::<_, String>(3)?.split(',').map(str::to_owned).collect::<Vec<_>>() })))?.collect::<Result<Vec<_>, _>>()?);
                    out.push(m);
                }
                Ok(out)
            })
            .await
            .map_err(db_err)?;
        let more = rows.len() > PAGE;
        rows.truncate(PAGE);
        rows.reverse();
        // Groupe : ses participants, nommés, pour dire qui a écrit chaque message.
        let members = match self.group(owner, peer).await {
            Some((name, list)) => {
                let mut out = Vec::new();
                for h in list.iter().filter(|h| *h != owner) {
                    out.push(json!({ "hex": h, "label": self.label(owner, h).await }));
                }
                json!({ "name": name, "members": out, "left": !list.iter().any(|h| h == owner) })
            }
            None => Value::Null,
        };
        Ok(json!({ "peer": peer, "messages": rows, "more": more, "group": members }))
    }

    /// Marque la conversation comme lue.
    pub async fn mark_read(&self, owner: &str, peer: &str) -> Result<Value, ChatError> {
        if !is_hex64(peer) {
            return Err(ChatError::Invalid("correspondant invalide".into()));
        }
        let peers = serde_json::to_string(&self.class(peer).await.iter().map(|k| k.hex.clone()).collect::<Vec<_>>()).unwrap_or_else(|_| "[]".into());
        let o = owner.to_owned();
        // Par clé de l'ami : la date du dernier message lu, pour l'accusé de lecture (pas à une station : elle ne lit pas).
        let (n, latest) = self
            .db
            .writer()
            .transaction(move |tx| {
                let latest = {
                    let mut s = tx.prepare(
                        "SELECT peer, MAX(at) FROM chat_messages WHERE owner_hex = ?1 AND peer IN (SELECT value FROM json_each(?2)) AND mine = 0 AND read = 0 \
                         AND NOT EXISTS (SELECT 1 FROM identities i WHERE i.hex = peer AND i.role = 'node') \
                         AND NOT EXISTS (SELECT 1 FROM chat_groups g WHERE g.owner_hex = ?1 AND g.id = peer) GROUP BY peer",
                    )?;
                    s.query_map(rusqlite::params![o, peers], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?.collect::<Result<Vec<_>, _>>()?
                };
                let n = tx.execute("UPDATE chat_messages SET read = 1 WHERE owner_hex = ?1 AND peer IN (SELECT value FROM json_each(?2)) AND mine = 0 AND read = 0", rusqlite::params![o, peers])?;
                Ok((n, latest))
            })
            .await
            .map_err(db_err)?;
        if self.shares_activity(owner).await {
            for (p, at) in latest {
                self.send_receipt(owner, &p, at).await;
            }
        }
        Ok(json!({ "read": n }))
    }

    /// Efface la conversation avec `peer` (toutes ses clés liées) pour le titulaire seul : l'autre personne garde la sienne. Les
    /// messages antérieurs ne reviennent pas, même si la station relit les enveloppes ; un nouveau message rouvre la conversation.
    pub async fn clear(&self, owner: &str, peer: &str) -> Result<Value, ChatError> {
        if !is_hex64(peer) {
            return Err(ChatError::Invalid("correspondant invalide".into()));
        }
        let peers: Vec<String> = self.class(peer).await.into_iter().map(|k| k.hex).collect();
        let (o, at) = (owner.to_owned(), now());
        let deleted = self
            .db
            .writer()
            .transaction(move |tx| {
                let mut n = 0;
                for p in &peers {
                    n += tx.execute("DELETE FROM chat_messages WHERE owner_hex = ?1 AND peer = ?2", rusqlite::params![o, p])?;
                    tx.execute("INSERT OR REPLACE INTO kv(scope, key, value, updated_at) VALUES ('chat_cleared', ?1, ?2, ?3)", rusqlite::params![format!("{o}:{p}"), at.to_string(), at])?;
                }
                Ok(n)
            })
            .await
            .map_err(db_err)?;
        self.activity.publish_to(Audience::Owner(owner.to_owned()), "chat", "chat:msg", peer.to_owned(), None);
        Ok(json!({ "deleted": deleted }))
    }

    async fn follows(&self, owner: &str, sender: &str) -> bool {
        let (o, s) = (owner.to_owned(), sender.to_owned());
        self.db
            .readers()
            .read(move |c| {
                let follows = c.query_row("SELECT 1 FROM social_graph WHERE owner_hex = ?1 AND pubkey = ?2 AND level IN (1, 2)", [&o, &s], |r| r.get::<_, i64>(0)).optional()?.is_some();
                let local = c.query_row("SELECT 1 FROM identities WHERE hex = ?1 AND hosted = 1", [&s], |r| r.get::<_, i64>(0)).optional()?.is_some();
                Ok(follows || local)
            })
            .await
            .unwrap_or(false)
    }

    async fn owners(&self) -> Vec<String> {
        self.db
            .readers()
            .read(|c| {
                let mut stmt = c.prepare("SELECT hex FROM identities WHERE hosted = 1 AND role IN ('multipass', 'captain')")?;
                Ok(stmt.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .unwrap_or_default()
    }

    /// Un passage : ouvre les enveloppes nouvelles de chaque MULTIPASS hébergé. Rend le nombre de messages rangés.
    pub async fn poll_once(&self) -> usize {
        let mut stored = 0;
        for owner in self.owners().await {
            let Some(secret) = self.keys.secret(&owner) else { continue };
            let o = owner.clone();
            let after: i64 = self.db.readers().read(move |c| Ok(c.query_row("SELECT value FROM kv WHERE scope = ?1 AND key = ?2", [SEQ_SCOPE, &o], |r| r.get::<_, String>(0)).optional()?)).await.ok().flatten().and_then(|v| v.parse().ok()).unwrap_or(0);
            let o = owner.clone();
            let wraps: Vec<(i64, Event)> = self
                .db
                .readers()
                .read(move |c| {
                    let mut stmt = c.prepare_cached(
                        "SELECT e.id, e.pubkey, e.kind, e.created_at, e.content, e.sig, e.tags, e.seq FROM nostr_events e \
                         JOIN nostr_tags t ON t.event_id = e.seq AND t.name = 'p' AND t.value = ?1 \
                         WHERE e.kind = 1059 AND e.seq > ?2 ORDER BY e.seq LIMIT ?3",
                    )?;
                    let mut rows = stmt.query(rusqlite::params![o, after, BATCH as i64])?;
                    let mut out = Vec::new();
                    while let Some(r) = rows.next()? {
                        out.push((r.get::<_, i64>(7)?, row_to_event(r)?));
                    }
                    Ok(out)
                })
                .await
                .unwrap_or_default();
            let Some(last) = wraps.last().map(|(s, _)| *s) else { continue };
            for (_, wrap) in wraps {
                stored += self.handle(&owner, &secret, &wrap).await;
            }
            let _ = self.db.writer().execute("INSERT INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at", vec![SEQ_SCOPE.to_owned().into(), owner.clone().into(), last.to_string().into(), now().into()]).await;
        }
        stored
    }

    /// Identifiant d'appel : 8 à 64 caractères hexadécimaux, tiré au hasard par l'appelant.
    fn call_id(v: &Value) -> Option<String> {
        v["call"].as_str().filter(|c| (8..=64).contains(&c.len()) && c.bytes().all(|b| b.is_ascii_hexdigit())).map(str::to_owned)
    }

    /// Participants d'une salle (`roster`) : 1 à [`MAX_ROOM`] clés hexadécimales, sans doublon.
    fn roster(v: &Value) -> Option<Vec<String>> {
        let list: Vec<String> = v["roster"].as_array()?.iter().filter_map(Value::as_str).map(str::to_owned).collect();
        let mut seen = std::collections::HashSet::new();
        (!list.is_empty() && list.len() <= MAX_ROOM && list.len() == v["roster"].as_array()?.len() && list.iter().all(|h| is_hex64(h) && seen.insert(h.clone()))).then_some(list)
    }

    /// Envoie un signal d'appel à `peer` : `{ type, call, sdp?, video?, missed?, roster? }` (voir [`CALL_TYPES`]). `roster`
    /// (offre ou mise à jour d'un appel de groupe) : les participants de la salle, que la station de `peer` admettra. Rien n'est rangé,
    /// sauf la trace « Appel sans réponse » quand l'appelant raccroche avant qu'on décroche (`missed`).
    pub async fn signal(&self, owner: &str, peer: &str, sig: &Value) -> Result<Value, ChatError> {
        if !is_hex64(peer) || peer == owner {
            return Err(ChatError::Invalid("destinataire invalide".into()));
        }
        let ty = sig["type"].as_str().filter(|t| CALL_TYPES.contains(t)).ok_or_else(|| ChatError::Invalid("type de signal inconnu".into()))?;
        let call = Self::call_id(sig).ok_or_else(|| ChatError::Invalid("identifiant d'appel invalide".into()))?;
        let sdp = sig["sdp"].as_str();
        if matches!(ty, "offer" | "answer") != sdp.is_some_and(|d| !d.is_empty() && d.len() <= MAX_SDP) {
            return Err(ChatError::Invalid("description de session attendue pour une offre ou une réponse (64 Kio au plus), et seulement pour elles".into()));
        }
        let missed = ty == "hangup" && sig["missed"].as_bool() == Some(true);
        let mut content = json!({ "type": ty, "call": call, "video": sig["video"].as_bool().unwrap_or(true) });
        if let Some(d) = sdp {
            content["sdp"] = json!(d);
        }
        if missed {
            content["missed"] = json!(true);
        }
        if !sig["roster"].is_null() || ty == "roster" {
            content["roster"] = json!(Self::roster(sig).ok_or_else(|| ChatError::Invalid(format!("participants invalides (1 à {MAX_ROOM} clés)")))?);
        }
        let peer = self.channel(peer).await?;
        // Relais d'appel : des identifiants pour ce seul destinataire, s'il est membre de la station ou ami réciproque (N¹).
        // `turn_host` : le nom sous lequel l'appelant joint sa station (le navigateur le connaît, la station pas toujours).
        let host = sig["turn_host"].as_str().filter(|h| !h.is_empty() && h.len() <= 253 && h.bytes().all(|b| b.is_ascii_alphanumeric() || b"-.:[]".contains(&b)));
        if let (true, Some((turn_secret, port)), Some(host)) = (ty == "offer", self.turn, host) {
            if self.mutual_or_member(owner, &peer).await {
                let (username, credential) = crate::turn::credentials(&turn_secret, &format!("n1-{}", &peer[..16]), u64::try_from(now()).unwrap_or(0), crate::turn::PEER_CREDENTIAL_TTL);
                content["ice"] = json!({ "urls": [format!("turn:{host}:{port}?transport=udp")], "username": username, "credential": credential });
            }
        }
        let secret = self.keys.secret(owner).ok_or(ChatError::NoKey)?;
        let at = now();
        let created = u64::try_from(at).unwrap_or(0);
        let rumor = nip59::Rumor { pubkey: owner.to_owned(), created_at: created, kind: kind::CALL_SIGNAL, tags: vec![vec!["p".into(), peer.clone()]], content: content.to_string() };
        let id = rumor.id().map_err(|e| ChatError::Invalid(e.to_string()))?;
        let wrap = nip59::gift_wrap_expiring(&secret, &peer, &rumor, created, created + CALL_TTL).map_err(|e| ChatError::Invalid(e.to_string()))?;
        (self.deliver)(peer.clone(), wrap).await.map_err(|e| ChatError::Unreachable(format!("appel non transmis : {e}")))?;
        if missed {
            self.store(owner, &peer, &id, true, at, UNANSWERED, "ok", true).await?;
            self.activity.publish_to(Audience::Owner(owner.to_owned()), "chat", "chat:msg", peer.clone(), None);
        }
        // Décroché, refusé ou occupé ici : les autres onglets du membre (téléphone, ordinateur) cessent de sonner.
        if matches!(ty, "answer" | "reject" | "busy") {
            self.activity.publish_to(Audience::Owner(owner.to_owned()), "call", "call:handled", String::new(), Some(json!({ "type": "handled", "call": call, "from": peer, "at": at })));
        }
        Ok(json!({ "sent": ty, "call": call, "peer": peer }))
    }

    /// Signal d'appel reçu. Seul un proche fait sonner ; un signal périmé ne sonne pas ; une sonnerie trop vieille ou un
    /// raccrochage sans réponse laisse « Appel manqué » dans la conversation. Rend le nombre de messages rangés.
    async fn handle_call(&self, owner: &str, sender: &str, rumor: &nip59::Rumor) -> usize {
        if sender == owner {
            return 0;
        }
        let Ok(v) = serde_json::from_str::<Value>(&rumor.content) else { return 0 };
        let (Some(ty), Some(call), Ok(id)) = (v["type"].as_str().filter(|t| CALL_TYPES.contains(t)), Self::call_id(&v), rumor.id()) else { return 0 };
        // Seul un proche fait sonner. Un participant d'appel de groupe qui n'est pas un proche n'est admis que dans une salle où
        // un proche a invité le membre, et seulement s'il figure sur la liste de cet hôte (jamais pour ouvrir une salle).
        let friend = self.follows(owner, sender).await;
        let key = (owner.to_owned(), call.clone());
        let in_room = {
            let mut rooms = self.rooms.lock().unwrap_or_else(|e| e.into_inner());
            rooms.retain(|_, r| r.2 > std::time::Instant::now());
            if friend && matches!(ty, "offer" | "roster") {
                if let Some(list) = Self::roster(&v) {
                    // L'hôte d'une salle est l'ami qui l'a ouverte : un autre ne peut pas en réécrire la liste.
                    let host = rooms.get(&key).map_or_else(|| sender.to_owned(), |r| r.0.clone());
                    if host == sender {
                        rooms.insert(key.clone(), (host, list, std::time::Instant::now() + std::time::Duration::from_secs(ROOM_SECS)));
                    }
                }
            }
            rooms.get(&key).is_some_and(|r| r.0 == sender || r.1.iter().any(|p| p == sender))
        };
        if !friend && !in_room {
            return 0;
        }
        let at = i64::try_from(rumor.created_at).unwrap_or(0).min(now());
        let stale = now() - at > RING_SECS;
        let mut stored = 0;
        if friend && ((ty == "offer" && stale) || (ty == "hangup" && v["missed"].as_bool() == Some(true))) {
            match self.store(owner, sender, &id, false, at, MISSED, "ok", false).await {
                Ok(true) => {
                    self.activity.publish_to(Audience::Owner(owner.to_owned()), "chat", "chat:msg", sender.to_owned(), None);
                    stored = 1;
                }
                Ok(false) => {}
                Err(e) => {
                    self.activity.publish_to(Audience::Captain, "alert", "chat:in", format!("appel manqué non rangé : {e}"), None);
                }
            }
        }
        if stale {
            return stored;
        }
        let label = self.label(owner, sender).await;
        let sdp = v["sdp"].as_str().filter(|d| d.len() <= MAX_SDP);
        // Accès au relais de l'appelant (offre seulement) : une adresse turn: et des identifiants, rien d'autre.
        let short = |k: &str| v["ice"][k].as_str().filter(|x| !x.is_empty() && x.len() <= 256).map(str::to_owned);
        let ice = (ty == "offer")
            .then(|| (v["ice"]["urls"][0].as_str().filter(|u| u.starts_with("turn:") && u.len() <= 300).map(str::to_owned), short("username"), short("credential")))
            .and_then(|(u, n, c)| Some(json!({ "urls": [u?], "username": n?, "credential": c? })));
        self.activity.publish_to(
            Audience::Owner(owner.to_owned()),
            "call",
            &format!("call:{ty}"),
            label.clone(),
            Some(json!({ "type": ty, "call": call, "from": sender, "label": label, "sdp": sdp, "ice": ice, "roster": Self::roster(&v), "friend": friend, "video": v["video"].as_bool().unwrap_or(true), "at": at })),
        );
        stored
    }

    /// Nom à montrer pour `peer` : sa fiche de contact, sinon son profil Nostr, sinon le début de sa clé.
    async fn label(&self, owner: &str, peer: &str) -> String {
        let (o, p) = (owner.to_owned(), peer.to_owned());
        let name: Option<String> = self
            .db
            .readers()
            .read(move |c| {
                Ok(c.query_row(
                    "SELECT COALESCE((SELECT json_extract(f.fields, '$.name') FROM contact_cards f WHERE f.owner_hex = ?1 AND json_extract(f.fields, '$.nostr') = ?2 LIMIT 1), \
                                     (SELECT json_extract(e.content, '$.name') FROM nostr_events e WHERE e.pubkey = ?2 AND e.kind = 0 ORDER BY e.created_at DESC LIMIT 1))",
                    [&o, &p],
                    |r| r.get::<_, Option<String>>(0),
                )?)
            })
            .await
            .ok()
            .flatten();
        name.filter(|n| !n.trim().is_empty()).unwrap_or_else(|| peer.chars().take(8).collect())
    }

    async fn handle(&self, owner: &str, secret: &[u8; 32], wrap: &Event) -> usize {
        let Ok(opened) = nip59::unwrap(secret, wrap) else { return 0 };
        let (sender, rumor) = (opened.sender, opened.rumor);
        if rumor.kind == kind::CALL_SIGNAL {
            return self.handle_call(owner, &sender, &rumor).await;
        }
        // Accusé de lecture : marque « lus » nos messages à cet ami, rien n'entre dans la conversation.
        if rumor.kind == kind::CHAT && rumor.tags.iter().any(|t| t.first().map(String::as_str) == Some("t") && t.get(1).map(String::as_str) == Some(RECEIPT_TAG)) {
            if sender != owner {
                if let Ok(at) = rumor.content.trim().parse::<i64>() {
                    self.seen(owner, &sender, at).await;
                }
            }
            return 0;
        }
        let (typing, edit, regroup, reaction) = (has_t(&rumor, TYPING_TAG), has_t(&rumor, EDIT_TAG), has_t(&rumor, GROUP_TAG), rumor.kind == kind::REACTION);
        let special = typing || edit || regroup || reaction;
        // Les messages de service (sauvegarde chez un ami, assistance) voyagent aussi en kind 14 : pas dans la conversation.
        if !special && (rumor.kind != kind::CHAT || rumor.is_station_protocol()) {
            return 0;
        }
        let Some(conv) = self.conversation_of(owner, &sender, &rumor).await else { return 0 };
        if typing {
            if sender != owner {
                self.activity.publish_to(Audience::Owner(owner.to_owned()), "chat", "chat:typing", conv.peer, Some(json!({ "author": sender })));
            }
            return 0;
        }
        if reaction {
            self.reaction_in(owner, &sender, &conv.peer, &rumor).await;
            return 0;
        }
        if edit {
            self.edit_in(owner, &sender, &conv.peer, &rumor).await;
            return 0;
        }
        if regroup {
            self.group_in(owner, &sender, &conv, &rumor).await;
            return 0;
        }
        let text = clean(&rumor.content);
        if text.is_empty() || text.chars().count() > MAX_CHARS {
            return 0;
        }
        let Ok(id) = rumor.id() else { return 0 };
        let at = i64::try_from(rumor.created_at).unwrap_or(0).min(now());
        if let Some((name, members)) = &conv.new_group {
            if self.save_group(owner, &conv.peer, name.as_deref(), members).await.is_err() {
                return 0;
            }
        }
        let group = conv.new_group.is_some() || self.group(owner, &conv.peer).await.is_some();
        let (peer, mine) = (conv.peer, sender == owner);
        let reply = rumor.tags.iter().find(|t| t.first().map(String::as_str) == Some("e")).and_then(|t| t.get(1)).filter(|e| is_hex64(e)).map(String::as_str);
        let reply_at = reply.and(rumor.tag_value("offset").and_then(|v| v.parse::<u64>().ok()));
        let files: Vec<Value> = rumor
            .tags
            .iter()
            .filter(|t| t.first().map(String::as_str) == Some("file"))
            .take(MAX_FILES)
            .filter_map(|t| file_of(&json!({ "name": t.get(1), "mime": t.get(2), "size": t.get(3).and_then(|s| s.parse::<u64>().ok()) })))
            .collect();
        let author = (group && !mine).then_some(sender.as_str());
        match self.store_row(owner, &peer, Row { id: &id, mine, at, text: &text, state: "ok", read: mine, reply, reply_at, files: &files, author }).await {
            Ok(true) => {
                if !mine {
                    self.activity.publish_to(Audience::Owner(owner.to_owned()), "chat", "chat:msg", peer.clone(), None);
                    // Réveil des navigateurs fermés (sans contenu), hors du chemin de réception ; pas pour une conversation en sourdine.
                    if let (Some(p), false) = (&self.push, self.muted(owner, &peer).await) {
                        let (p, o) = (p.clone(), owner.to_owned());
                        tokio::spawn(async move {
                            p.notify(&o).await;
                        });
                    }
                }
                1
            }
            Ok(false) => 0,
            Err(e) => {
                self.activity.publish_to(Audience::Captain, "alert", "chat:in", format!("message d'un ami non rangé : {e}"), None);
                0
            }
        }
    }

    /// Conversation d'une rumeur reçue, ou `None` si elle est refusée. Groupe : balise `g` (identifiant stable, même quand les
    /// participants changent) ou plusieurs `p` ; un groupe connu n'accepte que ses participants, un nouveau ne vient que d'un
    /// proche. 1 à 1 : l'expéditeur suivi, ou, pour la copie de soi, le destinataire.
    async fn conversation_of(&self, owner: &str, sender: &str, rumor: &nip59::Rumor) -> Option<Conv> {
        let mut members: Vec<String> = rumor.tags.iter().filter(|t| t.first().map(String::as_str) == Some("p")).filter_map(|t| t.get(1)).filter(|p| is_hex64(p)).cloned().collect();
        members.push(sender.to_owned());
        members.sort_unstable();
        members.dedup();
        let g = rumor.tag_value("g").filter(|g| is_hex64(g)).map(str::to_owned);
        if g.is_some() || members.len() > 2 {
            if !members.iter().any(|m| m == owner) || members.len() > MAX_GROUP + 1 {
                return None;
            }
            let gid = g.unwrap_or_else(|| group_id(&members));
            return match self.group(owner, &gid).await {
                Some((_, known)) => (sender == owner || known.iter().any(|m| m == sender)).then_some(Conv { peer: gid, new_group: None }),
                None => {
                    if sender != owner && !self.follows(owner, sender).await {
                        return None;
                    }
                    let name = rumor.tag_value("subject").map(|n| clean(n).chars().take(80).collect::<String>()).filter(|n| !n.is_empty());
                    Some(Conv { peer: gid, new_group: Some((name, members)) })
                }
            };
        }
        if sender == owner {
            return rumor.tag_value("p").filter(|p| is_hex64(p) && *p != owner).map(|p| Conv { peer: p.to_owned(), new_group: None });
        }
        self.follows(owner, sender).await.then(|| Conv { peer: sender.to_owned(), new_group: None })
    }

    /// Réaction reçue (kind 7) : une par auteur et par message de cette conversation ; vide, elle est retirée.
    async fn reaction_in(&self, owner: &str, sender: &str, peer: &str, rumor: &nip59::Rumor) {
        let (Some(msg), Some(emoji)) = (rumor.tag_value("e").filter(|e| is_hex64(e)), emoji_of(&rumor.content)) else { return };
        if self.put_reaction(owner, peer, msg, sender, &emoji).await {
            self.activity.publish_to(Audience::Owner(owner.to_owned()), "chat", "chat:seen", peer.to_owned(), None);
        }
    }

    async fn put_reaction(&self, owner: &str, peer: &str, msg: &str, author: &str, emoji: &str) -> bool {
        let (o, p, m, a, e, at) = (owner.to_owned(), peer.to_owned(), msg.to_owned(), author.to_owned(), emoji.to_owned(), now());
        self.db
            .writer()
            .transaction(move |tx| {
                let exists: Option<i64> = tx.query_row("SELECT 1 FROM chat_messages WHERE owner_hex = ?1 AND id = ?2 AND peer = ?3", rusqlite::params![o, m, p], |r| r.get(0)).optional()?;
                if exists.is_none() {
                    return Ok(false);
                }
                if e.is_empty() {
                    tx.execute("DELETE FROM chat_reactions WHERE owner_hex = ?1 AND msg_id = ?2 AND author = ?3", rusqlite::params![o, m, a])?;
                } else {
                    tx.execute("INSERT OR REPLACE INTO chat_reactions(owner_hex, msg_id, author, emoji, at) VALUES (?1, ?2, ?3, ?4, ?5)", rusqlite::params![o, m, a, e, at])?;
                }
                Ok(true)
            })
            .await
            .unwrap_or(false)
    }

    /// Modification reçue : seul l'auteur d'un message peut le modifier.
    async fn edit_in(&self, owner: &str, sender: &str, peer: &str, rumor: &nip59::Rumor) {
        let Some(msg) = rumor.tag_value("e").filter(|e| is_hex64(e)) else { return };
        let text = clean(&rumor.content);
        if text.is_empty() || text.chars().count() > MAX_CHARS {
            return;
        }
        let r = self
            .db
            .writer()
            .execute(
                "UPDATE chat_messages SET text = ?4, edited = 1 WHERE owner_hex = ?1 AND id = ?2 AND peer = ?3 \
                 AND ((mine = 1 AND ?5 = ?1) OR (mine = 0 AND COALESCE(author, peer) = ?5))",
                vec![owner.to_owned().into(), msg.to_owned().into(), peer.to_owned().into(), text.into(), sender.to_owned().into()],
            )
            .await;
        if r.is_ok_and(|r| r.changes > 0) {
            self.activity.publish_to(Audience::Owner(owner.to_owned()), "chat", "chat:seen", peer.to_owned(), None);
        }
    }

    /// Changement de groupe reçu, appliqué à la liste que le titulaire connaît (deux changements simultanés ne s'écrasent pas) :
    /// `{ op: rename | add | remove | leave, who?, name?, members }`. `members` (la liste vue par l'auteur) ne sert qu'au
    /// titulaire qui découvre le groupe — le nouveau venu.
    async fn group_in(&self, owner: &str, sender: &str, conv: &Conv, rumor: &nip59::Rumor) {
        let Ok(v) = serde_json::from_str::<Value>(&rumor.content) else { return };
        let full: Vec<String> = v["members"].as_array().map(|a| a.iter().filter_map(Value::as_str).filter(|h| is_hex64(h)).map(str::to_owned).collect()).unwrap_or_default();
        let (mut name, mut members) = match (&conv.new_group, self.group(owner, &conv.peer).await) {
            (None, Some((n, m))) => (n, m),
            _ => (conv.new_group.as_ref().and_then(|g| g.0.clone()), full),
        };
        let who = v["who"].as_str().filter(|h| is_hex64(h));
        match v["op"].as_str() {
            Some("rename") => name = v["name"].as_str().map(|n| clean(n).chars().take(80).collect::<String>()).filter(|n| !n.is_empty()),
            Some("add") => {
                if let Some(h) = who.filter(|h| !members.iter().any(|m| m == *h)) {
                    members.push(h.to_owned());
                }
            }
            Some("remove") => {
                if let Some(h) = who {
                    members.retain(|m| m != h);
                }
            }
            Some("leave") => members.retain(|m| m != sender),
            _ => return,
        }
        members.sort_unstable();
        members.dedup();
        if members.len() > MAX_GROUP {
            return;
        }
        if self.set_group(owner, &conv.peer, name.as_deref(), &members).await.is_ok() {
            self.activity.publish_to(Audience::Owner(owner.to_owned()), "chat", "chat:seen", conv.peer.clone(), None);
        }
    }

    async fn set_group(&self, owner: &str, id: &str, name: Option<&str>, members: &[String]) -> Result<(), ChatError> {
        self.db
            .writer()
            .execute(
                "INSERT INTO chat_groups(owner_hex, id, name, members) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(owner_hex, id) DO UPDATE SET name = COALESCE(excluded.name, chat_groups.name), members = excluded.members",
                vec![owner.to_owned().into(), id.to_owned().into(), name.map(str::to_owned).into(), serde_json::to_string(members).unwrap_or_default().into()],
            )
            .await
            .map_err(db_err)?;
        Ok(())
    }

    async fn muted(&self, owner: &str, peer: &str) -> bool {
        let (o, p) = (owner.to_owned(), peer.to_owned());
        self.db.readers().read(move |c| Ok(c.query_row("SELECT muted FROM chat_prefs WHERE owner_hex = ?1 AND peer = ?2", [&o, &p], |r| r.get::<_, i64>(0)).optional()?)).await.ok().flatten() == Some(1)
    }

    /// Accusés de lecture et « en train d'écrire » partagés par le titulaire (oui par défaut).
    async fn shares_activity(&self, owner: &str) -> bool {
        let o = owner.to_owned();
        self.db.readers().read(move |c| Ok(c.query_row("SELECT value FROM kv WHERE scope = 'chat_privacy' AND key = ?1", [&o], |r| r.get::<_, String>(0)).optional()?)).await.ok().flatten().as_deref() != Some("off")
    }

    /// Emballe une rumeur pour la conversation `peer` (chaque participant d'un groupe, balise `g` comprise) et la remet ; `extra`
    /// reçoit aussi la rumeur (participant ajouté), `ttl` la fait expirer, `copy` en garde une copie pour les autres stations du
    /// titulaire. Rend le nombre de remises réussies.
    #[allow(clippy::too_many_arguments)]
    async fn emit(&self, owner: &str, peer: &str, k: u32, extra_tags: Vec<Vec<String>>, content: String, ttl: Option<u64>, copy: bool, extra: &[String]) -> Result<usize, ChatError> {
        let group = self.group(owner, peer).await;
        let mut targets: Vec<String> = match &group {
            Some((_, m)) => m.iter().chain(extra).filter(|x| *x != owner).cloned().collect(),
            None => vec![self.channel(peer).await?],
        };
        targets.sort_unstable();
        targets.dedup();
        let secret = self.keys.secret(owner).ok_or(ChatError::NoKey)?;
        let created = u64::try_from(now()).unwrap_or(0);
        let mut tags: Vec<Vec<String>> = targets.iter().map(|t| vec!["p".into(), t.clone()]).collect();
        if group.is_some() {
            tags.push(vec!["g".into(), peer.to_owned()]);
        }
        tags.extend(extra_tags);
        let rumor = nip59::Rumor { pubkey: owner.to_owned(), created_at: created, kind: k, tags, content };
        let wrap = |to: &str| match ttl {
            Some(t) => nip59::gift_wrap_expiring(&secret, to, &rumor, created, created + t),
            None => nip59::gift_wrap(&secret, to, &rumor, created),
        };
        let mut ok = 0;
        for t in &targets {
            let w = wrap(t).map_err(|e| ChatError::Invalid(e.to_string()))?;
            if (self.deliver)(t.clone(), w).await.is_ok() {
                ok += 1;
            }
        }
        if copy {
            let w = wrap(owner).map_err(|e| ChatError::Invalid(e.to_string()))?;
            let _ = (self.publish_local)(owner.to_owned(), w).await;
        }
        Ok(ok)
    }

    /// Conversation d'un message du titulaire.
    async fn peer_of(&self, owner: &str, id: &str) -> Result<(String, bool), ChatError> {
        let (o, i) = (owner.to_owned(), id.to_owned());
        self.db
            .readers()
            .read(move |c| Ok(c.query_row("SELECT peer, mine FROM chat_messages WHERE owner_hex = ?1 AND id = ?2", [&o, &i], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? == 1))).optional()?))
            .await
            .map_err(db_err)?
            .ok_or_else(|| ChatError::Invalid("message inconnu".into()))
    }

    /// Réagit à un message (`emoji` vide : retire sa réaction).
    pub async fn react(&self, owner: &str, id: &str, emoji: &str) -> Result<Value, ChatError> {
        if !is_hex64(id) {
            return Err(ChatError::Invalid("message invalide".into()));
        }
        let emoji = emoji_of(emoji).ok_or_else(|| ChatError::Invalid("réaction : un emoji".into()))?;
        let (peer, _) = self.peer_of(owner, id).await?;
        if self.group(owner, &peer).await.is_some_and(|(_, m)| !m.iter().any(|x| x == owner)) {
            return Err(ChatError::Invalid("vous avez quitté ce groupe".into()));
        }
        self.put_reaction(owner, &peer, id, owner, &emoji).await;
        self.emit(owner, &peer, kind::REACTION, vec![vec!["e".into(), id.to_owned()], vec!["k".into(), kind::CHAT.to_string()]], emoji.clone(), None, true, &[]).await?;
        self.activity.publish_to(Audience::Owner(owner.to_owned()), "chat", "chat:seen", peer.clone(), None);
        Ok(json!({ "id": id, "emoji": emoji }))
    }

    /// Signale « en train d'écrire » à la conversation (rien si le titulaire ne partage pas son activité).
    pub async fn typing(&self, owner: &str, peer: &str) -> Result<Value, ChatError> {
        if !is_hex64(peer) || peer == owner {
            return Err(ChatError::Invalid("destinataire invalide".into()));
        }
        if !self.shares_activity(owner).await {
            return Ok(json!({ "sent": 0 }));
        }
        let n = self.emit(owner, peer, kind::CHAT, vec![vec!["t".into(), TYPING_TAG.into()]], String::new(), Some(TYPING_TTL), false, &[]).await?;
        Ok(json!({ "sent": n }))
    }

    /// Modifie un message envoyé par le titulaire.
    pub async fn edit(&self, owner: &str, id: &str, text: &str) -> Result<Value, ChatError> {
        if !is_hex64(id) {
            return Err(ChatError::Invalid("message invalide".into()));
        }
        let text = clean(text);
        if text.is_empty() || text.chars().count() > MAX_CHARS {
            return Err(ChatError::Invalid(format!("texte de 1 à {MAX_CHARS} caractères")));
        }
        let (peer, mine) = self.peer_of(owner, id).await?;
        if !mine {
            return Err(ChatError::Invalid("on ne modifie que ses propres messages".into()));
        }
        self.db.writer().execute("UPDATE chat_messages SET text = ?3, edited = 1 WHERE owner_hex = ?1 AND id = ?2", vec![owner.to_owned().into(), id.to_owned().into(), text.clone().into()]).await.map_err(db_err)?;
        self.emit(owner, &peer, kind::CHAT, vec![vec!["t".into(), EDIT_TAG.into()], vec!["e".into(), id.to_owned()]], text.clone(), None, true, &[]).await?;
        self.activity.publish_to(Audience::Owner(owner.to_owned()), "chat", "chat:seen", peer, None);
        Ok(json!({ "id": id, "text": text, "edited": true }))
    }

    /// Change un groupe : `{ rename }`, `{ add }`, `{ remove }` ou `{ leave: true }`. Tous les participants (anciens et nouveaux)
    /// sont prévenus ; un participant retiré, ou qui part, garde l'historique sans pouvoir écrire.
    pub async fn group_update(&self, owner: &str, id: &str, op: &Value) -> Result<Value, ChatError> {
        let (name, mut members) = self.group(owner, id).await.ok_or_else(|| ChatError::Invalid("groupe inconnu".into()))?;
        if !members.iter().any(|m| m == owner) {
            return Err(ChatError::Invalid("vous avez quitté ce groupe".into()));
        }
        let mut name = name;
        let mut extra = Vec::new();
        let delta;
        if let Some(n) = op["rename"].as_str() {
            let n: String = clean(n).chars().take(80).collect();
            name = (!n.is_empty()).then_some(n);
            delta = json!({ "op": "rename", "name": name });
        } else if let Some(h) = op["add"].as_str() {
            delta = json!({ "op": "add", "who": h });
            if !is_hex64(h) || members.len() >= MAX_GROUP {
                return Err(ChatError::Invalid(format!("{MAX_GROUP} participants au plus")));
            }
            if !members.iter().any(|m| m == h) {
                members.push(h.to_owned());
                extra.push(h.to_owned());
            }
        } else if let Some(h) = op["remove"].as_str() {
            if h == owner {
                return Err(ChatError::Invalid("pour partir, quittez le groupe".into()));
            }
            members.retain(|m| m != h);
            delta = json!({ "op": "remove", "who": h });
        } else if op["leave"].as_bool() == Some(true) {
            members.retain(|m| m != owner);
            delta = json!({ "op": "leave" });
        } else {
            return Err(ChatError::Invalid("changement de groupe inconnu".into()));
        }
        members.sort_unstable();
        let mut delta = delta;
        delta["members"] = json!(members);
        let content = delta.to_string();
        let mut tags = vec![vec!["t".into(), GROUP_TAG.into()]];
        if let Some(n) = &name {
            tags.push(vec!["subject".into(), n.clone()]);
        }
        // Avant l'enregistrement : la rumeur part vers les anciens participants (le retiré l'apprend) et les nouveaux.
        self.emit(owner, id, kind::CHAT, tags, content, None, true, &extra).await?;
        self.set_group(owner, id, name.as_deref(), &members).await?;
        self.activity.publish_to(Audience::Owner(owner.to_owned()), "chat", "chat:seen", id.to_owned(), None);
        Ok(json!({ "id": id, "name": name, "members": members }))
    }

    /// Épingler, archiver, mettre en sourdine une conversation (champs `pinned`, `archived`, `muted` présents seulement).
    pub async fn prefs(&self, owner: &str, peer: &str, v: &Value) -> Result<Value, ChatError> {
        if !is_hex64(peer) {
            return Err(ChatError::Invalid("conversation invalide".into()));
        }
        let get = |k: &str| v[k].as_bool().map(i64::from);
        let (o, p, pin, arc, mute) = (owner.to_owned(), peer.to_owned(), get("pinned"), get("archived"), get("muted"));
        self.db
            .writer()
            .execute(
                "INSERT INTO chat_prefs(owner_hex, peer, pinned, archived, muted) VALUES (?1, ?2, COALESCE(?3, 0), COALESCE(?4, 0), COALESCE(?5, 0)) \
                 ON CONFLICT(owner_hex, peer) DO UPDATE SET pinned = COALESCE(?3, pinned), archived = COALESCE(?4, archived), muted = COALESCE(?5, muted)",
                vec![o.into(), p.into(), pin.into(), arc.into(), mute.into()],
            )
            .await
            .map_err(db_err)?;
        self.activity.publish_to(Audience::Owner(owner.to_owned()), "chat", "chat:seen", peer.to_owned(), None);
        Ok(json!({ "peer": peer }))
    }

    /// Réglages du chat du titulaire : `receipts` (accusés de lecture et « en train d'écrire »). `set` les change.
    pub async fn settings(&self, owner: &str, set: Option<&Value>) -> Result<Value, ChatError> {
        if let Some(on) = set.and_then(|v| v["receipts"].as_bool()) {
            self.db
                .writer()
                .execute("INSERT OR REPLACE INTO kv(scope, key, value, updated_at) VALUES ('chat_privacy', ?1, ?2, ?3)", vec![owner.to_owned().into(), (if on { "on" } else { "off" }).to_owned().into(), now().into()])
                .await
                .map_err(db_err)?;
        }
        Ok(json!({ "receipts": self.shares_activity(owner).await }))
    }

    /// Boucle de réception jusqu'à l'arrêt. Une enveloppe NIP-59 acceptée par le relai réveille la lecture **tout de suite** :
    /// une sonnerie, puis la réponse de l'appelé, ne font plus chacune attendre le tour suivant (jusqu'à 2 × `every` de
    /// poignée de main perdus). Le passage régulier toutes les `every` reste le filet de sécurité : enveloppes rangées
    /// sans passer par le relai (synchronisation, rejeu), canal en retard ou fermé.
    pub async fn run(self: Arc<Self>, every: Duration, mut relay: broadcast::Receiver<Arc<Event>>, mut stop: watch::Receiver<bool>) {
        let mut live = true;
        loop {
            self.poll_once().await;
            let deadline = tokio::time::sleep(every);
            tokio::pin!(deadline);
            loop {
                tokio::select! {
                    () = &mut deadline => break,
                    e = relay.recv(), if live => match e {
                        Ok(ev) if ev.kind == kind::GIFT_WRAP => {
                            // Une rafale (offre puis candidats réseau) se lit en un seul passage.
                            while relay.try_recv().is_ok() {}
                            break;
                        }
                        Ok(_) => {}
                        Err(broadcast::error::RecvError::Lagged(_)) => break,
                        Err(broadcast::error::RecvError::Closed) => live = false,
                    },
                    _ = stop.changed() => return,
                }
            }
        }
    }
}
