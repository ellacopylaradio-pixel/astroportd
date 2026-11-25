//! uCloud : listing, photos, bibliothèque, partage, sauvegarde chez un ami, organisation, reprise WebDAV.
//!
//! Routes et gestionnaires de ce domaine ; le trait [`Backend`] et les types partagés restent dans `api/mod.rs`.

use super::*;

/// uCloud : listing, photos, bibliothèque, partage, sauvegarde chez un ami, organisation, reprise WebDAV.
pub trait UcloudApi: Auth {
    /// Contenu d'un dossier uCloud.
    fn ucloud_list(&self, who: &Principal, path: String) -> impl Future<Output = Result<Value, ApiError>> + Send;
    /// Album : photos et vidéos, `before` = horodatage exclusif (pagination).
    fn ucloud_photos(&self, who: &Principal, before: Option<i64>, limit: usize) -> impl Future<Output = Result<Value, ApiError>> + Send;
    /// Crée un lien magique ; doit renvoyer `{ "url", "expires_at", "items" }`.
    fn ucloud_share(&self, who: &Principal, paths: Vec<String>, ttl_secs: i64, origin: String) -> impl Future<Output = Result<Value, ApiError>> + Send;
    /// Sources WebDAV externes du compte (sans mot de passe), avec la reprise en cours ou le dernier bilan.
    fn pull_list(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("reprise depuis un autre serveur WebDAV non disponible sur cette station")) }
    }
    /// Enregistre une source (corps JSON : `url`, `username`, `password`, `label`, `remote_path`, `dest`) après l'avoir vérifiée.
    fn pull_add(&self, _who: &Principal, _body: Value) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("reprise depuis un autre serveur WebDAV non disponible sur cette station")) }
    }
    /// Retire une source.
    fn pull_remove(&self, _who: &Principal, _id: String) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("reprise depuis un autre serveur WebDAV non disponible sur cette station")) }
    }
    /// Lance une reprise en arrière-plan (`dry_run` : compte sans rien écrire).
    fn pull_run(&self, _who: &Principal, _id: String, _dry_run: bool) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("reprise depuis un autre serveur WebDAV non disponible sur cette station")) }
    }
    /// Demande l'arrêt de la reprise en cours.
    fn pull_stop(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("reprise depuis un autre serveur WebDAV non disponible sur cette station")) }
    }
    /// Rangement du uCloud par type : passage en cours ou dernier bilan, dossiers de type, passages annulables.
    fn organize_status(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("rangement non disponible sur cette station")) }
    }
    /// Aperçu d'un rangement (corps JSON : `scope`, `by_date`, `kinds`), sans rien écrire.
    fn organize_preview(&self, _who: &Principal, _body: Value) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("rangement non disponible sur cette station")) }
    }
    /// Lance un rangement en arrière-plan.
    fn organize_run(&self, _who: &Principal, _body: Value) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("rangement non disponible sur cette station")) }
    }
    /// Demande l'arrêt du rangement en cours.
    fn organize_stop(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("rangement non disponible sur cette station")) }
    }
    /// Annule un passage de rangement.
    fn organize_undo(&self, _who: &Principal, _run: String) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("rangement non disponible sur cette station")) }
    }
    /// Publie (`publish`) ou retire du public des fichiers de l'uCloud : un déplacement vers `/Public` ou hors de `/Public`.
    fn public_move(&self, _who: &Principal, _paths: Vec<String>, _publish: bool) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("site public non disponible sur cette station")) }
    }
    /// Images du dossier public du compte (pour choisir une image de profil).
    fn public_images(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("site public non disponible sur cette station")) }
    }
    /// Fiche plante de la photo `path` : `{ available, exists, fiche }` (`available` : reconnaissance Pl@ntNet proposée).
    fn plant_fiche(&self, _who: &Principal, _path: String) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("fiches plantes non disponibles sur cette station")) }
    }
    /// Toutes les fiches plantes du membre : `{ available, items: [{ photo, name, scientific, edible, medicinal, toxic, updated_at }] }`.
    fn plant_list(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("fiches plantes non disponibles sur cette station")) }
    }
    /// Enregistre la fiche plante éditée par le membre (`edit` : espèce, rubriques, `wiki` pour préremplir) ; rend `{ fiche, warning }`.
    fn plant_edit(&self, _who: &Principal, _path: String, _edit: Value, _lang: String) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("fiches plantes non disponibles sur cette station")) }
    }
    /// Lance la reconnaissance de la plante de la photo `path`, à la demande du membre ; rend `{ fiche, warning }`.
    fn plant_identify(&self, _who: &Principal, _path: String, _lang: String) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("reconnaissance des plantes non disponible sur cette station")) }
    }
    /// Tourne des photos d'un quart de tour (`turn` : 1 vers la droite, -1 vers la gauche) ; rend `{ rotated, failed: [{path, error}] }`.
    fn rotate_photos(&self, _who: &Principal, _paths: Vec<String>, _turn: i8) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("rotation non disponible sur cette station")) }
    }
    /// Déménagement : applique aux fichiers déjà envoyés la date, le lieu et la description lus dans l'archive (Google Takeout, iCloud).
    /// `items` : `[{ path, taken_at?, lat?, lon?, desc? }]`, 500 au plus. Rend `{ updated }`.
    fn import_meta(&self, _who: &Principal, _items: Value) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("import non disponible sur cette station")) }
    }
    /// Interroge la bibliothèque du compte (photos, souvenirs, chronologie, recherche, doublons).
    fn library(&self, _who: &Principal, _q: LibraryQuery) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("bibliothèque non disponible sur cette station")) }
    }
    /// Médiathèque : `{ items: [résumé de fiche], pending }` (films, séries, vidéos, morceaux déposés avec leur fiche).
    fn media_list(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("médiathèque non disponible sur cette station")) }
    }
    /// Adresses de lecture en flux (`/play/<jeton>`, Range) des fichiers `paths` : `{ urls: {chemin: url}, expires_at }`.
    fn media_play(&self, _who: &Principal, _paths: Vec<String>) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("médiathèque non disponible sur cette station")) }
    }
    /// Champs modifiables de la fiche `path` (`<nom>.media.json`).
    fn media_fiche(&self, _who: &Principal, _path: String) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("médiathèque non disponible sur cette station")) }
    }
    /// Corrige la fiche `path` avec les champs de `set` ; rend `{ item }`, son nouveau résumé.
    fn media_edit(&self, _who: &Principal, _path: String, _set: Value) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("médiathèque non disponible sur cette station")) }
    }
    /// Crée la fiche d'une vidéo déposée sans fiche (pour la corriger, la partager) ; rend `{ item }`.
    fn media_make(&self, _who: &Principal, _path: String, _licence: Option<String>) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("médiathèque non disponible sur cette station")) }
    }
    /// Médiathèque des amis : leurs films et morceaux partagés avec leur cercle, `{ items, friends }` ; `force` relit tout.
    fn media_friends(&self, _who: &Principal, _force: bool) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("médiathèque des amis non disponible sur cette station")) }
    }
    /// Un ami a-t-il déjà l'œuvre `work` ? `{ match }` : sa meilleure copie (résumé de la médiathèque des amis, avec
    /// `event_id` à demander), ou `null`. Le plugin `media` le demande avant un dépôt : une copie du même blob plutôt
    /// qu'un nouveau fichier.
    fn media_friend_match(&self, _who: &Principal, _work: MediaWork) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("médiathèque des amis non disponible sur cette station")) }
    }
    /// Demande à son auteur la copie du média `event_id` (liste des amis) : `{ state: copied | asked | sent }`.
    fn media_friend_request(&self, _who: &Principal, _event_id: String) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("médiathèque des amis non disponible sur cette station")) }
    }
    /// Partage (`on`) ou retire les fiches `paths` (dans l'ordre) ; `list` : la série ou l'album qu'elles forment.
    fn media_share(&self, _who: &Principal, _paths: Vec<String>, _on: bool, _list: Option<(String, String)>) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("partage depuis la médiathèque non disponible sur cette station")) }
    }
    /// Trace du partage d'une fiche : `{ shared, from, copies, reshared }`.
    fn media_trace(&self, _who: &Principal, _path: String) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("partage depuis la médiathèque non disponible sur cette station")) }
    }
    /// Mise à l'abri des fichiers partagés, en une lecture : `{ files: { <chemin>: { copies, at, verified_at, packed, safe } } }`.
    fn media_safety(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("sauvegarde privée non disponible sur cette station")) }
    }
    /// Proches réciproques du compte (double suivi) : `{ friends: [{ hex, label, hosted }] }`.
    fn friends(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("envoi à des proches non disponible sur cette station")) }
    }
    /// Envoie des fichiers (ou dossiers) du uCloud à des proches réciproques.
    fn ucloud_send(&self, _who: &Principal, _paths: Vec<String>, _friends: Vec<String>) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("envoi à des proches non disponible sur cette station")) }
    }
    /// Sauvegarde privée chez un ami : mon réglage, les demandes reçues comme hébergeur, mes proches.
    fn backup_status(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("sauvegarde privée non disponible sur cette station")) }
    }
    /// Active la sauvegarde chez `host` (la demande lui est envoyée) ou la suspend.
    fn backup_set(&self, _who: &Principal, _enabled: bool, _host: Option<String>) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("sauvegarde privée non disponible sur cette station")) }
    }
    /// Réponse de l'hébergeur : accepter avec un volume (en octets), refuser ou révoquer.
    fn backup_respond(&self, _who: &Principal, _owner: String, _accept: bool, _quota_bytes: u64) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("sauvegarde privée non disponible sur cette station")) }
    }
    /// Secours, côté gardien : `accept` rend sa part de la clé de secours à la station où `owner` se rétablit, sinon refuse.
    fn backup_rescue(&self, _who: &Principal, _owner: String, _accept: bool) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("sauvegarde privée non disponible sur cette station")) }
    }
    /// Réplication des données du compte : où et par qui. `{ crawl, routes, mine, peers }`.
    fn replication(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("réplication non mesurée sur cette station")) }
    }
    /// Envoi d'une image par un membre (NIP-96) : relue, réencodée sans métadonnées, rangée comme blob public (CID) compté dans son quota.
    /// `host` : l'hôte demandé (adresse publique si la station n'a pas de domaine).
    fn upload_image(&self, _who: &Principal, _data: Vec<u8>, _host: String) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("envoi d'images non disponible sur cette station")) }
    }
    /// Pièce jointe d'un message : octets déjà chiffrés par l'expéditeur (UENC v2), conservés `days` jours et servis par
    /// `/ipfs/<cid>`. `origin` : adresse publique de cette station.
    fn attach(&self, _who: &Principal, _data: Vec<u8>, _days: i64, _origin: String) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("pièces jointes non disponibles sur cette station")) }
    }
}

