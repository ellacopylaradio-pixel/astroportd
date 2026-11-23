//! Social : carnet de contacts, messagerie, présence et visio, agents, BRO, serveur MCP.
//!
//! Routes et gestionnaires de ce domaine ; le trait [`Backend`] et les types partagés restent dans `api/mod.rs`.

use super::*;

/// Social : carnet de contacts, messagerie, présence et visio, agents, BRO, serveur MCP.
pub trait SocialApi: Auth {
    /// Paramètres IA du MULTIPASS, sans la clé d'API : `{ ollama_url, provider, has_key, model, declared }`.
    fn ai_params(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("paramètres IA non disponibles sur cette station")) }
    }
    /// Change les paramètres IA (champ absent : inchangé ; `null` ou vide : effacé).
    fn set_ai_params(&self, _who: &Principal, _patch: Value) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("paramètres IA non disponibles sur cette station")) }
    }
    /// « Lecture de vos documents » : vue, stations proposées, accord, arrêt.
    fn reading(&self, _who: &Principal, _op: ReadingOp) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("lecture des documents non disponible sur cette station")) }
    }
    /// Carnet d'adresses : une opération ([`BookOp`]) sur les fiches du titulaire.
    fn book(&self, _who: &Principal, _op: BookOp) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("le carnet d'adresses n'est pas disponible sur cette station")) }
    }
    /// « Se rencontrer » ([`MeetOp`]). Jamais d'email complet dans les réponses.
    fn meet(&self, _who: &Principal, _op: MeetOp) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("« Se rencontrer » n'est pas disponible sur cette station")) }
    }
    /// « Mes cercles » : niveaux de proximité, privés. `set` = `(clé, niveau 1..=5 | None pour le défaut)`.
    /// Rend `{ levels, people: [{ hex, label, level, ranked, friends, met_verified }] }`.
    fn circles(&self, _who: &Principal, _set: Option<(String, Option<u8>)>) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("les cercles ne sont pas disponibles sur cette station")) }
    }
    /// Toile de confiance N² (`astro_economy::wot`) vue par le membre : `{ member, root, certs, needed, reached_pct,
    /// mutual, certifiers: [noms], web: { members, referees, roots } }`. Seules les rencontres prouvées comptent.
    fn wot(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("la toile de confiance n'est pas disponible sur cette station")) }
    }
    /// Présence visio : `{ reachable, friends: { hex: statut } }` ; `body` (POST) change le choix `reachable` ou l'état `in_call`.
    fn presence(&self, _who: &Principal, _body: Option<Value>) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("la présence visio n'est pas disponible sur cette station")) }
    }
    /// Visio : serveurs de traversée à donner au navigateur (`{ stun_port, servers }`).
    fn call_ice(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("la visio n'est pas disponible sur cette station")) }
    }
    /// Chat privé : une opération ([`ChatOp`]) pour le titulaire (dont les signaux d'appel visio).
    fn chat(&self, _who: &Principal, _op: ChatOp) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("le chat privé n'est pas disponible sur cette station")) }
    }
    /// Mes clés dans les UPlanet de cette station et les liens prouvés : `{ plane, keys: [{plane, hex, via}] }`.
    fn my_links(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("annuaire des UPlanet non disponible sur cette station")) }
    }
    /// Annonce, sur demande, que mes comptes des autres UPlanet de la station sont les miens (lien signé des deux clés, public).
    fn announce_links(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("annuaire des UPlanet non disponible sur cette station")) }
    }
    /// Dialogue avec BRO.
    fn bro_say(&self, who: &Principal, text: String) -> impl Future<Output = Result<Value, ApiError>> + Send;
    /// Agents déclarés par le membre (sans jeton ni empreinte).
    fn agents_list(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("agents non disponibles sur cette station")) }
    }
    /// Déclare un agent ; rend son jeton, montré une seule fois.
    fn agents_enroll(&self, _who: &Principal, _name: String, _group: String, _max_secs: u64, _totp: bool, _station: Option<String>) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("agents non disponibles sur cette station")) }
    }
    /// Retire un agent (son jeton cesse de fonctionner).
    fn agents_revoke(&self, _who: &Principal, _id: String) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("agents non disponibles sur cette station")) }
    }
    /// Demandes d'accès en cours : `{ requests: [{ id, agent, site, reason, state, until }] }`.
    fn agents_requests(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("agents non disponibles sur cette station")) }
    }
    /// Le membre accorde (`secs` facultatif) ou refuse une demande.
    fn agents_decide(&self, _who: &Principal, _id: String, _approve: bool, _secs: Option<u64>, _hosts: Vec<String>) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("agents non disponibles sur cette station")) }
    }
    /// Mode navigateur : le membre s'est connecté et prête le navigateur à l'agent.
    fn agents_ready(&self, _who: &Principal, _id: String) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("agents non disponibles sur cette station")) }
    }
    /// Le membre reprend la main : refuse, ou arrête tout de suite un accord ou un navigateur prêté.
    fn agents_stop(&self, _who: &Principal, _id: String) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("agents non disponibles sur cette station")) }
    }
    /// Écran du navigateur prêté (images JSON `{ seq, w, h, jpeg }`), pour le membre seul.
    fn agents_screen(&self, _who: &Principal, _id: String) -> impl Future<Output = Result<tokio::sync::watch::Receiver<Option<Arc<str>>>, ApiError>> + Send {
        async { Err(ApiError::unavailable("agents non disponibles sur cette station")) }
    }
    /// Geste du membre dans le navigateur prêté (`{ type: click|scroll|key|text, … }`).
    fn agents_input(&self, _who: &Principal, _id: String, _gesture: Value) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("agents non disponibles sur cette station")) }
    }
    /// Naviguer ensemble : mes navigations partagées et celles où je suis invité.
    fn cosurf_list(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("navigation partagée non disponible sur cette station")) }
    }
    /// Ouvre une navigation partagée sur `url` avec des amis (`(clé, nom)`), au niveau `level` (watch, point, drive).
    fn cosurf_create(&self, _who: &Principal, _url: String, _guests: Vec<(String, String)>, _level: String) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("navigation partagée non disponible sur cette station")) }
    }
    /// Une navigation partagée (`station` : celle qui la tient, si ce n'est pas celle-ci).
    fn cosurf_view(&self, _who: &Principal, _id: String, _station: Option<String>) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("navigation partagée non disponible sur cette station")) }
    }
    /// Le propriétaire change le niveau d'un invité.
    fn cosurf_level(&self, _who: &Principal, _id: String, _guest: String, _level: String) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("navigation partagée non disponible sur cette station")) }
    }
    /// Le propriétaire termine ; un invité s'en va.
    fn cosurf_stop(&self, _who: &Principal, _id: String, _station: Option<String>) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("navigation partagée non disponible sur cette station")) }
    }
    /// Flux de la navigation partagée : `(frame | pointer | level, JSON)`.
    fn cosurf_screen(&self, _who: &Principal, _id: String, _station: Option<String>) -> impl Future<Output = Result<tokio::sync::mpsc::Receiver<(String, Arc<str>)>, ApiError>> + Send {
        async { Err(ApiError::unavailable("navigation partagée non disponible sur cette station")) }
    }
    /// Geste dans la page partagée (`{ type: click|scroll|key|text|point, … }`).
    fn cosurf_input(&self, _who: &Principal, _id: String, _station: Option<String>, _gesture: Value) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("navigation partagée non disponible sur cette station")) }
    }
    /// Serveur MCP (JSON-RPC 2.0) : `bearer` = jeton d'agent. `Ok(None)` : notification, rien à répondre ;
    /// jeton absent ou faux : `Err` en 401.
    /// `station` (`?station=<PeerID>`) : station du membre quand l'agent tourne sur une autre station de la constellation.
    fn mcp(&self, _bearer: Option<String>, _station: Option<String>, _body: Value) -> impl Future<Output = Result<Option<Value>, ApiError>> + Send {
        async { Err(ApiError::unavailable("agents non disponibles sur cette station")) }
    }
    /// Annuaire : fiches de contact, état de leur liaison à un MULTIPASS, personnes reconnues sans fiche.
    fn contacts(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("l'annuaire n'est pas disponible sur cette station")) }
    }
    /// Relit le carnet d'adresses du compte et rapproche les visages.
    fn contacts_sync(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("l'annuaire n'est pas disponible sur cette station")) }
    }
    /// Crée la fiche d'une personne reconnue.
    fn contacts_create(&self, _who: &Principal, _name: String, _email: Option<String>) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("l'annuaire n'est pas disponible sur cette station")) }
    }
    /// Relie (ou délie) une fiche à un MULTIPASS.
    fn contacts_link(&self, _who: &Principal, _path: String, _target: Option<String>) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("l'annuaire n'est pas disponible sur cette station")) }
    }
}

