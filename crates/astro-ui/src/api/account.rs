//! Compte : profil, site, clés, jetons, appareils, restauration, retour d'usage.
//!
//! Routes et gestionnaires de ce domaine ; le trait [`Backend`] et les types partagés restent dans `api/mod.rs`.

use super::*;

/// Compte : profil, site, clés, jetons, appareils, restauration, retour d'usage.
pub trait AccountApi: Auth {
    /// Empreinte du compte sur les communs : disque utilisé et quota de sa formule, Opex et Capex.
    fn footprint(&self, who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send;
    /// Comptes hébergés : disque, cycle de vie (Capitaine).
    fn accounts(&self, who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send;
    /// Jeton de secours d'un compte hébergé (Capitaine).
    fn account_token(&self, who: &Principal, hex: String) -> impl Future<Output = Result<Value, ApiError>> + Send;
    /// Fixe (ou retire, avec `None`) le quota d'un compte hébergé (Capitaine) : il remplace celui de la formule du compte.
    fn account_quota(&self, _who: &Principal, _hex: String, _quota_bytes: Option<u64>) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("quota non réglable sur cette station")) }
    }
    /// Appareils du compte : un jeton WebDAV chacun, confiné à un dossier du uCloud.
    fn devices_list(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("appareils non disponibles sur cette station")) }
    }
    /// Ajoute un appareil (corps JSON : `label`, `root`, `mode`) ; rend son jeton, montré une seule fois.
    fn devices_add(&self, _who: &Principal, _body: Value) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("appareils non disponibles sur cette station")) }
    }
    /// Révoque un appareil.
    fn devices_revoke(&self, _who: &Principal, _id: String) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("appareils non disponibles sur cette station")) }
    }
    /// « Mon compte » : identité, profil, accès WebDAV, relais annoncés.
    fn account(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("Mon compte n'est pas disponible sur cette station")) }
    }
    /// Site public du compte (dossier `/Public`) : nom, adresses, état.
    fn account_site(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("site public non disponible sur cette station")) }
    }
    /// Dépose le site d'exemple dans `/Public` (`force` : écrase la page d'accueil existante).
    fn account_site_example(&self, _who: &Principal, _force: bool) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("site public non disponible sur cette station")) }
    }
    /// Reçoit l'image de profil du compte : `{ kind: "picture" | "banner", data: base64, origin? }`.
    fn account_image(&self, _who: &Principal, _body: Value) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("images de profil non disponibles sur cette station")) }
    }
    /// Images publiques du compte (avatar, bandeau, envois NIP-96) : `{ images: [{ cid, mime, size, added_at, in_use }], used, quota }`.
    fn public_images(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("images publiques non disponibles sur cette station")) }
    }
    /// Retire une image publique du compte : libère son quota. Refusée tant qu'elle sert d'image ou de bandeau de profil.
    fn public_image_delete(&self, _who: &Principal, _cid: String) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("images publiques non disponibles sur cette station")) }
    }
    /// Position du membre (arrondie par la station à la maille UMAP de 0,01°) : la station la plus proche tient ses
    /// informations de quartier. `{lat, lon}`, ou `{}` pour l'effacer.
    fn account_position(&self, _who: &Principal, _body: Value) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("position non disponible sur cette station")) }
    }
    /// Modifie le profil (`name`, `about`, `picture`, `banner`, `lang`).
    fn account_profile(&self, _who: &Principal, _patch: Value) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("Mon compte n'est pas disponible sur cette station")) }
    }
    /// Connexion par code PASS (public) : rend le jeton de session. `ip` : limite de débit par adresse.
    fn account_unlock(&self, _ip: Option<std::net::IpAddr>, _email: String, _pass: String) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("connexion par PASS non disponible sur cette station")) }
    }
    /// Mon blog : `list` (articles, brouillons, groupes, thème), `friends` (articles de mes amis réciproques, publics ou qu'ils m'ont
    /// réservés), `publish` (note, article, brouillon, suppression, signés ici), `preview`
    /// (Markdown rendu comme sur le site), `image` (image d'article rangée dans Public), `settings` (thème du site).
    fn blog(&self, _who: &Principal, _op: &'static str, _body: Value) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("blog non disponible sur cette station")) }
    }
    /// Recherche transversale (hors fichiers) : conversations, notes et articles du membre et de ses amis, carnet d'adresses.
    fn find(&self, _who: &Principal, _q: String) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Ok(json!({ "messages": [], "notes": [], "contacts": [] })) }
    }
    /// Signe un événement avec la clé du compte (signataire distant : la clé ne quitte jamais la station).
    fn account_sign(&self, _who: &Principal, _event: Value) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("signature non disponible sur cette station")) }
    }
    /// Le Capitaine lève le verrouillage du PASS d'un compte.
    fn pass_unlock(&self, _who: &Principal, _hex: String) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("connexion par PASS non disponible sur cette station")) }
    }
    /// Le Capitaine éjecte un MULTIPASS créé ici qui ne s'est jamais connecté en 24 h (aucune Ğ1 reçue).
    fn account_eject(&self, _who: &Principal, _hex: String) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("éjection non disponible sur cette station")) }
    }
    /// Le Capitaine retire un MULTIPASS resté sans activité plus d'un mois : clés mises à la corbeille (30 jours), compte
    /// plus hébergé ici. Réversible pendant 30 jours.
    fn account_remove(&self, _who: &Principal, _hex: String) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("retrait des comptes non disponible sur cette station")) }
    }
    /// Le Capitaine **supprime complètement** un MULTIPASS inactif depuis 30 jours ou déjà retiré : Ğ1 reversées, adieu
    /// Nostr signé de sa clé (profil vidé, suppressions), données, blobs et clés effacés. Sans retour : `confirm` doit
    /// redonner l'adresse du compte.
    fn account_delete(&self, _who: &Principal, _hex: String, _confirm: String) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("suppression des comptes non disponible sur cette station")) }
    }
    /// Montre le jeton WebDAV en vigueur, sans le changer.
    fn account_token_show(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("Mon compte n'est pas disponible sur cette station")) }
    }
    /// Change le jeton WebDAV : l'ancien cesse de fonctionner.
    fn account_token_rotate(&self, _who: &Principal) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("Mon compte n'est pas disponible sur cette station")) }
    }
    /// Exporte les clés secrètes du compte (confirmation explicite).
    fn account_export_keys(&self, _who: &Principal, _confirm: bool) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("Mon compte n'est pas disponible sur cette station")) }
    }
    /// Retour d'un utilisateur (`POST /api/v1/feedback`) : issue Git de la coopérative, ou gardé sur la station.
    fn feedback(&self, _who: &Principal, _body: Value) -> impl Future<Output = Result<Value, ApiError>> + Send {
        async { Err(ApiError::unavailable("retours non activés sur cette station")) }
    }
    /// Les fiches plantes sont-elles proposées ? (clé Pl@ntNet de la coopérative, ou fiches déjà écrites) Sans cela, pas de
    /// rubrique « Plantes » à côté de la Médiathèque.
    fn plants_offered(&self, _who: &Principal) -> impl Future<Output = bool> + Send {
        async { false }
    }
    /// Le MULTIPASS a-t-il déclaré une IA (Ollama ou clé d'API) ? Sans cela, l'onglet BRO n'apparaît pas.
    fn ai_declared(&self, _who: &Principal) -> impl Future<Output = bool> + Send {
        async { false }
    }
    /// Le compte peut-il créer des Ğ1Billets ? (le Capitaine, ou un MULTIPASS qu'il a autorisé)
    fn billet_issuer(&self, _who: &Principal) -> impl Future<Output = bool> + Send {
        async { false }
    }
}

