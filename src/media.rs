//! Médiathèque : films, séries, vidéos et morceaux déposés avec leur fiche `media/v1` (plugin `media`).
//!
//! - **Index** : chaque `<nom>.media.json` de l'uCloud est lu (déchiffré par la station) et résumé dans `media_index`,
//!   un **cache local** relu seulement quand le CID de la fiche change ; une base rejouée le reconstruit à la demande.
//! - **Lecture en flux** : `POST /api/v1/ucloud/media/play` donne une adresse `/play/<jeton>` que `<video>` et `<audio>`
//!   lisent directement, avec `Range` (avance rapide, reprise) et déchiffrement à la volée : jamais un film entier en mémoire
//!   dans le navigateur. Le jeton est signé par la station (HMAC), lié à **un** fichier de **un** compte, et expire.
//! - **Rangement** : la fiche et ses compagnons (affiche, pochette, sous-titres) ne s'affichent ni dans Photos ni dans la
//!   liste des fichiers ; ils restent dans le WebDAV, où un client ordinaire les voit.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use astro_dav::{BlobStore, DavState};
use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use hmac::{Hmac, Mac};
use rusqlite::params;
use serde_json::{Value, json};
use sha2::Sha256;

/// Suffixe d'une fiche.
pub const FICHE: &str = ".media.json";
/// Durée de vie d'une adresse de lecture (une soirée de films).
pub const PLAY_TTL_SECS: i64 = 12 * 3600;
/// Fiche plus grosse : ignorée (une vraie fiche pèse quelques Kio).
const MAX_FICHE_BYTES: i64 = 256 * 1024;
/// Fiches relues au plus par appel (le reste au suivant : la réponse dit combien attendent).
const MAX_PARSE: usize = 200;
/// Fichiers au plus par demande de lecture (le film et ses sous-titres).
pub const MAX_PLAY_PATHS: usize = 16;
const DOMAIN: &[u8] = b"astroportd/play/1\n";
/// Images posées par le plugin à côté du fichier : `<nom>.<rôle>.jpg`.
const ART: [&str; 5] = ["poster", "backdrop", "still", "cover", "logo"];

/// Fragment SQL (table aliasée `e`) : **vrai** pour une entrée qui n'est ni une fiche ni l'image ou le sous-titre d'une
/// fiche voisine (`<nom>.poster.jpg`, `<nom>.cover.jpg`, `<nom>.fr.vtt`… à côté de `<nom>.media.json`). Photos, chronologie
/// et recherche ne montrent ainsi que les souvenirs du membre, pas les affiches de sa vidéothèque.
pub const NOT_COMPANION_SQL: &str = crate::not_companion_sql!();

/// Le texte de [`NOT_COMPANION_SQL`], utilisable dans `concat!` (requêtes de la bibliothèque).
#[macro_export]
macro_rules! not_companion_sql {
    () => {
        "NOT (e.path GLOB '*.media.json' OR ((e.path GLOB '*.poster.jpg' OR e.path GLOB '*.backdrop.jpg' OR e.path GLOB '*.still.jpg' \
     OR e.path GLOB '*.cover.jpg' OR e.path GLOB '*.logo.jpg' OR e.path GLOB '*.vtt') AND EXISTS (SELECT 1 FROM ucloud_entries f WHERE f.owner_hex = e.owner_hex \
     AND f.parent = e.parent AND f.path GLOB '*.media.json' AND substr(e.path, 1, length(f.path) - 11) = substr(f.path, 1, length(f.path) - 11) \
     AND substr(e.path, length(f.path) - 10, 1) = '.')))"
    };
}

fn companion_of(name: &str, stem: &str) -> bool {
    let Some(rest) = name.strip_prefix(stem).and_then(|r| r.strip_prefix('.')) else { return false };
    rest == "media.json" || rest.ends_with(".vtt") || rest == "vtt" || ART.iter().any(|a| rest == format!("{a}.jpg"))
}

/// `name` accompagne-t-il le fichier `file` (même dossier) : sa fiche, une de ses images, un de ses sous-titres ?
pub fn companion_of_file(name: &str, file: &str) -> bool {
    let stem = file.rsplit_once('.').map_or(file, |(s, _)| s);
    name != file && companion_of(name, stem)
}

/// Noms d'un dossier à ne pas afficher : fiches et compagnons, **seulement si le fichier décrit est encore là** (sinon ils
/// réapparaissent, pour que le membre voie et puisse supprimer ce qui reste d'un film effacé).
pub fn hidden_names<'a>(names: impl IntoIterator<Item = &'a str> + Clone) -> HashSet<String> {
    let stems: Vec<&str> = names.clone().into_iter().filter_map(|n| n.strip_suffix(FICHE)).collect();
    let mut hide = HashSet::new();
    for stem in stems {
        let main_present = names.clone().into_iter().any(|n| n.strip_prefix(stem).and_then(|r| r.strip_prefix('.')).is_some() && !companion_of(n, stem));
        if main_present {
            hide.extend(names.clone().into_iter().filter(|n| companion_of(n, stem)).map(str::to_owned));
        }
    }
    hide
}

