//! **Paquet d'index** de chaque membre (lot 5) : ce que la recherche multimédia a calculé pour lui (texte lu, vecteurs des
//! photos et des images clés, états) est écrit dans **son** uCloud, sous [`PACK_DIR`]`/`[`PACK_NAME`]. C'est un fichier
//! comme un autre : chiffré, compté dans son espace, et emporté **tel quel** par la sauvegarde chez un ami
//! (`private_backup`) et par la restauration. Après une panne, la nouvelle station relit ce paquet au lieu de tout recalculer.
//!
//! - **écrire** : quand l'index du membre a changé (nombre et date des lignes faites), station au repos, au plus une fois toutes
//!   les [`REWRITE_SECS`] (le premier paquet tout de suite) ;
//! - **relire** : quand le paquet du uCloud n'est pas celui que la station a écrit ou déjà relu (`cid` différent) ; seules les
//!   lignes du **même modèle** que celui d'ici et des fichiers **présents** sont reprises ; s'il en manque (fichiers encore en
//!   route pendant une restauration), il est relu à chaque repos pendant [`PENDING_DAYS`] jours ;
//! - **jamais d'écrasement** : tant qu'un paquet revenu n'a pas été relu (ou pendant une restauration), il n'est pas réécrit.
//!
//! Le tiroir `/_` n'est ni analysé, ni montré dans la bibliothèque, ni publié. Tout le reste reste un **état local** : sans
//! paquet, les fichiers sont simplement relus.

use std::sync::Arc;

use astro_dav::{BlobStore, DavState};
use astro_db::{Db, VectorTable};
use base64::Engine as _;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::search_index::Job;

/// Dossier du paquet (tiroir système).
pub const PACK_DIR: &str = "/_/Recherche";
/// Nom du paquet.
pub const PACK_NAME: &str = "index.astroidx";
/// Taille maximale d'**une part** du paquet (écrite ou relue). Au-delà, le paquet est découpé : [`PACK_NAME`] (part 0, dont
/// l'en-tête donne le nombre de parts), puis `index-1.astroidx`, `index-2.astroidx`… (voir [`part_name`]).
pub const MAX_PACK: usize = 96 << 20;
/// Parts au plus (≈ 6 Gio) : au-delà, rien n'est écrit et le Capitaine en est prévenu.
pub const MAX_PARTS: usize = 64;
/// Écart minimal entre deux écritures du paquet : la sauvegarde chez l'ami ne part qu'une fois par nuit, inutile de réécrire
/// des dizaines de Mo toutes les 5 minutes pendant une longue analyse.
pub const REWRITE_SECS: i64 = 20 * 3600;
/// Jours pendant lesquels un paquet incomplet (fichiers pas encore revenus) est relu à chaque repos.
pub const PENDING_DAYS: i64 = 7;
/// Version du format.
const VERSION: u32 = 1;

/// Une ligne du paquet : un fichier (`sha`) pour une tâche.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Item {
    /// Empreinte du clair.
    pub s: String,
    /// Tâche (`ocr`, `vision`).
    pub t: String,
    /// État (`done`, `skipped`).
    pub st: String,
    /// Modèle.
    pub m: String,
    /// SHA-256 du modèle.
    pub h: String,
    /// Texte lu.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<String>,
    /// Images vues : `(instant en ms, vecteur f32 petit-boutiste en base64)`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub f: Vec<(i64, String)>,
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

/// Nom de la part `i` du paquet : la part 0 garde le nom historique (un paquet d'une seule part est lu par les versions
/// d'avant le découpage).
pub fn part_name(i: usize) -> String {
    if i == 0 { PACK_NAME.to_owned() } else { format!("index-{i}.astroidx") }
}

/// Une ligne de l'index à emporter : `(sha, tâche, état, modèle, empreinte du modèle)`.
type Row = (String, String, String, String, String);