pub(super) fn routes<B: SocialApi>() -> Router<Arc<UiState<B>>> {
    Router::new()
        .route("/api/v1/activity", get(activity_stream::<B>))
        .route("/api/v1/me/ai", get(ai_params::<B>).post(set_ai_params::<B>))
        .route("/api/v1/me/reading", get(reading_view::<B>).post(reading_set::<B>))
        .route("/api/v1/me/reading/offers", get(reading_offers::<B>))
        .route("/api/v1/book", get(book_list::<B>))
        .route("/api/v1/book/card", get(book_get::<B>).post(book_save::<B>))
        .route("/api/v1/book/delete", post(book_delete::<B>))
        .route("/api/v1/book/delete-many", post(book_delete_many::<B>))
        .route("/api/v1/book/photo", get(book_photo::<B>).post(book_set_photo::<B>))
        .route("/api/v1/book/photo-choices", get(book_photo_choices::<B>))
        .route("/api/v1/book/take", post(book_take::<B>))
        .route("/api/v1/book/group", post(book_group::<B>))
        .route("/api/v1/book/import", post(book_import::<B>))
        .route("/api/v1/book/duplicates", get(book_duplicates::<B>))
        .route("/api/v1/book/merge", post(book_merge::<B>))
        .route("/api/v1/book/merge-exact", post(book_merge_exact::<B>))
        .route("/api/v1/book/me", get(book_me::<B>).post(book_save_me::<B>))
        .route("/api/v1/me/links", get(my_links::<B>).post(announce_links::<B>))
        .route("/api/v1/chat", get(chat_list::<B>))
        .route("/api/v1/chat/messages", get(chat_messages::<B>))
        .route("/api/v1/chat/send", post(chat_send::<B>))
        .route("/api/v1/chat/read", post(chat_read::<B>))
        .route("/api/v1/chat/clear", post(chat_clear::<B>))
        .route("/api/v1/chat/delete", post(chat_delete::<B>))
        .route("/api/v1/chat/search", get(chat_search::<B>))
        .route("/api/v1/chat/group", post(chat_group::<B>))
        .route("/api/v1/chat/group/update", post(chat_group_update::<B>))
        .route("/api/v1/chat/react", post(chat_react::<B>))
        .route("/api/v1/chat/typing", post(chat_typing::<B>))
        .route("/api/v1/chat/edit", post(chat_edit::<B>))
        .route("/api/v1/chat/prefs", post(chat_prefs::<B>))
        .route("/api/v1/chat/settings", get(chat_settings_get::<B>).post(chat_settings_set::<B>))
        .route("/api/v1/chat/preview", get(chat_preview::<B>))
        .route("/api/v1/push/key", get(push_key::<B>))
        .route("/api/v1/push/subscribe", post(push_subscribe::<B>))
        .route("/api/v1/call/signal", post(call_signal::<B>))
        .route("/api/v1/call/ice", get(call_ice::<B>))
        .route("/api/v1/presence", get(presence_get::<B>).post(presence_set::<B>))
        .route("/api/v1/me/meet", get(meet_list::<B>))
        .route("/api/v1/me/circles", get(circles_get::<B>).post(circles_set::<B>))
        .route("/api/v1/me/wot", get(wot_get::<B>))
        .route("/api/v1/me/meet/code", get(meet_code::<B>))
        .route("/api/v1/me/meet/{hex}", get(meet_get::<B>).post(meet_follow::<B>))
        .route("/api/v1/bro/say", post(bro_say::<B>))
        .route("/api/v1/agents", get(agents_list::<B>).post(agents_enroll::<B>))
        .route("/api/v1/agents/revoke", post(agents_revoke::<B>))
        .route("/api/v1/agents/requests", get(agents_requests::<B>))
        .route("/api/v1/agents/decide", post(agents_decide::<B>))
        .route("/api/v1/agents/ready", post(agents_ready::<B>))
        .route("/api/v1/agents/stop", post(agents_stop::<B>))
        .route("/api/v1/agents/screen", get(agents_screen::<B>))
        .route("/api/v1/agents/input", post(agents_input::<B>))
        .route("/api/v1/cosurf", get(cosurf_list::<B>).post(cosurf_create::<B>))
        .route("/api/v1/cosurf/view", get(cosurf_view::<B>))
        .route("/api/v1/cosurf/level", post(cosurf_level::<B>))
        .route("/api/v1/cosurf/stop", post(cosurf_stop::<B>))
        .route("/api/v1/cosurf/screen", get(cosurf_screen::<B>))
        .route("/api/v1/cosurf/input", post(cosurf_input::<B>))
        .route("/mcp", post(mcp::<B>))
        .route("/api/v1/contacts", get(contacts::<B>))
        .route("/api/v1/contacts/sync", post(contacts_sync::<B>))
        .route("/api/v1/contacts/from-person", post(contacts_create::<B>))
        .route("/api/v1/contacts/link", post(contacts_link::<B>))
}

