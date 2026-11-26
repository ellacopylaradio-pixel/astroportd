//! Médiathèque **des amis** : les films et morceaux qu'un proche réciproque partage avec son cercle N¹, et leur **copie à la
//! demande**, au clic « Lire ».
//!
//! - **Liste** : pour chaque proche réciproque, ses vidéos et morceaux (kinds 21/22) sont lus chez lui — en base s'il est
//!   hébergé ici, sinon chez sa station, par le maillage, en répondant au défi NIP-42 **avec la clé du membre** : c'est le relai
//!   de l'ami qui décide de ce qu'il montre (son cercle N¹ seulement). Le résultat est gardé dans `friend_media`, un cache
//!   relu au plus toutes les [`REFRESH_SECS`] ; un ami hors ligne garde sa dernière liste, avec l'erreur.
//! - **Demande** : un message NIP-59 (rumeur kind 30078, `t = astro-media`, `op = request`, empreinte `x` du fichier en clair)
//!   part vers la station de l'ami ; s'il est hébergé ici, la demande est servie tout de suite.
//! - **Service, chez l'ami** : la station vérifie que le demandeur est un **proche réciproque** et que l'ami a **lui-même publié**
//!   ce fichier pour son cercle (un événement 21/22 de sa main porte cette empreinte), puis envoie le fichier, sa fiche, son
//!   affiche, sa pochette et ses sous-titres par le partage ordinaire ([`crate::sharing`]). Un refus est dit au demandeur.
//! - **Arrivée** : [`crate::share_inbox`] range la copie dans `/Partages/De_<ami>/` et rapatrie le blob par le maillage ; la
//!   médiathèque la reconnaît à son empreinte et la lit comme un fichier du membre.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use astro_crypto::KeySource;
use astro_crypto::nip59::{self, Rumor, gift_wrap};
use astro_db::Db;
use astro_db::events::row_to_event;
use astro_facecloud::Outbox;
use astro_nostr_proto::{Event, Filter, is_hex64, kind};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use tokio::sync::watch;

use crate::sharing::Sharing;

/// Étiquette des messages de la médiathèque.
pub const TAG: &str = "astro-media";
/// Une liste d'ami est relue au plus souvent toutes les 10 minutes.
pub const REFRESH_SECS: i64 = 600;
/// Une même demande (même fichier, même personne) n'est renvoyée ou resservie qu'après 6 h.
pub const AGAIN_SECS: i64 = 6 * 3600;
/// Vidéos et morceaux lus au plus par ami.
const MAX_ITEMS: usize = 500;
/// Temps laissé à l'ensemble des amis pour répondre lors d'un affichage.
const LIST_TIMEOUT: Duration = Duration::from_secs(12);
const CLAIM: &str = "media_inbox";
const LOOKBACK_SECS: i64 = 30 * 24 * 3600;
const BATCH: i64 = 50;

/// Synchronisation chez la station d'un ami : `(auteur, filtre, clé du membre, événements déjà connus)` → nouveautés et
/// suppressions (voir `MeshOutbox::sync_as` : Negentropy, NIP-77).
pub type FetchAs = Arc<dyn Fn(String, Filter, astro_sync::Signer, Vec<(u64, String)>) -> Pin<Box<dyn Future<Output = Result<astro_sync::SyncResult, String>> + Send>> + Send + Sync>;

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

fn tag<'a>(tags: &'a [Vec<String>], name: &str) -> Option<&'a str> {
    tags.iter().find(|t| t.first().is_some_and(|n| n == name)).and_then(|t| t.get(1)).map(String::as_str)
}

fn clip(s: Option<&str>, max: usize) -> Option<String> {
    s.map(str::trim).filter(|s| !s.is_empty()).map(|s| s.chars().take(max).collect())
}

/// Blob chiffré désigné par l'événement (`url /ipfs/<cid>`), s'il est échangeable.
pub fn cid_of(e: &Event) -> Option<String> {
    tag(&e.tags, "url").and_then(|u| u.strip_prefix("/ipfs/")).filter(|c| astro_dav::blob::is_cid(c)).map(str::to_owned)
}

