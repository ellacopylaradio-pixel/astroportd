//! Transport de la chaîne de partage N¹ (Lot 1b), liste officielle filtrée par la toile de confiance (Lot 2),
//! et publication vers le commun / remix « recette » (Lot 4, dont la base commune botanique).
//!
//! Conception : `docs/design/PARTAGE_CASCADE.md`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use astro_crypto::KeySource;
use astro_dav::adopt::SharedFile;
use astro_dav::{BlobStore, DavState};
use astro_db::Db;
use astro_db::events::row_to_event;
use astro_economy::cascade::{
    BPS_TOTAL, Bps, CommonsBlock, CompositionTrack, Credit, PlantData, PricingMode, RemixKind, Rights, ShareChain, SourceRef, SourceRole, SplitRule, flatten_dag,
};
use astro_economy::wot::{Cert, Params, Wot};
use astro_nostr_proto::{Event, UnsignedEvent, is_hex64, kind};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::register::PublishFn;

/// Portée `kv` des chaînes de partage N¹ détenues localement par chaque compte (état local).
pub const SHARE_CHAIN_SCOPE: &str = "share_chain";
/// Portée `kv` reliant le SHA-256 brut d'une fiche `.media.json` recette à son `commons.content_sha`.
pub const FICHE_WORK_SCOPE: &str = "fiche_work_sha";
/// Étiquette NIP-59 portant le JSON du `ShareChain` dans un message fichier (`kind::FILE`).
pub const SHARE_CHAIN_TAG: &str = "share-chain";
/// Taille maximale acceptée pour un `ShareChain` sérialisé (anti-DoS).
pub const MAX_CHAIN_JSON: usize = 32 * 1024;
/// Préfixe du tag `d` des événements d'inscription à la liste officielle (`kind 30005`).
pub const OFFICIAL_PREFIX: &str = "chaine:";
/// Étiquette `t` des inscriptions à la liste officielle.
pub const OFFICIAL_TAG: &str = "astro-chain";

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

fn chain_key(owner: &str, work_sha: &str) -> String {
    format!("{owner}:{work_sha}")
}

// ──────────────────────── Lot 1b : stockage, validation et extension du ShareChain ────────────────────────

/// Enregistre localement un `ShareChain` vérifié pour `(owner, chain.work_sha)`.
/// Ne remplace jamais une lignée déjà connue (la première lignée reçue fait foi).
pub async fn save_chain(db: &Db, owner: &str, chain: &ShareChain) -> bool {
    if !is_hex64(owner) || chain.verify().is_err() {
        return false;
    }
    let Ok(raw) = serde_json::to_string(chain) else { return false };
    if raw.len() > MAX_CHAIN_JSON {
        return false;
    }
    let key = chain_key(owner, &chain.work_sha);
    let ts = now();
    db.writer()
        .execute(
            "INSERT OR IGNORE INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4)",
            vec![SHARE_CHAIN_SCOPE.to_owned().into(), key.into(), raw.into(), ts.into()],
        )
        .await
        .is_ok_and(|r| r.changes > 0)
}

/// Relit et revérifie la lignée `ShareChain` de `(owner, work_sha)` stockée localement.
pub async fn load_chain(db: &Db, owner: &str, work_sha: &str) -> Option<ShareChain> {
    if !is_hex64(owner) || !is_hex64(work_sha) {
        return None;
    }
    let key = chain_key(owner, work_sha);
    let raw: String = db
        .readers()
        .read(move |c| Ok(c.query_row("SELECT value FROM kv WHERE scope = ?1 AND key = ?2", params![SHARE_CHAIN_SCOPE, key], |r| r.get(0)).optional()?))
        .await
        .ok()??;
    if raw.len() > MAX_CHAIN_JSON {
        return None;
    }
    let chain: ShareChain = serde_json::from_str(&raw).ok()?;
    if chain.work_sha != work_sha || chain.verify().is_err() {
        return None;
    }
    Some(chain)
}

/// Enregistre la correspondance `sha_plain du fichier .media.json -> commons.content_sha` pour les fiches recette.
pub async fn record_fiche_work_sha(db: &Db, file_sha: &str, work_sha: &str) {
    if !is_hex64(file_sha) || !is_hex64(work_sha) || file_sha == work_sha {
        return;
    }
    let _ = db
        .writer()
        .execute(
            "INSERT OR REPLACE INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4)",
            vec![FICHE_WORK_SCOPE.to_owned().into(), file_sha.to_owned().into(), work_sha.to_owned().into(), now().into()],
        )
        .await;
}

/// Retrouve le `commons.content_sha` associé à l'empreinte brute d'un fichier `.media.json` recette.
pub async fn lookup_work_sha(db: &Db, file_sha: &str) -> Option<String> {
    if !is_hex64(file_sha) {
        return None;
    }
    let k = file_sha.to_owned();
    db.readers()
        .read(move |c| Ok(c.query_row("SELECT value FROM kv WHERE scope = ?1 AND key = ?2", params![FICHE_WORK_SCOPE, k], |r| r.get::<_, String>(0)).optional()?))
        .await
        .ok()
        .flatten()
        .filter(|s| is_hex64(s))
}

/// Retrouve le `sha_plain` du fichier `.media.json` correspondant à un `commons.content_sha` de recette.
pub async fn lookup_fiche_sha_by_work(db: &Db, work_sha: &str) -> Option<String> {
    if !is_hex64(work_sha) {
        return None;
    }
    let v = work_sha.to_owned();
    db.readers()
        .read(move |c| Ok(c.query_row("SELECT key FROM kv WHERE scope = ?1 AND value = ?2 ORDER BY updated_at DESC LIMIT 1", params![FICHE_WORK_SCOPE, v], |r| r.get::<_, String>(0)).optional()?))
        .await
        .ok()
        .flatten()
        .filter(|s| is_hex64(s))
}

/// Vérifie un `ShareChain` reçu dans une enveloppe NIP-59 : taille bornée, signatures valides, même empreinte que le
/// fichier reçu, et dernier saut signé par `sender` pour `recipient`.
pub fn verify_incoming_chain(raw_json: &str, expected_sha: &str, sender: &str, recipient: &str) -> Option<ShareChain> {
    verify_incoming_chain_with_work(raw_json, expected_sha, None, sender, recipient)
}

/// Variante de [`verify_incoming_chain`] qui accepte aussi l'empreinte d'œuvre `work_sha_hint` d'une fiche recette.
pub fn verify_incoming_chain_with_work(raw_json: &str, file_sha: &str, work_sha_hint: Option<&str>, sender: &str, recipient: &str) -> Option<ShareChain> {
    if raw_json.len() > MAX_CHAIN_JSON || !is_hex64(file_sha) || !is_hex64(sender) || !is_hex64(recipient) {
        return None;
    }
    let chain: ShareChain = serde_json::from_str(raw_json).ok()?;
    let matches_work = chain.work_sha == file_sha || work_sha_hint.is_some_and(|w| is_hex64(w) && chain.work_sha == w);
    if !matches_work || chain.verify().is_err() {
        return None;
    }
    let last = chain.hops.last()?;
    if last.from_pubkey != sender || last.to_pubkey != recipient {
        return None;
    }
    Some(chain)
}

