//! « Partager avec mes amis » depuis la médiathèque, et la **trace** de ce qui a été partagé.
//!
//! - **Partager** un film, un épisode ou un morceau : la station signe, avec la clé du membre, l'événement NIP-71 (kind 21/22)
//!   que publiait le plugin `media` (mêmes étiquettes, voir [`event_tags`]) et le pose sur le relai, où seul le cercle N¹ du
//!   membre le lit. L'événement désigne le blob **tel qu'il est** : une copie reçue d'un ami se repartage sous le même CID,
//!   sans rechiffrer, pour qu'un même film n'occupe qu'un blob d'un bout à l'autre du cercle.
//! - **Une série, un album** se partagent comme une **liste** : chaque épisode (ou morceau) a son événement, et une liste
//!   NIP-51 (kind 30005, remplaçable) les range dans l'ordre.
//! - **Retirer** : suppression NIP-09 des événements (et de la liste) ; les copies déjà faites restent chez les amis.
//! - **Trace** : qui a reçu une copie et quand (servie par cette station), de qui le membre tient le fichier, quels amis le
//!   partagent à leur tour.

use std::sync::Arc;

use astro_crypto::KeySource;
use astro_dav::{BlobStore, DavState};
use astro_nostr_proto::{Event, UnsignedEvent, kind};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};

use crate::register::PublishFn;

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

/// Étiquette de genre (NIP-32) : minuscules, accents français retirés, mots reliés par des tirets (comme le plugin).
pub fn genre_slug(g: &str) -> String {
    let folded: String = g
        .to_lowercase()
        .chars()
        .map(|c| match c {
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'à' | 'â' | 'ä' => 'a',
            'î' | 'ï' => 'i',
            'ô' | 'ö' => 'o',
            'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            c => c,
        })
        .collect();
    folded.split(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit())).filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-")
}

fn s(v: &Value) -> Option<&str> {
    v.as_str().filter(|x| !x.is_empty())
}

fn display_title(f: &Value) -> String {
    let title = s(&f["title"]).unwrap_or_default();
    let sr = &f["series"];
    if f["type"] == "episode" && sr.is_object() {
        let name = s(&sr["name"]).unwrap_or_default();
        let base = format!("{name} S{:02}E{:02}", sr["season"].as_i64().unwrap_or(0), sr["episode"].as_i64().unwrap_or(0));
        return if title == name { base } else { format!("{base} — {title}") };
    }
    match f["year"].as_i64() {
        Some(y) => format!("{title} ({y})"),
        None => title.to_owned(),
    }
}

fn common_tags(f: &Value) -> Vec<Vec<String>> {
    let mut tags: Vec<Vec<String>> = Vec::new();
    let t = |a: &str, b: &str| vec![a.to_owned(), b.to_owned()];
    if f["source"]["rights"] == "private_copy" {
        tags.push(t("t", "private-copy"));
        tags.push(vec!["l".into(), "private-copy-cpi-l122-5".into(), "license".into()]);
    } else if let Some(l) = s(&f["source"]["licence"]) {
        tags.push(vec!["l".into(), l.to_owned(), "license".into()]);
    }
    for g in f["genres"].as_array().into_iter().flatten().filter_map(Value::as_str) {
        let slug = genre_slug(g);
        if !slug.is_empty() {
            tags.push(vec!["l".into(), g.to_owned(), "genre".into()]);
            tags.push(t("t", &slug));
        }
    }
    if let Some(y) = f["year"].as_i64() {
        tags.push(t("year", &y.to_string()));
    }
    if let Some(id) = f["ids"]["tmdb"].as_i64() {
        tags.push(t("i", &format!("tmdb:{id}")));
    }
    if let Some(id) = s(&f["ids"]["youtube"]) {
        tags.push(t("i", &format!("youtube:{id}")));
    }
    // Épisode : la série par son identifiant, pour reconnaître le même épisode chez un ami.
    if let Some(id) = f["series"]["ids"]["tmdb"].as_i64().filter(|_| f["type"] == "episode") {
        tags.push(t("i", &format!("tmdb-tv:{id}")));
    }
    tags
}