/// Ce que l'écran montre d'une vidéo ou d'un morceau d'ami, d'après son événement : `(empreinte x, résumé)`. `None` : ce
/// n'est pas un média désignant un fichier (pas d'empreinte, pas de titre).
pub fn item_of(e: &Event) -> Option<(String, Value)> {
    if e.kind != kind::VIDEO && e.kind != kind::VIDEO_SHORT {
        return None;
    }
    let x = tag(&e.tags, "x").filter(|x| is_hex64(x))?.to_ascii_lowercase();
    let title = clip(tag(&e.tags, "title"), 300)?;
    let ts: Vec<&str> = e.tags.iter().filter(|t| t.first().is_some_and(|n| n == "t")).filter_map(|t| t.get(1)).map(String::as_str).collect();
    let typ = if ts.contains(&"music") { "music" } else if tag(&e.tags, "series_name").is_some() { "episode" } else { "film" };
    let num = |k: &str| tag(&e.tags, k).and_then(|v| v.parse::<i64>().ok());
    let genres: Vec<String> = e.tags.iter().filter(|t| t.len() >= 3 && t[0] == "l" && t[2] == "genre").filter_map(|t| clip(Some(&t[1]), 40)).take(8).collect();
    let mut item = json!({
        "event_id": e.id, "type": typ, "title": title, "mime": clip(tag(&e.tags, "m"), 60), "duration_s": num("duration"),
        "size": num("size"), "year": num("year"), "genres": genres, "published_at": e.created_at,
    });
    if typ == "music" {
        item["music"] = json!({ "artist": clip(tag(&e.tags, "artist"), 200), "album": clip(tag(&e.tags, "album"), 200), "album_artist": clip(tag(&e.tags, "album_artist"), 200), "track": num("track_number") });
    }
    if typ == "episode" {
        item["series"] = json!({ "name": clip(tag(&e.tags, "series_name"), 200), "season": num("season_number"), "episode": num("episode_number") });
        item["episode_name"] = json!(clip(tag(&e.tags, "episode_name"), 300));
    }
    // Identifiants de l'œuvre (`i`, NIP-73) : reconnaître le même film chez plusieurs amis, quel que soit le fichier.
    let ext = |prefix: &str| e.tags.iter().filter(|t| t.first().is_some_and(|n| n == "i")).filter_map(|t| t.get(1)?.strip_prefix(prefix)).next();
    item["ids"] = json!({
        "tmdb": ext("tmdb:").and_then(|v| v.parse::<i64>().ok()),
        "tv": ext("tmdb-tv:").and_then(|v| v.parse::<i64>().ok()),
        "youtube": ext("youtube:").filter(|v| is_youtube_id(v)),
    });
    Some((x, item))
}

/// Identifiant de vidéo YouTube : 11 caractères `[A-Za-z0-9_-]`.
pub fn is_youtube_id(v: &str) -> bool {
    v.len() == 11 && v.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}

/// Une œuvre cherchée chez les amis avant un dépôt : un film (`tmdb`), une vidéo ou un morceau (`youtube`), un épisode
/// (`season` + `episode`, avec la série par son identifiant `tv` ou son nom `series`).
#[derive(Debug, Clone, Default)]
pub struct Work {
    /// Film TMDB.
    pub tmdb: Option<i64>,
    /// Série TMDB d'un épisode.
    pub tv: Option<i64>,
    /// Nom de la série d'un épisode (sans identifiant).
    pub series: Option<String>,
    /// Saison d'un épisode.
    pub season: Option<i64>,
    /// Numéro d'un épisode.
    pub episode: Option<i64>,
    /// Vidéo ou morceau YouTube.
    pub youtube: Option<String>,
}

/// Le média d'ami `item` (résumé de [`item_of`]) est-il l'œuvre `w` ? Seuls des identifiants de catalogue (ou série, saison et
/// épisode) font foi : un titre seul ne suffit pas, une autre version porterait le même.
pub fn same_work(item: &Value, w: &Work) -> bool {
    let ids = &item["ids"];
    if w.youtube.as_deref().is_some_and(|y| ids["youtube"].as_str() == Some(y)) {
        return true;
    }
    if item["type"] == "episode" {
        let s = &item["series"];
        let (Some(season), Some(episode)) = (w.season, w.episode) else { return false };
        if s["season"].as_i64() != Some(season) || s["episode"].as_i64() != Some(episode) {
            return false;
        }
        if let (Some(tv), Some(theirs)) = (w.tv, ids["tv"].as_i64()) {
            return tv == theirs;
        }
        let slug = |n: &str| crate::media_share::genre_slug(n);
        return matches!((w.series.as_deref(), s["name"].as_str()), (Some(a), Some(b)) if !slug(a).is_empty() && slug(a) == slug(b));
    }
    w.tmdb.is_some() && ids["tmdb"].as_i64() == w.tmdb
}