#[derive(Deserialize)]
struct BookListQuery {
    q: Option<String>,
    group: Option<String>,
}

#[derive(Deserialize)]
struct BookPathQuery {
    path: String,
}

#[derive(Deserialize)]
struct BookSaveBody {
    path: Option<String>,
    fields: Value,
}

#[derive(Deserialize)]
struct BookPathBody {
    path: String,
}

#[derive(Deserialize)]
struct BookTakeBody {
    path: String,
    keys: Vec<String>,
}

#[derive(Deserialize)]
struct BookGroupBody {
    from: String,
    to: Option<String>,
}

#[derive(Deserialize)]
struct BookImportBody {
    vcf: String,
}

#[derive(Deserialize)]
struct BookMergeBody {
    keep: String,
    drop: Vec<String>,
}

#[derive(Deserialize)]
struct BookMergeExactBody {
    #[serde(default)]
    apply: bool,
    #[serde(default)]
    limit: Option<usize>,
}

async fn book_list<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Query(q): Query<BookListQuery>) -> Response {
    reply(st.backend.book(&p, BookOp::List(q.q.unwrap_or_default().chars().take(200).collect(), q.group.filter(|g| g.len() <= 80))).await)
}

async fn book_get<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Query(q): Query<BookPathQuery>) -> Response {
    reply(st.backend.book(&p, BookOp::Get(q.path)).await)
}