// ───────────────────────────── Adresse de lecture ─────────────────────────────

fn mac(secret: &[u8], payload: &[u8]) -> Hmac<Sha256> {
    // Une clé HMAC accepte toute longueur : l'erreur n'arrive pas, mais on ne panique jamais pour autant.
    let mut m = <Hmac<Sha256> as Mac>::new_from_slice(secret).unwrap_or_else(|_| <Hmac<Sha256> as Mac>::new_from_slice(&[0u8; 32]).unwrap_or_else(|_| unreachable!()));
    m.update(DOMAIN);
    m.update(payload);
    m
}

/// Jeton de lecture de `path` (compte `owner`), valable jusqu'à `now + PLAY_TTL_SECS`.
pub fn play_token(secret: &[u8], owner: &str, path: &str, now: i64) -> String {
    let payload = format!("{owner}\n{}\n{path}", now + PLAY_TTL_SECS);
    let tag = mac(secret, payload.as_bytes()).finalize().into_bytes();
    format!("{}.{}", B64.encode(payload.as_bytes()), B64.encode(&tag[..18]))
}

/// `(compte, chemin)` d'un jeton authentique et non expiré.
pub fn check_token(secret: &[u8], token: &str, now: i64) -> Option<(String, String)> {
    let (body, tag) = token.split_once('.')?;
    let (payload, tag) = (B64.decode(body).ok()?, B64.decode(tag).ok()?);
    if tag.len() != 18 {
        return None;
    }
    mac(secret, &payload).verify_truncated_left(&tag).ok()?;
    let text = String::from_utf8(payload).ok()?;
    let mut it = text.splitn(3, '\n');
    let (owner, exp, path) = (it.next()?, it.next()?.parse::<i64>().ok()?, it.next()?);
    (exp >= now && owner.len() == 64 && owner.bytes().all(|b| b.is_ascii_hexdigit()) && path.starts_with('/')).then(|| (owner.to_owned(), path.to_owned()))
}

/// Ce type de fichier peut-il être lu par le lecteur (son, image animée, sous-titres) ?
pub fn playable(mime: Option<&str>, path: &str) -> bool {
    mime.is_some_and(|m| m.starts_with("audio/") || m.starts_with("video/")) || path.ends_with(".vtt")
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

/// Routes `/play/<jeton>` (GET et HEAD) : le clair du fichier, `Range` compris, sans autre authentification que le jeton.
pub fn router<B: BlobStore>(dav: Arc<DavState<B>>) -> Router {
    Router::new().route("/play/{token}", get(play::<B>).head(play::<B>)).with_state(dav)
}

async fn play<B: BlobStore>(State(dav): State<Arc<DavState<B>>>, method: Method, Path(token): Path<String>, headers: HeaderMap) -> Response {
    let Some((owner, path)) = check_token(&dav.config.station_secret, &token, now()) else {
        return (StatusCode::FORBIDDEN, "adresse de lecture invalide ou expirée").into_response();
    };
    let mut r = astro_dav::get_owner(&dav, &owner, &path, &headers, method == Method::HEAD).await;
    let h = r.headers_mut();
    h.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static("default-src 'none'; sandbox"));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("private, no-store"));
    if path.ends_with(".vtt") {
        h.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/vtt; charset=utf-8"));
    }
    r
}

// ───────────────────────────── Index des fiches ─────────────────────────────

fn parent_of(path: &str) -> &str {
    match path.rfind('/') {
        Some(0) | None => "",
        Some(i) => &path[..i],
    }
}

fn clip(v: &Value, max: usize) -> Option<String> {
    v.as_str().map(str::trim).filter(|s| !s.is_empty()).map(|s| s.chars().take(max).collect())
}

fn sibling(dir: &str, name: &Value) -> Option<String> {
    name.as_str().filter(|n| !n.is_empty() && !n.contains('/') && *n != "." && *n != "..").map(|n| format!("{dir}/{n}"))
}