pub(super) fn routes<B: UcloudApi>() -> Router<Arc<UiState<B>>> {
    Router::new()
        .route("/api/v1/ucloud/rotate", post(library_rotate::<B>))
        .route("/api/v1/ucloud/plant", get(plant_fiche::<B>).post(plant_identify::<B>))
        .route("/api/v1/ucloud/plant/fiche", post(plant_edit::<B>))
        .route("/api/v1/ucloud/plants", get(plant_list::<B>))
        .route("/api/v1/ucloud/list", get(ucloud_list::<B>))
        .route("/api/v1/ucloud/photos", get(ucloud_photos::<B>))
        .route("/api/v1/ucloud/have", post(library_have::<B>))
        .route("/api/v1/ucloud/import-meta", post(import_meta::<B>))
        .route("/api/v1/ucloud/memories", get(library_memories::<B>))
        .route("/api/v1/ucloud/timeline", get(library_timeline::<B>))
        .route("/api/v1/ucloud/trips", get(library_trips::<B>))
        .route("/api/v1/ucloud/person", get(library_person::<B>))
        .route("/api/v1/ucloud/search", get(library_search::<B>))
        .route("/api/v1/ucloud/duplicates", get(library_duplicates::<B>))
        .route("/api/v1/ucloud/text", get(library_text::<B>))
        .route("/api/v1/ucloud/map", get(library_map::<B>))
        .route("/api/v1/ucloud/share", post(ucloud_share::<B>))
        .route("/api/v1/ucloud/media", get(media_list::<B>))
        .route("/api/v1/ucloud/media/play", post(media_play::<B>))
        .route("/api/v1/ucloud/media/fiche", get(media_fiche::<B>))
        .route("/api/v1/ucloud/media/edit", post(media_edit::<B>))
        .route("/api/v1/ucloud/media/make", post(media_make::<B>))
        .route("/api/v1/ucloud/media/friends", get(media_friends::<B>))
        .route("/api/v1/ucloud/media/share", post(media_share::<B>))
        .route("/api/v1/ucloud/media/trace", get(media_trace::<B>))
        .route("/api/v1/ucloud/media/safety", get(media_safety::<B>))
        .route("/api/v1/ucloud/media/friends/request", post(media_friend_request::<B>))
        .route("/api/v1/ucloud/media/friends/match", get(media_friend_match::<B>))
        .route("/api/v1/pull", get(pull_list::<B>).post(pull_add::<B>))
        .route("/api/v1/pull/remove", post(pull_remove::<B>))
        .route("/api/v1/pull/run", post(pull_run::<B>))
        .route("/api/v1/pull/stop", post(pull_stop::<B>))
        .route("/api/v1/organize", get(organize_status::<B>))
        .route("/api/v1/organize/preview", post(organize_preview::<B>))
        .route("/api/v1/organize/run", post(organize_run::<B>))
        .route("/api/v1/organize/stop", post(organize_stop::<B>))
        .route("/api/v1/organize/undo", post(organize_undo::<B>))
        .route("/api/v1/replication", get(replication::<B>))
        .route("/api/v1/public/move", post(public_move::<B>))
        .route("/api/v1/public/images", get(public_images::<B>))
        .route("/api/v1/attachments", post(attach::<B>).options(cors_preflight).layer(axum::extract::DefaultBodyLimit::max(ATTACHMENT_BODY_MAX)))
        .route("/api/upload/image", post(upload_image::<B>).options(cors_preflight).layer(axum::extract::DefaultBodyLimit::max(UPLOAD_MAX + 64 * 1024)))
        .route("/api/v1/friends", get(friends::<B>))
        .route("/api/v1/ucloud/send", post(ucloud_send::<B>))
        .route("/api/v1/backup", get(backup_status::<B>).post(backup_set::<B>))
        .route("/api/v1/backup/respond", post(backup_respond::<B>))
        .route("/api/v1/backup/rescue", post(backup_rescue::<B>))
}