/// Lignes de l'index de `owner` à mettre dans le paquet.
fn rows(c: &rusqlite::Connection, owner: &str) -> rusqlite::Result<Vec<Row>> {
    let mut items = c.prepare_cached("SELECT sha, task, state, COALESCE(model_id, ''), COALESCE(model_sha, '') FROM search_index WHERE owner_hex = ?1 AND state IN ('done', 'skipped') AND task IN ('ocr', 'vision') ORDER BY sha, task")?;
    items.query_map([owner], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?.collect()
}

/// En-tête d'une part : version, compte, date, rang de la part et nombre de parts.
fn header(owner: &str, at: i64, part: usize, parts: usize) -> Vec<u8> {
    json!({ "astroidx": VERSION, "owner": owner, "at": at, "part": part, "parts": parts }).to_string().into_bytes()
}

/// Lignes JSON des `rows[start..]` qui tiennent dans `max` octets (en-tête compris, réservé). Rend `(lignes, suivant,
/// écartées)` : une ligne qui ne tiendrait pas seule dans une part est écartée (et comptée), jamais tronquée.
fn export_part(c: &rusqlite::Connection, owner: &str, rows: &[Row], start: usize, max: usize) -> rusqlite::Result<(Vec<u8>, usize, usize)> {
    const HEAD_ROOM: usize = 512;
    let b64 = base64::engine::general_purpose::STANDARD;
    let mut text = c.prepare_cached("SELECT body FROM search_text WHERE owner_hex = ?1 AND sha = ?2 AND src = ?3 LIMIT 1")?;
    let mut frames = c.prepare_cached("SELECT f.t_ms, v.embedding FROM search_frames f JOIN search_vision_vectors v ON v.vis_id = f.vis_id WHERE f.owner_hex = ?1 AND f.sha = ?2 ORDER BY f.t_ms")?;
    let (mut out, mut i, mut skipped) = (Vec::new(), start, 0);
    let room = max.saturating_sub(HEAD_ROOM);
    while let Some((s, t, st, m, h)) = rows.get(i).cloned() {
        let x: Option<String> = if t == "ocr" { text.query_row(params![owner, s, t], |r| r.get(0)).optional()? } else { None };
        let f: Vec<(i64, String)> = if t == "vision" { frames.query_map(params![owner, s], |r| Ok((r.get::<_, i64>(0)?, b64.encode(r.get::<_, Vec<u8>>(1)?))))?.collect::<Result<_, _>>()? } else { Vec::new() };
        let line = serde_json::to_vec(&Item { s, t, st, m, h, x, f }).unwrap_or_default();
        if line.len() + 2 > room {
            skipped += 1;
            i += 1;
            continue;
        }
        if out.len() + line.len() + 2 > room {
            break; // part pleine : la ligne ouvrira la suivante
        }
        out.push(b'\n');
        out.extend(line);
        i += 1;
    }
    out.push(b'\n');
    Ok((out, i, skipped))
}

/// Le paquet de `owner` en une seule part (une ligne d'en-tête JSON, puis une ligne JSON par [`Item`]). `None` : rien de
/// calculé, ou trop gros pour une part (voir [`sync`], qui découpe).
pub fn export(c: &rusqlite::Connection, owner: &str) -> rusqlite::Result<Option<Vec<u8>>> {
    let rows = rows(c, owner)?;
    if rows.is_empty() {
        return Ok(None);
    }
    let (body, next, skipped) = export_part(c, owner, &rows, 0, MAX_PACK)?;
    if next < rows.len() || skipped > 0 {
        return Ok(None);
    }
    let mut out = header(owner, now(), 0, 1);
    out.extend(body);
    Ok(Some(out))
}

/// Nombre de parts annoncé par l'en-tête d'une part 0 (1 pour un paquet d'avant le découpage, qui n'en dit rien).
pub fn parts_of(bytes: &[u8]) -> usize {
    let head = bytes.split(|b| *b == b'\n').next().unwrap_or_default();
    serde_json::from_slice::<serde_json::Value>(head).ok().and_then(|h| h["parts"].as_u64()).map_or(1, |n| usize::try_from(n).unwrap_or(1).clamp(1, MAX_PARTS))
}

/// Lit un paquet ; refuse une autre version ou le paquet d'un autre compte.
pub fn parse(bytes: &[u8], owner: &str) -> Result<Vec<Item>, String> {
    if bytes.len() > MAX_PACK {
        return Err("paquet trop gros".into());
    }
    let mut lines = bytes.split(|b| *b == b'\n').filter(|l| !l.is_empty());
    let head: serde_json::Value = serde_json::from_slice(lines.next().ok_or("paquet vide")?).map_err(|e| format!("en-tête illisible : {e}"))?;
    if head["astroidx"] != VERSION || head["owner"] != owner {
        return Err("paquet d'une autre version ou d'un autre compte".into());
    }
    lines.map(|l| serde_json::from_slice::<Item>(l).map_err(|e| format!("ligne illisible : {e}"))).collect()
}

/// Reprend les lignes utiles de `items` pour `owner` : même modèle qu'ici (`jobs`), fichier présent, pas déjà fait avec ce
/// modèle. Rend `(reprises, en attente de leur fichier)`.
pub async fn import(db: &Db, owner: &str, items: Vec<Item>, jobs: &[Job]) -> Result<(usize, usize), astro_db::DbError> {
    let models: Vec<(&'static str, String, String)> = jobs.iter().map(|j| (j.db, j.model.id.clone(), j.model.sha256.clone())).collect();
    let owner = owner.to_owned();
    db.writer()
        .transaction(move |tx| {
            let b64 = base64::engine::general_purpose::STANDARD;
            let (mut taken, mut missing) = (0, 0);
            for it in items {
                let Some((task, id, _)) = models.iter().find(|(t, _, h)| *t == it.t && *h == it.h) else { continue };
                if !matches!(it.st.as_str(), "done" | "skipped") {
                    continue;
                }
                let present: bool = tx.query_row("SELECT EXISTS (SELECT 1 FROM ucloud_entries WHERE owner_hex = ?1 AND sha_plain = ?2 AND type = 'file')", params![owner, it.s], |r| r.get(0))?;
                if !present {
                    missing += 1;
                    continue;
                }
                let done: bool = tx.query_row("SELECT EXISTS (SELECT 1 FROM search_index WHERE owner_hex = ?1 AND sha = ?2 AND task = ?3 AND state IN ('done', 'skipped') AND model_sha = ?4)", params![owner, it.s, task, it.h], |r| r.get(0))?;
                if done {
                    continue;
                }
                let blobs: Vec<(i64, Vec<u8>)> = it.f.iter().filter_map(|(t, v)| b64.decode(v).ok().filter(|b| b.len() == VectorTable::SearchVision.dim() * 4).map(|b| (*t, b))).collect();
                if blobs.len() != it.f.len() {
                    continue; // vecteur abîmé : le fichier sera simplement relu
                }
                tx.execute("DELETE FROM search_text WHERE owner_hex = ?1 AND sha = ?2 AND src = ?3", params![owner, it.s, task])?;
                tx.execute("DELETE FROM search_vision_vectors WHERE vis_id IN (SELECT vis_id FROM search_frames WHERE owner_hex = ?1 AND sha = ?2)", params![owner, it.s])?;
                tx.execute("DELETE FROM search_frames WHERE owner_hex = ?1 AND sha = ?2", params![owner, it.s])?;
                for (t_ms, blob) in &blobs {
                    tx.execute("INSERT INTO search_frames(owner_hex, sha, t_ms) VALUES (?1, ?2, ?3)", params![owner, it.s, t_ms])?;
                    tx.execute("INSERT INTO search_vision_vectors(vis_id, owner_hex, embedding) VALUES (?1, ?2, ?3)", params![tx.last_insert_rowid(), owner, blob])?;
                }
                if let Some(x) = it.x.as_ref().filter(|x| !x.trim().is_empty()) {
                    tx.execute("INSERT INTO search_text(body, owner_hex, sha, src, t_ms) VALUES (?1, ?2, ?3, ?4, 0)", params![x, owner, it.s, task])?;
                }
                tx.execute(
                    "INSERT INTO search_index(owner_hex, sha, task, state, model_id, model_sha, via, tries, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, 1, ?7) \
                     ON CONFLICT(owner_hex, sha, task) DO UPDATE SET state = excluded.state, model_id = excluded.model_id, model_sha = excluded.model_sha, via = NULL, tries = 1, updated_at = excluded.updated_at",
                    params![owner, it.s, task, it.st, id, it.h, now()],
                )?;
                taken += 1;
            }
            Ok((taken, missing))
        })
        .await
}

/// Ce que la station sait du paquet d'un membre (`kv` scope `search_pack`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct Seen {
    /// CID du paquet écrit ou relu par cette station.
    cid: String,
    /// Empreinte de l'index au moment de l'écriture : `nombre:dernière date`.
    stamp: String,
    /// Relu mais incomplet (fichiers pas encore là) depuis cette date.
    #[serde(default)]
    pending_since: Option<i64>,
    /// Date de la dernière écriture par cette station.
    #[serde(default)]
    written_at: i64,
}

/// Un tour de paquets, station au repos : relit les paquets revenus, puis écrit ceux qui ont changé. Rend `(relus, écrits)`.
///
/// Un paquet qui n'a pas pu être écrit (trop de parts, écriture refusée) ou dont des lignes ont été écartées n'est jamais tu :
/// les autres membres sont traités, puis l'erreur est rendue (l'appelant la dit au Capitaine sur le bus d'activité).
pub async fn sync<B: BlobStore + 'static>(dav: &Arc<DavState<B>>, jobs: &[Job]) -> Result<(usize, usize), astro_db::DbError> {
    sync_with(dav, jobs, MAX_PACK).await
}

