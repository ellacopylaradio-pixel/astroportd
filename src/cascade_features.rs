//! Fonctionnalités avancées autour du partage en cascade :
//! - #1 Tableau de bord « Mes retombées » (`earnings_summary`)
//! - #2 Gratitude chiffrée « Merci » en NIP-59 (`wrap_thank_you`, `parse_thank_you`, `record_received`)
//! - #3 Ardoise / Panier de soutien groupé (`slate_list`, `slate_upsert`, `slate_remove`, `slate_clear`)
//! - #6 Atlas botanique collaboratif basé sur la maille UMAP (`parse_umap`, `plants_atlas`)
//! - #7 Reçu Nostr `kind 30852` (`build_receipt_event`).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use astro_crypto::nip59::{Rumor, gift_wrap};
use astro_dav::{BlobStore, DavState};
use astro_db::Db;
use astro_nostr_proto::{Event, UnsignedEvent, is_hex64, kind};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Flux `journal` des soutiens reçus (locaux ou par NIP-59).
pub const EARNINGS_STREAM: &str = "cascade_received";
/// Étiquette `t` identifiant un « Merci » chiffré en NIP-59 (`kind::PRIVATE_DIRECT_MESSAGE`).
pub const THANKS_TAG: &str = "astro-cascade-thanks";
/// Portée `kv` de l'ardoise locale d'un membre.
pub const SLATE_SCOPE: &str = "cascade_slate";
/// Kind Nostr remplaçable (NIP-33) des reçus de paiement en cascade (#7).
pub const CASCADE_RECEIPT_KIND: u32 = 30_852;
/// Nombre maximal d'œuvres dans l'ardoise d'un membre.
pub const MAX_SLATE_ITEMS: usize = 50;
/// Longueur maximale d'un mot de merci (caractères Unicode).
pub const MAX_NOTE_CHARS: usize = 280;

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

/// Nettoie un mot de merci facultatif : pas de caractères de contrôle, plafonné à [`MAX_NOTE_CHARS`].
pub fn clean_note(raw: Option<&str>) -> Option<String> {
    let s = raw?.trim();
    if s.is_empty() {
        return None;
    }
    let clean: String = s.chars().filter(|c| !c.is_control() || *c == ' ').take(MAX_NOTE_CHARS).collect();
    let clean = clean.trim().to_owned();
    (!clean.is_empty()).then_some(clean)
}

// ──────────────────────── #1 & #2 : Reçus de soutien et « Merci » NIP-59 ────────────────────────

/// Contenu structuré d'un reçu de soutien (enregistré localement ou décodé depuis un NIP-59).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThankYouReceipt {
    /// Identifiant de référence unique (`<credit_id>:<work_sha[..8]>:<to[..8]>`) assurant l'idempotence.
    pub tx_ref: String,
    /// Clé publique du bénéficiaire.
    pub to: String,
    /// Clé publique du payeur (`None` si le soutien est anonyme).
    pub from: Option<String>,
    /// Empreinte de l'œuvre soutenue.
    pub work_sha: String,
    /// Titre lisible de l'œuvre.
    pub title: String,
    /// Rôle rétribué : `"creation"`, `"relay"` ou `"mixed"`.
    pub role: String,
    /// Montant reçu en déci-Ẑen (`1 DeciZen = 0,10 Ẑen`).
    pub deci_zen: u64,
    /// Mot de merci facultatif.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Vrai si le payeur a choisi de rester anonyme.
    #[serde(default)]
    pub anonymous: bool,
    /// Horodatage UNIX (secondes).
    pub at: i64,
}