#[derive(Deserialize)]
struct ListQuery {
    path: Option<String>,
}

async fn ucloud_list<B: UcloudApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Query(q): Query<ListQuery>) -> Response {
    reply(st.backend.ucloud_list(&p, q.path.unwrap_or_else(|| "/".into())).await)
}

async fn media_list<B: UcloudApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.media_list(&p).await)
}

#[derive(Deserialize)]
struct PlayBody {
    paths: Vec<String>,
}

async fn media_play<B: UcloudApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<PlayBody>) -> Response {
    if b.paths.is_empty() || b.paths.len() > 16 || b.paths.iter().any(|x| x.len() > 1024) {
        return reply(Err(ApiError::bad_request("1 à 16 fichiers à lire")));
    }
    reply(st.backend.media_play(&p, b.paths).await)
}

#[derive(Deserialize)]
struct FicheQuery {
    path: String,
}

async fn media_fiche<B: UcloudApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Query(q): Query<FicheQuery>) -> Response {
    reply(st.backend.media_fiche(&p, q.path).await)
}

#[derive(Deserialize)]
struct FriendsQuery {
    force: Option<u8>,
}

async fn media_friends<B: UcloudApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Query(q): Query<FriendsQuery>) -> Response {
    reply(st.backend.media_friends(&p, q.force == Some(1)).await)
}