/// Meilleure copie de l'œuvre `w` parmi les `items` de la médiathèque des amis : celle déjà copiée chez le membre d'abord,
/// sinon celle du blob le plus répandu (une nouvelle copie le renforce au lieu d'en créer un autre).
pub fn best_copy<'a>(items: &'a [Value], w: &Work) -> Option<&'a Value> {
    items.iter().filter(|i| same_work(i, w)).max_by_key(|i| (i["copied"].is_string(), i["sources"].as_u64().unwrap_or(0)))
}

/// `(auteur, empreinte, blob, résumé, état)` d'un élément du cache.
type Row = (String, String, Option<String>, String, String);
/// Une source d'un film : `(auteur, nom, blob, résumé, état)`.
type Copy = (String, String, Option<String>, Value, Value);
/// `(auteur, date, erreur)` de la dernière lecture d'un ami.
type SyncRow = (String, i64, Option<String>);

/// Un ami : clé, nom, hébergé ici ou non.
struct Friend {
    hex: String,
    label: String,
    hosted: bool,
}

/// La médiathèque des amis.
pub struct FriendsMedia<O: Outbox> {
    db: Arc<Db>,
    keys: Arc<dyn KeySource>,
    outbox: Arc<O>,
    sharing: Arc<Sharing<O>>,
    fetch: Option<FetchAs>,
}