/// Construit la chaîne signée à joindre à l'envoi de `file` par `owner` vers `friend`.
/// Résout automatiquement `commons.content_sha` lorsque `file` est une fiche `.media.json` recette.
pub async fn extend_for_send(
    db: &Db,
    owner: &str,
    owner_secret: &[u8; 32],
    friend: &str,
    file: &SharedFile,
    fiche_cid: Option<String>,
    at: i64,
) -> Option<ShareChain> {
    if !is_hex64(&file.sha_plain) || !is_hex64(owner) || !is_hex64(friend) || owner == friend {
        return None;
    }
    let work_sha = if file.name.ends_with(".media.json") {
        lookup_work_sha(db, &file.sha_plain).await.unwrap_or_else(|| file.sha_plain.clone())
    } else {
        file.sha_plain.clone()
    };
    let base = match load_chain(db, owner, &work_sha).await {
        Some(existing) if existing.hops.last().map_or(existing.origin_pubkey.as_str(), |h| h.to_pubkey.as_str()) == owner => existing,
        _ => ShareChain::new_root(&work_sha, &file.cid, fiche_cid, owner).ok()?,
    };
    base.extend(owner_secret, friend, at).ok()
}

/// Cherche le CID de la fiche `.media.json` voisine d'un fichier `path` (si elle existe).
pub async fn sibling_fiche_cid(db: &Db, owner: &str, path: &str) -> Option<String> {
    if path.ends_with(".media.json") {
        let (o, p) = (owner.to_owned(), path.to_owned());
        return db
            .readers()
            .read(move |c| Ok(c.query_row("SELECT cid FROM ucloud_entries WHERE owner_hex = ?1 AND path = ?2 AND type = 'file'", params![o, p], |r| r.get(0)).optional()?.flatten()))
            .await
            .ok()?;
    }
    let parent = astro_dav::vfs::parent_of(path);
    let dir = parent.trim_end_matches('/');
    let name = astro_dav::vfs::name_of(path);
    let stem = name.rsplit_once('.').map_or(name, |(s, _)| s);
    let fiche_path = format!("{dir}/{stem}.media.json");
    let o = owner.to_owned();
    db.readers()
        .read(move |c| Ok(c.query_row("SELECT cid FROM ucloud_entries WHERE owner_hex = ?1 AND path = ?2 AND type = 'file'", params![o, fiche_path], |r| r.get(0)).optional()?.flatten()))
        .await
        .ok()?
}

// ──────────────────────── Lot 2 : liste officielle filtrée par la toile de confiance ────────────────────────

/// Contenu JSON d'un événement `kind 30005` (`d = chaine:<work_sha>`) d'inscription à la liste officielle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OfficialPayload {
    /// Version (`1`).
    pub v: u32,
    /// Empreinte SHA-256 de l'œuvre.
    pub work_sha: String,
    /// Rang prouvé (`0` = source, `>= 1` = profondeur du `ShareChain` joint).
    pub rank: u32,
    /// Preuve de chaîne complète lorsque `rank > 0`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chain: Option<ShareChain>,
}

/// Inscrit `owner` sur la liste officielle (`kind 30005`, `d = chaine:<work_sha>`) avec la preuve cryptographique de son rang.
pub async fn register_official(
    db: &Db,
    keys: &Arc<dyn KeySource>,
    publish: &PublishFn,
    owner: &str,
    work_sha: &str,
) -> Result<Value, String> {
    if !is_hex64(owner) || !is_hex64(work_sha) {
        return Err("identifiant ou empreinte invalide".into());
    }
    let secret = keys.secret(owner).ok_or("clé du compte indisponible")?;
    let chain = load_chain(db, owner, work_sha).await;
    let rank = chain.as_ref().map_or(0, ShareChain::depth);
    let payload = OfficialPayload { v: 1, work_sha: work_sha.to_owned(), rank, chain };
    let content = serde_json::to_string(&payload).map_err(|e| e.to_string())?;
    let d_tag = format!("{OFFICIAL_PREFIX}{work_sha}");
    let tags = vec![
        vec!["d".to_owned(), d_tag],
        vec!["x".to_owned(), work_sha.to_owned()],
        vec!["t".to_owned(), OFFICIAL_TAG.to_owned()],
        vec!["rank".to_owned(), rank.to_string()],
    ];
    let ev = UnsignedEvent { created_at: u64::try_from(now()).unwrap_or(0), kind: kind::VIDEO_SET, tags, content }
        .sign(&secret)
        .map_err(|e| e.to_string())?;
    publish(ev.clone()).await?;
    Ok(json!({ "registered": true, "event_id": ev.id, "work_sha": work_sha, "rank": rank }))
}

/// Retire l'inscription de `owner` de la liste officielle (suppression NIP-09, `kind 5`).
pub async fn unregister_official(
    keys: &Arc<dyn KeySource>,
    publish: &PublishFn,
    owner: &str,
    work_sha: &str,
) -> Result<Value, String> {
    if !is_hex64(owner) || !is_hex64(work_sha) {
        return Err("identifiant ou empreinte invalide".into());
    }
    let secret = keys.secret(owner).ok_or("clé du compte indisponible")?;
    let address = format!("{}:{owner}:{OFFICIAL_PREFIX}{work_sha}", kind::VIDEO_SET);
    let tags = vec![vec!["a".to_owned(), address], vec!["k".to_owned(), kind::VIDEO_SET.to_string()]];
    let ev = UnsignedEvent { created_at: u64::try_from(now()).unwrap_or(0), kind: kind::DELETION, tags, content: String::new() }
        .sign(&secret)
        .map_err(|e| e.to_string())?;
    publish(ev.clone()).await?;
    Ok(json!({ "registered": false, "event_id": ev.id, "work_sha": work_sha }))
}

/// Calcule l'ensemble des clés membres de la toile de confiance (`Wot::compute` depuis les Capitaines + rencontres prouvées).
/// Utilisé pour filtrer à la fois la liste officielle (Lot 2) et les relais éligibles à la tranche Diffusion (anti-Sybil).
pub async fn wot_members(db: &Db, at: i64) -> Result<BTreeSet<String>, String> {
    let (certs, roots): (Vec<Cert>, BTreeSet<String>) = db
        .readers()
        .read(|c| {
            let mut st = c.prepare("SELECT owner_hex, target_hex, at FROM meetings WHERE verified = 1 LIMIT 200000")?;
            let certs = st.query_map([], |r| Ok(Cert { from: r.get(0)?, to: r.get(1)?, at: u64::try_from(r.get::<_, i64>(2)?).unwrap_or(0) }))?.collect::<Result<Vec<_>, _>>()?;
            let mut st = c.prepare("SELECT hex FROM identities WHERE role = 'captain'")?;
            let roots = st.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<BTreeSet<_>, _>>()?;
            Ok((certs, roots))
        })
        .await
        .map_err(|e| e.to_string())?;
    let now_u64 = u64::try_from(at).unwrap_or(0);
    let wot = tokio::task::spawn_blocking(move || Wot::compute(&certs, &roots, now_u64, Params::G1))
        .await
        .map_err(|e| e.to_string())?;
    Ok(wot.members)
}