/// [`sync`] avec une taille de part donnée (les tests découpent de petits paquets).
pub async fn sync_with<B: BlobStore + 'static>(dav: &Arc<DavState<B>>, jobs: &[Job], max_part: usize) -> Result<(usize, usize), astro_db::DbError> {
    let mut problems: Vec<String> = Vec::new();
    let path = format!("{PACK_DIR}/{PACK_NAME}");
    let p = path.clone();
    let owners: Vec<(String, Option<String>, Option<String>, String)> = dav
        .db
        .readers()
        .read(move |c| {
            let mut stmt = c.prepare_cached(
                "SELECT i.hex, (SELECT e.cid FROM ucloud_entries e WHERE e.owner_hex = i.hex AND e.path = ?1 AND e.type = 'file'), \
                 (SELECT value FROM kv WHERE scope = 'search_pack' AND key = i.hex), \
                 (SELECT count(*) || ':' || COALESCE(max(updated_at), 0) FROM search_index s WHERE s.owner_hex = i.hex AND s.state IN ('done', 'skipped')) \
                 FROM identities i WHERE i.hosted = 1",
            )?;
            Ok(stmt.query_map([&p], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<Result<Vec<_>, _>>()?)
        })
        .await?;
    let (mut read, mut written) = (0, 0);
    for (owner, cid, seen, stamp) in owners {
        let mut seen: Seen = seen.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        let at = now();
        // 1. Un paquet revenu (restauration, autre station) ou encore incomplet : le relire d'abord.
        let fresh = cid.as_ref().is_some_and(|c| *c != seen.cid);
        let retry = seen.pending_since.is_some_and(|s| at - s < PENDING_DAYS * 86_400);
        if let (Some(c), true) = (&cid, fresh || retry) {
            let Some(bytes) = astro_dav::carddav::read_file(dav, &owner, &path).await else { continue }; // blob pas encore là : plus tard
            // Paquet découpé : toutes ses parts, ou rien (une part encore en route : plus tard).
            let mut whole = Some(bytes.clone());
            for i in 1..parts_of(&bytes) {
                match (&mut whole, astro_dav::carddav::read_file(dav, &owner, &format!("{PACK_DIR}/{}", part_name(i))).await) {
                    (Some(acc), Some(more)) => match parse(&more, &owner) {
                        Ok(_) => {
                            // Les lignes de la part (sans son en-tête) rejoignent celles de la part 0.
                            let body = more.iter().position(|b| *b == b'\n').map_or(&[][..], |n| &more[n..]);
                            acc.extend_from_slice(body);
                        }
                        Err(_) => whole = None,
                    },
                    _ => whole = None,
                }
            }
            let Some(bytes) = whole else { continue };
            let missing = match parse_parts(&bytes, &owner) {
                Ok(items) => import(&dav.db, &owner, items, jobs).await.map(|(n, missing)| {
                    read += usize::from(n > 0);
                    missing
                })?,
                Err(_) => 0, // illisible : il sera remplacé par le prochain paquet écrit
            };
            seen = Seen { cid: c.clone(), stamp: String::new(), pending_since: (missing > 0).then(|| seen.pending_since.filter(|_| !fresh).unwrap_or(at)), written_at: seen.written_at };
            save(&dav.db, &owner, &seen).await?;
            if seen.pending_since.is_some() {
                continue; // jamais d'écrasement d'un paquet pas encore entièrement repris
            }
        }
        // 2. L'index a changé : écrire le paquet (pas pendant une restauration).
        let recent = cid.is_some() && at - seen.written_at < REWRITE_SECS;
        if stamp == seen.stamp || stamp.starts_with("0:") || recent || astro_dav::manifest::restoring(&dav.db, &owner).await {
            continue;
        }
        match write_parts(dav, &owner, max_part, at).await {
            Ok(Some(skipped)) if skipped > 0 => problems.push(format!("{} : {skipped} ligne(s) trop grosse(s) écartée(s) du paquet", short(&owner))),
            Ok(Some(_)) => {}
            Ok(None) => continue,
            Err(why) => {
                problems.push(format!("{} : {why}", short(&owner)));
                continue;
            }
        }
        {
            let (o, p) = (owner.clone(), path.clone());
            let cid: Option<String> = dav.db.readers().read(move |c| Ok(c.query_row("SELECT cid FROM ucloud_entries WHERE owner_hex = ?1 AND path = ?2", params![o, p], |r| r.get(0)).optional()?.flatten())).await?;
            seen = Seen { cid: cid.unwrap_or_default(), stamp, pending_since: None, written_at: at };
            save(&dav.db, &owner, &seen).await?;
            written += 1;
        }
    }
    if !problems.is_empty() {
        return Err(astro_db::DbError::Io(std::io::Error::other(format!("paquet d'index : {}", problems.join(" ; ")))));
    }
    Ok((read, written))
}