pub(super) fn routes<B: AccountApi>() -> Router<Arc<UiState<B>>> {
    Router::new()
        .route("/api/v1/me", get(me::<B>))
        .route("/api/v1/devices", get(devices_list::<B>).post(devices_add::<B>))
        .route("/api/v1/devices/revoke", post(devices_revoke::<B>))
        .route("/api/v1/me/footprint", get(footprint::<B>))
        .route("/api/v1/feedback", post(feedback::<B>).options(cors_preflight))
        .route("/api/v1/account", get(account::<B>))
        .route("/api/v1/account/profile", post(account_profile::<B>))
        .route("/api/v1/account/position", post(account_position::<B>))
        .route("/api/v1/account/image", post(account_image::<B>))
        .route("/api/v1/account/images", get(public_images::<B>))
        .route("/api/v1/account/images/{cid}", axum::routing::delete(public_image_delete::<B>))
        .route("/api/v1/account/site", get(account_site::<B>))
        .route("/api/v1/account/site/example", post(account_site_example::<B>))
        .route("/api/v1/account/token", post(account_token_rotate::<B>))
        .route("/api/v1/account/token/show", post(account_token_show::<B>))
        .route("/api/v1/account/unlock", post(account_unlock::<B>).options(cors_preflight))
        .route("/api/v1/account/sign", post(account_sign::<B>))
        .route("/api/v1/me/blog", get(blog_list::<B>))
        .route("/api/v1/me/blog/friends", get(blog_friends::<B>))
        .route("/api/v1/me/find", get(find::<B>))
        .route("/api/v1/me/blog/publish", post(blog_op::<B, 0>))
        .route("/api/v1/me/blog/preview", post(blog_op::<B, 1>))
        .route("/api/v1/me/blog/image", post(blog_op::<B, 2>).layer(axum::extract::DefaultBodyLimit::max(24 * 1024 * 1024)))
        .route("/api/v1/me/blog/settings", post(blog_op::<B, 3>))
        .route("/api/v1/station/accounts/{hex}/unlock-pass", post(pass_unlock::<B>))
        .route("/api/v1/station/accounts/{hex}/eject", post(account_eject::<B>))
        .route("/api/v1/station/accounts/{hex}/remove", post(account_remove::<B>))
        .route("/api/v1/station/accounts/{hex}/delete", post(account_delete::<B>))
        .route("/api/v1/account/keys", post(account_keys::<B>).options(cors_preflight))
        .route("/api/v1/station/accounts", get(accounts::<B>))
        .route("/api/v1/station/accounts/{hex}/token", post(account_token::<B>))
        .route("/api/v1/station/accounts/{hex}/quota", post(account_quota::<B>))
}