/// Ce que l'écran affiche d'une fiche : un résumé borné (pas la fiche entière, qui porte jusqu'à 50 personnes). `None` : ce
/// n'est pas une fiche `media/v1` lisible.
pub fn summarize(fiche: &Value, fiche_path: &str) -> Option<Value> {
    if fiche["schema"] != "media/v1" {
        return None;
    }
    let kind = fiche["type"].as_str().filter(|t| ["film", "episode", "video", "music"].contains(t))?;
    let dir = parent_of(fiche_path);
    let path = sibling(dir, &fiche["file"])?;
    let title = clip(&fiche["title"], 300)?;
    let art: serde_json::Map<String, Value> = ART.iter().filter_map(|role| sibling(dir, &fiche["artwork"][*role]).map(|p| ((*role).to_owned(), Value::String(p)))).collect();
    let subtitles: Vec<Value> = fiche["subtitles"].as_array().into_iter().flatten().take(12).filter_map(|s| Some(json!({ "path": sibling(dir, &s["file"])?, "lang": clip(&s["lang"], 8) }))).collect();
    let names = |v: &Value, n: usize| -> Vec<String> { v.as_array().into_iter().flatten().filter_map(|x| clip(&x["name"], 80)).take(n).collect() };
    let directors: Vec<String> = fiche["people"]["crew"].as_array().into_iter().flatten().filter(|c| c["job"] == "Director").filter_map(|c| clip(&c["name"], 80)).take(3).collect();
    let t = &fiche["technical"];
    let s = &fiche["series"];
    let m = &fiche["music"];
    let mut out = json!({
        "path": path, "fiche": fiche_path, "type": kind, "title": title,
        "original_title": clip(&fiche["original_title"], 300), "year": fiche["year"].as_i64(),
        "overview": clip(&fiche["overview"], 700),
        "genres": fiche["genres"].as_array().into_iter().flatten().filter_map(|g| clip(g, 40)).take(8).collect::<Vec<_>>(),
        "keywords": fiche["keywords"].as_array().into_iter().flatten().filter_map(|g| clip(g, 60)).take(30).collect::<Vec<_>>(),
        "cast": names(&fiche["people"]["cast"], 12), "directors": directors,
        "art": art, "subtitles": subtitles,
        "duration_s": t["duration_s"].as_f64().map(|d| d.round() as i64), "mime": clip(&t["mime"], 60),
        "level": clip(&fiche["sharing"]["level"], 16).unwrap_or_else(|| "private".into()),
        "rights": clip(&fiche["source"]["rights"], 24), "added": fiche["generated_at"].as_i64(),
    });
    if t["video"].is_object() {
        out["video"] = json!({ "width": t["video"]["width"].as_i64(), "height": t["video"]["height"].as_i64() });
    }
    if kind == "episode" {
        out["series"] = json!({ "name": clip(&s["name"], 200), "season": s["season"].as_i64(), "episode": s["episode"].as_i64() });
    }
    if kind == "music" {
        out["music"] = json!({
            "artist": clip(&m["artist"], 200), "album": clip(&m["album"], 200), "album_artist": clip(&m["album_artist"], 200),
            "track": m["track"].as_i64(), "composer": clip(&m["composer"], 200), "mix_start_s": m["from_mix"]["start_s"].as_i64(),
        });
    }
    Some(out)
}

/// Texte cherchable d'une fiche résumée : titres, résumé, année, genres, mots-clés, personnes, série, musique.
pub fn search_body(item: &Value) -> String {
    let mut parts: Vec<String> = ["title", "original_title", "overview"].iter().filter_map(|k| item[*k].as_str().map(str::to_owned)).collect();
    parts.extend(item["year"].as_i64().map(|y| y.to_string()));
    for k in ["genres", "keywords", "cast", "directors"] {
        parts.extend(item[k].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_owned)));
    }
    for (o, k) in [("series", "name"), ("music", "artist"), ("music", "album"), ("music", "album_artist"), ("music", "composer")] {
        parts.extend(item[o][k].as_str().map(str::to_owned));
    }
    parts.join(" · ")
}

// ───────────────────────────── Correction d'une fiche ─────────────────────────────

/// Champs modifiables d'une fiche, tels que le formulaire les montre (listes complètes, pas le résumé de l'index).
pub fn editable(fiche: &Value) -> Value {
    let names = |v: &Value| -> Vec<Value> { v.as_array().into_iter().flatten().filter_map(|x| x["name"].as_str().map(|s| Value::String(s.to_owned()))).collect() };
    let directors: Vec<Value> = fiche["people"]["crew"].as_array().into_iter().flatten().filter(|c| c["job"] == "Director").filter_map(|c| c["name"].as_str().map(|s| Value::String(s.to_owned()))).collect();
    let mut out = json!({
        "type": fiche["type"], "title": fiche["title"], "original_title": fiche["original_title"], "year": fiche["year"], "overview": fiche["overview"],
        "genres": fiche["genres"].as_array().cloned().unwrap_or_default(), "keywords": fiche["keywords"].as_array().cloned().unwrap_or_default(),
    });
    match fiche["type"].as_str() {
        Some("music") => {
            let m = &fiche["music"];
            out["music"] = json!({ "artist": m["artist"], "album": m["album"], "album_artist": m["album_artist"], "track": m["track"], "composer": m["composer"] });
        }
        _ => {
            out["cast"] = Value::Array(names(&fiche["people"]["cast"]));
            out["directors"] = Value::Array(directors);
            if fiche["type"] == "episode" {
                let s = &fiche["series"];
                out["series"] = json!({ "name": s["name"], "season": s["season"], "episode": s["episode"] });
            }
        }
    }
    out
}