#[derive(Deserialize)]
struct MediaShareList {
    kind: String,
    title: String,
}

#[derive(Deserialize)]
struct MediaShareBody {
    paths: Vec<String>,
    on: bool,
    list: Option<MediaShareList>,
}

async fn media_share<B: UcloudApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<MediaShareBody>) -> Response {
    if b.paths.is_empty() || b.paths.len() > 500 || b.paths.iter().any(|x| x.len() > 1024) {
        return reply(Err(ApiError::bad_request("1 à 500 fiches")));
    }
    if b.list.as_ref().is_some_and(|l| !matches!(l.kind.as_str(), "series" | "album") || l.title.trim().is_empty() || l.title.len() > 300) {
        return reply(Err(ApiError::bad_request("liste : series ou album, avec un titre")));
    }
    reply(st.backend.media_share(&p, b.paths, b.on, b.list.map(|l| (l.kind, l.title))).await)
}

async fn media_safety<B: UcloudApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.media_safety(&p).await)
}

async fn media_trace<B: UcloudApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Query(q): Query<FicheQuery>) -> Response {
    reply(st.backend.media_trace(&p, q.path).await)
}

/// Œuvre cherchée chez les amis (`GET /api/v1/ucloud/media/friends/match`) : un film (`tmdb`), une vidéo ou un morceau
/// (`youtube`), un épisode (`season` + `episode`, et la série par `tv` ou `series`).
#[derive(Debug, Default, Deserialize)]
pub struct MediaWork {
    /// Film TMDB.
    pub tmdb: Option<i64>,
    /// Série TMDB d'un épisode.
    pub tv: Option<i64>,
    /// Nom de la série d'un épisode.
    pub series: Option<String>,
    /// Saison.
    pub season: Option<i64>,
    /// Épisode.
    pub episode: Option<i64>,
    /// Vidéo ou morceau YouTube (11 caractères).
    pub youtube: Option<String>,
}