async fn footprint<B: AccountApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.footprint(&p).await)
}

async fn accounts<B: AccountApi>(State(st): State<Arc<UiState<B>>>, Captain(p): Captain) -> Response {
    reply(st.backend.accounts(&p).await)
}

async fn account_token<B: AccountApi>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, method: Method, uri: Uri, Path(hex): Path<String>) -> Response {
    match who(&st, &headers, &method, &uri).await {
        Ok(p) => match captain_only(&p) {
            Ok(()) if hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()) => reply(st.backend.account_token(&p, hex.to_ascii_lowercase()).await),
            Ok(()) => reply(Err(ApiError::bad_request("clé de compte invalide (64 hexadécimaux)"))),
            Err(r) => r,
        },
        Err(r) => r,
    }
}

#[derive(Deserialize)]
struct QuotaBody {
    /// Octets ; absent ou `null` : retire le quota propre au compte (retour à celui de sa formule).
    quota_bytes: Option<u64>,
}

async fn account_quota<B: AccountApi>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, method: Method, uri: Uri, Path(hex): Path<String>, Json(body): Json<QuotaBody>) -> Response {
    match who(&st, &headers, &method, &uri).await {
        Ok(p) => match captain_only(&p) {
            Ok(()) if hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()) => reply(st.backend.account_quota(&p, hex.to_ascii_lowercase(), body.quota_bytes).await),
            Ok(()) => reply(Err(ApiError::bad_request("clé de compte invalide (64 hexadécimaux)"))),
            Err(r) => r,
        },
        Err(r) => r,
    }
}