fn short(owner: &str) -> &str {
    owner.get(..8).unwrap_or(owner)
}

/// Lit un paquet réassemblé (part 0 + lignes des parts suivantes).
fn parse_parts(bytes: &[u8], owner: &str) -> Result<Vec<Item>, String> {
    let mut lines = bytes.split(|b| *b == b'\n').filter(|l| !l.is_empty());
    let head: serde_json::Value = serde_json::from_slice(lines.next().ok_or("paquet vide")?).map_err(|e| format!("en-tête illisible : {e}"))?;
    if head["astroidx"] != VERSION || head["owner"] != owner {
        return Err("paquet d'une autre version ou d'un autre compte".into());
    }
    lines.map(|l| serde_json::from_slice::<Item>(l).map_err(|e| format!("ligne illisible : {e}"))).collect()
}

/// Écrit le paquet de `owner` en parts de `max_part` octets au plus : les parts 1… d'abord, la part 0 (qui annonce leur
/// nombre) en dernier, puis retire les parts d'un paquet précédent plus long. `None` : rien de calculé. `Some(n)` : écrit,
/// `n` lignes trop grosses écartées.
async fn write_parts<B: BlobStore + 'static>(dav: &Arc<DavState<B>>, owner: &str, max_part: usize, at: i64) -> Result<Option<usize>, String> {
    let o = owner.to_owned();
    let all = Arc::new(dav.db.readers().read(move |c| Ok(rows(c, &o)?)).await.map_err(|e| e.to_string())?);
    if all.is_empty() {
        return Ok(None);
    }
    let (mut first, mut start, mut part, mut skipped) = (Vec::new(), 0, 0, 0);
    loop {
        let (o, rows) = (owner.to_owned(), all.clone());
        let (body, next, gone) = dav.db.readers().read(move |c| Ok(export_part(c, &o, &rows, start, max_part)?)).await.map_err(|e| e.to_string())?;
        skipped += gone;
        if part == 0 {
            first = body;
        } else {
            let mut bytes = header(owner, at, part, 0);
            bytes.extend(body);
            astro_dav::ingest::replace(dav, owner, PACK_DIR, &part_name(part), Some("application/octet-stream"), &bytes[..]).await.map_err(|e| format!("écriture de {} : {e}", part_name(part)))?;
        }
        part += 1;
        if next >= all.len() {
            break;
        }
        if part >= MAX_PARTS {
            return Err(format!("plus de {MAX_PARTS} parts de {} Mio : paquet non écrit", max_part >> 20));
        }
        start = next;
    }
    let mut bytes = header(owner, at, 0, part);
    bytes.extend(first);
    astro_dav::ingest::replace(dav, owner, PACK_DIR, PACK_NAME, Some("application/octet-stream"), &bytes[..]).await.map_err(|e| format!("écriture de {PACK_NAME} : {e}"))?;
    // Parts d'un paquet précédent plus long : retirées (elles ne seraient plus lues, mais occuperaient la place du membre).
    for i in part..MAX_PARTS {
        let (o, p) = (owner.to_owned(), format!("{PACK_DIR}/{}", part_name(i)));
        match astro_dav::vfs::vfs_tx(&dav.db, move |tx| astro_dav::vfs::delete_tree_tx(tx, &o, &p)).await {
            Ok(cids) => astro_dav::server::release_blobs(dav, cids).await,
            Err(astro_dav::vfs::VfsError::NotFound) => break,
            Err(e) => return Err(format!("retrait de {} : {e}", part_name(i))),
        }
    }
    Ok(Some(skipped))
}