impl MediaWork {
    /// Une œuvre désignée sans ambiguïté, aux champs bornés : sinon le message de refus.
    fn check(&self) -> Result<(), &'static str> {
        let id = |v: Option<i64>| v.is_none_or(|n| (1..=1_000_000_000).contains(&n));
        if !(id(self.tmdb) && id(self.tv) && id(self.season.map(|s| s.saturating_add(1))) && id(self.episode)) {
            return Err("identifiant hors limites");
        }
        if self.youtube.as_deref().is_some_and(|y| y.len() != 11 || !y.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')) {
            return Err("identifiant YouTube invalide");
        }
        if self.series.as_deref().is_some_and(|s| s.trim().is_empty() || s.len() > 200) {
            return Err("nom de série : 1 à 200 caractères");
        }
        let episode = self.tv.is_some() || self.series.is_some();
        if episode && (self.season.is_none() || self.episode.is_none()) {
            return Err("un épisode exige sa saison et son numéro");
        }
        if self.tmdb.is_none() && self.youtube.is_none() && !episode {
            return Err("précisez tmdb, youtube, ou la série avec saison et épisode");
        }
        Ok(())
    }
}

async fn media_friend_match<B: UcloudApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Query(w): Query<MediaWork>) -> Response {
    if let Err(why) = w.check() {
        return reply(Err(ApiError::bad_request(why)));
    }
    reply(st.backend.media_friend_match(&p, w).await)
}

#[derive(Deserialize)]
struct RequestBody {
    event_id: String,
}

async fn media_friend_request<B: UcloudApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<RequestBody>) -> Response {
    if b.event_id.len() != 64 || !b.event_id.bytes().all(|c| c.is_ascii_hexdigit()) {
        return reply(Err(ApiError::bad_request("identifiant de média invalide")));
    }
    reply(st.backend.media_friend_request(&p, b.event_id).await)
}

#[derive(Deserialize)]
struct EditBody {
    path: String,
    set: Value,
}

async fn media_edit<B: UcloudApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<EditBody>) -> Response {
    if b.path.len() > 1024 || serde_json::to_string(&b.set).map_or(true, |s| s.len() > 64 * 1024) {
        return reply(Err(ApiError::bad_request("correction trop volumineuse")));
    }
    reply(st.backend.media_edit(&p, b.path, b.set).await)
}

#[derive(Deserialize)]
struct MakeBody {
    path: String,
    #[serde(default)]
    licence: Option<String>,
}

async fn media_make<B: UcloudApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<MakeBody>) -> Response {
    if b.path.len() > 1024 {
        return reply(Err(ApiError::bad_request("chemin trop long")));
    }
    reply(st.backend.media_make(&p, b.path, b.licence).await)
}

#[derive(Deserialize)]
struct HaveBody {
    shas: Vec<String>,
}

#[derive(Deserialize)]
struct RotateBody {
paths: Vec<String>,
turn: i8,
}

async fn library_rotate<B: UcloudApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<RotateBody>) -> Response {
if b.paths.is_empty() || b.paths.len() > 200 || !matches!(b.turn, 1 | -1) {
return reply(Err(ApiError::bad_request("1 à 200 photos, quart de tour 1 ou -1")));
}
reply(st.backend.rotate_photos(&p, b.paths, b.turn).await)
}

#[derive(Deserialize)]
struct PlantBody {
    path: String,
    #[serde(default)]
    lang: String,
    #[serde(default)]
    fiche: Value,
}

/// Langue des noms communs et de Wikipédia : deux lettres minuscules, français sinon.
fn plant_lang(l: String) -> String {
    if l.len() == 2 && l.bytes().all(|c| c.is_ascii_lowercase()) { l } else { "fr".to_owned() }
}

async fn plant_fiche<B: UcloudApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Query(q): Query<TextQuery>) -> Response {
    match q.path.filter(|p| p.starts_with('/') && p.len() <= 4096) {
        Some(path) => reply(st.backend.plant_fiche(&p, path).await),
        None => reply(Err(ApiError::bad_request("chemin attendu (?path=/…)"))),
    }
}

async fn plant_list<B: UcloudApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.plant_list(&p).await)
}

async fn plant_identify<B: UcloudApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<PlantBody>) -> Response {
    if !b.path.starts_with('/') || b.path.len() > 4096 {
        return reply(Err(ApiError::bad_request("chemin attendu")));
    }
    reply(st.backend.plant_identify(&p, b.path, plant_lang(b.lang)).await)
}

async fn plant_edit<B: UcloudApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(b): Json<PlantBody>) -> Response {
    if !b.path.starts_with('/') || b.path.len() > 4096 || !b.fiche.is_object() {
        return reply(Err(ApiError::bad_request("chemin et fiche attendus")));
    }
    reply(st.backend.plant_edit(&p, b.path, b.fiche, plant_lang(b.lang)).await)
}