async fn book_save<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<BookSaveBody>) -> Response {
    reply(st.backend.book(&p, BookOp::Save(b.path, b.fields)).await)
}

#[derive(Deserialize)]
struct BookPathsBody {
paths: Vec<String>,
}

async fn book_delete_many<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<BookPathsBody>) -> Response {
reply(st.backend.book(&p, BookOp::DeleteMany(b.paths)).await)
}

async fn book_photo<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Query(q): Query<BookPathQuery>) -> Response {
reply(st.backend.book(&p, BookOp::Photo(q.path)).await)
}

#[derive(Deserialize)]
struct BookPhotoBody {
path: String,
uri: Option<String>,
}

async fn book_set_photo<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<BookPhotoBody>) -> Response {
reply(st.backend.book(&p, BookOp::SetPhoto(b.path, b.uri)).await)
}

async fn book_photo_choices<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Query(q): Query<BookPathQuery>) -> Response {
reply(st.backend.book(&p, BookOp::PhotoChoices(q.path)).await)
}

async fn book_delete<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<BookPathBody>) -> Response {
    reply(st.backend.book(&p, BookOp::Delete(b.path)).await)
}

async fn book_take<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<BookTakeBody>) -> Response {
    reply(st.backend.book(&p, BookOp::Take(b.path, b.keys.into_iter().take(10).collect())).await)
}

async fn book_group<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<BookGroupBody>) -> Response {
    reply(st.backend.book(&p, BookOp::Group(b.from, b.to)).await)
}

async fn book_import<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<BookImportBody>) -> Response {
    reply(st.backend.book(&p, BookOp::Import(b.vcf)).await)
}

async fn book_duplicates<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.book(&p, BookOp::Duplicates).await)
}

async fn book_merge<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<BookMergeBody>) -> Response {
    reply(st.backend.book(&p, BookOp::Merge(b.keep, b.drop)).await)
}

async fn book_merge_exact<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<BookMergeExactBody>) -> Response {
    reply(st.backend.book(&p, BookOp::MergeExact(b.apply, b.limit.unwrap_or(50).clamp(1, 200))).await)
}

async fn book_me<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.book(&p, BookOp::Me).await)
}

async fn book_save_me<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(fields): Json<Value>) -> Response {
    reply(st.backend.book(&p, BookOp::SaveMe(fields)).await)
}

#[derive(Deserialize)]
struct ChatPeerQuery {
    peer: String,
    before: Option<i64>,
}

#[derive(Deserialize)]
struct ChatSendBody {
    peer: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    reply: Option<String>,
    #[serde(default)]
    reply_at: Option<u64>,
    #[serde(default)]
    files: Vec<Value>,
}

#[derive(Deserialize)]
struct ChatPeerBody {
    peer: String,
}

#[derive(Deserialize)]
struct ChatIdBody {
    id: String,
}

#[derive(Deserialize)]
struct ChatSearchQuery {
    q: String,
}

#[derive(Deserialize)]
struct ChatPreviewQuery {
    url: String,
}

async fn my_links<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.my_links(&p).await)
}

async fn announce_links<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.announce_links(&p).await)
}

async fn chat_list<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.chat(&p, ChatOp::List).await)
}

async fn chat_messages<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Query(q): Query<ChatPeerQuery>) -> Response {
    reply(st.backend.chat(&p, ChatOp::Messages(q.peer.chars().take(80).collect(), q.before)).await)
}

async fn call_signal<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<Value>) -> Response {
    reply(st.backend.chat(&p, ChatOp::Signal(b)).await)
}

async fn wot_get<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    private(reply(st.backend.wot(&p).await))
}

async fn circles_get<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    private(reply(st.backend.circles(&p, None).await))
}