async fn devices_list<B: AccountApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.devices_list(&p).await)
}

async fn devices_add<B: AccountApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<Value>) -> Response {
    reply(st.backend.devices_add(&p, body).await)
}

async fn devices_revoke<B: AccountApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<AgentIdBody>) -> Response {
    reply(st.backend.devices_revoke(&p, body.id).await)
}

async fn account<B: AccountApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.account(&p).await)
}

async fn account_site<B: AccountApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.account_site(&p).await)
}

#[derive(Deserialize, Default)]
struct SiteExampleBody {
    #[serde(default)]
    force: bool,
}

async fn account_site_example<B: AccountApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, body: Option<Json<SiteExampleBody>>) -> Response {
    reply(st.backend.account_site_example(&p, body.map(|b| b.0.force).unwrap_or(false)).await)
}

async fn account_image<B: AccountApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<Value>) -> Response {
    reply(st.backend.account_image(&p, body).await)
}

async fn public_images<B: AccountApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.public_images(&p).await)
}

async fn public_image_delete<B: AccountApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Path(cid): Path<String>) -> Response {
    reply(st.backend.public_image_delete(&p, cid).await)
}

async fn account_profile<B: AccountApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<Value>) -> Response {
    reply(st.backend.account_profile(&p, body).await)
}

async fn account_position<B: AccountApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<Value>) -> Response {
    reply(st.backend.account_position(&p, body).await)
}

async fn account_token_show<B: AccountApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.account_token_show(&p).await)
}

async fn account_token_rotate<B: AccountApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.account_token_rotate(&p).await)
}

#[derive(Deserialize)]
struct UnlockBody {
    email: String,
    pass: String,
}

/// Connexion par code PASS : publique (c'est la porte d'entrée). Le jeton rendu n'est jamais mis en cache.
async fn account_unlock<B: AccountApi>(State(st): State<Arc<UiState<B>>>, req: axum::extract::Request) -> Response {
    let ip = req.extensions().get::<axum::extract::ConnectInfo<std::net::SocketAddr>>().map(|c| c.0.ip());
    let Ok(bytes) = axum::body::to_bytes(req.into_body(), 1024).await else { return reply(Err(ApiError::bad_request("requête trop grande"))) };
    let Ok(body) = serde_json::from_slice::<UnlockBody>(&bytes) else { return reply(Err(ApiError::bad_request("JSON attendu : { \"email\": \"…\", \"pass\": \"123456\" }"))) };
    any_origin(reply(st.backend.account_unlock(ip, body.email, body.pass).await))
}

async fn blog_list<B: AccountApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.blog(&p, "list", Value::Null).await)
}

#[derive(Deserialize)]
struct FindQuery {
    q: String,
}

/// `GET /api/v1/me/find?q=` : 200 caractères au plus.
async fn find<B: AccountApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Query(f): Query<FindQuery>) -> Response {
    if f.q.len() > 200 {
        return reply(Err(ApiError::bad_request("recherche : 200 caractères au plus")));
    }
    reply(st.backend.find(&p, f.q).await)
}

async fn blog_friends<B: AccountApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    reply(st.backend.blog(&p, "friends", Value::Null).await)
}

async fn blog_op<B: AccountApi, const OP: u8>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<Value>) -> Response {
    let op = match OP {
        0 => "publish",
        1 => "preview",
        2 => "image",
        _ => "settings",
    };
    reply(st.backend.blog(&p, op, body).await)
}

async fn account_sign<B: AccountApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<Value>) -> Response {
    reply(st.backend.account_sign(&p, body).await)
}