async fn import_meta<B: UcloudApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<Value>) -> Response {
    reply(st.backend.import_meta(&p, body).await)
}

async fn library_have<B: UcloudApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<HaveBody>) -> Response {
    if body.shas.len() > 500 {
        return reply(Err(ApiError::bad_request("500 empreintes au plus par appel")));
    }
    reply(st.backend.library(&p, LibraryQuery::Have(body.shas)).await)
}

#[derive(Deserialize)]
struct MemoriesQuery {
    md: Option<String>,
    y: Option<i64>,
}

async fn library_memories<B: UcloudApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Query(q): Query<MemoriesQuery>) -> Response {
    reply(st.backend.library(&p, LibraryQuery::Memories(q.md.zip(q.y))).await)
}

async fn library_trips<B: UcloudApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.library(&p, LibraryQuery::Trips).await)
}

#[derive(Deserialize)]
struct PersonQuery {
    name: String,
    uid: Option<String>,
}

async fn library_person<B: UcloudApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Query(q): Query<PersonQuery>) -> Response {
    if q.name.trim().is_empty() || q.name.len() > 128 || q.uid.as_ref().is_some_and(|u| u.len() > 256) {
        return reply(Err(ApiError::bad_request("nom attendu (128 octets au plus)")));
    }
    reply(st.backend.library(&p, LibraryQuery::Person(q.name, q.uid.filter(|u| !u.is_empty()))).await)
}

#[derive(Deserialize)]
struct TimelineQuery {
    ym: Option<String>,
}

async fn library_timeline<B: UcloudApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Query(q): Query<TimelineQuery>) -> Response {
    reply(st.backend.library(&p, LibraryQuery::Timeline(q.ym)).await)
}

async fn library_search<B: UcloudApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Query(q): Query<LibrarySearch>) -> Response {
    if q.q.as_deref().is_some_and(|s| s.len() > 256) || q.person.as_deref().is_some_and(|s| s.len() > 128) {
        return reply(Err(ApiError::bad_request("recherche trop longue")));
    }
    reply(st.backend.library(&p, LibraryQuery::Search(q)).await)
}

async fn library_map<B: UcloudApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Query(q): Query<MapQuery>) -> Response {
    if let Some(b) = &q.bbox {
        let ok = b.split(',').count() == 4 && b.split(',').all(|v| v.parse::<f64>().is_ok_and(f64::is_finite));
        if !ok {
            return reply(Err(ApiError::bad_request("zone invalide : sud,ouest,nord,est en degrés")));
        }
    }
    reply(st.backend.library(&p, LibraryQuery::Map(q)).await)
}

#[derive(Deserialize)]
struct DuplicatesQuery {
    all: Option<u8>,
}

async fn library_duplicates<B: UcloudApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Query(q): Query<DuplicatesQuery>) -> Response {
    reply(st.backend.library(&p, LibraryQuery::Duplicates(q.all == Some(1))).await)
}

#[derive(Deserialize)]
struct TextQuery {
    path: Option<String>,
}

async fn library_text<B: UcloudApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Query(q): Query<TextQuery>) -> Response {
    match q.path.filter(|p| p.starts_with('/') && p.len() <= 4096) {
        Some(path) => reply(st.backend.library(&p, LibraryQuery::Text(path)).await),
        None => reply(Err(ApiError::bad_request("chemin attendu (?path=/…)"))),
    }
}

#[derive(Deserialize)]
struct PhotosQuery {
    before: Option<i64>,
    limit: Option<usize>,
}

async fn ucloud_photos<B: UcloudApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Query(q): Query<PhotosQuery>) -> Response {
    reply(st.backend.ucloud_photos(&p, q.before, q.limit.unwrap_or(60).clamp(1, 200)).await)
}

#[derive(Deserialize)]
struct ShareBody {
    paths: Vec<String>,
    ttl_hours: Option<u32>,
}