/// Ranger une personne : `{ "hex": "<clé>", "level": 1..5 | null }`.
#[derive(Deserialize)]
struct CircleBody {
    hex: String,
    level: Option<u8>,
}

async fn circles_set<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<CircleBody>) -> Response {
    let to = match meet_target(&p, &b.hex) {
        Ok(h) => h,
        Err(e) => return reply(Err(e)),
    };
    if b.level.is_some_and(|l| !(1..=5).contains(&l)) {
        return reply(Err(ApiError::bad_request("niveau de 1 (Contact) à 5 (Famille)")));
    }
    private(reply(st.backend.circles(&p, Some((to, b.level))).await))
}

/// Opération « Se rencontrer ».
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MeetOp {
    /// Demandes reçues (`requests` : qui vient d'ajouter ce membre) et, avec une recherche, les MULTIPASS de la
    /// constellation dont le nom la contient (`people`).
    List(Option<String>),
    /// Où en est le lien avec cette clé : `i_follow`, `follows_me`, `friends`, `met` (rencontre attestée : `how`, `verified`).
    Status(String),
    /// Ajouter cette clé à ses proches. Avec la preuve de présence d'un code « rencontre » scanné (`qr`), sinon liaison par
    /// nomination (`named`) : Ğ1-N² ne compte, contre les faux comptes, que les rencontres prouvées.
    Add {
        /// Clé de la personne.
        to: String,
        /// Preuve de présence lue dans son code « rencontre ».
        proof: Option<String>,
    },
    /// Ma preuve de présence du moment (`proof`, `expires_in`), pour mon code « rencontre ».
    Code,
}

/// Recherche « Se rencontrer » : 2 à 64 caractères.
#[derive(Deserialize)]
struct MeetQuery {
    q: Option<String>,
}

async fn meet_list<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Query(m): Query<MeetQuery>) -> Response {
    let q = m.q.map(|q| q.trim().to_lowercase()).filter(|q| !q.is_empty());
    if q.as_ref().is_some_and(|q| !(2..=64).contains(&q.chars().count())) {
        return reply(Err(ApiError::bad_request("recherche de 2 à 64 caractères")));
    }
    private(reply(st.backend.meet(&p, MeetOp::List(q)).await))
}

/// Clé d'un MULTIPASS (64 hexadécimaux, minuscules), autre que la sienne.
fn meet_target(p: &Principal, hex: &str) -> Result<String, ApiError> {
    let h = hex.to_ascii_lowercase();
    if h.len() != 64 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(ApiError::bad_request("clé attendue (64 hexadécimaux)"));
    }
    if h == p.hex {
        return Err(ApiError::bad_request("c'est votre propre code"));
    }
    Ok(h)
}

async fn meet_get<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Path(hex): Path<String>) -> Response {
    match meet_target(&p, &hex) {
        Ok(h) => private(reply(st.backend.meet(&p, MeetOp::Status(h)).await)),
        Err(e) => reply(Err(e)),
    }
}

/// Corps facultatif de l'ajout : `{ "proof": "<tranche>.<signature>" }` (code « rencontre » scanné).
#[derive(Deserialize, Default)]
struct MeetBody {
    proof: Option<String>,
}

async fn meet_follow<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Path(hex): Path<String>, body: axum::body::Bytes) -> Response {
    let b: MeetBody = if body.is_empty() { MeetBody::default() } else {
        match serde_json::from_slice(&body) {
            Ok(b) => b,
            Err(_) => return reply(Err(ApiError::bad_request("JSON attendu : { \"proof\": \"…\" }"))),
        }
    };
    if b.proof.as_ref().is_some_and(|x| x.len() > 160) {
        return reply(Err(ApiError::bad_request("preuve trop longue")));
    }
    match meet_target(&p, &hex) {
        Ok(to) => private(reply(st.backend.meet(&p, MeetOp::Add { to, proof: b.proof }).await)),
        Err(e) => reply(Err(e)),
    }
}

/// Mon code « rencontre » : `<station>/#/rencontre?to=<clé>&m=<preuve>` et son QR. La preuve change toutes les 2 minutes :
/// l'écran le redemande à chaque minute.
async fn meet_code<B: SocialApi>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, Authed(p): Authed) -> Response {
    let v = match st.backend.meet(&p, MeetOp::Code).await {
        Ok(v) => v,
        Err(e) => return reply(Err(e)),
    };
    let Some(proof) = v["proof"].as_str() else { return reply(Err(ApiError::internal("preuve indisponible"))) };
    let url = format!("{}/#/rencontre?to={}&m={proof}", origin_of(&st, &headers).trim_end_matches('/'), p.hex);
    let Some(svg) = crate::qr::svg(&url) else { return reply(Err(ApiError::internal("QR impossible"))) };
    private(Json(json!({ "url": url, "qr_svg": svg, "hex": p.hex, "expires_in": v["expires_in"] })).into_response())
}