/// Enregistre un soutien reçu dans `journal(stream = 'cascade_received')`, de manière idempotente sur `tx_ref`.
/// Rend `true` si l'entrée a été insérée, `false` si déjà connue ou invalide.
pub async fn record_received(db: &Db, r: &ThankYouReceipt) -> bool {
    if !is_hex64(&r.to) || !is_hex64(&r.work_sha) || r.deci_zen == 0 || r.tx_ref.is_empty() || r.tx_ref.len() > 128 {
        return false;
    }
    let tx_ref = r.tx_ref.clone();
    let to = r.to.clone();
    let exists: bool = db
        .readers()
        .read(move |c| {
            Ok(c.query_row(
                "SELECT EXISTS(SELECT 1 FROM journal WHERE stream = ?1 AND json_extract(data, '$.tx_ref') = ?2 AND json_extract(data, '$.to') = ?3)",
                params![EARNINGS_STREAM, tx_ref, to],
                |row| row.get(0),
            )?)
        })
        .await
        .unwrap_or(false);
    if exists {
        return false;
    }
    let data = serde_json::to_string(r).unwrap_or_default();
    db.writer()
        .execute("INSERT INTO journal(stream, at, data) VALUES (?1, ?2, ?3)", vec![EARNINGS_STREAM.to_owned().into(), r.at.into(), data.into()])
        .await
        .is_ok()
}

/// Emballe un « Merci » chiffré en NIP-59 (`kind::PRIVATE_DIRECT_MESSAGE`) à destination d'un bénéficiaire distant.
#[allow(clippy::too_many_arguments)]
pub fn wrap_thank_you(
    secret: &[u8; 32],
    from_pubkey: &str,
    to_pubkey: &str,
    work_sha: &str,
    title: &str,
    role: &str,
    deci_zen: u64,
    note: Option<&str>,
    anonymous: bool,
    tx_ref: &str,
    at: i64,
) -> Result<Event, String> {
    if !is_hex64(from_pubkey) || !is_hex64(to_pubkey) || !is_hex64(work_sha) || deci_zen == 0 {
        return Err("paramètres de merci invalides".into());
    }
    let role = if matches!(role, "creation" | "relay" | "mixed") { role } else { "creation" };
    let clean_title: String = title.trim().chars().filter(|c| !c.is_control()).take(200).collect();
    let content = clean_note(note).unwrap_or_default();
    let mut tags = vec![
        vec!["p".to_owned(), to_pubkey.to_owned()],
        vec!["t".to_owned(), THANKS_TAG.to_owned()],
        vec!["x".to_owned(), work_sha.to_owned()],
        vec!["title".to_owned(), clean_title],
        vec!["role".to_owned(), role.to_owned()],
        vec!["deci_zen".to_owned(), deci_zen.to_string()],
        vec!["ref".to_owned(), tx_ref.to_owned()],
    ];
    if anonymous {
        tags.push(vec!["anon".to_owned(), "1".to_owned()]);
    }
    let rumor = Rumor { pubkey: from_pubkey.to_owned(), created_at: u64::try_from(at).unwrap_or(0), kind: kind::CHAT, tags, content };
    gift_wrap(secret, to_pubkey, &rumor, rumor.created_at).map_err(|_| "emballage NIP-59 du merci impossible".to_owned())
}

/// Décode un « Merci » reçu dans une rumeur NIP-59 (`kind::CHAT` portant le tag [`THANKS_TAG`]).
pub fn parse_thank_you(rumor: &Rumor, sender: &str, recipient: &str) -> Option<ThankYouReceipt> {
    if rumor.kind != kind::CHAT {
        return None;
    }
    let has_tag = rumor.tags.iter().any(|t| t.first().map(String::as_str) == Some("t") && t.get(1).map(String::as_str) == Some(THANKS_TAG));
    if !has_tag {
        return None;
    }
    let tag = |name: &str| rumor.tags.iter().find(|t| t.first().map(String::as_str) == Some(name)).and_then(|t| t.get(1)).map(String::as_str);
    let work_sha = tag("x").filter(|s| is_hex64(s))?.to_owned();
    let deci_zen: u64 = tag("deci_zen")?.parse().ok().filter(|v| *v > 0 && *v <= 100_000)?;
    let tx_ref = tag("ref").filter(|s| !s.is_empty() && s.len() <= 128)?.to_owned();
    let role = match tag("role") {
        Some("relay") => "relay",
        Some("mixed") => "mixed",
        _ => "creation",
    }
    .to_owned();
    let title: String = tag("title").unwrap_or("").trim().chars().filter(|c| !c.is_control()).take(200).collect();
    let anonymous = tag("anon") == Some("1");
    let from = if anonymous || !is_hex64(sender) { None } else { Some(sender.to_owned()) };
    let note = clean_note(Some(&rumor.content));
    let at = i64::try_from(rumor.created_at).unwrap_or_else(|_| now());
    Some(ThankYouReceipt { tx_ref, to: recipient.to_owned(), from, work_sha, title, role, deci_zen, note, anonymous, at })
}

