//! **Suppression complète d'un MULTIPASS** par le Capitaine (écran Membres) : le compte et toutes ses données, sans retour.
//!
//! Permise seulement pour un MULTIPASS resté inactif 30 jours ou déjà retiré ([`crate::member_activity`]). L'ordre va du
//! réversible à l'irréversible :
//! 1. ses Ğ1 sont reversées au portefeuille primal (fait par l'appelant, avant tout le reste) ;
//! 2. **adieu Nostr** ([`farewell`]) : signé de **sa** clé, un profil (kind 0) vidé et des suppressions NIP-09 (kind 5) de tout
//!    ce qu'il a publié. Rangés ici d'abord (la base efface et retient de ne plus les reprendre), puis envoyés aux relais de
//!    la constellation par l'appelant : la vérité, c'est Nostr ; elle dit maintenant que ces événements n'existent plus ;
//! 3. **ses données** ([`purge`]) : uCloud et trousseau, recherche (texte lu, images regardées, plantes, paquet
//!    `/_/Recherche`), visages, mémoire de BRO, chat, carnet, partages, sauvegardes, appareils, réglages (`kv`), événements
//!    restants et messages privés qui lui étaient adressés ; les blobs que plus rien ne référence (rendus par [`purge`])
//!    sont effacés par l'appelant — le bassin partagé garde ceux qu'un autre plan référence, le miroir Kubo désépingle au
//!    passage suivant ce que la base ne référence plus ;
//! 4. en dernier, ses **clés** : dossier du compte (ou celui de la corbeille, s'il était retiré) effacé, identité oubliée.
//!
//! Après cela, un `replay` ne le fait pas revenir : ses manifestes et son DID sont supprimés par ses propres kind 5, et
//! l'identité hébergée (état local) n'existe plus.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use astro_db::Db;
use astro_nostr_proto::{Event, UnsignedEvent};
use rusqlite::{OptionalExtension, params};

/// Étiquettes au plus par suppression (kind 5) : un compte très actif en signe plusieurs.
const TAGS_PER_DELETION: usize = 400;

/// Tables vidées des lignes du membre : `(table, colonne qui le désigne)`. Une table absente (ancienne base) est passée.
const OWNED: &[(&str, &str)] = &[
    ("ucloud_entries", "owner_hex"),
    ("ucloud_keyring", "owner_hex"),
    ("ucloud_sharing", "owner_hex"),
    ("identity_links", "owner_hex"),
    ("contact_cards", "owner_hex"),
    ("chat_messages", "owner_hex"),
    ("chat_groups", "owner_hex"),
    ("chat_reactions", "owner_hex"),
    ("chat_prefs", "owner_hex"),
    ("face_shares", "owner_hex"),
    ("face_share_pending", "owner_hex"),
    ("invites", "owner_hex"),
    ("meetings", "owner_hex"),
    ("faces", "owner_hex"),
    ("memory", "owner_hex"),
    ("backup_pacts", "owner_hex"),
    ("backup_items", "owner_hex"),
    ("backup_packets", "owner_hex"),
    ("backup_blobs", "owner_hex"),
    ("backup_compact", "owner_hex"),
    ("backup_frags", "owner_hex"),
    ("backup_index", "owner_hex"),
    ("blob_copies", "owner_hex"),
    ("media_index", "owner_hex"),
    ("friend_media", "owner_hex"),
    ("friend_media_sync", "owner_hex"),
    ("media_requests", "owner_hex"),
    ("ai_usage", "owner_hex"),
    ("dav_changes", "owner_hex"),
    ("pull_imported", "owner_hex"),
    ("organize_runs", "owner_hex"),
    ("organize_moves", "owner_hex"),
    ("dav_tokens", "owner_hex"),
    ("public_blobs", "owner"),
    ("site_visits", "hex"),
    ("search_index", "owner_hex"),
    ("search_frames", "owner_hex"),
    ("plant_ids", "owner_hex"),
    ("index_members", "owner_hex"),
    ("index_delegation", "owner_hex"),
    ("social_graph", "owner_hex"),
    ("g1_snapshot", "hex"),
];

/// Tables de vecteurs (`sqlite-vec`, partitionnées par membre) : `(table, clé primaire)`.
const VECTORS: &[(&str, &str)] = &[("face_vectors", "face_id"), ("memory_vectors", "mem_id"), ("ucloud_desc_vectors", "emb_id"), ("dream_vectors", "dream_id"), ("search_vision_vectors", "vis_id")];