async fn presence_get<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.presence(&p, None).await)
}

async fn presence_set<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<Value>) -> Response {
    reply(st.backend.presence(&p, Some(b)).await)
}

async fn call_ice<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.call_ice(&p).await)
}

async fn chat_send<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<ChatSendBody>) -> Response {
    let op = ChatOp::Send { peer: b.peer.chars().take(80).collect(), text: b.text.chars().take(20_000).collect(), reply: b.reply.map(|r| r.chars().take(80).collect()), reply_at: b.reply_at, files: b.files.into_iter().take(20).collect() };
    reply(st.backend.chat(&p, op).await)
}

async fn chat_delete<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<ChatIdBody>) -> Response {
    reply(st.backend.chat(&p, ChatOp::Delete(b.id.chars().take(80).collect())).await)
}

async fn chat_group<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<Value>) -> Response {
    reply(st.backend.chat(&p, ChatOp::Group(b)).await)
}

async fn chat_preview<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Query(q): Query<ChatPreviewQuery>) -> Response {
    reply(st.backend.chat(&p, ChatOp::Preview(q.url.chars().take(2000).collect())).await)
}

async fn chat_react<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<Value>) -> Response {
    reply(st.backend.chat(&p, ChatOp::React(b)).await)
}

async fn chat_typing<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<ChatPeerBody>) -> Response {
    reply(st.backend.chat(&p, ChatOp::Typing(b.peer.chars().take(80).collect())).await)
}

async fn chat_edit<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<Value>) -> Response {
    reply(st.backend.chat(&p, ChatOp::Edit(b)).await)
}

async fn chat_group_update<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<Value>) -> Response {
    reply(st.backend.chat(&p, ChatOp::GroupUpdate(b)).await)
}

async fn chat_prefs<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<Value>) -> Response {
    reply(st.backend.chat(&p, ChatOp::Prefs(b)).await)
}

async fn chat_settings_get<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.chat(&p, ChatOp::Settings(None)).await)
}

async fn chat_settings_set<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<Value>) -> Response {
    reply(st.backend.chat(&p, ChatOp::Settings(Some(b))).await)
}

async fn push_key<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.chat(&p, ChatOp::PushKey).await)
}

async fn push_subscribe<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<Value>) -> Response {
    reply(st.backend.chat(&p, ChatOp::Push(b)).await)
}

async fn chat_search<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Query(q): Query<ChatSearchQuery>) -> Response {
    reply(st.backend.chat(&p, ChatOp::Search(q.q.chars().take(200).collect())).await)
}

async fn chat_read<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<ChatPeerBody>) -> Response {
    reply(st.backend.chat(&p, ChatOp::Read(b.peer.chars().take(80).collect())).await)
}

async fn chat_clear<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<ChatPeerBody>) -> Response {
    reply(st.backend.chat(&p, ChatOp::Clear(b.peer.chars().take(80).collect())).await)
}

async fn ai_params<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.ai_params(&p).await)
}

async fn reading_view<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.reading(&p, ReadingOp::View).await)
}

async fn reading_offers<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.reading(&p, ReadingOp::Offers).await)
}

/// `{ accept: { peers, tasks } }`, `{ revoke: true }` (toutes les stations), `{ revoke: "<PeerID>" }` (une seule),
/// `{ pause: true | false }` ou `{ erase: true }`.
async fn reading_set<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<Value>) -> Response {
    let ok = |s: &&str| !s.is_empty() && s.len() <= 128;
    let op = if let Some(on) = b["pause"].as_bool() {
        ReadingOp::Pause(on)
    } else if b["erase"] == true {
        ReadingOp::Erase
    } else if b["revoke"] == true {
        ReadingOp::Revoke(None)
    } else if let Some(peer) = b["revoke"].as_str().filter(ok) {
        ReadingOp::Revoke(Some(peer.to_owned()))
    } else if let (Some(peers), Some(tasks)) = (b["accept"]["peers"].as_array(), b["accept"]["tasks"].as_array()) {
        let list = |v: &Vec<Value>| v.iter().filter_map(Value::as_str).filter(ok).take(8).map(str::to_owned).collect::<Vec<_>>();
        ReadingOp::Accept { peers: list(peers), tasks: list(tasks) }
    } else {
        return reply(Err(ApiError::bad_request("attendu : { accept: { peers, tasks } }, { revoke: true | \"<station>\" }, { pause: true | false } ou { erase: true }")));
    };
    reply(st.backend.reading(&p, op).await)
}

async fn set_ai_params<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(patch): Json<Value>) -> Response {
    if !patch.is_object() {
        return reply(Err(ApiError::bad_request("objet JSON attendu")));
    }
    reply(st.backend.set_ai_params(&p, patch).await)
}