/// Synthèse « Mes retombées » pour `owner` : reçus (création vs relais), top œuvres, derniers mercis et total donné.
pub async fn earnings_summary(db: &Db, owner: &str) -> Result<Value, String> {
    if !is_hex64(owner) {
        return Err("compte invalide".into());
    }
    let o = owner.to_owned();
    let (received_raw, given_total, given_count): (Vec<String>, i64, i64) = db
        .readers()
        .read(move |c| {
            let mut st = c.prepare("SELECT data FROM journal WHERE stream = ?1 AND json_extract(data, '$.to') = ?2 ORDER BY at DESC LIMIT 1000")?;
            let rows = st.query_map(params![EARNINGS_STREAM, o], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
            let (gt, gc): (i64, i64) = c.query_row(
                "SELECT COALESCE(SUM(CAST(COALESCE(json_extract(data, '$.g1_cents'), 0) AS INTEGER)), 0), COUNT(*) 
                 FROM journal WHERE stream = 'zen_credit' AND json_extract(data, '$.kind') IN ('cascade', 'cascade_slate') AND json_extract(data, '$.actor') = ?1",
                params![o],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            Ok((rows, gt, gc))
        })
        .await
        .map_err(|e| e.to_string())?;

    let receipts: Vec<ThankYouReceipt> = received_raw.iter().filter_map(|s| serde_json::from_str(s).ok()).collect();
    let senders: Vec<String> = receipts.iter().filter_map(|r| r.from.clone()).collect();
    let labels = crate::cascade_share::resolve_labels(db, &senders).await;

    let (mut total_received, mut creation_total, mut relay_total) = (0u64, 0u64, 0u64);
    let mut by_work: BTreeMap<String, (String, u64, usize)> = BTreeMap::new();
    for r in &receipts {
        total_received = total_received.saturating_add(r.deci_zen);
        match r.role.as_str() {
            "relay" => relay_total = relay_total.saturating_add(r.deci_zen),
            _ => creation_total = creation_total.saturating_add(r.deci_zen),
        }
        let e = by_work.entry(r.work_sha.clone()).or_insert_with(|| (r.title.clone(), 0, 0));
        if e.0.is_empty() && !r.title.is_empty() {
            e.0 = r.title.clone();
        }
        e.1 = e.1.saturating_add(r.deci_zen);
        e.2 += 1;
    }

    let mut top_works: Vec<Value> = by_work
        .into_iter()
        .map(|(work_sha, (title, deci_zen, count))| json!({ "work_sha": work_sha, "title": title, "total_deci_zen": deci_zen, "count": count }))
        .collect();
    top_works.sort_by(|a, b| b["total_deci_zen"].as_u64().cmp(&a["total_deci_zen"].as_u64()));
    top_works.truncate(15);

    let recent: Vec<Value> = receipts
        .into_iter()
        .take(30)
        .map(|r| {
            let from_label = if r.anonymous {
                None
            } else {
                r.from.as_ref().and_then(|pk| labels.get(pk).cloned())
            };
            json!({
                "at": r.at, "work_sha": r.work_sha, "title": r.title, "role": r.role,
                "deci_zen": r.deci_zen, "note": r.note, "anonymous": r.anonymous, "from_label": from_label,
            })
        })
        .collect();

    Ok(json!({
        "total_received_deci_zen": total_received,
        "creation_deci_zen": creation_total,
        "relay_deci_zen": relay_total,
        "received_count": recent.len(),
        "total_given_deci_zen": given_total,
        "given_count": given_count,
        "top_works": top_works,
        "recent": recent,
    }))
}

// ──────────────────────── #3 : Ardoise / Panier de soutien groupé ────────────────────────

/// Une intention de soutien empilée dans l'ardoise locale du membre.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlateEntry {
    /// Chemin de la fiche `.media.json` dans l'uCloud du membre.
    pub path: String,
    /// Empreinte `commons.content_sha` de l'œuvre.
    pub work_sha: String,
    /// Titre lisible de l'œuvre.
    pub title: String,
    /// Montant souhaité en déci-Ẑen (`>= 1`).
    pub amount_deci_zen: u64,
    /// Mot de merci facultatif.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Rester anonyme lors de l'envoi du merci.
    #[serde(default)]
    pub anonymous: bool,
    /// Date d'ajout (secondes UNIX).
    pub added_at: i64,
}

/// Lit toutes les entrées de l'ardoise de `owner`.
pub async fn slate_list(db: &Db, owner: &str) -> Result<Vec<SlateEntry>, String> {
    if !is_hex64(owner) {
        return Err("compte invalide".into());
    }
    let o = owner.to_owned();
    let raw: Option<String> = db
        .readers()
        .read(move |c| Ok(c.query_row("SELECT value FROM kv WHERE scope = ?1 AND key = ?2", params![SLATE_SCOPE, o], |r| r.get(0)).optional()?))
        .await
        .map_err(|e| e.to_string())?;
    Ok(raw.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default())
}

async fn slate_save(db: &Db, owner: &str, items: &[SlateEntry]) -> Result<(), String> {
    let raw = serde_json::to_string(items).map_err(|e| e.to_string())?;
    db.writer()
        .execute(
            "INSERT OR REPLACE INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4)",
            vec![SLATE_SCOPE.to_owned().into(), owner.to_owned().into(), raw.into(), now().into()],
        )
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Ajoute ou met à jour une œuvre dans l'ardoise (`add = true` additionne au montant existant, `false` le remplace).
pub async fn slate_upsert(db: &Db, owner: &str, entry: SlateEntry, add: bool) -> Result<Vec<SlateEntry>, String> {
    if entry.amount_deci_zen == 0 || entry.amount_deci_zen > 5_000 {
        return Err("montant d'ardoise invalide (0,10 à 500 Ẑen)".into());
    }
    let mut items = slate_list(db, owner).await?;
    if let Some(existing) = items.iter_mut().find(|e| e.path == entry.path || e.work_sha == entry.work_sha) {
        existing.amount_deci_zen = if add { existing.amount_deci_zen.saturating_add(entry.amount_deci_zen).min(5_000) } else { entry.amount_deci_zen };
        if entry.note.is_some() {
            existing.note = entry.note;
        }
        existing.anonymous = entry.anonymous;
        existing.added_at = entry.added_at;
    } else {
        if items.len() >= MAX_SLATE_ITEMS {
            return Err(format!("ardoise pleine (maximum {MAX_SLATE_ITEMS} œuvres)"));
        }
        items.push(entry);
    }
    slate_save(db, owner, &items).await?;
    Ok(items)
}

/// Retire une œuvre de l'ardoise par son `path`.
pub async fn slate_remove(db: &Db, owner: &str, path: &str) -> Result<Vec<SlateEntry>, String> {
    let mut items = slate_list(db, owner).await?;
    items.retain(|e| e.path != path);
    slate_save(db, owner, &items).await?;
    Ok(items)
}

/// Vide entièrement l'ardoise de `owner` (après règlement).
pub async fn slate_clear(db: &Db, owner: &str) -> Result<(), String> {
    db.writer()
        .execute("DELETE FROM kv WHERE scope = ?1 AND key = ?2", vec![SLATE_SCOPE.to_owned().into(), owner.to_owned().into()])
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

// ──────────────────────── #6 : Atlas botanique collaboratif (maille UMAP) ────────────────────────

/// Décode une maille UMAP arrondie (`"_43.61_1.44"` → `Some((43.61, 1.44))`), sans jamais exposer de GPS fin.
pub fn parse_umap(umap: &str) -> Option<(f64, f64)> {
    let s = umap.trim().strip_prefix('_')?;
    let (lat_s, lon_s) = s.split_once('_')?;
    let lat: f64 = lat_s.parse().ok()?;
    let lon: f64 = lon_s.parse().ok()?;
    if !lat.is_finite() || !lon.is_finite() || !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        return None;
    }
    // Arrondit défensivement à 0,01° (~1,1 km) pour garantir l'absence de position fine.
    Some(((lat * 100.0).round() / 100.0, (lon * 100.0).round() / 100.0))
}

/// Construit l'atlas botanique du cercle visible par `owner` à partir des fiches `type = "plant"` de sa médiathèque
/// (les siennes + celles reçues de ses proches). Seule la maille UMAP arrondie (`0,01°`) est exposée.
pub async fn plants_atlas<B: BlobStore>(dav: &Arc<DavState<B>>, owner: &str) -> Result<Value, String> {
    let lib = crate::media::library(dav, owner).await.map_err(|e| e.to_string())?;
    let items = lib["items"].as_array().cloned().unwrap_or_default();
    let mut raw_points: Vec<(Value, String, f64, f64, Option<String>)> = Vec::new();
    let mut authors = Vec::new();

    for it in items {
        if it["type"].as_str() != Some("plant") {
            continue;
        }
        let Some(umap) = it.get("plant").and_then(|v| v.get("umap")).and_then(Value::as_str).map(str::to_owned) else { continue };
        let Some((lat, lon)) = parse_umap(&umap) else { continue };
        let author_pk = it["commons"]["author_pubkey"].as_str().map(str::to_owned);
        if let Some(a) = &author_pk {
            authors.push(a.clone());
        }
        raw_points.push((it, umap, lat, lon, author_pk));
    }

    let labels = crate::cascade_share::resolve_labels(&dav.db, &authors).await;
    let mut points = Vec::with_capacity(raw_points.len());
    for (it, umap, lat, lon, author_pk) in raw_points {
        let plant = &it["plant"];
        let work_sha = it["commons"]["content_sha"].as_str().unwrap_or("");
        let paid = if is_hex64(work_sha) { crate::cascade_share::paid_by_me(&dav.db, owner, work_sha).await } else { None };
        let author_label = author_pk.as_ref().and_then(|pk| labels.get(pk).cloned());
        points.push(json!({
            "fiche": it["fiche"],
            "title": it["title"],
            "scientific": plant["scientific"],
            "common": plant["common"],
            "family": plant["family"],
            "edible": plant["edible"],
            "medicinal": plant["medicinal"],
            "dangers": plant["dangers"],
            "umap": umap,
            "lat": lat,
            "lon": lon,
            "observed_at": plant["observed_at"],
            "spdx": it["commons"]["spdx"],
            "author_label": author_label,
            "paid_by_me": paid,
        }));
    }
    Ok(json!({ "points": points, "count": points.len() }))
}

// ──────────────────────── #7 : Reçu Nostr kind 30852 ────────────────────────

/// Construit et signe l'événement Nostr `kind 30852` (reçu public léger de règlement Voie A, sans note privée ni détail par bénéficiaire).
pub fn build_receipt_event(
    secret: &[u8; 32],
    work_sha: &str,
    credit_id: &str,
    total_deci_zen: u64,
    transfers: usize,
    block_hash_hex: Option<&str>,
    at: i64,
) -> Result<Event, String> {
    let d_tag = format!("cascade:{}:{credit_id}", &work_sha[..work_sha.len().min(16)]);
    let mut tags = vec![
        vec!["d".to_owned(), d_tag],
        vec!["t".to_owned(), "astro-cascade-receipt".to_owned()],
        vec!["x".to_owned(), work_sha.to_owned()],
        vec!["deci_zen".to_owned(), total_deci_zen.to_string()],
        vec!["transfers".to_owned(), transfers.to_string()],
    ];
    if let Some(bh) = block_hash_hex {
        tags.push(vec!["block".to_owned(), bh.to_owned()]);
    }
    let content = json!({ "v": 1, "work_sha": work_sha, "deci_zen": total_deci_zen, "transfers": transfers, "block": block_hash_hex }).to_string();
    UnsignedEvent { created_at: u64::try_from(at).unwrap_or(0), kind: CASCADE_RECEIPT_KIND, tags, content }
        .sign(secret)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use astro_crypto::nip59::unwrap;
    use astro_nostr_proto::pubkey_of;

    use super::*;

    const ALICE_SK: [u8; 32] = [0xA1; 32];
    const BOB_SK: [u8; 32] = [0xB2; 32];

    #[tokio::test]
    async fn earnings_and_thank_you_roundtrip_and_idempotence() {
        let dir = std::env::temp_dir().join(format!("astroportd-cascade-feat-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("u.db"), 2).unwrap();
        let (alice, bob) = (pubkey_of(&ALICE_SK).unwrap(), pubkey_of(&BOB_SK).unwrap());
        db.writer()
            .execute("INSERT INTO identities(hex, role, email, hosted) VALUES (?1, 'multipass', 'alice@exemple.org', 1)", vec![alice.clone().into()])
            .await
            .unwrap();

        let work = "ab".repeat(32);
        let wrap = wrap_thank_you(&ALICE_SK, &alice, &bob, &work, "Ortie", "creation", 12, Some("Super fiche !"), false, "tx-1", 1_790_000_000).unwrap();
        let opened = unwrap(&BOB_SK, &wrap).unwrap();
        let receipt = parse_thank_you(&opened.rumor, &opened.sender, &bob).unwrap();
        assert_eq!((receipt.deci_zen, receipt.role.as_str(), receipt.note.as_deref()), (12, "creation", Some("Super fiche !")));

        assert!(record_received(&db, &receipt).await);
        assert!(!record_received(&db, &receipt).await, "même tx_ref ignoré (idempotence)");

        let relay_receipt = ThankYouReceipt {
            tx_ref: "tx-2".into(),
            to: bob.clone(),
            from: None,
            work_sha: work.clone(),
            title: "Ortie".into(),
            role: "relay".into(),
            deci_zen: 5,
            note: None,
            anonymous: true,
            at: 1_790_000_010,
        };
        assert!(record_received(&db, &relay_receipt).await);

        let sum = earnings_summary(&db, &bob).await.unwrap();
        assert_eq!((sum["total_received_deci_zen"].as_u64(), sum["creation_deci_zen"].as_u64(), sum["relay_deci_zen"].as_u64()), (Some(17), Some(12), Some(5)));
        assert_eq!(sum["recent"].as_array().unwrap().len(), 2);
        assert_eq!(sum["recent"][1]["from_label"].as_str(), Some("alice"));
        assert!(sum["recent"][0]["from_label"].is_null());

        // Ardoise (#3)
        let e = SlateEntry { path: "/Communs/ortie.media.json".into(), work_sha: work.clone(), title: "Ortie".into(), amount_deci_zen: 5, note: None, anonymous: false, added_at: 100 };
        let l1 = slate_upsert(&db, &alice, e.clone(), true).await.unwrap();
        assert_eq!(l1[0].amount_deci_zen, 5);
        let l2 = slate_upsert(&db, &alice, e, true).await.unwrap();
        assert_eq!(l2[0].amount_deci_zen, 10, "cumule les appuis rapides");
        assert!(slate_remove(&db, &alice, "/Communs/ortie.media.json").await.unwrap().is_empty());

        // UMAP (#6) + reçu 30852 (#7)
        assert_eq!(parse_umap("_43.61_1.44"), Some((43.61, 1.44)));
        assert_eq!(parse_umap("_43.6149_1.4449"), Some((43.61, 1.44)));
        assert_eq!(parse_umap("43.61,1.44"), None);
        let ev = build_receipt_event(&ALICE_SK, &work, "cid-1", 25, 3, Some("0x01"), 1_790_000_020).unwrap();
        assert_eq!(ev.kind, CASCADE_RECEIPT_KIND);
        assert!(ev.verify().is_ok());

        let _ = std::fs::remove_dir_all(dir);
    }
}