/// Étiquettes de l'événement d'une fiche (les mêmes que le plugin `media` : test d'interopérabilité). `cid` : le blob du
/// fichier ; `x` : l'empreinte de son clair ; `cover` : le blob de la pochette d'un morceau. Rend `(kind, étiquettes)`.
pub fn event_tags(f: &Value, cid: &str, x: &str, cover: Option<&str>, at: i64) -> (u32, Vec<Vec<String>>) {
    let tech = &f["technical"];
    let duration = tech["duration_s"].as_f64().unwrap_or(0.0);
    let size = tech["size"].as_i64().unwrap_or(0);
    let mime = s(&tech["mime"]).unwrap_or("application/octet-stream");
    let k = if duration.round() <= 60.0 { kind::VIDEO_SHORT } else { kind::VIDEO };
    let url = format!("/ipfs/{cid}");
    let t = |a: &str, b: &str| vec![a.to_owned(), b.to_owned()];
    let mut timing = Vec::new();
    if duration > 0.0 {
        timing.push(format!("duration {duration:.3}"));
        timing.push(format!("bitrate {}", (size as f64 * 8.0 / duration) as i64));
    }
    let mut tags: Vec<Vec<String>>;
    if f["type"] == "music" {
        let m = &f["music"];
        let artist = s(&m["artist"]).unwrap_or_default();
        let mut imeta = vec!["imeta".to_owned(), format!("url {url}"), format!("m {mime}"), format!("x {x}"), format!("size {size}")];
        imeta.extend(timing);
        if let Some(c) = cover {
            imeta.push(format!("image /ipfs/{c}"));
        }
        imeta.push("encrypted uenc-v2".into());
        tags = vec![
            t("title", &format!("{artist} — {}", s(&f["title"]).unwrap_or_default())), t("url", &url), t("m", mime), imeta, t("size", &size.to_string()),
            t("duration", &format!("{}", duration.round() as i64)), t("published_at", &at.to_string()), t("x", x), t("encrypted", "uenc-v2"),
            t("t", "music"), t("t", if k == kind::VIDEO_SHORT { "short" } else { "regular" }), vec!["-".into()], t("L", "genre"), t("L", "license"), t("artist", artist),
        ];
        if let Some(c) = cover {
            tags.push(t("image", &format!("/ipfs/{c}")));
        }
        for key in ["album", "album_artist"] {
            if let Some(v) = s(&m[key]) {
                tags.push(t(key, v));
            }
        }
        if let Some(n) = m["track"].as_i64() {
            tags.push(t("track_number", &n.to_string()));
        }
        tags.extend(common_tags(f));
        return (k, tags);
    }
    let episode = f["type"] == "episode";
    let dim = tech["video"]["width"].as_i64().zip(tech["video"]["height"].as_i64()).map(|(w, h)| format!("{w}x{h}"));
    let mut imeta = vec!["imeta".to_owned()];
    imeta.extend(dim.iter().map(|d| format!("dim {d}")));
    imeta.extend([format!("url {url}"), format!("m {mime}"), format!("x {x}"), format!("size {size}")]);
    imeta.extend(timing);
    imeta.push("encrypted uenc-v2".into());
    tags = vec![t("title", &display_title(f)), t("url", &url), t("m", mime)];
    if let Some(d) = &dim {
        tags.push(t("dim", d));
    }
    tags.extend([
        imeta, t("size", &size.to_string()), t("duration", &format!("{}", duration.round() as i64)), t("published_at", &at.to_string()), t("x", x), t("encrypted", "uenc-v2"),
        t("t", if episode { "series" } else { "film" }), t("t", if episode { "tv-show" } else { "movie" }), t("t", if k == kind::VIDEO_SHORT { "short" } else { "regular" }),
        vec!["-".into()], t("L", "genre"), t("L", "license"),
    ]);
    tags.extend(common_tags(f));
    if episode {
        let sr = &f["series"];
        let name = s(&sr["name"]).unwrap_or_default();
        tags.extend([t("series_name", name), t("season_number", &sr["season"].as_i64().unwrap_or(0).to_string()), t("episode_number", &sr["episode"].as_i64().unwrap_or(0).to_string())]);
        if let Some(title) = s(&f["title"]).filter(|title| *title != name) {
            tags.push(t("episode_name", title));
        }
    }
    (k, tags)
}