#[derive(Deserialize)]
struct SayBody {
    text: String,
}

async fn agents_list<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.agents_list(&p).await)
}

#[derive(Deserialize)]
struct EnrollBody {
    name: String,
    group: String,
    #[serde(default = "default_grant_secs")]
    max_secs: u64,
    #[serde(default)]
    totp: bool,
    /// Station de la constellation où tourne l'agent (PeerID) ; absente : cette station.
    #[serde(default)]
    station: Option<String>,
}

fn default_grant_secs() -> u64 {
    600
}

async fn agents_enroll<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<EnrollBody>) -> Response {
    reply(st.backend.agents_enroll(&p, body.name, body.group, body.max_secs, body.totp, body.station).await)
}

async fn agents_revoke<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<AgentIdBody>) -> Response {
    reply(st.backend.agents_revoke(&p, body.id).await)
}

async fn agents_requests<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.agents_requests(&p).await)
}

#[derive(Deserialize)]
struct DecideBody {
    id: String,
    approve: bool,
    secs: Option<u64>,
    /// Mode navigateur : sites liés permis en plus du site demandé.
    #[serde(default)]
    hosts: Vec<String>,
}

async fn agents_decide<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<DecideBody>) -> Response {
    reply(st.backend.agents_decide(&p, body.id, body.approve, body.secs, body.hosts).await)
}

async fn agents_ready<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<AgentIdBody>) -> Response {
    reply(st.backend.agents_ready(&p, body.id).await)
}

async fn agents_stop<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<AgentIdBody>) -> Response {
    reply(st.backend.agents_stop(&p, body.id).await)
}

#[derive(Deserialize)]
struct ScreenQuery {
    id: String,
}

/// Écran du navigateur prêté, en flux SSE : une image à chaque changement (la plus récente seulement, jamais d'arriéré).
async fn agents_screen<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Query(q): Query<ScreenQuery>) -> Response {
    let rx = match st.backend.agents_screen(&p, q.id).await {
        Ok(rx) => rx,
        Err(e) => return reply(Err(e)),
    };
    let paced = stream::unfold((rx, true), |(mut rx, first)| async move {
        if !first {
            // Pas plus de ~8 images par seconde vers un téléphone, et jamais deux fois la même.
            tokio::time::sleep(Duration::from_millis(120)).await;
            if rx.changed().await.is_err() {
                return None;
            }
        }
        loop {
            // Image déjà là (première connexion) ou suivante ; flux fermé quand le navigateur s'arrête.
            let current = rx.borrow_and_update().clone();
            if let Some(f) = current {
                return Some((Ok::<_, Infallible>(Event::default().event("frame").data(&*f)), (rx, false)));
            }
            if rx.changed().await.is_err() {
                return None;
            }
        }
    });
    private(Sse::new(paced).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)).text("ping")).into_response())
}

async fn cosurf_list<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.cosurf_list(&p).await)
}

#[derive(Deserialize)]
struct CoGuest {
    hex: String,
    #[serde(default)]
    label: String,
}

#[derive(Deserialize)]
struct CoCreate {
    url: String,
    guests: Vec<CoGuest>,
    #[serde(default = "default_level")]
    level: String,
}

fn default_level() -> String {
    "point".into()
}

async fn cosurf_create<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<CoCreate>) -> Response {
    reply(st.backend.cosurf_create(&p, b.url, b.guests.into_iter().map(|g| (g.hex, g.label)).collect(), b.level).await)
}

#[derive(Deserialize)]
struct CoQuery {
    id: String,
    station: Option<String>,
}

async fn cosurf_view<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Query(q): Query<CoQuery>) -> Response {
    reply(st.backend.cosurf_view(&p, q.id, q.station).await)
}

#[derive(Deserialize)]
struct CoLevel {
    id: String,
    guest: String,
    level: String,
}

async fn cosurf_level<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<CoLevel>) -> Response {
    reply(st.backend.cosurf_level(&p, b.id, b.guest, b.level).await)
}

#[derive(Deserialize)]
struct CoStop {
    id: String,
    station: Option<String>,
}

async fn cosurf_stop<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<CoStop>) -> Response {
    reply(st.backend.cosurf_stop(&p, b.id, b.station).await)
}

/// Flux SSE de la navigation partagée : images, pointeurs, niveau.
async fn cosurf_screen<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Query(q): Query<CoQuery>) -> Response {
    let rx = match st.backend.cosurf_screen(&p, q.id, q.station).await {
        Ok(rx) => rx,
        Err(e) => return reply(Err(e)),
    };
    let events = stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|(k, d)| (Ok::<_, Infallible>(Event::default().event(k).data(&*d)), rx)) });
    private(Sse::new(events).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)).text("ping")).into_response())
}