async fn pass_unlock<B: AccountApi>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, method: Method, uri: Uri, Path(hex): Path<String>) -> Response {
    match who(&st, &headers, &method, &uri).await {
        Ok(p) => match captain_only(&p) {
            Ok(()) if hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()) => reply(st.backend.pass_unlock(&p, hex.to_ascii_lowercase()).await),
            Ok(()) => reply(Err(ApiError::bad_request("clé de compte invalide (64 hexadécimaux)"))),
            Err(r) => r,
        },
        Err(r) => r,
    }
}

async fn account_eject<B: AccountApi>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, method: Method, uri: Uri, Path(hex): Path<String>) -> Response {
    match who(&st, &headers, &method, &uri).await {
        Ok(p) => match captain_only(&p) {
            Ok(()) if hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()) => reply(st.backend.account_eject(&p, hex.to_ascii_lowercase()).await),
            Ok(()) => reply(Err(ApiError::bad_request("clé de compte invalide (64 hexadécimaux)"))),
            Err(r) => r,
        },
        Err(r) => r,
    }
}

async fn account_remove<B: AccountApi>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, method: Method, uri: Uri, Path(hex): Path<String>) -> Response {
    match who(&st, &headers, &method, &uri).await {
        Ok(p) => match captain_only(&p) {
            Ok(()) if hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()) => reply(st.backend.account_remove(&p, hex.to_ascii_lowercase()).await),
            Ok(()) => reply(Err(ApiError::bad_request("clé de compte invalide (64 hexadécimaux)"))),
            Err(r) => r,
        },
        Err(r) => r,
    }
}

#[derive(Deserialize)]
struct DeleteBody {
    /// L'adresse du compte, retapée par le Capitaine.
    #[serde(default)]
    confirm: String,
}

async fn account_delete<B: AccountApi>(State(st): State<Arc<UiState<B>>>, headers: HeaderMap, method: Method, uri: Uri, Path(hex): Path<String>, Json(body): Json<DeleteBody>) -> Response {
    match who(&st, &headers, &method, &uri).await {
        Ok(p) => match captain_only(&p) {
            Ok(()) if hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()) && body.confirm.len() <= 320 => reply(st.backend.account_delete(&p, hex.to_ascii_lowercase(), body.confirm).await),
            Ok(()) => reply(Err(ApiError::bad_request("clé de compte invalide (64 hexadécimaux) ou confirmation trop longue"))),
            Err(r) => r,
        },
        Err(r) => r,
    }
}

#[derive(serde::Deserialize)]
struct KeysBody {
    #[serde(default)]
    confirm: bool,
}

async fn account_keys<B: AccountApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, Json(body): Json<KeysBody>) -> Response {
    any_origin(reply(st.backend.account_export_keys(&p, body.confirm).await))
}

/// Retour d'un utilisateur : authentifié (le jeton Git de la coopérative n'est jamais offert à un anonyme).
async fn feedback<B: AccountApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed, headers: HeaderMap, body: axum::body::Bytes) -> Response {
    if body.len() > 32 * 1024 {
        return any_origin(reply(Err(ApiError::bad_request("retour trop long (32 Ko au plus)"))));
    }
    let _ = headers;
    let Ok(v) = serde_json::from_slice::<Value>(&body) else { return any_origin(reply(Err(ApiError::bad_request("JSON attendu : { \"title\", \"description\", \"source\", \"category\"? }")))) };
    any_origin(reply(st.backend.feedback(&p, v).await))
}

async fn me<B: AccountApi>(State(st): State<Arc<UiState<B>>>, Authed(p): Authed) -> Response {
    private(Json(json!({ "hex": p.hex, "email": p.email, "captain": p.captain, "ai": st.backend.ai_declared(&p).await, "plants": st.backend.plants_offered(&p).await, "billet_issuer": st.backend.billet_issuer(&p).await })).into_response())
}