async fn ucloud_share<B: UcloudApi>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, method: Method, uri: Uri, Json(body): Json<ShareBody>) -> Response {
    let principal = match who(&st, &headers, &method, &uri).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    if body.paths.is_empty() || body.paths.len() > 200 {
        return reply(Err(ApiError::bad_request("1 à 200 chemins attendus")));
    }
    let origin = st
        .public_origin
        .clone()
        .or_else(|| headers.get(header::HOST).and_then(|h| h.to_str().ok()).map(|h| format!("http://{h}")))
        .unwrap_or_default();
    let ttl_secs = i64::from(body.ttl_hours.unwrap_or(72).clamp(1, 720)) * 3600;
    let mut out = match st.backend.ucloud_share(&principal, body.paths, ttl_secs, origin).await {
        Ok(v) => v,
        Err(e) => return reply(Err(e)),
    };
    // Le QR est rendu ici à partir de l'URL complète (`#clé` comprise) : l'application n'a rien à calculer.
    if let Some(url) = out.get("url").and_then(Value::as_str) {
        let svg = crate::qr::svg(url);
        if let (Some(obj), Some(svg)) = (out.as_object_mut(), svg) {
            obj.insert("qr_svg".into(), Value::String(svg));
        }
    }
    reply(Ok(out))
}

async fn pull_list<B: UcloudApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.pull_list(&p).await)
}

async fn pull_add<B: UcloudApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<Value>) -> Response {
    reply(st.backend.pull_add(&p, body).await)
}

async fn pull_remove<B: UcloudApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<AgentIdBody>) -> Response {
    reply(st.backend.pull_remove(&p, body.id).await)
}

#[derive(Deserialize)]
struct PullRunBody {
    id: String,
    #[serde(default)]
    dry_run: bool,
}

async fn pull_run<B: UcloudApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<PullRunBody>) -> Response {
    reply(st.backend.pull_run(&p, body.id, body.dry_run).await)
}

async fn pull_stop<B: UcloudApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.pull_stop(&p).await)
}

async fn organize_status<B: UcloudApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.organize_status(&p).await)
}

async fn organize_preview<B: UcloudApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<Value>) -> Response {
    reply(st.backend.organize_preview(&p, body).await)
}

async fn organize_run<B: UcloudApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<Value>) -> Response {
    reply(st.backend.organize_run(&p, body).await)
}

async fn organize_stop<B: UcloudApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.organize_stop(&p).await)
}

#[derive(Deserialize)]
struct UndoBody {
    run: String,
}

async fn organize_undo<B: UcloudApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<UndoBody>) -> Response {
    reply(st.backend.organize_undo(&p, body.run).await)
}

#[derive(Deserialize)]
struct RescueBody {
    owner: String,
    accept: bool,
}

#[derive(Deserialize)]
struct PublicMoveBody {
    paths: Vec<String>,
    publish: bool,
}

async fn public_move<B: UcloudApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<PublicMoveBody>) -> Response {
    reply(st.backend.public_move(&p, body.paths, body.publish).await)
}

async fn public_images<B: UcloudApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.public_images(&p).await)
}

async fn friends<B: UcloudApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.friends(&p).await)
}

#[derive(serde::Deserialize)]
struct SendBody {
    paths: Vec<String>,
    friends: Vec<String>,
}

async fn ucloud_send<B: UcloudApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<SendBody>) -> Response {
    if body.paths.is_empty() || body.paths.len() > 200 || body.paths.iter().any(|p| !p.starts_with('/') || p.len() > 1024 || p.split('/').any(|s| s == "..")) {
        return reply(Err(ApiError::bad_request("1 à 200 chemins absolus attendus (sans « .. »)")));
    }
    if body.friends.is_empty() || body.friends.len() > 50 || body.friends.iter().any(|f| f.len() != 64 || !f.bytes().all(|b| b.is_ascii_hexdigit())) {
        return reply(Err(ApiError::bad_request("1 à 50 proches (clés Nostr de 64 caractères hexadécimaux) attendus")));
    }
    let friends = body.friends.into_iter().map(|f| f.to_ascii_lowercase()).collect();
    reply(st.backend.ucloud_send(&p, body.paths, friends).await)
}

async fn backup_status<B: UcloudApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.backup_status(&p).await)
}

#[derive(serde::Deserialize)]
struct BackupSetBody {
    enabled: bool,
    host: Option<String>,
}

async fn backup_set<B: UcloudApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<BackupSetBody>) -> Response {
    if body.host.as_ref().is_some_and(|h| h.len() != 64 || !h.bytes().all(|b| b.is_ascii_hexdigit())) {
        return reply(Err(ApiError::bad_request("clé de l'ami : 64 caractères hexadécimaux attendus")));
    }
    reply(st.backend.backup_set(&p, body.enabled, body.host.map(|h| h.to_ascii_lowercase())).await)
}

#[derive(serde::Deserialize)]
struct BackupRespondBody {
    owner: String,
    accept: bool,
    /// Volume mis à disposition, en octets (obligatoire pour accepter).
    #[serde(default)]
    quota_bytes: u64,
}