/// Une liste partagée : une série (`series`) ou un album (`album`), nommée.
#[derive(Debug, Clone)]
pub struct SharedList {
    /// `series` ou `album`.
    pub kind: String,
    /// Nom de la série, ou « artiste — album ».
    pub title: String,
}

impl SharedList {
    fn d(&self) -> String {
        format!("{}:{}", self.kind, genre_slug(&self.title))
    }
}

/// Partage depuis la médiathèque.
pub struct MediaShare<B: BlobStore> {
    dav: Arc<DavState<B>>,
    keys: Arc<dyn KeySource>,
    publish: PublishFn,
}

/// Ce qu'il faut d'une fiche pour la partager : la fiche, le chemin du fichier, son blob, son empreinte, la pochette.
struct Target {
    fiche_path: String,
    fiche: Value,
    cid: String,
    x: String,
    cover: Option<String>,
}

impl<B: BlobStore> MediaShare<B> {
    /// `publish` : pose un événement signé sur le relai de la station (et ses relais).
    pub fn new(dav: Arc<DavState<B>>, keys: Arc<dyn KeySource>, publish: PublishFn) -> Self {
        Self { dav, keys, publish }
    }

    async fn target(&self, owner: &str, fiche_path: &str) -> Result<Target, String> {
        let fiche = crate::media::read_fiche(&self.dav, owner, fiche_path).await?;
        let summary = crate::media::summarize(&fiche, fiche_path).ok_or("fiche incomplète")?;
        let file = summary["path"].as_str().unwrap_or_default().to_owned();
        let cover_path = summary["art"]["cover"].as_str().map(str::to_owned);
        let (o, f) = (owner.to_owned(), file.clone());
        let (row, cover): (Option<(String, Option<String>)>, Option<String>) = self
            .dav
            .db
            .readers()
            .read(move |c| {
                let row = c.query_row("SELECT cid, sha_plain FROM ucloud_entries WHERE owner_hex = ?1 AND path = ?2 AND type = 'file' AND cid IS NOT NULL", params![o, f], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
                let cover = match cover_path {
                    Some(p) => c.query_row("SELECT cid FROM ucloud_entries WHERE owner_hex = ?1 AND path = ?2 AND type = 'file'", params![o, p], |r| r.get(0)).optional()?.flatten(),
                    None => None,
                };
                Ok((row, cover))
            })
            .await
            .map_err(|e| e.to_string())?;
        let (cid, sha) = row.ok_or("le fichier décrit par la fiche est introuvable")?;
        let x = sha.filter(|s| s.len() == 64).or_else(|| s(&fiche["technical"]["sha256"]).map(str::to_owned)).ok_or("empreinte du fichier inconnue")?;
        Ok(Target { fiche_path: fiche_path.to_owned(), fiche, cid, x, cover })
    }

    async fn sign_publish(&self, owner: &str, k: u32, tags: Vec<Vec<String>>, content: String) -> Result<Event, String> {
        let secret = self.keys.secret(owner).ok_or("clé du compte indisponible : partage impossible")?;
        let event = UnsignedEvent { created_at: u64::try_from(now()).unwrap_or(0), kind: k, tags, content }.sign(&secret).map_err(|e| e.to_string())?;
        (self.publish)(event.clone()).await?;
        Ok(event)
    }

    /// Mes événements 21/22 encore en place pour le fichier d'empreinte `x`.
    async fn my_events(&self, owner: &str, x: &str) -> Vec<String> {
        let (o, x) = (owner.to_owned(), x.to_owned());
        self.dav
            .db
            .readers()
            .read(move |c| {
                let mut stmt = c.prepare("SELECT e.id FROM nostr_events e JOIN nostr_tags t ON t.event_id = e.seq AND t.name = 'x' AND t.value = ?2 WHERE e.pubkey = ?1 AND e.kind IN (21, 22)")?;
                Ok(stmt.query_map(params![o, x], |r| r.get(0))?.collect::<Result<Vec<String>, _>>()?)
            })
            .await
            .unwrap_or_default()
    }

    async fn set_level(&self, owner: &str, t: &mut Target, level: &str) -> Result<(), String> {
        if t.fiche["sharing"]["level"] != level {
            t.fiche["sharing"] = json!({ "level": level });
            crate::media::write_fiche(&self.dav, owner, &t.fiche_path, &t.fiche).await?;
        }
        Ok(())
    }

    /// Partage (`on`) ou retire les fiches `fiche_paths`, dans l'ordre donné ; `list` : la série ou l'album qu'elles forment.
    /// Rend `{ shared, events, list }`.
    pub async fn set(&self, owner: &str, fiche_paths: &[String], on: bool, list: Option<SharedList>) -> Result<Value, String> {
        if fiche_paths.is_empty() || fiche_paths.len() > 500 {
            return Err("1 à 500 fiches".into());
        }
        let mut ids = Vec::new();
        for path in fiche_paths {
            let mut t = self.target(owner, path).await?;
            let old = self.my_events(owner, &t.x).await;
            if !old.is_empty() {
                // Republier (fiche corrigée) ou retirer : les anciens événements sont supprimés (NIP-09), jamais doublés.
                let mut tags: Vec<Vec<String>> = old.iter().map(|id| vec!["e".to_owned(), id.clone()]).collect();
                tags.extend([vec!["k".to_owned(), "21".to_owned()], vec!["k".to_owned(), "22".to_owned()]]);
                self.sign_publish(owner, kind::DELETION, tags, String::new()).await?;
            }
            if on {
                let (k, tags) = event_tags(&t.fiche, &t.cid, &t.x, t.cover.as_deref(), now());
                let content = if t.fiche["type"] == "music" { String::new() } else { s(&t.fiche["overview"]).unwrap_or_default().to_owned() };
                ids.push(self.sign_publish(owner, k, tags, content).await?.id);
            }
            self.set_level(owner, &mut t, if on { "circle" } else { "private" }).await?;
        }
        if let Some(l) = &list {
            if on {
                let mut tags = vec![vec!["d".to_owned(), l.d()], vec!["title".into(), l.title.clone()], vec!["t".into(), l.kind.clone()], vec!["-".into()]];
                tags.extend(ids.iter().map(|id| vec!["e".to_owned(), id.clone()]));
                self.sign_publish(owner, kind::VIDEO_SET, tags, String::new()).await?;
            } else {
                let address = format!("{}:{owner}:{}", kind::VIDEO_SET, l.d());
                self.sign_publish(owner, kind::DELETION, vec![vec!["a".into(), address], vec!["k".into(), kind::VIDEO_SET.to_string()]], String::new()).await?;
            }
        }
        Ok(json!({ "shared": on, "events": ids.len(), "list": list.map(|l| l.d()) }))
    }

    /// Trace d'un partage : `{ shared, from, copies: [{label, at}], reshared: [label] }`.
    pub async fn trace(&self, owner: &str, fiche_path: &str) -> Result<Value, String> {
        let t = self.target(owner, fiche_path).await?;
        let shared = !self.my_events(owner, &t.x).await.is_empty();
        let (o, x, file) = (owner.to_owned(), t.x.clone(), crate::media::summarize(&t.fiche, fiche_path).and_then(|v| v["path"].as_str().map(str::to_owned)).unwrap_or_default());
        let (copies, from, reshared): (Vec<(String, i64)>, Option<String>, Vec<String>) = self
            .dav
            .db
            .readers()
            .read(move |c| {
                let label = "COALESCE((SELECT json_extract(k.content, '$.name') FROM nostr_events k WHERE k.pubkey = {who} AND k.kind = 0 ORDER BY k.created_at DESC LIMIT 1), \
                              (SELECT substr(i.email, 1, instr(i.email, '@') - 1) FROM identities i WHERE i.hex = {who}), substr({who}, 1, 8))";
                let copies = c
                    .prepare(&format!("SELECT {}, r.at FROM media_requests r WHERE r.side = 'served' AND r.owner_hex = ?1 AND r.x = ?2 ORDER BY r.at", label.replace("{who}", "r.peer")))?
                    .query_map(params![o, x], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<Result<Vec<_>, _>>()?;
                let from: Option<String> = c
                    .query_row(&format!("SELECT {} FROM ucloud_entries e WHERE e.owner_hex = ?1 AND e.path = ?2 AND json_extract(e.data, '$.shared_by') IS NOT NULL", label.replace("{who}", "json_extract(e.data, '$.shared_by')")), params![o, file], |r| r.get(0))
                    .optional()?;
                let reshared = c
                    .prepare(&format!("SELECT DISTINCT {} FROM friend_media m WHERE m.owner_hex = ?1 AND m.x = ?2", label.replace("{who}", "m.author")))?
                    .query_map(params![o, x], |r| r.get(0))?
                    .collect::<Result<Vec<String>, _>>()?;
                Ok((copies, from, reshared))
            })
            .await
            .map_err(|e| e.to_string())?;
        Ok(json!({
            "shared": shared, "from": from, "reshared": reshared,
            "copies": copies.into_iter().map(|(label, at)| json!({ "label": label, "at": at })).collect::<Vec<_>>(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn genres_become_the_same_slugs_as_in_the_plugin() {
        assert_eq!(genre_slug("Science-Fiction"), "science-fiction");
        assert_eq!(genre_slug("Comédie dramatique"), "comedie-dramatique");
        assert_eq!(genre_slug("  "), "");
    }

    #[test]
    fn a_track_and_an_episode_carry_the_plugin_tags() {
        let track = json!({ "type": "music", "title": "Titre", "music": { "artist": "A", "album": "Al", "track": 2 }, "technical": { "duration_s": 245.0, "size": 4_000_000, "mime": "audio/mpeg" }, "source": { "rights": "private_copy" } });
        let (k, tags) = event_tags(&track, "bafkreicid", "ab", Some("bafkreicover"), 5);
        assert_eq!(k, 21);
        assert!(tags.contains(&vec!["title".into(), "A — Titre".into()]) && tags.contains(&vec!["image".into(), "/ipfs/bafkreicover".into()]) && tags.contains(&vec!["track_number".into(), "2".into()]));
        let ep = json!({ "type": "episode", "title": "Pilote", "series": { "name": "S", "season": 1, "episode": 2 }, "technical": { "duration_s": 45.0, "size": 100, "mime": "video/mp4" }, "source": { "rights": "owner" } });
        let (k, tags) = event_tags(&ep, "c", "x", None, 5);
        assert_eq!(k, 22, "60 s ou moins");
        assert!(tags.contains(&vec!["title".into(), "S S01E02 — Pilote".into()]) && tags.contains(&vec!["episode_name".into(), "Pilote".into()]));
        assert!(!tags.iter().any(|t| t[0] == "dim"), "sans dimensions connues, pas de dim");
    }
}