#[derive(Deserialize)]
struct CoInput {
    id: String,
    station: Option<String>,
    #[serde(flatten)]
    gesture: Value,
}

async fn cosurf_input<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<CoInput>) -> Response {
    reply(st.backend.cosurf_input(&p, b.id, b.station, b.gesture).await)
}

#[derive(Deserialize)]
struct InputBody {
    id: String,
    #[serde(flatten)]
    gesture: Value,
}

async fn agents_input<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<InputBody>) -> Response {
    reply(st.backend.agents_input(&p, body.id, body.gesture).await)
}

/// Serveur MCP : le jeton d'agent (`Authorization: Bearer`) remplace la connexion d'un compte.
#[derive(Deserialize)]
struct McpQuery {
    station: Option<String>,
}

async fn mcp<B: SocialApi>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, Query(q): Query<McpQuery>, Json(body): Json<Value>) -> Response {
    let bearer = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer ")).map(|t| t.trim().to_owned());
    match st.backend.mcp(bearer, q.station, body).await {
        Ok(Some(v)) => private(Json(v).into_response()),
        Ok(None) => private(StatusCode::ACCEPTED.into_response()),
        Err(e) => {
            // Un jeton refusé annonce le schéma attendu, comme le veut HTTP.
            let mut r = e.into_response();
            if r.status() == StatusCode::UNAUTHORIZED {
                r.headers_mut().insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
            }
            private(r)
        }
    }
}

async fn bro_say<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<SayBody>) -> Response {
    let text = body.text.trim().to_owned();
    if text.is_empty() || text.chars().count() > 2000 {
        return reply(Err(ApiError::bad_request("message de 1 à 2000 caractères attendu")));
    }
    reply(st.backend.bro_say(&p, text).await)
}

async fn contacts<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.contacts(&p).await)
}

async fn contacts_sync<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.contacts_sync(&p).await)
}

#[derive(serde::Deserialize)]
struct PersonBody {
    name: String,
    #[serde(default)]
    email: Option<String>,
}

async fn contacts_create<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<PersonBody>) -> Response {
    reply(st.backend.contacts_create(&p, body.name, body.email).await)
}

#[derive(serde::Deserialize)]
struct LinkBody {
    path: String,
    #[serde(default)]
    target: Option<String>,
}

async fn contacts_link<B: SocialApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<LinkBody>) -> Response {
    reply(st.backend.contacts_link(&p, body.path, body.target).await)
}

/// Flux SSE : d'abord ce que le client a manqué (`Last-Event-ID`), puis le direct.
async fn activity_stream<B: SocialApi>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, method: Method, uri: Uri) -> Response {
    let principal = match who(&st, &headers, &method, &uri).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    let after = headers.get("last-event-id").and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
    let (backlog, rx) = st.activity.subscribe(after);
    // Tant que ce flux est ouvert, le compte est « là » (présence visio, pour ses amis qui l'ont choisie).
    let here = st.activity.listen(&principal.hex);
    // Un événement personnel n'est ni rejoué ni diffusé à un autre compte (virement, e-mail d'un nouveau membre…).
    let backlog: Vec<_> = backlog.into_iter().filter(|a| a.audience.admits(&principal.hex, principal.captain)).collect();
    let sse = Sse::new(event_stream(backlog, rx, principal, here)).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)).text("ping"));
    private(sse.into_response())
}

fn to_event(a: &astro_core::Activity) -> Event {
    Event::default().id(a.id.to_string()).event(a.kind.clone()).data(serde_json::to_string(a).unwrap_or_default())
}

type Live = tokio::sync::broadcast::Receiver<Arc<astro_core::Activity>>;

fn event_stream(backlog: Vec<Arc<astro_core::Activity>>, rx: Live, who: Principal, here: astro_core::activity::ListenGuard) -> impl Stream<Item = Result<Event, Infallible>> {
    stream::unfold((backlog.into_iter(), rx, who, here), |(mut backlog, mut rx, who, here)| async move {
        if let Some(a) = backlog.next() {
            return Some((Ok(to_event(&a)), (backlog, rx, who, here)));
        }
        loop {
            match rx.recv().await {
                Ok(a) if a.audience.admits(&who.hex, who.captain) => return Some((Ok(to_event(&a)), (backlog, rx, who, here))),
                Ok(_) => {}
                // Le client lisait trop lentement : on le dit plutôt que de laisser croire à un flux complet.
                Err(RecvError::Lagged(n)) => return Some((Ok(Event::default().event("lag").data(n.to_string())), (backlog, rx, who, here))),
                Err(RecvError::Closed) => return None,
            }
        }
    })
}