async fn save(db: &Db, owner: &str, seen: &Seen) -> Result<(), astro_db::DbError> {
    let value = serde_json::to_string(seen).unwrap_or_default();
    db.writer()
        .execute(
            "INSERT INTO kv(scope, key, value, updated_at) VALUES ('search_pack', ?1, ?2, ?3) ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            vec![owner.to_owned().into(), value.into(), now().into()],
        )
        .await
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_big_pack_is_cut_into_parts_that_read_back_whole() {
        let dir = std::env::temp_dir().join(format!("astroportd-pack-parts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        drop(Db::open(&dir.join("u.db"), 1).unwrap());
        let c = rusqlite::Connection::open(dir.join("u.db")).unwrap();
        for n in 0..40 {
            let sha = format!("{n:064}");
            c.execute("INSERT INTO search_index(owner_hex, sha, task, state, model_id, model_sha, tries, updated_at) VALUES ('alice', ?1, 'ocr', 'done', 'm', 'h', 1, 1)", [&sha]).unwrap();
            c.execute("INSERT INTO search_text(body, owner_hex, sha, src, t_ms) VALUES (?1, 'alice', ?2, 'ocr', 0)", params![format!("FACTURE {n} ").repeat(20), sha]).unwrap();
        }
        // Une ligne énorme : elle ne tient dans aucune part, elle est écartée et comptée (jamais tronquée en silence).
        c.execute("INSERT INTO search_index(owner_hex, sha, task, state, model_id, model_sha, tries, updated_at) VALUES ('alice', 'zz', 'ocr', 'done', 'm', 'h', 1, 1)", []).unwrap();
        c.execute("INSERT INTO search_text(body, owner_hex, sha, src, t_ms) VALUES (?1, 'alice', 'zz', 'ocr', 0)", ["x".repeat(10_000)]).unwrap();
        let all = rows(&c, "alice").unwrap();
        assert!(export(&c, "alice").unwrap().is_some(), "une seule part tient dans MAX_PACK");
        let max = 4096;
        let (mut start, mut parts, mut skipped, mut whole) = (0, 0, 0, Vec::new());
        while start < all.len() {
            let (body, next, gone) = export_part(&c, "alice", &all, start, max).unwrap();
            let mut bytes = header("alice", 1, parts, 0);
            bytes.extend(&body);
            assert!(bytes.len() <= max, "part {parts} : {} octets", bytes.len());
            assert!(!parse(&bytes, "alice").unwrap().is_empty() || gone > 0);
            if parts == 0 {
                whole = header("alice", 1, 0, 9);
            }
            whole.extend(body);
            (start, skipped, parts) = (next, skipped + gone, parts + 1);
        }
        assert!(parts > 3, "{parts} parts");
        assert_eq!(skipped, 1);
        assert_eq!(parse_parts(&whole, "alice").unwrap().len(), 40, "toutes les lignes reviennent, dans l'ordre des parts");
        assert_eq!(parts_of(&whole), 9);
        assert_eq!(parts_of(b"{\"astroidx\":1,\"owner\":\"alice\",\"at\":1}\n"), 1, "un paquet d'avant le découpage est d'une part");
        assert_eq!((part_name(0).as_str(), part_name(2).as_str()), (PACK_NAME, "index-2.astroidx"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_pack_belongs_to_one_account_and_one_version() {
        let head = |v: u32, o: &str| format!("{}\n{}\n", json!({ "astroidx": v, "owner": o, "at": 1 }), serde_json::to_string(&Item { s: "a".into(), t: "ocr".into(), st: "done".into(), m: "m".into(), h: "h".into(), x: Some("FACTURE".into()), f: vec![] }).unwrap());
        assert_eq!(parse(head(1, "alice").as_bytes(), "alice").unwrap()[0].x.as_deref(), Some("FACTURE"));
        assert!(parse(head(1, "alice").as_bytes(), "bob").is_err(), "le paquet d'un autre compte");
        assert!(parse(head(2, "alice").as_bytes(), "alice").is_err(), "une autre version");
        assert!(parse(b"", "alice").is_err() && parse(b"{}\n", "alice").is_err());
    }
}