fn text(v: &Value, field: &str, max: usize) -> Result<Option<String>, String> {
    match v {
        Value::Null => Ok(None),
        Value::String(s) if s.trim().is_empty() => Ok(None),
        Value::String(s) if s.chars().count() > max => Err(format!("{field} : {max} caractères au plus")),
        Value::String(s) if s.chars().any(char::is_control) && field != "résumé" => Err(format!("{field} : caractères de contrôle refusés")),
        Value::String(s) => Ok(Some(s.trim().to_owned())),
        _ => Err(format!("{field} : texte attendu")),
    }
}

fn number(v: &Value, field: &str, range: std::ops::RangeInclusive<i64>) -> Result<Option<i64>, String> {
    match v {
        Value::Null => Ok(None),
        Value::String(s) if s.trim().is_empty() => Ok(None),
        Value::String(s) => s.trim().parse::<i64>().map_err(|_| format!("{field} : nombre attendu")).and_then(|n| number(&json!(n), field, range)),
        Value::Number(n) => n.as_i64().filter(|n| range.contains(n)).map(Some).ok_or_else(|| format!("{field} : entre {} et {}", range.start(), range.end())),
        _ => Err(format!("{field} : nombre attendu")),
    }
}

fn list(v: &Value, field: &str, max_items: usize, max_len: usize) -> Result<Vec<String>, String> {
    let items: Vec<&Value> = match v {
        Value::Null => return Ok(Vec::new()),
        Value::Array(a) => a.iter().collect(),
        _ => return Err(format!("{field} : liste attendue")),
    };
    let mut out: Vec<String> = Vec::new();
    for x in items {
        if let Some(s) = text(x, field, max_len)? {
            if !out.iter().any(|o| o.eq_ignore_ascii_case(&s)) {
                out.push(s);
            }
        }
    }
    if out.len() > max_items {
        return Err(format!("{field} : {max_items} au plus"));
    }
    Ok(out)
}

fn put(obj: &mut Value, key: &str, v: Option<Value>) {
    if let Some(map) = obj.as_object_mut() {
        match v {
            Some(v) => {
                map.insert(key.to_owned(), v);
            }
            None => {
                map.remove(key);
            }
        }
    }
}