impl<O: Outbox + 'static> FriendsMedia<O> {
    /// `fetch` : lecture chez la station d'un ami (absent : seuls les amis hébergés ici sont lus).
    pub fn new(db: Arc<Db>, keys: Arc<dyn KeySource>, outbox: Arc<O>, sharing: Arc<Sharing<O>>, fetch: Option<FetchAs>) -> Self {
        Self { db, keys, outbox, sharing, fetch }
    }

    async fn friends(&self, owner: &str) -> Result<Vec<Friend>, String> {
        Ok(self.sharing.friends(owner).await?.into_iter().map(|f| Friend { hex: f.hex, label: f.label, hosted: f.hosted }).collect())
    }

    /// Nouveautés et suppressions de `author` depuis `local` (son cache) : en base s'il est hébergé ici, sinon chez sa station
    /// au nom de `owner`, par réconciliation Negentropy (seuls les nouveaux événements voyagent).
    async fn read_friend(&self, owner: &str, author: &str, hosted: bool, local: Vec<(u64, String)>) -> Result<astro_sync::SyncResult, String> {
        let filter = Filter { authors: Some(vec![author.to_owned()]), kinds: Some(vec![kind::VIDEO, kind::VIDEO_SHORT]), limit: Some(MAX_ITEMS), ..Filter::default() };
        if hosted {
            let a = author.to_owned();
            return self
                .db
                .readers()
                .read(move |c| {
                    let mut stmt = c.prepare_cached("SELECT id, pubkey, kind, created_at, content, sig, tags FROM nostr_events WHERE pubkey = ?1 AND kind IN (21, 22) ORDER BY created_at DESC LIMIT ?2")?;
                    let mut rows = stmt.query(params![a, MAX_ITEMS as i64])?;
                    let mut out = Vec::new();
                    while let Some(r) = rows.next()? {
                        out.push(row_to_event(r)?);
                    }
                    Ok(out)
                })
                .await
                .map_err(|e| e.to_string())
                .map(|events: Vec<Event>| {
                    let theirs: std::collections::HashSet<&str> = events.iter().map(|e| e.id.as_str()).collect();
                    let mine: std::collections::HashSet<&str> = local.iter().map(|(_, id)| id.as_str()).collect();
                    let gone = local.iter().filter(|(_, id)| !theirs.contains(id.as_str())).map(|(_, id)| id.clone()).collect();
                    let missing = events.iter().filter(|e| !mine.contains(e.id.as_str())).cloned().collect();
                    astro_sync::SyncResult { missing, gone, negentropy: false }
                });
        }
        let fetch = self.fetch.as_ref().ok_or("maillage inactif : la station de cet ami est injoignable")?;
        let secret = self.keys.secret(owner).ok_or("clé du compte indisponible")?;
        fetch(author.to_owned(), filter, astro_sync::Signer::new(*secret), local).await
    }

    async fn refresh(&self, owner: &str, f: &Friend, at: i64) {
        let (o, author) = (owner.to_owned(), f.hex.clone());
        let local: Vec<(u64, String)> = self
            .db
            .readers()
            .read(move |c| Ok(c.prepare("SELECT created_at, event_id FROM friend_media WHERE owner_hex = ?1 AND author = ?2")?.query_map([&o, &author], |r| Ok((r.get::<_, i64>(0)? as u64, r.get(1)?)))?.collect::<Result<Vec<_>, _>>()?))
            .await
            .unwrap_or_default();
        let got = self.read_friend(owner, &f.hex, f.hosted, local).await;
        let (o, author) = (owner.to_owned(), f.hex.clone());
        let _ = self
            .db
            .writer()
            .transaction(move |tx| {
                match &got {
                    Ok(sync) => {
                        // Ce que l'ami a retiré disparaît ; seuls les nouveaux événements, authentiques et de sa main, entrent.
                        for id in &sync.gone {
                            tx.execute("DELETE FROM friend_media WHERE owner_hex = ?1 AND author = ?2 AND event_id = ?3", params![o, author, id])?;
                        }
                        for e in sync.missing.iter().filter(|e| e.pubkey == author && e.verify().is_ok()).take(MAX_ITEMS) {
                            if let Some((x, item)) = item_of(e) {
                                tx.execute(
                                    "INSERT OR REPLACE INTO friend_media(owner_hex, author, event_id, x, cid, item, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                                    params![o, author, e.id, x, cid_of(e), item.to_string(), i64::try_from(e.created_at).unwrap_or(0)],
                                )?;
                            }
                        }
                        tx.execute("INSERT OR REPLACE INTO friend_media_sync(owner_hex, author, at, error) VALUES (?1, ?2, ?3, NULL)", params![o, author, at])?;
                    }
                    Err(why) => {
                        tx.execute("INSERT OR REPLACE INTO friend_media_sync(owner_hex, author, at, error) VALUES (?1, ?2, ?3, ?4)", params![o, author, at, why])?;
                    }
                }
                Ok(())
            })
            .await;
    }

    /// Médiathèque des amis de `owner` : `{ items, friends }`. Chaque élément dit s'il est déjà copié chez le membre
    /// (`copied` : son chemin) ou demandé (`asked_at`, `error`). `force` relit tous les amis.
    pub async fn list(&self, owner: &str, force: bool) -> Result<Value, String> {
        let at = now();
        let friends = self.friends(owner).await?;
        let o = owner.to_owned();
        let synced: HashMap<String, i64> = self
            .db
            .readers()
            .read(move |c| Ok(c.prepare("SELECT author, at FROM friend_media_sync WHERE owner_hex = ?1")?.query_map([&o], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?))
            .await
            .map_err(|e| e.to_string())?;
        let stale: Vec<&Friend> = friends.iter().filter(|f| force || synced.get(&f.hex).is_none_or(|t| *t < at - REFRESH_SECS)).collect();
        let _ = tokio::time::timeout(LIST_TIMEOUT, futures_util::future::join_all(stale.into_iter().map(|f| self.refresh(owner, f, at)))).await;
        let o = owner.to_owned();
        let labels: HashMap<String, String> = friends.iter().map(|f| (f.hex.clone(), f.label.clone())).collect();
        let (rows, sync): (Vec<Row>, Vec<SyncRow>) = self
            .db
            .readers()
            .read(move |c| {
                let mut stmt = c.prepare(
                    "SELECT m.author, m.x, m.cid, m.item, json_object('copied', (SELECT e.path FROM ucloud_entries e WHERE e.owner_hex = m.owner_hex AND e.sha_plain = m.x AND e.type = 'file' AND e.cid IS NOT NULL ORDER BY e.path LIMIT 1), \
                            'asked_at', r.at, 'error', r.error) \
                     FROM friend_media m LEFT JOIN media_requests r ON r.side = 'asked' AND r.owner_hex = m.owner_hex AND r.peer = m.author AND r.x = m.x \
                     WHERE m.owner_hex = ?1 ORDER BY m.created_at DESC",
                )?;
                let rows = stmt.query_map([&o], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?.collect::<Result<Vec<_>, _>>()?;
                let sync = c.prepare("SELECT author, at, error FROM friend_media_sync WHERE owner_hex = ?1")?.query_map([&o], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<Result<Vec<_>, _>>()?;
                Ok((rows, sync))
            })
            .await
            .map_err(|e| e.to_string())?;
        // Seuls les proches réciproques d'aujourd'hui : un ancien ami disparaît de la liste (son cache reste, sans être montré).
        // Un même film (même empreinte) ne fait qu'une entrée. Ses sources sont regroupées par blob : la demande part vers une
        // source du blob **le plus répandu**, pour que chaque copie renforce ce blob-là au lieu d'en faire vivre plusieurs
        // (un dépôt indépendant du même film est chiffré avec une autre clé : c'est un autre CID).
        let mut by_x: Vec<(String, Vec<Copy>)> = Vec::new();
        for (author, x, cid, item, state) in rows {
            let Some(label) = labels.get(&author) else { continue };
            let (Ok(item), Ok(state)) = (serde_json::from_str::<Value>(&item), serde_json::from_str::<Value>(&state)) else { continue };
            let entry = (author, label.clone(), cid, item, state);
            match by_x.iter_mut().find(|(k, _)| *k == x) {
                Some((_, v)) => v.push(entry),
                None => by_x.push((x, vec![entry])),
            }
        }
        let items: Vec<Value> = by_x
            .into_iter()
            .filter_map(|(x, copies)| {
                let count = |c: &Option<String>| copies.iter().filter(|e| e.2 == *c && c.is_some()).count();
                let best = copies.iter().max_by_key(|e| count(&e.2)).map(|e| e.2.clone())?;
                let pick = copies.iter().find(|e| e.2 == best)?;
                let mut item = pick.3.clone();
                item["x"] = json!(x);
                item["author"] = json!(pick.0);
                item["author_label"] = json!(pick.1);
                item["seeders"] = json!(copies.iter().map(|e| json!({ "hex": e.0, "label": e.1, "same_blob": e.2 == best && best.is_some() })).collect::<Vec<_>>());
                item["sources"] = json!(count(&best));
                for k in ["copied", "asked_at", "error"] {
                    item[k] = pick.4[k].clone();
                }
                Some(item)
            })
            .collect();
        let friends: Vec<Value> = friends
            .iter()
            .map(|f| {
                let s = sync.iter().find(|s| s.0 == f.hex);
                json!({ "hex": f.hex, "label": f.label, "at": s.map(|s| s.1), "error": s.and_then(|s| s.2.clone()) })
            })
            .collect();
        Ok(json!({ "items": items, "friends": friends }))
    }

    /// Demande la copie du média `event_id` (de la liste de `owner`) à son auteur. `{ state }` : `copied` (déjà là, `path`),
    /// `asked` (demande partie, ou déjà partie il y a moins de 6 h) ou `sent` (ami hébergé ici : la copie est déjà faite).
    pub async fn request(&self, owner: &str, event_id: &str) -> Result<Value, String> {
        let at = now();
        let (o, e) = (owner.to_owned(), event_id.to_owned());
        let row: Option<(String, String, Option<String>, Option<i64>)> = self
            .db
            .readers()
            .read(move |c| {
                Ok(c.query_row(
                    "SELECT m.author, m.x, (SELECT u.path FROM ucloud_entries u WHERE u.owner_hex = m.owner_hex AND u.sha_plain = m.x AND u.type = 'file' AND u.cid IS NOT NULL LIMIT 1), \
                            (SELECT r.at FROM media_requests r WHERE r.side = 'asked' AND r.owner_hex = m.owner_hex AND r.peer = m.author AND r.x = m.x AND r.error IS NULL) \
                     FROM friend_media m WHERE m.owner_hex = ?1 AND m.event_id = ?2",
                    params![o, e],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .optional()?)
            })
            .await
            .map_err(|e| e.to_string())?;
        let Some((author, x, copied, asked)) = row else { return Err("ce média n'est pas (ou plus) dans la liste de vos amis".into()) };
        if let Some(path) = copied {
            return Ok(json!({ "state": "copied", "path": path }));
        }
        if asked.is_some_and(|t| t > at - AGAIN_SECS) {
            return Ok(json!({ "state": "asked", "at": asked }));
        }
        let friend = self.friends(owner).await?.into_iter().find(|f| f.hex == author).ok_or("plus un proche réciproque : la copie n'est pas possible")?;
        self.note(owner, &author, &x, at, None).await;
        if friend.hosted {
            // Hébergé ici : pas de message, la station sert la demande comme le ferait celle de l'ami.
            return match self.serve(&author, owner, &x, at).await {
                Ok(_) => Ok(json!({ "state": "sent" })),
                Err(why) => {
                    self.note(owner, &author, &x, at, Some(&why)).await;
                    Err(why)
                }
            };
        }
        let rumor = vec![vec!["op".to_owned(), "request".to_owned()], vec!["x".into(), x.clone()], vec!["e".into(), event_id.to_owned()]];
        if let Err(why) = self.send(owner, &author, rumor, "").await {
            self.note(owner, &author, &x, at, Some(&why)).await;
            return Err(why);
        }
        Ok(json!({ "state": "asked", "at": at }))
    }

    async fn note(&self, owner: &str, peer: &str, x: &str, at: i64, error: Option<&str>) {
        let (o, p, x, e) = (owner.to_owned(), peer.to_owned(), x.to_owned(), error.map(str::to_owned));
        let _ = self.db.writer().execute("INSERT OR REPLACE INTO media_requests(side, owner_hex, peer, x, at, error) VALUES ('asked', ?1, ?2, ?3, ?4, ?5)", vec![o.into(), p.into(), x.into(), at.into(), e.into()]).await;
    }

    async fn send(&self, from: &str, to: &str, mut tags: Vec<Vec<String>>, content: &str) -> Result<(), String> {
        let secret = self.keys.secret(from).ok_or("clé du compte indisponible")?;
        tags.insert(0, vec!["t".into(), TAG.into()]);
        tags.insert(0, vec!["p".into(), to.to_owned()]);
        let at = u64::try_from(now()).unwrap_or(0);
        let rumor = Rumor { pubkey: from.to_owned(), created_at: at, kind: kind::APP_DATA, tags, content: content.to_owned() };
        let wrap = gift_wrap(&secret, to, &rumor, at).map_err(|_| "emballage NIP-59 impossible".to_owned())?;
        self.outbox.send(to, wrap).await.map_err(|e| e.to_string())
    }

    /// Chez l'ami `author` : sert la demande de `requester` pour le fichier d'empreinte `x`. Rend le nombre de fichiers envoyés
    /// (0 : déjà servi il y a moins de 6 h).
    pub async fn serve(&self, author: &str, requester: &str, x: &str, at: i64) -> Result<usize, String> {
        if !self.friends(author).await?.iter().any(|f| f.hex == requester) {
            return Err("vous n'êtes pas un proche réciproque de cette personne".into());
        }
        let (a, r, xx) = (author.to_owned(), requester.to_owned(), x.to_owned());
        let (published, path, served): (bool, Option<String>, Option<i64>) = self
            .db
            .readers()
            .read(move |c| {
                let published = c.query_row(
                    "SELECT EXISTS(SELECT 1 FROM nostr_events e JOIN nostr_tags t ON t.event_id = e.seq AND t.name = 'x' AND t.value = ?2 WHERE e.pubkey = ?1 AND e.kind IN (21, 22))",
                    params![a, xx],
                    |r| r.get(0),
                )?;
                let path = c.query_row("SELECT path FROM ucloud_entries WHERE owner_hex = ?1 AND sha_plain = ?2 AND type = 'file' AND cid IS NOT NULL ORDER BY path LIMIT 1", params![a, xx], |r| r.get(0)).optional()?;
                let served = c.query_row("SELECT at FROM media_requests WHERE side = 'served' AND owner_hex = ?1 AND peer = ?2 AND x = ?3", params![a, r, xx], |r| r.get(0)).optional()?;
                Ok((published, path, served))
            })
            .await
            .map_err(|e| e.to_string())?;
        if !published {
            return Err("ce fichier n'est pas partagé avec le cercle de son propriétaire".into());
        }
        let path = match path {
            Some(p) => p,
            None => {
                let fiche_sha = crate::cascade_share::lookup_fiche_sha_by_work(&self.db, x).await.ok_or("le fichier n'est plus dans l'uCloud de son propriétaire")?;
                let a = author.to_owned();
                self.db
                    .readers()
                    .read(move |c| Ok(c.query_row("SELECT path FROM ucloud_entries WHERE owner_hex = ?1 AND sha_plain = ?2 AND type = 'file' AND cid IS NOT NULL ORDER BY path LIMIT 1", params![a, fiche_sha], |r| r.get::<_, String>(0)).optional()?))
                    .await
                    .map_err(|e| e.to_string())?
                    .ok_or("le fichier n'est plus dans l'uCloud de son propriétaire")?
            }
        };
        if served.is_some_and(|t| t > at - AGAIN_SECS) {
            return Ok(0);
        }
        // Le fichier, puis sa fiche et ses compagnons (affiche, pochette, sous-titres) : la copie arrive prête à être lue.
        let dir = path.rsplit_once('/').map_or("", |(d, _)| d).to_owned();
        let name = astro_dav::vfs::name_of(&path).to_owned();
        let (a, d) = (author.to_owned(), if dir.is_empty() { "/".to_owned() } else { dir.clone() });
        let siblings: Vec<String> = self
            .db
            .readers()
            .read(move |c| Ok(astro_dav::vfs::list_children(c, &a, &d).map_err(|_| astro_db::DbError::Aborted)?.into_iter().map(|e| e.path).collect()))
            .await
            .unwrap_or_default();
        let mut paths = vec![path.clone()];
        paths.extend(siblings.into_iter().filter(|p| crate::media::companion_of_file(astro_dav::vfs::name_of(p), &name)));
        let report = self.sharing.send_to_library(author, &paths, &[requester.to_owned()]).await?;
        let sent = report["results"][0]["sent"].as_u64().unwrap_or(0) as usize;
        if sent == 0 {
            return Err(report["results"][0]["error"].as_str().unwrap_or("envoi impossible").to_owned());
        }
        let (a, r, xx) = (author.to_owned(), requester.to_owned(), x.to_owned());
        let _ = self.db.writer().execute("INSERT OR REPLACE INTO media_requests(side, owner_hex, peer, x, at, error) VALUES ('served', ?1, ?2, ?3, ?4, NULL)", vec![a.into(), r.into(), xx.into(), at.into()]).await;
        Ok(sent)
    }

    /// Un passage de réception : demandes reçues par les comptes hébergés ici, et refus reçus pour leurs propres demandes.
    pub async fn poll_once(&self, at: i64) -> usize {
        let owners: Vec<String> = self
            .db
            .readers()
            .read(|c| Ok(c.prepare("SELECT hex FROM identities WHERE hosted = 1")?.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?))
            .await
            .unwrap_or_default();
        let mut handled = 0;
        for me in owners {
            let Some(secret) = self.keys.secret(&me) else { continue };
            let (m, since) = (me.clone(), at - LOOKBACK_SECS);
            let wraps: Vec<Event> = self
                .db
                .readers()
                .read(move |c| {
                    let mut stmt = c.prepare_cached(
                        "SELECT e.id, e.pubkey, e.kind, e.created_at, e.content, e.sig, e.tags FROM nostr_events e \
                         JOIN nostr_tags t ON t.event_id = e.seq AND t.name = 'p' AND t.value = ?1 \
                         WHERE e.kind = 1059 AND e.created_at >= ?2 AND NOT EXISTS (SELECT 1 FROM claimed c WHERE c.owner = 'media_inbox' AND c.event_id = e.id) \
                         ORDER BY e.created_at LIMIT ?3",
                    )?;
                    let mut rows = stmt.query(params![m, since, BATCH])?;
                    let mut out = Vec::new();
                    while let Some(r) = rows.next()? {
                        out.push(row_to_event(r)?);
                    }
                    Ok(out)
                })
                .await
                .unwrap_or_default();
            for wrap in wraps {
                if let Ok(opened) = nip59::unwrap(&secret, &wrap) {
                    let (sender, rumor) = (opened.sender, opened.rumor);
                    let x = tag(&rumor.tags, "x").filter(|x| is_hex64(x)).map(str::to_ascii_lowercase);
                    if rumor.kind == kind::APP_DATA && tag(&rumor.tags, "t") == Some(TAG) && sender != me {
                        match (tag(&rumor.tags, "op"), x) {
                            (Some("request"), Some(x)) => {
                                handled += 1;
                                if let Err(why) = self.serve(&me, &sender, &x, at).await {
                                    let _ = self.send(&me, &sender, vec![vec!["op".into(), "refused".into()], vec!["x".into(), x.clone()]], &why).await;
                                }
                            }
                            (Some("refused"), Some(x)) => {
                                handled += 1;
                                let why: String = rumor.content.chars().take(300).collect();
                                self.note(&me, &sender, &x, at, Some(if why.trim().is_empty() { "refusé" } else { &why })).await;
                            }
                            _ => {}
                        }
                    }
                }
                let _ = self.db.writer().execute("INSERT OR IGNORE INTO claimed(owner, event_id, at) VALUES (?1, ?2, ?3)", vec![CLAIM.to_owned().into(), wrap.id.clone().into(), at.into()]).await;
            }
        }
        handled
    }

    /// Réception en continu.
    pub async fn run(self: Arc<Self>, every: Duration, mut stop: watch::Receiver<bool>) {
        loop {
            self.poll_once(now()).await;
            tokio::select! {
                _ = tokio::time::sleep(every) => {}
                _ = stop.changed() => return,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use astro_nostr_proto::UnsignedEvent;

    use super::*;

    fn ev(k: u32, tags: Vec<Vec<&str>>) -> Event {
        UnsignedEvent { created_at: 100, kind: k, tags: tags.into_iter().map(|t| t.into_iter().map(String::from).collect()).collect(), content: String::new() }.sign(&[9; 32]).unwrap()
    }

    #[test]
    fn the_same_work_is_recognised_by_its_catalogue_ids_not_by_its_title() {
        let x = "ab".repeat(32);
        let (_, film) = item_of(&ev(21, vec![vec!["title", "Chihiro (2001)"], vec!["x", &x], vec!["i", "tmdb:129"], vec!["i", "youtube:bad"]])).unwrap();
        assert_eq!(film["ids"]["youtube"], Value::Null, "un identifiant YouTube mal formé est ignoré");
        assert!(same_work(&film, &Work { tmdb: Some(129), ..Work::default() }));
        assert!(!same_work(&film, &Work { tmdb: Some(130), ..Work::default() }));
        assert!(!same_work(&film, &Work::default()), "sans identifiant, aucune œuvre");
        let (_, song) = item_of(&ev(21, vec![vec!["title", "A — B"], vec!["x", &x], vec!["t", "music"], vec!["i", "youtube:dQw4w9WgXcQ"]])).unwrap();
        assert!(same_work(&song, &Work { youtube: Some("dQw4w9WgXcQ".into()), ..Work::default() }));
        let ep = |tags: Vec<Vec<&str>>| item_of(&ev(21, [vec![vec!["title", "S"], vec!["x", &x], vec!["series_name", "Le Bureau des Légendes"], vec!["season_number", "2"], vec!["episode_number", "5"]], tags].concat())).unwrap().1;
        let w = |tv: Option<i64>, name: Option<&str>, e: i64| Work { tv, series: name.map(str::to_owned), season: Some(2), episode: Some(e), ..Work::default() };
        assert!(same_work(&ep(vec![]), &w(None, Some("le bureau des legendes"), 5)), "sans identifiant : le nom de la série, accents et casse ignorés");
        assert!(!same_work(&ep(vec![]), &w(None, Some("le bureau des legendes"), 6)), "autre épisode");
        assert!(same_work(&ep(vec![vec!["i", "tmdb-tv:64439"]]), &w(Some(64439), Some("The Bureau"), 5)), "l'identifiant prime sur le nom");
        assert!(!same_work(&ep(vec![vec!["i", "tmdb-tv:64439"]]), &w(Some(1), Some("Le Bureau des Légendes"), 5)), "autre série homonyme");
        assert!(!same_work(&ep(vec![vec!["i", "tmdb:64439"]]), &Work { tmdb: Some(64439), ..Work::default() }), "un épisode n'est pas un film");
        // La copie déjà chez le membre d'abord, puis le blob le plus répandu.
        let items = vec![json!({"type": "film", "ids": {"tmdb": 1}, "sources": 1, "copied": null, "n": 1}), json!({"type": "film", "ids": {"tmdb": 1}, "sources": 3, "copied": null, "n": 2})];
        assert_eq!(best_copy(&items, &Work { tmdb: Some(1), ..Work::default() }).unwrap()["n"], 2);
        let items = [items, vec![json!({"type": "film", "ids": {"tmdb": 1}, "sources": 1, "copied": "/Vidéos/Films/x.mp4", "n": 3})]].concat();
        assert_eq!(best_copy(&items, &Work { tmdb: Some(1), ..Work::default() }).unwrap()["n"], 3);
    }

    #[test]
    fn a_track_and_an_episode_are_read_from_their_events() {
        let x = "ab".repeat(32);
        let (hash, item) = item_of(&ev(21, vec![vec!["title", "Artiste B — Titre Deux"], vec!["x", &x], vec!["t", "music"], vec!["artist", "Artiste B"], vec!["album", "Mon Mix"], vec!["track_number", "2"], vec!["duration", "245"], vec!["l", "Jazz", "genre"]])).unwrap();
        assert_eq!(hash, x);
        assert_eq!((item["type"].as_str(), item["music"]["track"].as_i64(), item["genres"][0].as_str(), item["duration_s"].as_i64()), (Some("music"), Some(2), Some("Jazz"), Some(245)));
        let (_, ep) = item_of(&ev(22, vec![vec!["title", "S"], vec!["x", &x], vec!["series_name", "Kaamelott"], vec!["season_number", "1"], vec!["episode_number", "3"]])).unwrap();
        assert_eq!((ep["type"].as_str(), ep["series"]["episode"].as_i64()), (Some("episode"), Some(3)));
        assert!(item_of(&ev(21, vec![vec!["title", "sans empreinte"]])).is_none());
        assert!(item_of(&ev(1, vec![vec!["title", "t"], vec!["x", &x]])).is_none(), "seuls les kinds 21/22");
    }
}