/// Dernier paiement en cascade effectué par `actor` pour `work_sha` (journal `zen_credit`), s'il existe.
pub async fn paid_by_me(db: &Db, actor: &str, work_sha: &str) -> Option<Value> {
    if !is_hex64(actor) || !is_hex64(work_sha) {
        return None;
    }
    let (a, w) = (actor.to_owned(), work_sha.to_owned());
    let row: Option<(i64, i64)> = db
        .readers()
        .read(move |c| {
            Ok(c.query_row(
                "SELECT CAST(COALESCE(json_extract(data, '$.g1_cents'), 0) AS INTEGER), at FROM journal \
                 WHERE stream = 'zen_credit' AND json_extract(data, '$.kind') = 'cascade' \
                 AND json_extract(data, '$.actor') = ?1 AND json_extract(data, '$.work_sha') = ?2 \
                 ORDER BY at DESC LIMIT 1",
                params![a, w],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
        })
        .await
        .ok()?;
    row.map(|(deci_zen, at)| json!({ "amount_deci_zen": deci_zen, "at": at }))
}

/// Résout un libellé lisible (nom Nostr `kind 0`, préfixe email, sinon 8 premiers hex) pour chaque clé publique.
pub async fn resolve_labels(db: &Db, pubkeys: &[String]) -> BTreeMap<String, String> {
    let unique: BTreeSet<String> = pubkeys.iter().filter(|p| is_hex64(p)).cloned().collect();
    if unique.is_empty() {
        return BTreeMap::new();
    }
    let list: Vec<String> = unique.into_iter().collect();
    db.readers()
        .read(move |c| {
            let mut out = BTreeMap::new();
            let mut st = c.prepare_cached(
                "SELECT COALESCE(\
                    (SELECT json_extract(k.content, '$.name') FROM nostr_events k WHERE k.pubkey = ?1 AND k.kind = 0 ORDER BY k.created_at DESC LIMIT 1),\
                    (SELECT substr(i.email, 1, instr(i.email, '@') - 1) FROM identities i WHERE i.hex = ?1),\
                    substr(?1, 1, 8))",
            )?;
            for pk in list {
                let name: String = st.query_row([&pk], |r| r.get(0)).unwrap_or_else(|_| pk[..8].to_owned());
                let clean = name.trim();
                out.insert(pk.clone(), if clean.is_empty() { pk[..8].to_owned() } else { clean.to_owned() });
            }
            Ok(out)
        })
        .await
        .unwrap_or_default()
}

/// Détecte automatiquement si une photo possède à côté d'elle une fiche `.media.json` commune remixable (ex. Fiche A d'Alice).
pub async fn find_parent_fiche_for_photo<B: BlobStore>(dav: &Arc<DavState<B>>, owner: &str, photo_path: &str) -> Option<Value> {
    let photo_path = astro_dav::vfs::normalize_path(photo_path).ok()?;
    let parent = astro_dav::vfs::parent_of(&photo_path);
    let dir = parent.trim_end_matches('/');
    let name = astro_dav::vfs::name_of(&photo_path);
    let stem = name.rsplit_once('.').map_or(name, |(s, _)| s);
    let fiche_path = format!("{dir}/{stem}.media.json");
    let fiche = crate::media::read_fiche(dav, owner, &fiche_path).await.ok()?;
    let commons = crate::media::extract_commons(&fiche)?;
    if !commons.verify_signature() {
        return None;
    }
    let Rights::OpenCommon { spdx, allow_remix: true, remix_min_bps } = commons.rights else {
        return None;
    };
    let labels = resolve_labels(&dav.db, std::slice::from_ref(&commons.author_pubkey)).await;
    Some(json!({
        "path": fiche_path,
        "title": fiche["title"].as_str().unwrap_or(stem),
        "spdx": spdx,
        "remix_min_bps": remix_min_bps.0,
        "author_pubkey": commons.author_pubkey,
        "author_label": labels.get(&commons.author_pubkey).cloned().unwrap_or_default(),
    }))
}

/// Vérifie la table aplatie d'un remix contre ses fiches parentes lorsqu'elles sont présentes dans l'uCloud de `owner`.
/// Rend `None` si l'œuvre est une racine ou si au moins un parent n'est pas présent localement ; `Some(true)` si tous
/// les parents sont vérifiés conformes ; `Some(false)` si une incohérence est détectée.
pub async fn verify_local_dag<B: BlobStore>(dav: &Arc<DavState<B>>, owner: &str, commons: &CommonsBlock) -> Option<bool> {
    if commons.sources.is_empty() {
        return Some(commons.verify_signature());
    }
    let mut parents = BTreeMap::new();
    for src in &commons.sources {
        let o = owner.to_owned();
        let cid = src.cid.clone();
        let sha = src.sha_plain.clone();
        let path: Option<String> = dav
            .db
            .readers()
            .read(move |c| {
                Ok(c.query_row(
                    "SELECT path FROM ucloud_entries WHERE owner_hex = ?1 AND type = 'file' AND path LIKE '%.media.json' \
                     AND (cid = ?2 OR sha_plain = ?3) LIMIT 1",
                    params![o, cid, sha],
                    |r| r.get(0),
                )
                .optional()?)
            })
            .await
            .ok()?;
        let path = path?;
        let fiche = crate::media::read_fiche(dav, owner, &path).await.ok()?;
        let pblk = crate::media::extract_commons(&fiche)?;
        if pblk.content_sha != src.sha_plain {
            return Some(false);
        }
        parents.insert(src.sha_plain.clone(), pblk);
    }
    Some(astro_economy::cascade::verify_flattened_dag(commons, &parents).is_ok())
}

/// Pour une fiche recette sans fichier propre, retrouve dans l'uCloud de `owner` la photo parente (`sources[0].sha_plain`)
/// afin que la médiathèque puisse afficher sa vignette.
pub async fn source_photo_path_of(db: &Db, owner: &str, commons: &CommonsBlock) -> Option<String> {
    let first = commons.sources.first()?;
    let (o, sha) = (owner.to_owned(), first.sha_plain.clone());
    db.readers()
        .read(move |c| {
            Ok(c.query_row(
                "SELECT path FROM ucloud_entries WHERE owner_hex = ?1 AND type = 'file' AND sha_plain = ?2 \
                 AND COALESCE(mime, '') LIKE 'image/%' LIMIT 1",
                params![o, sha],
                |r| r.get(0),
            )
            .optional()?)
        })
        .await
        .ok()?
}

/// Calcule les statistiques de la liste officielle de `work_sha`, filtrées par la toile des rencontres prouvées (`Wot`).
///
/// Ne retient que les inscriptions dont la signature Nostr **et** la chaîne `ShareChain` embarquée (pour `rank > 0`)
/// sont valides : le rang affiché reflète la plus longue chaîne prouvée, jamais une déclaration brute.
pub async fn official_stats(db: &Db, viewer: &str, work_sha: &str, at: i64) -> Result<Value, String> {
    if !is_hex64(work_sha) {
        return Err("empreinte invalide".into());
    }
    let d_tag = format!("{OFFICIAL_PREFIX}{work_sha}");
    let events: Vec<Event> = db
        .readers()
        .read(move |c| {
            let mut st = c.prepare(
                "SELECT e.id, e.pubkey, e.kind, e.created_at, e.content, e.sig, e.tags FROM nostr_events e 
                 JOIN nostr_tags t ON t.event_id = e.seq AND t.name = 'd' AND t.value = ?1 
                 WHERE e.kind = ?2 ORDER BY e.created_at DESC LIMIT 2000",
            )?;
            let mut rows = st.query(params![d_tag, i64::from(kind::VIDEO_SET)])?;
            let mut events = Vec::new();
            while let Some(r) = rows.next()? {
                events.push(row_to_event(r)?);
            }
            Ok(events)
        })
        .await
        .map_err(|e| e.to_string())?;

    // Un seul événement (le plus récent) par pubkey ; preuve vérifiée.
    let mut latest: BTreeMap<String, (u32, u64)> = BTreeMap::new();
    for ev in events {
        if latest.contains_key(&ev.pubkey) || ev.content.len() > MAX_CHAIN_JSON || ev.verify().is_err() {
            continue;
        }
        let Ok(p) = serde_json::from_str::<OfficialPayload>(&ev.content) else { continue };
        if p.v != 1 || p.work_sha != work_sha {
            continue;
        }
        if p.rank > 0 {
            let Some(c) = &p.chain else { continue };
            if c.verify().is_err() || c.work_sha != work_sha || c.depth() != p.rank {
                continue;
            }
            if c.hops.last().map(|h| h.to_pubkey.as_str()) != Some(ev.pubkey.as_str()) {
                continue;
            }
        }
        latest.insert(ev.pubkey, (p.rank, ev.created_at));
    }

    let wot_set = wot_members(db, at).await?;

    let mut raw_max_rank = 0u32;
    let mut wot_count = 0usize;
    let mut wot_max_rank = 0u32;
    let mut entries: Vec<Value> = Vec::new();
    let registered = latest.contains_key(viewer);

    for (pubkey, (rank, created_at)) in &latest {
        raw_max_rank = raw_max_rank.max(*rank);
        let in_wot = wot_set.contains(pubkey);
        if in_wot {
            wot_count += 1;
            wot_max_rank = wot_max_rank.max(*rank);
        }
        entries.push(json!({ "pubkey": pubkey, "rank": rank, "wot": in_wot, "at": created_at }));
    }
    entries.sort_by(|a, b| b["wot"].as_bool().cmp(&a["wot"].as_bool()).then_with(|| b["rank"].as_u64().cmp(&a["rank"].as_u64())));
    entries.truncate(100);

    Ok(json!({
        "work_sha": work_sha,
        "raw_count": latest.len(),
        "raw_max_rank": raw_max_rank,
        "wot_count": wot_count,
        "wot_max_rank": wot_max_rank,
        "registered": registered,
        "entries": entries,
    }))
}

// ──────────────────────── Lot 4 : publication vers le commun et remix « recette » ────────────────────────

/// Paramètres de publication d'une œuvre commune ou d'un remix (y compris fiche botanique dérivée).
#[derive(Debug, Clone, Deserialize)]
pub struct PublishRemixRequest {
    /// Titre de l'œuvre ou de la fiche.
    pub title: String,
    /// Type de fiche (`"photo"`, `"plant"`, `"video"`, `"music"`).
    pub kind: String,
    /// Licence SPDX (ex. `"CC-BY-SA-4.0"`, `"CC-BY-4.0"`, `"CC0-1.0"`, `"LAL-1.3"`).
    pub spdx: String,
    /// Autoriser le remix par des tiers (défaut : `true`).
    #[serde(default = "default_true")]
    pub allow_remix: bool,
    /// Plancher minimal exigé en cas de remix (en bps de la tranche création).
    #[serde(default)]
    pub remix_min_bps: u16,
    /// Mode tarifaire global (`Free`, `Hat`, `Fixed`).
    pub pricing: PricingMode,
    /// Clé de répartition création / diffusion (défaut : 80 % / 20 %).
    #[serde(default)]
    pub split: Option<SplitRule>,
    /// Générique propre facultatif ; si absent, l'auteur reçoit toute la part non reversée au parent.
    #[serde(default)]
    pub credits: Option<Vec<Credit>>,
    /// Fichier média propre dans l'uCloud du membre (obligatoire pour une œuvre racine ou un remix rendu).
    #[serde(default)]
    pub file_path: Option<String>,
    /// Fiche `.media.json` parente dans l'uCloud du membre (ex. Fiche A d'Alice pour le remix botanique de Bob).
    #[serde(default)]
    pub parent_fiche_path: Option<String>,
    /// Part reversée au parent (en bps, `>= parent.remix_min_bps`).
    #[serde(default)]
    pub parent_share_bps: Option<u16>,
    /// Rôle du parent (défaut : `PrimaryMedia`).
    #[serde(default)]
    pub parent_role: Option<SourceRole>,
    /// Photo dont la fiche plante privée (`/_/Plantes/`) est exportée vers `PlantData` (sans `notes` ni GPS fin).
    #[serde(default)]
    pub plant_from_photo: Option<String>,
    /// Données botaniques directes (alternatives à `plant_from_photo`).
    #[serde(default)]
    pub plant: Option<PlantData>,
    /// Maille géographique UMAP arrondie (`0,01°`, ex. `"_43.61_1.44"`).
    #[serde(default)]
    pub umap: Option<String>,
    /// Date d'observation arrondie (secondes UNIX).
    #[serde(default)]
    pub observed_at: Option<i64>,
    /// Pistes explicites pour un remix « recette » (optionnel).
    #[serde(default)]
    pub tracks: Option<Vec<CompositionTrack>>,
    /// Fichier sous-titres (`.vtt` / `.srt`) dans l'uCloud du traducteur pour un remix sous-titré sans dupliquer la vidéo (#4).
    #[serde(default)]
    pub subtitle_path: Option<String>,
    /// Langue ISO 639-1 des sous-titres ajoutés (ex. `"fr"`, `"en"`, `"es"`).
    #[serde(default)]
    pub subtitle_lang: Option<String>,
    /// Résumé ou texte d'article pour un commun `kind = "article"` ou `kind = "document"` (#5).
    #[serde(default)]
    pub overview: Option<String>,
    /// Dossier de destination dans l'uCloud (défaut : dossier du fichier, ou `/Communs/Plantes` pour une recette plante).
    #[serde(default)]
    pub dest_dir: Option<String>,
    /// Nom de base du fichier `.media.json` (défaut : dérivé du fichier ou du titre).
    #[serde(default)]
    pub dest_name: Option<String>,
}

const fn default_true() -> bool {
    true
}

fn slug_name(s: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for ch in s.trim().chars().take(80) {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('_');
            dash = true;
        }
    }
    let out = out.trim_matches('_').to_owned();
    if out.is_empty() { "commun".into() } else { out }
}

/// Publie une fiche `.media.json` signée pour une œuvre commune (racine) ou un remix « recette » / « rendu ».
///
/// Cas d'usage phare : Bob publie sa fiche botanique (`plant_from_photo` ou `plant`) en référençant la Fiche A d'Alice
/// (`parent_fiche_path`), sans dupliquer l'image et en garantissant le respect du plancher `remix_min_bps`.
pub async fn publish_commons_or_remix<B: BlobStore>(
    dav: &Arc<DavState<B>>,
    keys: &Arc<dyn KeySource>,
    owner: &str,
    req: PublishRemixRequest,
) -> Result<Value, String> {
    if !is_hex64(owner) {
        return Err("compte invalide".into());
    }
    let secret = keys.secret(owner).ok_or("clé du compte indisponible")?;
    let title = req.title.trim().to_owned();
    if title.is_empty() || title.chars().count() > 300 || title.chars().any(char::is_control) {
        return Err("titre invalide".into());
    }
    if !["photo", "plant", "video", "music", "document", "article"].contains(&req.kind.as_str()) {
        return Err("type de fiche invalide (photo, plant, video, music, document ou article)".into());
    }
    let spdx = req.spdx.trim().to_owned();
    if spdx.is_empty() || spdx.len() > 64 || spdx.chars().any(char::is_control) {
        return Err("licence SPDX invalide".into());
    }
    let split = req.split.unwrap_or_default();
    if !split.is_valid() {
        return Err("répartition création/diffusion invalide (somme != 10000 bps)".into());
    }

    // 1. Données botaniques éventuelles (depuis la fiche privée `plant_fiche.rs` ou directes).
    let plant_data: Option<PlantData> = if let Some(photo_path) = req.plant_from_photo.as_deref() {
        let photo_path = astro_dav::vfs::normalize_path(photo_path).map_err(|_| "chemin photo plante invalide")?;
        let (o, p) = (owner.to_owned(), photo_path.clone());
        let sha: String = dav
            .db
            .readers()
            .read(move |c| Ok(c.query_row("SELECT sha_plain FROM ucloud_entries WHERE owner_hex = ?1 AND path = ?2 AND type = 'file'", params![o, p], |r| r.get::<_, Option<String>>(0)).optional()?.flatten()))
            .await
            .map_err(|e| e.to_string())?
            .filter(|s| is_hex64(s))
            .ok_or("photo plante introuvable ou empreinte inconnue")?;
        crate::plant_fiche::sync(dav, owner).await?;
        let private_fiche = crate::plant_fiche::find(&dav.db, owner, &sha, &photo_path).await?.ok_or("aucune fiche plante enregistrée pour cette photo")?;
        let p = private_fiche.to_commons_plant_data(req.umap.clone(), req.observed_at).ok_or("aucune espèce retenue dans la fiche plante")?;
        Some(p)
    } else {
        req.plant.clone()
    };

    // 1b. Piste sous-titres éventuelle (#4 : traduction / sous-titrage en recette sans dupliquer la vidéo parente).
    let subtitle_entry: Option<(String, String, String)> = if let Some(sub_p) = req.subtitle_path.as_deref() {
        let sub_p = astro_dav::vfs::normalize_path(sub_p).map_err(|_| "chemin de sous-titres invalide")?;
        let lower = sub_p.to_ascii_lowercase();
        if !lower.ends_with(".vtt") && !lower.ends_with(".srt") {
            return Err("les sous-titres doivent être au format .vtt ou .srt".into());
        }
        let (o, p) = (owner.to_owned(), sub_p.clone());
        let sub_sha: String = dav
            .db
            .readers()
            .read(move |c| Ok(c.query_row("SELECT sha_plain FROM ucloud_entries WHERE owner_hex = ?1 AND path = ?2 AND type = 'file' AND cid IS NOT NULL", params![o, p], |r| r.get::<_, Option<String>>(0)).optional()?.flatten()))
            .await
            .map_err(|e| e.to_string())?
            .filter(|s| is_hex64(s))
            .ok_or("fichier de sous-titres introuvable dans votre uCloud")?;
        let lang: String = req.subtitle_lang.as_deref().unwrap_or("fr").trim().chars().filter(char::is_ascii_alphanumeric).take(8).collect();
        let lang = if lang.is_empty() { "fr".to_owned() } else { lang.to_ascii_lowercase() };
        Some((sub_p, sub_sha, lang))
    } else {
        None
    };

    // 2. Parent éventuel du DAG.
    let mut parents = BTreeMap::new();
    let mut sources = Vec::new();
    let mut parent_share_total: u16 = 0;

    if let Some(parent_path) = req.parent_fiche_path.as_deref() {
        let parent_path = astro_dav::vfs::normalize_path(parent_path).map_err(|_| "chemin de la fiche parente invalide")?;
        let parent_json = crate::media::read_fiche(dav, owner, &parent_path).await?;
        let parent_commons = crate::media::extract_commons(&parent_json).ok_or("la fiche parente ne porte pas de bloc commons")?;
        if !parent_commons.verify_signature() {
            return Err("la fiche parente n'est pas signée valablement".into());
        }
        let min_bps = match &parent_commons.rights {
            Rights::PrivateCopy => return Err("une copie privée ne peut pas être remixée dans le commun".into()),
            Rights::OpenCommon { allow_remix: false, .. } => return Err("l'auteur parent n'autorise pas le remix".into()),
            Rights::OpenCommon { allow_remix: true, remix_min_bps, .. } => remix_min_bps.0,
        };
        let share_bps = req.parent_share_bps.unwrap_or(min_bps);
        if share_bps < min_bps {
            return Err(format!("part reversée au parent insuffisante ({share_bps} < {min_bps} bps)"));
        }
        let (o, p) = (owner.to_owned(), parent_path.clone());
        let parent_cid: String = dav
            .db
            .readers()
            .read(move |c| Ok(c.query_row("SELECT cid FROM ucloud_entries WHERE owner_hex = ?1 AND path = ?2 AND type = 'file'", params![o, p], |r| r.get(0)).optional()?.flatten()))
            .await
            .map_err(|e| e.to_string())?
            .ok_or("CID de la fiche parente introuvable")?;
        sources.push(SourceRef {
            sha_plain: parent_commons.content_sha.clone(),
            cid: parent_cid,
            author_pubkey: parent_commons.author_pubkey.clone(),
            event_id: None,
            role: req.parent_role.clone().unwrap_or(SourceRole::PrimaryMedia),
            share_bps: Bps(share_bps),
        });
        parent_share_total = share_bps;
        parents.insert(parent_commons.content_sha.clone(), parent_commons);
    }

    // 3. Fichier média propre, article texte ou recette non destructive.
    let overview_clean = req.overview.as_deref().map(|s| s.trim().chars().filter(|c| !c.is_control() || *c == '\n').take(2000).collect::<String>()).filter(|s| !s.is_empty());
    let (file_name, file_sha, file_mime, file_size, fiche_path, remix) = if let Some(fp) = req.file_path.as_deref() {
        let fp = astro_dav::vfs::normalize_path(fp).map_err(|_| "chemin du fichier invalide")?;
        let (o, p) = (owner.to_owned(), fp.clone());
        let row: Option<(Option<String>, Option<String>, i64)> = dav
            .db
            .readers()
            .read(move |c| {
                Ok(c.query_row(
                    "SELECT sha_plain, mime, COALESCE(size, 0) FROM ucloud_entries WHERE owner_hex = ?1 AND path = ?2 AND type = 'file' AND cid IS NOT NULL",
                    params![o, p],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?)
            })
            .await
            .map_err(|e| e.to_string())?;
        let (sha, mime, size) = row.ok_or("fichier média introuvable dans votre uCloud")?;
        let sha = sha.filter(|s| is_hex64(s)).ok_or("empreinte SHA-256 du fichier inconnue")?;
        let name = astro_dav::vfs::name_of(&fp).to_owned();
        let stem = name.rsplit_once('.').map_or(name.as_str(), |(s, _)| s);
        let default_dir = astro_dav::vfs::parent_of(&fp);
        let dir = req.dest_dir.as_deref().unwrap_or(default_dir.as_str()).trim_end_matches('/');
        let base_name = req.dest_name.as_deref().map(slug_name).unwrap_or_else(|| stem.to_owned());
        let fpath = format!("{dir}/{base_name}.media.json");
        let remix = (!sources.is_empty()).then_some(RemixKind::Rendered);
        (Some(name), sha, mime, Some(size), fpath, remix)
    } else if req.kind == "article" && sources.is_empty() {
        let body = overview_clean.as_deref().ok_or("un article exige un texte ou résumé (`overview`)")?;
        let sha = hex::encode(Sha256::digest(json!({ "v": 1, "owner": owner, "title": title, "body": body }).to_string().as_bytes()));
        let dir = astro_dav::vfs::normalize_path(req.dest_dir.as_deref().unwrap_or("/Communs/Articles")).map_err(|_| "dossier de destination invalide")?;
        let base_name = req.dest_name.as_deref().map(slug_name).unwrap_or_else(|| slug_name(&title));
        let fpath = format!("{}/{base_name}.media.json", dir.trim_end_matches('/'));
        (None, sha, None, None, fpath, None)
    } else {
        if sources.is_empty() {
            return Err("une œuvre racine exige un fichier (`file_path`) ; sans fichier, fournissez une fiche parente (`parent_fiche_path`) pour un remix recette".into());
        }
        let mut tracks = req.tracks.unwrap_or_else(|| {
            let stream = if req.kind == "video" { "video" } else if req.kind == "music" { "audio" } else { "image" };
            sources
                .iter()
                .map(|s| CompositionTrack { source_sha: s.sha_plain.clone(), stream: stream.into(), start_ms: None, end_ms: None, offset_ms: 0, lang: None })
                .collect()
        });
        if let Some((_, sub_sha, lang)) = &subtitle_entry {
            tracks.push(CompositionTrack { source_sha: sub_sha.clone(), stream: "subtitle".into(), start_ms: None, end_ms: None, offset_ms: 0, lang: Some(lang.clone()) });
        }
        // Empreinte canonique déterministe de la recette (évite toute collision avec le parent).
        let recipe_hash_input = json!({
            "v": 1, "owner": owner, "title": title, "kind": req.kind,
            "sources": sources.iter().map(|s| &s.sha_plain).collect::<Vec<_>>(),
            "tracks": tracks, "plant": plant_data, "overview": overview_clean,
        });
        let sha = hex::encode(Sha256::digest(recipe_hash_input.to_string().as_bytes()));
        let default_dir = match req.kind.as_str() {
            "plant" => "/Communs/Plantes",
            "article" => "/Communs/Articles",
            _ => "/Communs/Remixes",
        };
        let dir = astro_dav::vfs::normalize_path(req.dest_dir.as_deref().unwrap_or(default_dir)).map_err(|_| "dossier de destination invalide")?;
        let base_name = req.dest_name.as_deref().map(slug_name).unwrap_or_else(|| slug_name(&title));
        let fpath = format!("{}/{base_name}.media.json", dir.trim_end_matches('/'));
        (None, sha, None, None, fpath, Some(RemixKind::Recipe { tracks }))
    };

    // 4. Générique propre + aplatissement déterministe du DAG.
    let remaining_bps = BPS_TOTAL.checked_sub(parent_share_total).ok_or("part parent supérieure à 10000 bps")?;
    let credits = req.credits.unwrap_or_else(|| {
        if remaining_bps == 0 {
            Vec::new()
        } else {
            let role = if subtitle_entry.is_some() {
                "sous-titres"
            } else if req.kind == "plant" {
                "identification-botanique"
            } else {
                "auteur"
            };
            vec![Credit { pubkey: owner.to_owned(), role: role.into(), bps: Bps(remaining_bps), attest_sig: None }]
        }
    });

    let (generation, flattened_payouts) = flatten_dag(owner, &file_sha, &credits, &sources, &parents).map_err(|e| e.to_string())?;

    let mut commons = CommonsBlock {
        v: 1,
        author_pubkey: owner.to_owned(),
        content_sha: file_sha,
        rights: Rights::OpenCommon { spdx: spdx.clone(), allow_remix: req.allow_remix, remix_min_bps: Bps(req.remix_min_bps) },
        pricing: req.pricing,
        split,
        credits,
        sources,
        generation,
        remix,
        flattened_payouts,
        signature: None,
    };
    commons.sign(&secret).map_err(|e| e.to_string())?;

    // 5. Construction et écriture de la fiche `media/v1` signée.
    let mut fiche = json!({
        "schema": "media/v1",
        "type": req.kind,
        "title": title,
        "generated_at": now(),
        "source": { "kind": "own", "rights": "own", "licence": spdx },
        "sharing": { "level": "private" },
        "commons": commons,
    });
    if let Some(ov) = overview_clean {
        fiche["overview"] = json!(ov);
    }
    if let Some((sub_path, _, lang)) = subtitle_entry {
        fiche["subtitles"] = json!([{ "path": sub_path, "lang": lang }]);
    }
    if let Some(name) = file_name {
        fiche["file"] = json!(name);
        fiche["technical"] = json!({ "mime": file_mime, "size": file_size });
    }
    if let Some(p) = plant_data {
        fiche["plant"] = serde_json::to_value(p).map_err(|e| e.to_string())?;
    }

    crate::media::write_fiche(dav, owner, &fiche_path, &fiche).await?;
    let item = crate::media::summarize(&fiche, &fiche_path).ok_or("fiche écrite mais illisible")?;
    Ok(json!({ "fiche_path": fiche_path, "item": item, "commons": commons }))
}

/// Appose la contresignature BIP-340 du compte `owner` sur sa ligne de générique (`credits`) dans `fiche_path`.
/// Ne modifie ni les parts ni la signature principale de l'auteur.
pub async fn attest_credit_on_fiche<B: BlobStore>(
    dav: &Arc<DavState<B>>,
    keys: &Arc<dyn KeySource>,
    owner: &str,
    fiche_path: &str,
) -> Result<Value, String> {
    if !is_hex64(owner) {
        return Err("compte invalide".into());
    }
    let secret = keys.secret(owner).ok_or("clé du compte indisponible")?;
    let path = astro_dav::vfs::normalize_path(fiche_path).map_err(|_| "chemin de fiche invalide")?;
    let mut fiche = crate::media::read_fiche(dav, owner, &path).await?;
    let mut commons = crate::media::extract_commons(&fiche).ok_or("cette fiche ne porte pas de bloc commons")?;
    if !commons.verify_signature() {
        return Err("la fiche n'est pas signée valablement par son auteur".into());
    }
    let work_sha = commons.content_sha.clone();
    let mut count = 0usize;
    for credit in &mut commons.credits {
        if credit.pubkey == owner {
            credit.attest(&secret, &work_sha).map_err(|e| e.to_string())?;
            count += 1;
        }
    }
    if count == 0 {
        return Err("vous ne figurez pas dans le générique de cette fiche".into());
    }
    fiche["commons"] = serde_json::to_value(&commons).map_err(|e| e.to_string())?;
    crate::media::write_fiche(dav, owner, &path, &fiche).await?;
    Ok(json!({
        "attested": true,
        "count": count,
        "work_sha": work_sha,
        "fiche_path": path,
        "commons_valid": commons.verify_signature(),
    }))
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use astro_crypto::StaticKeySource;
    use astro_dav::{DavConfig, FsBlobStore};
    use astro_db::events::insert_event;
    use astro_economy::cascade::DeciZen;
    use astro_nostr_proto::pubkey_of;

    use super::*;

    const CAPTAIN_SK: [u8; 32] = [0x01; 32];
    const ALICE_SK: [u8; 32] = [0xA1; 32];
    const BOB_SK: [u8; 32] = [0xB2; 32];
    const CHARLIE_SK: [u8; 32] = [0xC3; 32];
    const SYBIL_SK: [u8; 32] = [0x99; 32];

    fn pk(sk: &[u8; 32]) -> String {
        pubkey_of(sk).unwrap()
    }

    async fn setup_dav(tag: &str) -> (std::path::PathBuf, Arc<DavState<FsBlobStore>>, Arc<dyn KeySource>) {
        let dir = std::env::temp_dir().join(format!("astroportd-cascade-share-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("blobs")).unwrap();
        let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
        let blobs = Arc::new(FsBlobStore::open(dir.join("blobs")).unwrap());
        let dav = Arc::new(DavState {
            db: db.clone(),
            blobs,
            replay: Arc::new(astro_db::SqliteReplayStore::in_memory().unwrap()),
            publisher: None,
            config: DavConfig::new(vec![7; 32]),
            on_upload: None,
        });
        for (sk, email, role) in [
            (&CAPTAIN_SK, "cap@example.org", "captain"),
            (&ALICE_SK, "alice@example.org", "multipass"),
            (&BOB_SK, "bob@example.org", "multipass"),
            (&CHARLIE_SK, "charlie@example.org", "multipass"),
            (&SYBIL_SK, "sybil@example.org", "multipass"),
        ] {
            db.writer()
                .execute(
                    "INSERT INTO identities(hex, role, email, hosted) VALUES (?1, ?2, ?3, 1)",
                    vec![pk(sk).into(), role.to_owned().into(), email.to_owned().into()],
                )
                .await
                .unwrap();
        }
        let keys: Arc<dyn KeySource> = Arc::new(StaticKeySource::new().with(CAPTAIN_SK).with(ALICE_SK).with(BOB_SK).with(CHARLIE_SK).with(SYBIL_SK));
        (dir, dav, keys)
    }

    #[tokio::test]
    async fn lot2_official_list_verifies_chain_proofs_and_filters_sybils_with_wot() {
        let (dir, dav, keys) = setup_dav("official").await;
        let published: Arc<Mutex<Vec<Event>>> = Arc::new(Mutex::new(Vec::new()));
        let db_pub = dav.db.clone();
        let pub_cap = published.clone();
        let publish: PublishFn = Arc::new(move |ev: Event| {
            let db = db_pub.clone();
            let cap = pub_cap.clone();
            Box::pin(async move {
                cap.lock().unwrap().push(ev.clone());
                db.writer().transaction(move |tx| insert_event(tx, &ev).map(|_| ())).await.map_err(|e| e.to_string())
            })
        });

        let work_sha = hex::encode([0x42u8; 32]);
        let root = ShareChain::new_root(&work_sha, "bafkreiwork", None, pk(&CAPTAIN_SK)).unwrap();
        let c1 = root.extend(&CAPTAIN_SK, &pk(&ALICE_SK), 1_790_000_001).unwrap();
        let c2 = c1.extend(&ALICE_SK, &pk(&SYBIL_SK), 1_790_000_002).unwrap();
        assert!(save_chain(&dav.db, &pk(&ALICE_SK), &c1).await);
        assert!(save_chain(&dav.db, &pk(&SYBIL_SK), &c2).await);

        // Capitaine (racine WoT, rang 0), Alice (rang 1, pas encore WoT) et Sybil (rang 2, hors WoT) s'inscrivent.
        register_official(&dav.db, &keys, &publish, &pk(&CAPTAIN_SK), &work_sha).await.unwrap();
        register_official(&dav.db, &keys, &publish, &pk(&ALICE_SK), &work_sha).await.unwrap();
        register_official(&dav.db, &keys, &publish, &pk(&SYBIL_SK), &work_sha).await.unwrap();

        // Un tricheur publie un rang 99 sans preuve valide → ignoré !
        let fake_payload = json!({ "v": 1, "work_sha": work_sha, "rank": 99 });
        let fake_ev = UnsignedEvent {
            created_at: 1_790_000_100,
            kind: kind::VIDEO_SET,
            tags: vec![vec!["d".into(), format!("{OFFICIAL_PREFIX}{work_sha}")], vec!["rank".into(), "99".into()]],
            content: fake_payload.to_string(),
        }
        .sign(&CHARLIE_SK)
        .unwrap();
        dav.db.writer().transaction(move |tx| insert_event(tx, &fake_ev).map(|_| ())).await.unwrap();

        let stats = official_stats(&dav.db, &pk(&CAPTAIN_SK), &work_sha, 1_790_000_200).await.unwrap();
        assert_eq!(stats["raw_count"], 3, "le tricheur sans preuve est écarté");
        assert_eq!(stats["raw_max_rank"], 2, "rang maximal prouvé = Sybil (2)");
        assert_eq!(stats["wot_count"], 1, "seul le Capitaine est membre WoT");
        assert_eq!(stats["wot_max_rank"], 0, "le Sybil ne gonfle pas le compteur WoT");
        assert_eq!(stats["registered"], true);

        // Désinscription (NIP-09)
        let unreg = unregister_official(&keys, &publish, &pk(&CAPTAIN_SK), &work_sha).await.unwrap();
        assert_eq!(unreg["registered"], false);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn lot4_alice_photo_to_bob_botanical_recipe_remix_end_to_end() {
        let (dir, dav, keys) = setup_dav("botanical").await;
        let alice = pk(&ALICE_SK);
        let bob = pk(&BOB_SK);

        // 1. Alice dépose une photo de plante et la publie en commun (Fiche A, remix_min_bps = 4000).
        astro_dav::ingest::replace(&dav, &alice, "/Photos", "ortie.jpg", Some("image/jpeg"), &b"fake-jpg-ortie"[..]).await.unwrap();
        let res_a = publish_commons_or_remix(
            &dav,
            &keys,
            &alice,
            PublishRemixRequest {
                title: "Ortie dioïque au bord du chemin".into(),
                kind: "photo".into(),
                spdx: "CC-BY-SA-4.0".into(),
                allow_remix: true,
                remix_min_bps: 4_000,
                pricing: PricingMode::Hat { suggested_deci_zen: Some(DeciZen(10)), min_deci_zen: None },
                split: None,
                credits: None,
                file_path: Some("/Photos/ortie.jpg".into()),
                parent_fiche_path: None,
                parent_share_bps: None,
                parent_role: None,
                plant_from_photo: None,
                plant: None,
                umap: None,
                observed_at: None,
                tracks: None,
                subtitle_path: None,
                subtitle_lang: None,
                overview: None,
                dest_dir: None,
                dest_name: None,
            },
        )
        .await
        .unwrap();
        let fiche_a_path = res_a["fiche_path"].as_str().unwrap().to_owned();
        assert_eq!(fiche_a_path, "/Photos/ortie.media.json");
        assert_eq!(res_a["item"]["commons"]["signed"], true);

        // 2. Bob reçoit la Fiche A d'Alice dans son uCloud (partage N¹) et publie la Fiche B (identification botanique)
        // en remix « recette » sans dupliquer la photo.
        let raw_a = astro_dav::carddav::read_file(&dav, &alice, &fiche_a_path).await.unwrap();
        astro_dav::ingest::replace(&dav, &bob, "/Partages/De_alice", "ortie.media.json", Some("application/json"), raw_a.as_slice()).await.unwrap();

        // Tentative hostile : Bob propose moins que le plancher 4000 bps → refusé !
        let err = publish_commons_or_remix(
            &dav,
            &keys,
            &bob,
            PublishRemixRequest {
                title: "Urtica dioica".into(),
                kind: "plant".into(),
                spdx: "CC-BY-SA-4.0".into(),
                allow_remix: true,
                remix_min_bps: 3_000,
                pricing: PricingMode::Fixed { price_deci_zen: DeciZen(25) },
                split: None,
                credits: None,
                file_path: None,
                parent_fiche_path: Some("/Partages/De_alice/ortie.media.json".into()),
                parent_share_bps: Some(2_000),
                parent_role: Some(SourceRole::PrimaryMedia),
                plant_from_photo: None,
                plant: Some(PlantData { scientific: "Urtica dioica".into(), ..Default::default() }),
                umap: Some("_43.61_1.44".into()),
                observed_at: Some(1_790_000_000),
                tracks: None,
                subtitle_path: None,
                subtitle_lang: None,
                overview: None,
                dest_dir: None,
                dest_name: None,
            },
        )
        .await
        .unwrap_err();
        assert!(err.contains("insuffisante"), "{err}");

        // Publication valide : Bob donne 4000 bps à Alice et garde 6000 bps.
        let res_b = publish_commons_or_remix(
            &dav,
            &keys,
            &bob,
            PublishRemixRequest {
                title: "Urtica dioica — Ortie dioïque".into(),
                kind: "plant".into(),
                spdx: "CC-BY-SA-4.0".into(),
                allow_remix: true,
                remix_min_bps: 3_000,
                pricing: PricingMode::Fixed { price_deci_zen: DeciZen(25) },
                split: None,
                credits: None,
                file_path: None,
                parent_fiche_path: Some("/Partages/De_alice/ortie.media.json".into()),
                parent_share_bps: Some(4_000),
                parent_role: Some(SourceRole::PrimaryMedia),
                plant_from_photo: None,
                plant: Some(PlantData {
                    scientific: "Urtica dioica".into(),
                    common: vec!["Grande ortie".into()],
                    family: "Urticaceae".into(),
                    umap: Some("_43.61_1.44".into()),
                    edible: "Jeunes pousses cuites.".into(),
                    ..Default::default()
                }),
                umap: None,
                observed_at: None,
                tracks: None,
                subtitle_path: None,
                subtitle_lang: None,
                overview: None,
                dest_dir: None,
                dest_name: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(res_b["item"]["recipe"], true);
        assert_eq!(res_b["item"]["commons"]["generation"], 1);
        assert_eq!(res_b["item"]["commons"]["signed"], true);
        assert_eq!(res_b["item"]["plant"]["scientific"], "Urtica dioica");
        let commons_b: CommonsBlock = serde_json::from_value(res_b["commons"].clone()).unwrap();
        let flat: BTreeMap<String, u16> = commons_b.flattened_payouts.iter().map(|p| (p.pubkey.clone(), p.bps.0)).collect();
        assert_eq!(flat.get(&alice), Some(&4_000));
        assert_eq!(flat.get(&bob), Some(&6_000));

        let _ = std::fs::remove_dir_all(dir);
    }
}