/// Ce que la suppression emporte, dit au Capitaine avant qu'il confirme.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Loss {
    /// Fichiers du uCloud.
    pub files: i64,
    /// Octets (uCloud et images publiques).
    pub bytes: i64,
    /// Événements Nostr publiés par lui et gardés ici.
    pub events: i64,
}

/// Mesure ce que la suppression emporterait.
pub fn loss(c: &rusqlite::Connection, hex: &str) -> rusqlite::Result<Loss> {
    c.query_row(
        "SELECT (SELECT COUNT(*) FROM ucloud_entries WHERE owner_hex = ?1 AND type = 'file'), \
                COALESCE((SELECT SUM(size) FROM ucloud_entries WHERE owner_hex = ?1 AND type = 'file'), 0) + COALESCE((SELECT SUM(size) FROM public_blobs WHERE owner = ?1), 0), \
                (SELECT COUNT(*) FROM nostr_events WHERE pubkey = ?1 AND kind <> 5)",
        [hex],
        |r| Ok(Loss { files: r.get(0)?, bytes: r.get(1)?, events: r.get(2)? }),
    )
}

/// Signe l'adieu Nostr du compte : un profil vidé puis les suppressions (kind 5) de tous ses événements gardés ici, et les
/// range dans la base (les événements visés disparaissent et ne seront plus repris). Rend les événements, à publier.
pub async fn farewell(db: &Db, hex: &str, secret: &[u8; 32], now: u64) -> Result<Vec<Event>, String> {
    let h = hex.to_owned();
    let mine: Vec<(String, u32, Option<String>)> = db
        .readers()
        .read(move |c| {
            let mut stmt = c.prepare("SELECT id, kind, d_tag FROM nostr_events WHERE pubkey = ?1 AND kind <> 5 ORDER BY seq")?;
            let rows = stmt.query_map([&h], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .await
        .map_err(|e| e.to_string())?;
    let mut tags: Vec<Vec<String>> = Vec::new();
    let mut seen = HashSet::new();
    for (id, kind, d) in mine {
        let tag = match kind {
            0 => continue, // remplacé par le profil vidé
            3 | 10_000..=19_999 => vec!["a".to_owned(), format!("{kind}:{hex}:")],
            30_000..=39_999 => vec!["a".to_owned(), format!("{kind}:{hex}:{}", d.unwrap_or_default())],
            _ => vec!["e".to_owned(), id],
        };
        if seen.insert(tag[1].clone()) {
            tags.push(tag);
        }
    }
    let sign = |kind: u32, tags: Vec<Vec<String>>, content: &str, at: u64| UnsignedEvent { created_at: at, kind, tags, content: content.to_owned() }.sign(secret).map_err(|e| format!("signature de l'adieu : {e}"));
    let mut out = vec![sign(0, Vec::new(), r#"{"name":"","about":"","deleted":true}"#, now)?];
    for chunk in tags.chunks(TAGS_PER_DELETION) {
        out.push(sign(5, chunk.to_vec(), "compte supprimé par sa station", now + 1)?);
    }
    let events = out.clone();
    db.writer()
        .transaction(move |tx| {
            for e in &events {
                astro_db::events::insert_event(tx, e)?;
            }
            Ok(())
        })
        .await
        .map_err(|e| format!("adieu Nostr non rangé : {e}"))?;
    Ok(out)
}

/// Efface toutes les données du membre `hex` en une transaction (voir le module) et rend les blobs que **cette base** ne
/// référence plus : à effacer par l'appelant ([`astro_dav::BlobStore::remove`], qui garde ce qu'un plan voisin référence).
/// L'identité elle-même n'est pas touchée (voir [`forget_identity`]).
pub async fn purge(db: &Db, hex: &str) -> Result<Vec<String>, String> {
    let h = hex.to_owned();
    db.writer()
        .transaction(move |tx| {
            let before = astro_dav::pool::referenced_set_of(tx).map_err(astro_db::DbError::Corrupt)?;
            let exists = |t: &str| -> rusqlite::Result<bool> { Ok(tx.query_row("SELECT 1 FROM sqlite_master WHERE name = ?1", [t], |_| Ok(())).optional()?.is_some()) };
            // Vecteurs d'abord (leurs clés se lisent dans la partition du membre), texte lu, puis les tables ordinaires.
            for (t, id) in VECTORS {
                if exists(t)? {
                    tx.execute(&format!("DELETE FROM {t} WHERE {id} IN (SELECT {id} FROM {t} WHERE owner_hex = ?1)"), [&h])?;
                }
            }
            tx.execute("DELETE FROM search_text WHERE rowid IN (SELECT rowid FROM search_text WHERE owner_hex = ?1)", [&h])?;
            // Descriptions calculées des fichiers qu'il était seul à avoir.
            tx.execute(
                "DELETE FROM ucloud_embeddings WHERE cid IN (SELECT cid FROM ucloud_entries WHERE owner_hex = ?1 AND cid IS NOT NULL) \
                 AND NOT EXISTS (SELECT 1 FROM ucloud_entries o WHERE o.cid = ucloud_embeddings.cid AND o.owner_hex <> ?1)",
                [&h],
            )?;
            for (t, col) in OWNED {
                if exists(t)? {
                    tx.execute(&format!("DELETE FROM {t} WHERE {col} = ?1"), [&h])?;
                }
            }
            tx.execute("DELETE FROM tasks WHERE CASE WHEN json_valid(payload) THEN json_extract(payload, '$.owner') END = ?1", [&h])?;
            // Réglages et traces du membre, quel que soit leur domaine (clé = sa clé publique, ou la préfixe).
            // `member_removed` reste jusqu'à l'oubli de l'identité : une suppression interrompue peut être reprise.
            tx.execute("DELETE FROM kv WHERE (key = ?1 OR key LIKE ?1 || ':%') AND scope <> 'member_removed'", [&h])?;
            // Ses événements restants (hors adieu) et les messages privés qui lui étaient adressés : plus servis.
            tx.execute("DELETE FROM nostr_events WHERE pubkey = ?1 AND kind NOT IN (0, 5)", [&h])?;
            tx.execute("DELETE FROM nostr_events WHERE kind IN (4, 14, 1059) AND seq IN (SELECT event_id FROM nostr_tags WHERE name = 'p' AND value = ?1)", [&h])?;
            let after = astro_dav::pool::referenced_set_of(tx).map_err(astro_db::DbError::Corrupt)?;
            Ok(before.difference(&after).cloned().collect())
        })
        .await
        .map_err(|e| format!("données du compte non effacées : {e}"))
}

/// Oublie l'identité hébergée (dernière étape) et note la suppression dans le journal (sans rien de personnel que sa clé publique).
pub async fn forget_identity(db: &Db, hex: &str, at: i64, freed: usize) -> Result<(), String> {
    let h = hex.to_owned();
    db.writer()
        .transaction(move |tx| {
            tx.execute("DELETE FROM identities WHERE hex = ?1", [&h])?;
            tx.execute("DELETE FROM kv WHERE key = ?1", [&h])?;
            tx.execute("INSERT INTO journal(stream, at, data) VALUES ('member_deleted', ?1, ?2)", params![at, serde_json::json!({ "hex": h, "blobs": freed }).to_string()])?;
            Ok(())
        })
        .await
        .map_err(|e| format!("identité non oubliée : {e}"))
}

/// Dossier des clés du compte : `nostr/<email>`, sinon le plus récent de la corbeille (`trash/removed/<email>.<date>`).
pub fn key_dir(nostr_root: &Path, email: &str) -> Option<PathBuf> {
    let live = nostr_root.join(email);
    if live.is_dir() {
        return Some(live);
    }
    let trash = crate::trash::root(nostr_root.parent()?).join(crate::member_activity::TRASH_REASON);
    let prefix = format!("{email}.");
    std::fs::read_dir(trash).ok()?.flatten().map(|e| e.path()).filter(|p| p.is_dir() && p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(&prefix))).max()
}

/// Tous les dossiers de clés du compte (vivant et corbeille), à effacer en dernier.
pub fn key_dirs(nostr_root: &Path, email: &str) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = vec![nostr_root.join(email)].into_iter().filter(|p| p.is_dir()).collect();
    if let Some(game) = nostr_root.parent() {
        let prefix = format!("{email}.");
        for reason in [crate::member_activity::TRASH_REASON, "ejected"] {
            if let Ok(rd) = std::fs::read_dir(crate::trash::root(game).join(reason)) {
                out.extend(rd.flatten().map(|e| e.path()).filter(|p| p.is_dir() && p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(&prefix))));
            }
        }
    }
    out
}