/// Applique au document `fiche` les champs présents dans `set` (absents : inchangés ; `null` ou vide : effacés). Seuls les
/// champs de son type sont acceptés ; la fiche reste conforme au contrat `media/v1` (titre et artiste obligatoires).
pub fn apply_edit(fiche: &mut Value, set: &Value) -> Result<(), String> {
    let Some(set) = set.as_object() else { return Err("modifications attendues sous forme d'objet".into()) };
    let kind = fiche["type"].as_str().unwrap_or_default().to_owned();
    let allowed: &[&str] = match kind.as_str() {
        "music" => &["title", "original_title", "year", "overview", "genres", "keywords", "music"],
        "episode" => &["title", "original_title", "year", "overview", "genres", "keywords", "cast", "directors", "series"],
        "film" | "video" => &["title", "original_title", "year", "overview", "genres", "keywords", "cast", "directors"],
        _ => return Err("fiche d'un type inconnu".into()),
    };
    if let Some(k) = set.keys().find(|k| !allowed.contains(&k.as_str())) {
        return Err(format!("« {k} » ne se modifie pas pour ce type"));
    }
    if let Some(v) = set.get("title") {
        let t = text(v, "titre", 300)?.ok_or("le titre est obligatoire")?;
        put(fiche, "title", Some(json!(t)));
    }
    if let Some(v) = set.get("original_title") {
        put(fiche, "original_title", text(v, "titre original", 300)?.map(Value::String));
    }
    if let Some(v) = set.get("year") {
        put(fiche, "year", number(v, "année", 1870..=2200)?.map(Value::from));
    }
    if let Some(v) = set.get("overview") {
        put(fiche, "overview", text(v, "résumé", 4000)?.map(Value::String));
    }
    for (key, label, n, len) in [("genres", "genres", 20, 40), ("keywords", "mots-clés", 50, 60)] {
        if let Some(v) = set.get(key) {
            let l = list(v, label, n, len)?;
            put(fiche, key, (!l.is_empty()).then(|| json!(l)));
        }
    }
    if let Some(v) = set.get("cast") {
        let names = list(v, "distribution", 50, 80)?;
        let before: Vec<Value> = fiche["people"]["cast"].as_array().cloned().unwrap_or_default();
        // Un acteur déjà connu garde son rôle et ses identifiants ; l'ordre est celui du formulaire.
        let cast: Vec<Value> = names
            .iter()
            .enumerate()
            .map(|(i, n)| {
                let mut c = before.iter().find(|c| c["name"] == n.as_str()).cloned().unwrap_or_else(|| json!({ "name": n }));
                c["order"] = json!(i);
                c
            })
            .collect();
        if !fiche["people"].is_object() {
            fiche["people"] = json!({});
        }
        put(&mut fiche["people"], "cast", Some(json!(cast)));
    }
    if let Some(v) = set.get("directors") {
        let names = list(v, "réalisation", 10, 80)?;
        if !fiche["people"].is_object() {
            fiche["people"] = json!({});
        }
        let mut crew: Vec<Value> = fiche["people"]["crew"].as_array().cloned().unwrap_or_default().into_iter().filter(|c| c["job"] != "Director").collect();
        let before: Vec<Value> = fiche["people"]["crew"].as_array().cloned().unwrap_or_default();
        for n in names.iter().rev() {
            let c = before.iter().find(|c| c["job"] == "Director" && c["name"] == n.as_str()).cloned().unwrap_or_else(|| json!({ "name": n, "job": "Director", "department": "Directing" }));
            crew.insert(0, c);
        }
        put(&mut fiche["people"], "crew", Some(json!(crew)));
    }
    if let Some(v) = set.get("series") {
        let name = text(&v["name"], "série", 200)?.ok_or("une série a un nom")?;
        let season = number(&v["season"], "saison", 0..=999)?.ok_or("une saison est attendue")?;
        let episode = number(&v["episode"], "épisode", 0..=9999)?.ok_or("un numéro d'épisode est attendu")?;
        if !fiche["series"].is_object() {
            fiche["series"] = json!({});
        }
        fiche["series"]["name"] = json!(name);
        fiche["series"]["season"] = json!(season);
        fiche["series"]["episode"] = json!(episode);
    }
    if let Some(v) = set.get("music") {
        let Some(m) = v.as_object() else { return Err("music : objet attendu".into()) };
        if let Some(k) = m.keys().find(|k| !["artist", "album", "album_artist", "track", "composer"].contains(&k.as_str())) {
            return Err(format!("« {k} » ne se modifie pas pour un morceau"));
        }
        if !fiche["music"].is_object() {
            fiche["music"] = json!({});
        }
        let target = &mut fiche["music"];
        if let Some(a) = m.get("artist") {
            put(target, "artist", Some(json!(text(a, "artiste", 200)?.ok_or("un morceau a un artiste")?)));
        }
        for (key, label) in [("album", "album"), ("album_artist", "artiste de l'album"), ("composer", "compositeur")] {
            if let Some(x) = m.get(key) {
                put(target, key, text(x, label, 200)?.map(Value::String));
            }
        }
        if let Some(x) = m.get("track") {
            put(target, "track", number(x, "piste", 0..=9999)?.map(Value::from));
        }
    }
    Ok(())
}

/// Fiche `fiche_path` de `owner`, lue en clair et vérifiée (`media/v1`).
pub(crate) async fn read_fiche<B: BlobStore>(dav: &DavState<B>, owner: &str, fiche_path: &str) -> Result<Value, String> {
    if !fiche_path.ends_with(FICHE) {
        return Err("ce n'est pas une fiche (<nom>.media.json)".into());
    }
    let raw = astro_dav::carddav::read_file(dav, owner, fiche_path).await.ok_or("fiche introuvable")?;
    if raw.len() as i64 > MAX_FICHE_BYTES {
        return Err("fiche trop grande".into());
    }
    let fiche: Value = serde_json::from_slice(&raw).map_err(|_| "fiche illisible (JSON)".to_owned())?;
    if fiche["schema"] != "media/v1" {
        return Err("fiche d'un autre format que media/v1".into());
    }
    Ok(fiche)
}

/// Champs modifiables de la fiche `fiche_path`.
pub async fn fiche<B: BlobStore>(dav: &DavState<B>, owner: &str, fiche_path: &str) -> Result<Value, String> {
    Ok(editable(&read_fiche(dav, owner, fiche_path).await?))
}

/// Corrige la fiche `fiche_path` (voir [`apply_edit`]) et la réécrit à sa place ; rend son nouveau résumé.
pub async fn edit<B: BlobStore>(dav: &Arc<DavState<B>>, owner: &str, fiche_path: &str, set: &Value) -> Result<Value, String> {
    let mut fiche = read_fiche(dav, owner, fiche_path).await?;
    apply_edit(&mut fiche, set)?;
    write_fiche(dav, owner, fiche_path, &fiche).await?;
    summarize(&fiche, fiche_path).ok_or_else(|| "fiche réécrite mais illisible".to_owned())
}