async fn backup_respond<B: UcloudApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<BackupRespondBody>) -> Response {
    if body.owner.len() != 64 || !body.owner.bytes().all(|b| b.is_ascii_hexdigit()) {
        return reply(Err(ApiError::bad_request("clé de l'ami : 64 caractères hexadécimaux attendus")));
    }
    reply(st.backend.backup_respond(&p, body.owner.to_ascii_lowercase(), body.accept, body.quota_bytes).await)
}

/// Réplication des données du compte connecté.
async fn replication<B: UcloudApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.replication(&p).await)
}

/// Pièce jointe d'un message (`POST /api/v1/attachments?days=N`, corps `application/octet-stream` UENC) : authentifiée.
async fn attach<B: UcloudApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, headers: HeaderMap, axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>, body: axum::body::Bytes) -> Response {
    let days = match q.get("days") {
        None => 30,
        Some(d) => match d.parse::<i64>() {
            Ok(d) if (1..=90).contains(&d) => d,
            _ => return any_origin(reply(Err(ApiError::bad_request("days : 1 à 90")))),
        },
    };
    any_origin(reply(st.backend.attach(&p, body.to_vec(), days, st.backend.public_origin(&host_of(&headers))).await))
}

/// Envoi d'une image (`multipart/form-data`, champ `file`) : authentifié (Basic ou NIP-98), jamais anonyme.
async fn upload_image<B: UcloudApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, headers: HeaderMap, body: axum::body::Bytes) -> Response {
    let ct = headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("");
    let part = match crate::multipart::first_file(ct, &body) {
        Ok(p) if p.data.len() <= UPLOAD_MAX && !p.data.is_empty() => p,
        Ok(_) => return any_origin(reply(Err(ApiError::bad_request("fichier vide ou trop lourd (8 Mo au plus)")))),
        Err(e) => return any_origin(reply(Err(ApiError::bad_request(e)))),
    };
    any_origin(reply(st.backend.upload_image(&p, part.data, host_of(&headers)).await))
}

/// Réponse du gardien à une demande de secours.
async fn backup_rescue<B: UcloudApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<RescueBody>) -> Response {
    if body.owner.len() != 64 || !body.owner.bytes().all(|b| b.is_ascii_hexdigit()) {
        return reply(Err(ApiError::bad_request("clé de l'ami : 64 caractères hexadécimaux attendus")));
    }
    reply(st.backend.backup_rescue(&p, body.owner.to_ascii_lowercase(), body.accept).await)
}

#[cfg(test)]
mod tests {
    use super::MediaWork;

    #[test]
    fn a_work_query_names_one_work_with_bounded_fields() {
        let ok = |w: MediaWork| w.check().is_ok();
        assert!(ok(MediaWork { tmdb: Some(129), ..MediaWork::default() }));
        assert!(ok(MediaWork { youtube: Some("dQw4w9WgXcQ".into()), ..MediaWork::default() }));
        assert!(ok(MediaWork { series: Some("Kaamelott".into()), season: Some(0), episode: Some(1), ..MediaWork::default() }), "saison 0 : les épisodes spéciaux");
        assert!(ok(MediaWork { tv: Some(1), season: Some(1), episode: Some(1), ..MediaWork::default() }));
        // Hostile : vide, ambigu, hors limites, injecté.
        assert!(!ok(MediaWork::default()), "aucune œuvre");
        assert!(!ok(MediaWork { series: Some("Kaamelott".into()), ..MediaWork::default() }), "épisode sans saison ni numéro");
        assert!(!ok(MediaWork { series: Some("  ".into()), season: Some(1), episode: Some(1), ..MediaWork::default() }));
        assert!(!ok(MediaWork { series: Some("x".repeat(201)), season: Some(1), episode: Some(1), ..MediaWork::default() }));
        assert!(!ok(MediaWork { tmdb: Some(-1), ..MediaWork::default() }));
        assert!(!ok(MediaWork { tmdb: Some(i64::MAX), ..MediaWork::default() }));
        assert!(!ok(MediaWork { tv: Some(1), season: Some(i64::MAX), episode: Some(1), ..MediaWork::default() }), "pas de débordement");
        assert!(!ok(MediaWork { tv: Some(1), season: Some(-1), episode: Some(1), ..MediaWork::default() }));
        assert!(!ok(MediaWork { youtube: Some("../../etc/pa".into()), ..MediaWork::default() }));
        assert!(!ok(MediaWork { youtube: Some("short".into()), ..MediaWork::default() }));
    }
}