/// Réécrit la fiche à sa place (même chiffrement, même quota) : seule la fiche change de CID, jamais le fichier décrit.
pub(crate) async fn write_fiche<B: BlobStore>(dav: &Arc<DavState<B>>, owner: &str, fiche_path: &str, fiche: &Value) -> Result<(), String> {
    let body = serde_json::to_vec_pretty(fiche).map_err(|e| e.to_string())?;
    let (dir, name) = (parent_of(fiche_path), fiche_path.rsplit('/').next().unwrap_or_default());
    let dir = if dir.is_empty() { "/" } else { dir };
    astro_dav::ingest::replace(dav, owner, dir, name, Some("application/json"), body.as_slice()).await.map(|_| ()).map_err(|e| e.to_string())
}

/// Médiathèque de `owner` : `{ items, pending }`. Les fiches nouvelles ou modifiées sont relues (au plus [`MAX_PARSE`] par
/// appel, `pending` dit combien attendent) ; une fiche dont le fichier a disparu n'est pas rendue.
pub async fn library<B: BlobStore>(dav: &DavState<B>, owner: &str) -> Result<Value, astro_db::DbError> {
    let o = owner.to_owned();
    let fiches: Vec<(String, String, i64, Option<String>)> = dav
        .db
        .readers()
        .read(move |c| {
            let mut stmt = c.prepare_cached(
                "SELECT e.path, e.cid, COALESCE(e.size, 0), m.cid FROM ucloud_entries e LEFT JOIN media_index m ON m.owner_hex = e.owner_hex AND m.path = e.path \
                 WHERE e.owner_hex = ?1 AND e.type = 'file' AND e.cid IS NOT NULL AND e.path GLOB '*.media.json'",
            )?;
            let rows = stmt.query_map([&o], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .await?;
    let stale: Vec<&(String, String, i64, Option<String>)> = fiches.iter().filter(|(_, cid, _, seen)| seen.as_deref() != Some(cid.as_str())).collect();
    let pending = stale.len().saturating_sub(MAX_PARSE);
    let mut fresh: Vec<(String, String, Option<Value>)> = Vec::new();
    for (path, cid, size, _) in stale.into_iter().take(MAX_PARSE) {
        let item = if *size > MAX_FICHE_BYTES {
            None
        } else {
            astro_dav::carddav::read_file(dav, owner, path).await.and_then(|raw| serde_json::from_slice::<Value>(&raw).ok()).and_then(|f| summarize(&f, path))
        };
        fresh.push((path.clone(), cid.clone(), item));
    }
    let present: HashSet<String> = fiches.iter().map(|f| f.0.clone()).collect();
    let o = owner.to_owned();
    dav.db
        .writer()
        .transaction(move |tx| {
            let known: Vec<String> = tx.prepare("SELECT path FROM media_index WHERE owner_hex = ?1")?.query_map([&o], |r| r.get(0))?.collect::<Result<_, _>>()?;
            for gone in known.iter().filter(|p| !present.contains(*p)) {
                tx.execute("DELETE FROM media_index WHERE owner_hex = ?1 AND path = ?2", params![o, gone])?;
                tx.execute("DELETE FROM search_text WHERE owner_hex = ?1 AND src = 'fiche' AND ref = ?2", params![o, gone])?;
            }
            for (path, cid, item) in &fresh {
                tx.execute("INSERT OR REPLACE INTO media_index(owner_hex, path, cid, item) VALUES (?1, ?2, ?3, ?4)", params![o, path, cid, item.as_ref().map(Value::to_string)])?;
                // La fiche entre dans la recherche multimédia, rattachée au fichier qu'elle décrit par son empreinte (`sha_plain`) :
                // un moment trouvé dans le film (images clés, parole) renvoie ainsi à sa fiche, et inversement.
                tx.execute("DELETE FROM search_text WHERE owner_hex = ?1 AND src = 'fiche' AND ref = ?2", params![o, path])?;
                if let Some(item) = item {
                    tx.execute(
                        "INSERT INTO search_text(body, owner_hex, sha, src, t_ms, ref) SELECT ?1, ?2, COALESCE((SELECT sha_plain FROM ucloud_entries WHERE owner_hex = ?2 AND path = ?3), ''), 'fiche', 0, ?4",
                        params![search_body(item), o, item["path"].as_str().unwrap_or_default(), path],
                    )?;
                }
            }
            Ok(())
        })
        .await?;
    let o = owner.to_owned();
    let items: Vec<Value> = dav
        .db
        .readers()
        .read(move |c| {
            let mut stmt = c.prepare_cached(
                "SELECT m.item FROM media_index m WHERE m.owner_hex = ?1 AND m.item IS NOT NULL \
                 AND EXISTS (SELECT 1 FROM ucloud_entries f WHERE f.owner_hex = m.owner_hex AND f.path = json_extract(m.item, '$.path') AND f.type = 'file')",
            )?;
            let rows = stmt.query_map([&o], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
            Ok(rows.iter().filter_map(|s| serde_json::from_str(s).ok()).collect())
        })
        .await?;
    Ok(json!({ "items": items, "pending": pending }))
}

/// Adresses de lecture des `paths` de `owner` : `{ urls: {chemin: "/play/…"}, expires_at }`. Seuls les fichiers du compte
/// lisibles par un lecteur (son, vidéo, sous-titres) en reçoivent une.
pub async fn play_urls<B: BlobStore>(dav: &DavState<B>, owner: &str, paths: Vec<String>, now: i64) -> Result<Value, astro_db::DbError> {
    let o = owner.to_owned();
    let found: Vec<(String, Option<String>)> = dav
        .db
        .readers()
        .read(move |c| {
            let mut out = Vec::new();
            for p in &paths {
                let row = rusqlite::OptionalExtension::optional(c.query_row(
                    "SELECT path, mime FROM ucloud_entries WHERE owner_hex = ?1 AND path = ?2 AND type = 'file' AND cid IS NOT NULL",
                    params![o, p],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
                ))?;
                out.extend(row);
            }
            Ok(out)
        })
        .await?;
    let urls: HashMap<String, String> = found
        .into_iter()
        .filter(|(p, mime)| playable(mime.as_deref(), p))
        .map(|(p, _)| {
            let url = format!("/play/{}", play_token(&dav.config.station_secret, owner, &p, now));
            (p, url)
        })
        .collect();
    Ok(json!({ "urls": urls, "expires_at": now + PLAY_TTL_SECS }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    #[test]
    fn a_play_token_opens_one_file_of_one_account_until_it_expires() {
        let t = play_token(b"secret", ME, "/Vidéos/Films/Le Voyage (2001).mp4", 1000);
        assert_eq!(check_token(b"secret", &t, 1000), Some((ME.to_owned(), "/Vidéos/Films/Le Voyage (2001).mp4".to_owned())));
        assert_eq!(check_token(b"secret", &t, 1000 + PLAY_TTL_SECS), Some((ME.to_owned(), "/Vidéos/Films/Le Voyage (2001).mp4".to_owned())));
        assert_eq!(check_token(b"secret", &t, 1001 + PLAY_TTL_SECS), None, "expiré");
        assert_eq!(check_token(b"autre", &t, 1000), None, "signé par une autre station");
        // Changer le chemin (ou le compte) sans la clé de la station : refusé.
        let (body, tag) = t.split_once('.').unwrap();
        let forged = String::from_utf8(B64.decode(body).unwrap()).unwrap().replace("Le Voyage", "Autre");
        assert_eq!(check_token(b"secret", &format!("{}.{tag}", B64.encode(forged)), 1000), None);
        for bad in ["", ".", "x.y", &format!("{body}."), &format!("{body}.{}", B64.encode([0u8; 18]))] {
            assert_eq!(check_token(b"secret", bad, 1000), None, "{bad:?}");
        }
    }

    #[test]
    fn only_sound_video_and_subtitles_are_playable() {
        assert!(playable(Some("video/mp4"), "/a.mp4") && playable(Some("audio/mpeg"), "/a.mp3") && playable(None, "/a.fr.vtt"));
        assert!(!playable(Some("image/jpeg"), "/a.jpg") && !playable(Some("application/json"), "/a.media.json") && !playable(None, "/a"));
    }

    #[test]
    fn a_fiche_and_its_companions_are_hidden_while_the_described_file_is_there() {
        let names = ["Film (2001).mp4", "Film (2001).media.json", "Film (2001).poster.jpg", "Film (2001).fr.vtt", "Film (2001).en.web.vtt", "Vacances.jpg", "Film (2001) bis.jpg"];
        let hide = hidden_names(names);
        assert_eq!(hide, ["Film (2001).media.json", "Film (2001).poster.jpg", "Film (2001).fr.vtt", "Film (2001).en.web.vtt"].into_iter().map(String::from).collect());
        // Le film effacé : ce qui reste réapparaît, pour être vu et supprimé.
        assert!(hidden_names(["Film (2001).media.json", "Film (2001).poster.jpg"]).is_empty());
    }

    #[test]
    fn a_fiche_is_summarized_with_paths_next_to_it_and_nothing_outside() {
        let fiche = json!({
            "schema": "media/v1", "type": "music", "file": "02 - A - B.mp3", "title": "B", "generated_at": 5,
            "music": { "artist": "A", "album": "Mon Mix", "album_artist": "DJ", "track": 2, "from_mix": { "title": "Mon Mix", "start_s": 200 } },
            "artwork": { "cover": "02 - A - B.cover.jpg", "poster": "../../../etc/passwd" },
            "technical": { "duration_s": 245.4, "mime": "audio/mpeg" }, "sharing": { "level": "circle" }, "source": { "rights": "private_copy" },
        });
        let s = summarize(&fiche, "/Musique/DJ/Mon Mix/02 - A - B.media.json").unwrap();
        assert_eq!(s["path"], "/Musique/DJ/Mon Mix/02 - A - B.mp3");
        assert_eq!(s["art"], json!({ "cover": "/Musique/DJ/Mon Mix/02 - A - B.cover.jpg" }), "un nom qui sort du dossier est ignoré");
        assert_eq!((s["duration_s"].as_i64(), s["level"].as_str(), s["music"]["mix_start_s"].as_i64()), (Some(245), Some("circle"), Some(200)));
        assert!(summarize(&json!({ "schema": "media/v1", "type": "music", "file": "x/y.mp3", "title": "t" }), "/a.media.json").is_none());
        assert!(summarize(&json!({ "schema": "autre", "type": "film", "file": "f.mp4", "title": "t" }), "/a.media.json").is_none());
        assert_eq!(summarize(&json!({ "schema": "media/v1", "type": "film", "file": "f.mp4", "title": "t" }), "/f.media.json").unwrap()["path"], "/f.mp4", "à la racine");
    }

    fn film() -> Value {
        json!({ "schema": "media/v1", "type": "film", "file": "f.mp4", "title": "Chihiro", "year": 2001, "genres": ["Animation"],
                "people": { "cast": [{ "name": "Rumi Hiiragi", "character": "Chihiro", "order": 0 }], "crew": [{ "name": "Hayao Miyazaki", "job": "Director" }, { "name": "Joe Hisaishi", "job": "Original Music Composer" }] },
                "technical": {}, "source": { "kind": "disc", "rights": "private_copy" }, "sharing": { "level": "private" } })
    }

    #[test]
    fn a_film_fiche_is_corrected_and_enriched_field_by_field() {
        let mut f = film();
        apply_edit(&mut f, &json!({ "title": " Le Voyage de Chihiro ", "year": "2002", "keywords": ["esprits", "Esprits", "bains"], "original_title": "",
            "cast": ["Miyu Irino", "Rumi Hiiragi"], "directors": ["Hayao Miyazaki"] })).unwrap();
        assert_eq!((f["title"].as_str(), f["year"].as_i64()), (Some("Le Voyage de Chihiro"), Some(2002)));
        assert_eq!(f["keywords"], json!(["esprits", "bains"]), "doublons retirés sans tenir compte de la casse");
        assert!(f.get("original_title").is_none(), "vide : effacé");
        assert_eq!(f["people"]["cast"][1], json!({ "name": "Rumi Hiiragi", "character": "Chihiro", "order": 1 }), "rôle conservé, nouvel ordre");
        assert_eq!(f["people"]["crew"].as_array().unwrap().len(), 2, "le reste de l'équipe est conservé");
        assert_eq!(f["genres"], json!(["Animation"]), "absent : inchangé");
        let e = editable(&f);
        assert_eq!((e["cast"].as_array().unwrap().len(), e["directors"][0].as_str()), (2, Some("Hayao Miyazaki")));
    }

    #[test]
    fn a_correction_keeps_the_fiche_within_the_contract() {
        let mut f = film();
        for (bad, why) in [
            (json!({ "title": "" }), "titre"), (json!({ "year": 1500 }), "année"), (json!({ "music": { "artist": "x" } }), "ne se modifie pas"),
            (json!({ "series": { "name": "S" } }), "ne se modifie pas"), (json!({ "technical": {} }), "ne se modifie pas"),
            (json!({ "genres": "Drame" }), "liste"), (json!({ "title": "a\u{0}b" }), "contrôle"), (json!(["title"]), "objet"),
        ] {
            let err = apply_edit(&mut f, &bad).unwrap_err();
            assert!(err.contains(why), "{bad} → {err}");
        }
        let mut m = json!({ "schema": "media/v1", "type": "music", "file": "x.mp3", "title": "B", "music": { "artist": "A", "track": 2 } });
        assert!(apply_edit(&mut m, &json!({ "music": { "artist": "" } })).unwrap_err().contains("artiste"));
        apply_edit(&mut m, &json!({ "music": { "album": "Mon album", "track": null, "composer": "C" }, "genres": ["Jazz"] })).unwrap();
        assert_eq!(m["music"], json!({ "artist": "A", "album": "Mon album", "composer": "C" }));
        let mut ep = json!({ "schema": "media/v1", "type": "episode", "file": "e.mp4", "title": "Pilote", "series": { "name": "S", "season": 1, "episode": 1, "networks": ["N"] } });
        apply_edit(&mut ep, &json!({ "series": { "name": "Série", "season": "2", "episode": 5 } })).unwrap();
        assert_eq!(ep["series"], json!({ "name": "Série", "season": 2, "episode": 5, "networks": ["N"] }));
    }
}
