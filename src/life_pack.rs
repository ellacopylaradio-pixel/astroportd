//! **Paquets de vie** de chaque membre : ce qui n'existe que dans la base de la station et que rien d'autre ne sait refaire —
//! les **visages nommés** (`faces`), l'**historique du chat** (`chat_messages`, `chat_groups`, `chat_reactions`, `chat_prefs`,
//! effacements `kv` chat_deleted / chat_cleared) et la **mémoire de BRO** (`memory`). Sur le modèle de [`crate::search_pack`],
//! chacun est écrit dans **son** uCloud, sous le tiroir système :
//!
//! - [`Part::Faces`] → `/_/Visages/faces.astroidx`
//! - [`Part::Chat`] → `/_/Messages/chat.astroidx`
//! - [`Part::Memory`] → `/_/Memoire/memory.astroidx`
//!
//! Trois paquets plutôt qu'un : chacun ne se réécrit que s'il a changé (le chat bouge chaque jour, les visages rarement), un
//! paquet abîmé n'entraîne pas les deux autres, et leurs tailles font d'elles-mêmes la répartition ([`sizes`]).
//!
//! Ce sont des fichiers comme les autres : chiffrés, comptés dans l'espace du membre, emportés **tels quels** par la sauvegarde
//! chez un ami (`private_backup` ne retient que `/Partages` et `/Sauvegardes`) et par la restauration.
//!
//! - **format** : une ligne d'en-tête JSON `{"astroidx":1,"part":"faces","owner":<hex>,"at":<s>}`, puis une ligne JSON par
//!   enregistrement ([`Line`], étiquette `t`). Les vecteurs (visages 512, mémoire 1024 f32) voyagent en base64 petit-boutiste :
//!   sans eux, une photo réanalysée redonnerait « Inconnu » à un visage nommé et un souvenir ne serait plus retrouvé ;
//! - **écrire** : quand le contenu a changé (empreinte SHA-256 du corps), au plus une fois toutes les [`REWRITE_SECS`] (le
//!   premier paquet tout de suite), jamais pendant une restauration ;
//! - **relire** : quand le paquet du uCloud n'est pas celui que la station a écrit ou déjà relu (`cid` différent). La fusion est
//!   **idempotente** : rien de ce qui est ici n'est écrasé (un visage déjà nommé ici garde son nom, un message garde son texte
//!   sauf modification plus récente), les effacements du membre s'appliquent, un souvenir ne repasse jamais d'effacé à actif ;
//! - **jamais d'écrasement** : tant qu'un paquet revenu n'est pas lisible (blob encore en route), il n'est pas réécrit ; un
//!   paquet illisible ou d'un autre compte est refusé sans rien toucher, puis remplacé par le prochain paquet écrit.

use std::sync::Arc;

use astro_dav::{BlobStore, DavState};
use astro_db::{Db, VectorTable};
use base64::Engine as _;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

/// Taille maximale d'un paquet (écrit ou relu).
pub const MAX_PACK: usize = 96 << 20;
/// Écart minimal entre deux écritures d'un même paquet (la sauvegarde chez l'ami ne part qu'une fois par nuit).
pub const REWRITE_SECS: i64 = 20 * 3600;
/// Version du format.
const VERSION: u32 = 1;
/// Espace `kv` de ce que la station sait des paquets (clé `<owner>:<part>`).
const SCOPE: &str = "life_pack";

/// Un des trois paquets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    /// Visages nommés.
    Faces,
    /// Historique du chat.
    Chat,
    /// Mémoire de BRO.
    Memory,
}

impl Part {
    /// Les trois paquets.
    pub const ALL: [Part; 3] = [Part::Faces, Part::Chat, Part::Memory];

    /// Nom court (en-tête, `kv`).
    pub fn tag(self) -> &'static str {
        match self {
            Part::Faces => "faces",
            Part::Chat => "chat",
            Part::Memory => "memory",
        }
    }

    /// Dossier du paquet (tiroir système).
    pub fn dir(self) -> &'static str {
        match self {
            Part::Faces => "/_/Visages",
            Part::Chat => "/_/Messages",
            Part::Memory => "/_/Memoire",
        }
    }

    /// Nom du fichier.
    pub fn name(self) -> &'static str {
        match self {
            Part::Faces => "faces.astroidx",
            Part::Chat => "chat.astroidx",
            Part::Memory => "memory.astroidx",
        }
    }

    /// Chemin complet dans l'uCloud.
    pub fn path(self) -> String {
        format!("{}/{}", self.dir(), self.name())
    }
}

/// Un visage nommé (ou rattaché à une fiche, à une clé).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FaceLine {
    /// Identifiant déterministe du visage (empreinte de l'embedding).
    pub embed_hash: String,
    /// Nom donné.
    pub name: Option<String>,
    /// Clé Nostr de la personne.
    pub pubkey: Option<String>,
    /// `contact` ou `shared`.
    #[serde(rename = "ref")]
    pub reference: Option<String>,
    /// Adresse.
    pub email: Option<String>,
    /// UID de la fiche de contact : le lien fait foi.
    pub contact_uid: Option<String>,
    /// CID de la photo.
    pub cid: Option<String>,
    /// Chemin d'origine.
    pub source_path: Option<String>,
    /// Photo d'origine dans le VFS.
    pub ucloud_path: Option<String>,
    /// CID source.
    pub src_cid: Option<String>,
    /// Score de détection.
    pub det_score: Option<f64>,
    /// Cadre `[x1, y1, x2, y2]` dans l'image originale.
    pub bbox: Option<[f64; 4]>,
    /// Date de détection.
    pub ts: i64,
    /// Vecteur (512 f32, base64).
    pub v: String,
}

/// Un message du chat.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MsgLine {
    /// Identifiant de la rumeur.
    pub id: String,
    /// L'autre personne.
    pub peer: String,
    /// Écrit par le titulaire.
    pub mine: bool,
    /// Date.
    pub at: i64,
    /// Texte.
    pub text: String,
    /// `ok` ou `failed`.
    pub state: String,
    /// Lu.
    pub read: bool,
    /// Ordre d'insertion d'origine.
    pub n: i64,
    /// Message cité.
    pub reply: Option<String>,
    /// Pièces jointes (JSON).
    pub meta: Option<String>,
    /// Accusé de lecture reçu.
    pub seen: i64,
    /// Auteur (groupe).
    pub author: Option<String>,
    /// Modifié après l'envoi.
    pub edited: i64,
}

/// Une conversation de groupe.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupLine {
    /// Identifiant (empreinte des participants).
    pub id: String,
    /// Nom.
    pub name: Option<String>,
    /// Participants (JSON).
    pub members: String,
}

/// Une réaction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReactionLine {
    /// Message.
    pub msg_id: String,
    /// Auteur.
    pub author: String,
    /// Émoji.
    pub emoji: String,
    /// Date.
    pub at: i64,
}

/// Préférences d'une conversation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrefLine {
    /// Conversation.
    pub peer: String,
    /// Épinglée.
    pub pinned: bool,
    /// Archivée.
    pub archived: bool,
    /// En sourdine.
    pub muted: bool,
}

/// Un effacement voulu par le membre (`kv` chat_deleted : un message ; chat_cleared : une conversation jusqu'à une date).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TombLine {
    /// `chat_deleted` ou `chat_cleared`.
    pub scope: String,
    /// Identifiant du message ou clé du correspondant (sans le préfixe `<owner>:`).
    pub key: String,
    /// Valeur (`1`, ou la date d'effacement).
    pub value: String,
    /// Date d'écriture.
    pub at: i64,
}

/// Un souvenir de BRO (y compris ses pierres tombales : un souvenir effacé ne doit pas revenir).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryLine {
    /// Identifiant stable.
    pub point_id: String,
    /// Contenu.
    pub content: String,
    /// Empreinte du contenu.
    pub content_hash: String,
    /// Date.
    pub ts: i64,
    /// Événement d'origine.
    pub event_id: Option<String>,
    /// Saison.
    pub season: Option<String>,
    /// Importance.
    pub importance: Option<f64>,
    /// `fact`, `episode`, `trait`, `person`.
    pub kind: Option<String>,
    /// Source.
    pub source: Option<String>,
    /// `private`, `love`, `public`.
    pub visibility: Option<String>,
    /// Confiance.
    pub confidence: Option<f64>,
    /// `active`, `superseded`, `deleted`.
    pub status: String,
    /// Modèle d'embedding.
    pub embed_model: Option<String>,
    /// Version du schéma BRO.
    pub schema_v: Option<i64>,
    /// Origine.
    pub origin: Option<String>,
    /// Case.
    pub slot: Option<i64>,
    /// CID source.
    pub src_cid: Option<String>,
    /// Vecteur (1024 f32, base64) ; absent si la base n'en avait pas.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub v: Option<String>,
}

/// Une ligne d'un paquet.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Line {
    /// Visage (paquet `faces`).
    Face(FaceLine),
    /// Message (paquet `chat`).
    Msg(MsgLine),
    /// Groupe (paquet `chat`).
    Group(GroupLine),
    /// Réaction (paquet `chat`).
    Reaction(ReactionLine),
    /// Préférences (paquet `chat`).
    Pref(PrefLine),
    /// Effacement (paquet `chat`).
    Tomb(TombLine),
    /// Souvenir (paquet `memory`).
    Memory(MemoryLine),
}

impl Line {
    fn part(&self) -> Part {
        match self {
            Line::Face(_) => Part::Faces,
            Line::Memory(_) => Part::Memory,
            _ => Part::Chat,
        }
    }

    /// Vérifie ce que les contraintes de la base refuseraient : un paquet abîmé est refusé en entier, avant toute écriture.
    fn check(&self) -> Result<(), String> {
        let one_of = |v: Option<&str>, set: &[&str], what: &str| match v {
            Some(s) if !set.contains(&s) => Err(format!("{what} inconnu : {s}")),
            _ => Ok(()),
        };
        match self {
            Line::Face(f) => {
                one_of(f.reference.as_deref(), &["contact", "shared"], "lien")?;
                vector(&f.v, VectorTable::Face).map(|_| ())
            }
            Line::Msg(m) => one_of(Some(&m.state), &["ok", "failed"], "état"),
            Line::Tomb(t) => one_of(Some(&t.scope), &["chat_deleted", "chat_cleared"], "effacement"),
            Line::Memory(m) => {
                one_of(m.kind.as_deref(), &["fact", "episode", "trait", "person"], "type")?;
                one_of(m.visibility.as_deref(), &["private", "love", "public"], "visibilité")?;
                one_of(Some(&m.status), &["active", "superseded", "deleted"], "statut")?;
                m.v.as_deref().map_or(Ok(()), |v| vector(v, VectorTable::Memory).map(|_| ()))
            }
            Line::Group(_) | Line::Reaction(_) | Line::Pref(_) => Ok(()),
        }
    }
}

/// Décode un vecteur base64 et vérifie sa dimension.
fn vector(v: &str, table: VectorTable) -> Result<Vec<u8>, String> {
    let b = base64::engine::general_purpose::STANDARD.decode(v).map_err(|e| format!("vecteur illisible : {e}"))?;
    if b.len() != table.dim() * 4 {
        return Err("vecteur de mauvaise dimension".into());
    }
    Ok(b)
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

/// Les lignes du paquet `part` de `owner`, dans un ordre stable.
pub fn lines(c: &rusqlite::Connection, owner: &str, part: Part) -> rusqlite::Result<Vec<Line>> {
    let b64 = base64::engine::general_purpose::STANDARD;
    let mut out = Vec::new();
    match part {
        Part::Faces => {
            // Seuls les visages qui portent un nom, une fiche ou une clé : les inconnus se recalculent de la photo.
            let mut stmt = c.prepare_cached(
                "SELECT f.embed_hash, f.name, f.pubkey, f.ref, f.email, f.contact_uid, f.cid, f.source_path, f.ucloud_path, f.src_cid, f.det_score, \
                        f.bbox_x1, f.bbox_y1, f.bbox_x2, f.bbox_y2, f.ts, (SELECT v.embedding FROM face_vectors v WHERE v.face_id = f.face_id) \
                 FROM faces f WHERE f.owner_hex = ?1 \
                   AND ((f.name IS NOT NULL AND substr(f.name, 1, 8) <> 'Inconnu_') OR f.contact_uid IS NOT NULL OR f.pubkey IS NOT NULL) \
                 ORDER BY f.embed_hash",
            )?;
            let rows = stmt.query_map([owner], |r| {
                let bbox = match (r.get::<_, Option<f64>>(11)?, r.get::<_, Option<f64>>(12)?, r.get::<_, Option<f64>>(13)?, r.get::<_, Option<f64>>(14)?) {
                    (Some(a), Some(b), Some(c), Some(d)) => Some([a, b, c, d]),
                    _ => None,
                };
                let Some(v) = r.get::<_, Option<Vec<u8>>>(16)? else { return Ok(None) };
                Ok(Some(FaceLine {
                    embed_hash: r.get(0)?,
                    name: r.get(1)?,
                    pubkey: r.get(2)?,
                    reference: r.get(3)?,
                    email: r.get(4)?,
                    contact_uid: r.get(5)?,
                    cid: r.get(6)?,
                    source_path: r.get(7)?,
                    ucloud_path: r.get(8)?,
                    src_cid: r.get(9)?,
                    det_score: r.get(10)?,
                    bbox,
                    ts: r.get(15)?,
                    v: b64.encode(v),
                }))
            })?;
            // Un visage sans vecteur n'existe pas (le pipeline écrit les deux ensemble) : rien à emporter.
            for f in rows {
                if let Some(f) = f? {
                    out.push(Line::Face(f));
                }
            }
        }
        Part::Chat => {
            let mut stmt = c.prepare_cached("SELECT id, peer, mine, at, text, state, read, n, reply, meta, seen, author, edited FROM chat_messages WHERE owner_hex = ?1 ORDER BY n, id")?;
            for m in stmt.query_map([owner], |r| {
                Ok(MsgLine {
                    id: r.get(0)?,
                    peer: r.get(1)?,
                    mine: r.get::<_, i64>(2)? != 0,
                    at: r.get(3)?,
                    text: r.get(4)?,
                    state: r.get(5)?,
                    read: r.get::<_, i64>(6)? != 0,
                    n: r.get(7)?,
                    reply: r.get(8)?,
                    meta: r.get(9)?,
                    seen: r.get(10)?,
                    author: r.get(11)?,
                    edited: r.get(12)?,
                })
            })? {
                out.push(Line::Msg(m?));
            }
            let mut stmt = c.prepare_cached("SELECT id, name, members FROM chat_groups WHERE owner_hex = ?1 ORDER BY id")?;
            for g in stmt.query_map([owner], |r| Ok(GroupLine { id: r.get(0)?, name: r.get(1)?, members: r.get(2)? }))? {
                out.push(Line::Group(g?));
            }
            let mut stmt = c.prepare_cached("SELECT msg_id, author, emoji, at FROM chat_reactions WHERE owner_hex = ?1 ORDER BY msg_id, author")?;
            for x in stmt.query_map([owner], |r| Ok(ReactionLine { msg_id: r.get(0)?, author: r.get(1)?, emoji: r.get(2)?, at: r.get(3)? }))? {
                out.push(Line::Reaction(x?));
            }
            let mut stmt = c.prepare_cached("SELECT peer, pinned, archived, muted FROM chat_prefs WHERE owner_hex = ?1 ORDER BY peer")?;
            for p in stmt.query_map([owner], |r| Ok(PrefLine { peer: r.get(0)?, pinned: r.get::<_, i64>(1)? != 0, archived: r.get::<_, i64>(2)? != 0, muted: r.get::<_, i64>(3)? != 0 }))? {
                out.push(Line::Pref(p?));
            }
            let mut stmt = c.prepare_cached(
                "SELECT scope, substr(key, length(?1) + 2), value, updated_at FROM kv \
                 WHERE scope IN ('chat_deleted', 'chat_cleared') AND substr(key, 1, length(?1) + 1) = ?1 || ':' ORDER BY scope, key",
            )?;
            for t in stmt.query_map([owner], |r| Ok(TombLine { scope: r.get(0)?, key: r.get(1)?, value: r.get(2)?, at: r.get(3)? }))? {
                out.push(Line::Tomb(t?));
            }
        }
        Part::Memory => {
            let mut stmt = c.prepare_cached(
                "SELECT m.point_id, m.content, m.content_hash, m.ts, m.event_id, m.season, m.importance, m.kind, m.source, m.visibility, m.confidence, m.status, \
                        m.embed_model, m.schema_v, m.origin, m.slot, m.src_cid, (SELECT v.embedding FROM memory_vectors v WHERE v.mem_id = m.mem_id) \
                 FROM memory m WHERE m.owner_hex = ?1 ORDER BY m.point_id",
            )?;
            for m in stmt.query_map([owner], |r| {
                Ok(MemoryLine {
                    point_id: r.get(0)?,
                    content: r.get(1)?,
                    content_hash: r.get(2)?,
                    ts: r.get(3)?,
                    event_id: r.get(4)?,
                    season: r.get(5)?,
                    importance: r.get(6)?,
                    kind: r.get(7)?,
                    source: r.get(8)?,
                    visibility: r.get(9)?,
                    confidence: r.get(10)?,
                    status: r.get(11)?,
                    embed_model: r.get(12)?,
                    schema_v: r.get(13)?,
                    origin: r.get(14)?,
                    slot: r.get(15)?,
                    src_cid: r.get(16)?,
                    v: r.get::<_, Option<Vec<u8>>>(17)?.map(|v| b64.encode(v)),
                })
            })? {
                out.push(Line::Memory(m?));
            }
        }
    }
    Ok(out)
}

/// Corps d'un paquet (une ligne JSON par enregistrement) ; `None` : rien à emporter.
fn body(c: &rusqlite::Connection, owner: &str, part: Part) -> Result<Option<Vec<u8>>, astro_db::DbError> {
    let lines = lines(c, owner, part)?;
    if lines.is_empty() {
        return Ok(None);
    }
    let mut out = Vec::new();
    for l in &lines {
        // Sérialiser des chaînes, des nombres et des booléens ne peut pas échouer ; si cela arrivait, mieux vaut ne rien écrire.
        let Ok(bytes) = serde_json::to_vec(l) else { return Ok(None) };
        out.extend(bytes);
        out.push(b'\n');
    }
    Ok(Some(out))
}

/// Le paquet complet (en-tête + corps) de `part` pour `owner`. `None` : rien à emporter.
pub fn export(c: &rusqlite::Connection, owner: &str, part: Part) -> Result<Option<Vec<u8>>, astro_db::DbError> {
    Ok(body(c, owner, part)?.map(|b| with_head(owner, part, &b)))
}

fn with_head(owner: &str, part: Part, body: &[u8]) -> Vec<u8> {
    let mut out = json!({ "astroidx": VERSION, "part": part.tag(), "owner": owner, "at": now() }).to_string().into_bytes();
    out.push(b'\n');
    out.extend_from_slice(body);
    out
}

/// Lit un paquet ; refuse une autre version, un autre paquet, le paquet d'un autre compte, ou la moindre ligne abîmée.
pub fn parse(bytes: &[u8], owner: &str, part: Part) -> Result<Vec<Line>, String> {
    if bytes.len() > MAX_PACK {
        return Err("paquet trop gros".into());
    }
    let mut it = bytes.split(|b| *b == b'\n').filter(|l| !l.is_empty());
    let head: serde_json::Value = serde_json::from_slice(it.next().ok_or("paquet vide")?).map_err(|e| format!("en-tête illisible : {e}"))?;
    if head["astroidx"] != VERSION || head["part"] != part.tag() || head["owner"] != owner {
        return Err("paquet d'une autre version, d'une autre sorte ou d'un autre compte".into());
    }
    it.map(|l| {
        let line = serde_json::from_slice::<Line>(l).map_err(|e| format!("ligne illisible : {e}"))?;
        if line.part() != part {
            return Err(format!("ligne étrangère au paquet {}", part.tag()));
        }
        line.check()?;
        Ok(line)
    })
    .collect()
}

/// Fusionne `lines` dans la base pour `owner`, en une transaction (tout ou rien). Rend le nombre de lignes qui ont changé
/// quelque chose ; relire le même paquet une seconde fois rend 0.
pub async fn import(db: &Db, owner: &str, lines: Vec<Line>) -> Result<usize, astro_db::DbError> {
    let owner = owner.to_owned();
    db.writer()
        .transaction(move |tx| {
            let mut changed = 0;
            // Les effacements d'abord : un message effacé par le membre ne doit pas revenir avec le reste.
            let (tombs, rest): (Vec<Line>, Vec<Line>) = lines.into_iter().partition(|l| matches!(l, Line::Tomb(_)));
            for l in tombs.into_iter().chain(rest) {
                changed += match l {
                    Line::Face(f) => import_face(tx, &owner, &f)?,
                    Line::Memory(m) => import_memory(tx, &owner, &m)?,
                    Line::Tomb(t) => {
                        let key = format!("{owner}:{}", t.key);
                        if t.scope == "chat_deleted" {
                            tx.execute("INSERT OR IGNORE INTO kv(scope, key, value, updated_at) VALUES ('chat_deleted', ?1, ?2, ?3)", params![key, t.value, t.at])?
                                + tx.execute("DELETE FROM chat_messages WHERE owner_hex = ?1 AND id = ?2", params![owner, t.key])?
                        } else {
                            tx.execute(
                                "INSERT INTO kv(scope, key, value, updated_at) VALUES ('chat_cleared', ?1, ?2, ?3) \
                                 ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at \
                                 WHERE CAST(excluded.value AS INTEGER) > CAST(kv.value AS INTEGER)",
                                params![key, t.value, t.at],
                            )? + tx.execute("DELETE FROM chat_messages WHERE owner_hex = ?1 AND peer = ?2 AND at <= CAST(?3 AS INTEGER)", params![owner, t.key, t.value])?
                        }
                    }
                    Line::Msg(m) => tx.execute(
                        // Mêmes gardes que `chat::store_row` ; un message déjà là ne prend que ce qui avance (lu, accusé, modification).
                        "INSERT INTO chat_messages(owner_hex, id, peer, mine, at, text, state, read, n, reply, meta, seen, author, edited) \
                         SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, (SELECT COALESCE(MAX(n), 0) + 1 FROM chat_messages WHERE owner_hex = ?1), ?9, ?10, ?11, ?12, ?13 \
                         WHERE NOT EXISTS (SELECT 1 FROM kv WHERE scope = 'chat_cleared' AND key = ?1 || ':' || ?3 AND CAST(value AS INTEGER) >= ?5) \
                           AND NOT EXISTS (SELECT 1 FROM kv WHERE scope = 'chat_deleted' AND key = ?1 || ':' || ?2) \
                         ON CONFLICT(owner_hex, id) DO UPDATE SET \
                           read = max(chat_messages.read, excluded.read), seen = max(chat_messages.seen, excluded.seen), \
                           text = CASE WHEN excluded.edited > chat_messages.edited THEN excluded.text ELSE chat_messages.text END, \
                           edited = max(chat_messages.edited, excluded.edited), \
                           state = CASE WHEN excluded.state = 'ok' THEN 'ok' ELSE chat_messages.state END \
                         WHERE excluded.read > chat_messages.read OR excluded.seen > chat_messages.seen OR excluded.edited > chat_messages.edited \
                            OR (excluded.state = 'ok' AND chat_messages.state = 'failed')",
                        params![owner, m.id, m.peer, i64::from(m.mine), m.at, m.text, m.state, i64::from(m.read), m.reply, m.meta, m.seen, m.author, m.edited],
                    )?,
                    Line::Group(g) => tx.execute("INSERT OR IGNORE INTO chat_groups(owner_hex, id, name, members) VALUES (?1, ?2, ?3, ?4)", params![owner, g.id, g.name, g.members])?,
                    Line::Reaction(x) => tx.execute(
                        "INSERT INTO chat_reactions(owner_hex, msg_id, author, emoji, at) VALUES (?1, ?2, ?3, ?4, ?5) \
                         ON CONFLICT(owner_hex, msg_id, author) DO UPDATE SET emoji = excluded.emoji, at = excluded.at WHERE excluded.at > chat_reactions.at",
                        params![owner, x.msg_id, x.author, x.emoji, x.at],
                    )?,
                    Line::Pref(p) => tx.execute(
                        "INSERT OR IGNORE INTO chat_prefs(owner_hex, peer, pinned, archived, muted) VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![owner, p.peer, i64::from(p.pinned), i64::from(p.archived), i64::from(p.muted)],
                    )?,
                };
            }
            Ok(changed)
        })
        .await
}

/// Un visage : ajouté s'il n'est pas là ; s'il est là mais encore inconnu, il reprend le nom, la fiche et la clé du paquet.
/// Un visage nommé ici garde son nom.
fn import_face(tx: &rusqlite::Transaction<'_>, owner: &str, f: &FaceLine) -> rusqlite::Result<usize> {
    let here: Option<(i64, bool)> = tx
        .query_row(
            "SELECT face_id, (name IS NULL OR substr(name, 1, 8) = 'Inconnu_') AND contact_uid IS NULL AND pubkey IS NULL FROM faces WHERE owner_hex = ?1 AND embed_hash = ?2",
            params![owner, f.embed_hash],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    match here {
        Some((id, true)) => tx.execute(
            "UPDATE faces SET name = ?2, pubkey = ?3, ref = ?4, email = ?5, contact_uid = ?6, suggest_pubkey = NULL, suggest_name = NULL, suggest_score = NULL, suggest_contact_uid = NULL WHERE face_id = ?1",
            params![id, f.name, f.pubkey, f.reference, f.email, f.contact_uid],
        ),
        Some((_, false)) => Ok(0),
        None => {
            let Ok(blob) = vector(&f.v, VectorTable::Face) else { return Ok(0) }; // déjà vérifié par `parse`
            let [x1, y1, x2, y2] = f.bbox.map_or([None; 4], |b| b.map(Some));
            tx.execute(
                "INSERT INTO faces(owner_hex, cid, source_path, name, pubkey, ref, email, det_score, bbox_x1, bbox_y1, bbox_x2, bbox_y2, embed_hash, ts, src_cid, ucloud_path, contact_uid) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
                params![owner, f.cid, f.source_path, f.name, f.pubkey, f.reference, f.email, f.det_score, x1, y1, x2, y2, f.embed_hash, f.ts, f.src_cid, f.ucloud_path, f.contact_uid],
            )?;
            tx.execute("INSERT INTO face_vectors(face_id, owner_hex, embedding) VALUES (?1, ?2, ?3)", params![tx.last_insert_rowid(), owner, blob])?;
            Ok(1)
        }
    }
}

/// Un souvenir : ajouté s'il n'est pas là ; s'il est là, son statut ne fait qu'avancer (actif → remplacé → effacé).
fn import_memory(tx: &rusqlite::Transaction<'_>, owner: &str, m: &MemoryLine) -> rusqlite::Result<usize> {
    let here: Option<String> = tx.query_row("SELECT owner_hex FROM memory WHERE point_id = ?1", [&m.point_id], |r| r.get(0)).optional()?;
    match here {
        Some(o) if o == owner => tx.execute(
            "UPDATE memory SET status = ?2 WHERE point_id = ?1 \
               AND (CASE status WHEN 'active' THEN 0 WHEN 'superseded' THEN 1 ELSE 2 END) < (CASE ?2 WHEN 'active' THEN 0 WHEN 'superseded' THEN 1 ELSE 2 END)",
            params![m.point_id, m.status],
        ),
        Some(_) => Ok(0), // identifiant d'un autre compte : jamais touché
        None => {
            tx.execute(
                "INSERT INTO memory(point_id, owner_hex, content, content_hash, ts, event_id, season, importance, kind, source, visibility, confidence, status, embed_model, schema_v, origin, slot, src_cid) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
                params![m.point_id, owner, m.content, m.content_hash, m.ts, m.event_id, m.season, m.importance, m.kind, m.source, m.visibility, m.confidence, m.status, m.embed_model, m.schema_v, m.origin, m.slot, m.src_cid],
            )?;
            if let Some(blob) = m.v.as_deref().and_then(|v| vector(v, VectorTable::Memory).ok()) {
                tx.execute("INSERT INTO memory_vectors(mem_id, owner_hex, embedding) VALUES (?1, ?2, ?3)", params![tx.last_insert_rowid(), owner, blob])?;
            }
            Ok(1)
        }
    }
}

/// Octets occupés dans l'uCloud de `owner` par ses paquets `(visages, chat, mémoire)` : la taille en clair des fichiers
/// écrits, 0 tant qu'un paquet n'existe pas (pour l'écran de répartition de l'espace).
pub fn sizes(c: &rusqlite::Connection, owner: &str) -> rusqlite::Result<(u64, u64, u64)> {
    let mut stmt = c.prepare_cached("SELECT COALESCE(size, 0) FROM ucloud_entries WHERE owner_hex = ?1 AND path = ?2 AND type = 'file'")?;
    let mut size = |p: Part| -> rusqlite::Result<u64> { Ok(stmt.query_row(params![owner, p.path()], |r| r.get::<_, i64>(0)).optional()?.map_or(0, |n| u64::try_from(n).unwrap_or(0))) };
    Ok((size(Part::Faces)?, size(Part::Chat)?, size(Part::Memory)?))
}

/// Ce que la station sait d'un paquet (`kv` scope `life_pack`, clé `<owner>:<part>`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct Seen {
    /// CID du paquet écrit ou relu par cette station.
    cid: String,
    /// Empreinte (SHA-256) du corps écrit.
    stamp: String,
    /// Date de la dernière écriture par cette station.
    #[serde(default)]
    written_at: i64,
}

/// Bilan d'un tour.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    /// Paquets relus qui ont apporté quelque chose.
    pub read: usize,
    /// Paquets écrits.
    pub written: usize,
    /// Problèmes rencontrés (paquet refusé, écriture impossible) : à dire au Capitaine, jamais tus.
    pub problems: Vec<String>,
}

/// Un tour de paquets : relit les paquets revenus, puis écrit ceux qui ont changé, pour chaque compte hébergé.
pub async fn sync<B: BlobStore + 'static>(dav: &Arc<DavState<B>>) -> Result<Report, astro_db::DbError> {
    let owners: Vec<String> = dav.db.readers().read(|c| Ok(c.prepare_cached("SELECT hex FROM identities WHERE hosted = 1")?.query_map([], |r| r.get(0))?.collect::<Result<Vec<_>, _>>()?)).await?;
    let mut report = Report::default();
    for owner in owners {
        for part in Part::ALL {
            sync_one(dav, &owner, part, &mut report).await?;
        }
    }
    Ok(report)
}

async fn sync_one<B: BlobStore + 'static>(dav: &Arc<DavState<B>>, owner: &str, part: Part, report: &mut Report) -> Result<(), astro_db::DbError> {
    let path = part.path();
    let key = format!("{owner}:{}", part.tag());
    let (o, p, k) = (owner.to_owned(), path.clone(), key.clone());
    let (cid, seen): (Option<String>, Option<String>) = dav
        .db
        .readers()
        .read(move |c| {
            let cid = c.query_row("SELECT cid FROM ucloud_entries WHERE owner_hex = ?1 AND path = ?2 AND type = 'file'", params![o, p], |r| r.get::<_, Option<String>>(0)).optional()?.flatten();
            let seen = c.query_row("SELECT value FROM kv WHERE scope = ?1 AND key = ?2", params![SCOPE, k], |r| r.get(0)).optional()?;
            Ok((cid, seen))
        })
        .await?;
    let mut seen: Seen = seen.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    let at = now();
    // 1. Un paquet revenu (restauration, déménagement) : le relire, et ne rien écrire ce tour-ci.
    if let Some(c) = cid.as_ref().filter(|c| **c != seen.cid) {
        let Some(bytes) = astro_dav::carddav::read_file(dav, owner, &path).await else { return Ok(()) }; // blob pas encore là : plus tard
        match parse(&bytes, owner, part) {
            Ok(lines) => report.read += usize::from(import(&dav.db, owner, lines).await? > 0),
            Err(e) => report.problems.push(format!("{path} de {} refusé : {e} (il sera remplacé)", short(owner))),
        }
        seen = Seen { cid: c.clone(), stamp: String::new(), written_at: seen.written_at };
        return save(&dav.db, &key, &seen).await;
    }
    // 2. Le contenu a changé : écrire le paquet (pas pendant une restauration, pas plus d'une fois par REWRITE_SECS).
    if (cid.is_some() && at - seen.written_at < REWRITE_SECS) || astro_dav::manifest::restoring(&dav.db, owner).await {
        return Ok(());
    }
    let o = owner.to_owned();
    let Some(body) = dav.db.readers().read(move |c| body(c, &o, part)).await? else { return Ok(()) };
    let stamp = hex::encode(Sha256::digest(&body));
    if stamp == seen.stamp {
        return Ok(());
    }
    let bytes = with_head(owner, part, &body);
    if bytes.len() > MAX_PACK {
        report.problems.push(format!("{path} de {} : paquet trop gros ({} octets), non écrit", short(owner), bytes.len()));
        return Ok(());
    }
    if let Err(e) = astro_dav::ingest::replace(dav, owner, part.dir(), part.name(), Some("application/octet-stream"), &bytes[..]).await {
        report.problems.push(format!("{path} de {} non écrit : {e}", short(owner)));
        return Ok(());
    }
    let (o, p) = (owner.to_owned(), path);
    let cid: Option<String> = dav.db.readers().read(move |c| Ok(c.query_row("SELECT cid FROM ucloud_entries WHERE owner_hex = ?1 AND path = ?2", params![o, p], |r| r.get(0)).optional()?.flatten())).await?;
    save(&dav.db, &key, &Seen { cid: cid.unwrap_or_default(), stamp, written_at: at }).await?;
    report.written += 1;
    Ok(())
}

fn short(owner: &str) -> &str {
    owner.get(..8).unwrap_or(owner)
}

async fn save(db: &Db, key: &str, seen: &Seen) -> Result<(), astro_db::DbError> {
    let value = json!({ "cid": seen.cid, "stamp": seen.stamp, "written_at": seen.written_at }).to_string();
    db.writer()
        .execute(
            "INSERT INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            vec![SCOPE.to_owned().into(), key.to_owned().into(), value.into(), now().into()],
        )
        .await
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use astro_dav::{DavConfig, FsBlobStore};
    use astro_db::SqliteReplayStore;

    const ALICE: &str = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";
    const PEER: &str = "b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2";

    fn blob(dim: usize, x: f32) -> Vec<u8> {
        (0..dim).flat_map(|i| (x + i as f32 * 0.001).to_le_bytes()).collect()
    }

    async fn station(tag: &str) -> Arc<DavState<FsBlobStore>> {
        let dir = std::env::temp_dir().join(format!("astroportd-life-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
        db.writer().execute("INSERT INTO identities(hex, role, hosted) VALUES (?1, 'multipass', 1)", vec![ALICE.to_owned().into()]).await.unwrap();
        let blobs = Arc::new(FsBlobStore::open(dir.join("blobs")).unwrap());
        Arc::new(DavState { db, blobs, replay: Arc::new(SqliteReplayStore::in_memory().unwrap()), publisher: None, config: DavConfig::new(vec![7; 32]), on_upload: None })
    }

    /// Un peu de vie : un visage nommé, un inconnu, un message, un effacé, une réaction, des préférences, deux souvenirs.
    async fn live(db: &Db) {
        let (f1, f2, m1, m2) = (blob(512, 0.1), blob(512, 0.2), blob(1024, 0.3), blob(1024, 0.4));
        db.writer()
            .transaction(move |tx| {
                tx.execute("INSERT INTO faces(owner_hex, name, contact_uid, embed_hash, ts, ucloud_path, bbox_x1, bbox_y1, bbox_x2, bbox_y2) VALUES (?1, 'Léa', 'uid-léa', 'h1', 5, '/Photos/a.jpg', 1, 2, 3, 4)", [ALICE])?;
                tx.execute("INSERT INTO face_vectors(face_id, owner_hex, embedding) VALUES (?1, ?2, ?3)", params![tx.last_insert_rowid(), ALICE, f1])?;
                tx.execute("INSERT INTO faces(owner_hex, name, embed_hash, ts, ucloud_path) VALUES (?1, 'Inconnu_12345678', 'h2', 5, '/Photos/a.jpg')", [ALICE])?;
                tx.execute("INSERT INTO face_vectors(face_id, owner_hex, embedding) VALUES (?1, ?2, ?3)", params![tx.last_insert_rowid(), ALICE, f2])?;
                tx.execute("INSERT INTO chat_messages(owner_hex, id, peer, mine, at, text, n, read) VALUES (?1, 'm1', ?2, 1, 100, 'Bonjour Léa', 1, 1)", [ALICE, PEER])?;
                tx.execute("INSERT INTO chat_messages(owner_hex, id, peer, mine, at, text, n) VALUES (?1, 'm2', ?2, 0, 101, 'Salut !', 2)", [ALICE, PEER])?;
                tx.execute("INSERT INTO chat_reactions(owner_hex, msg_id, author, emoji, at) VALUES (?1, 'm1', ?2, '❤', 102)", [ALICE, PEER])?;
                tx.execute("INSERT INTO chat_prefs(owner_hex, peer, pinned) VALUES (?1, ?2, 1)", [ALICE, PEER])?;
                tx.execute("INSERT INTO chat_groups(owner_hex, id, name, members) VALUES (?1, 'g1', 'Famille', '[]')", [ALICE])?;
                tx.execute("INSERT INTO kv(scope, key, value, updated_at) VALUES ('chat_deleted', ?1 || ':m0', '1', 99)", [ALICE])?;
                tx.execute("INSERT INTO memory(point_id, owner_hex, content, content_hash, ts, kind, status) VALUES ('p1', ?1, 'Aime le thé', 'c1', 1, 'fact', 'active')", [ALICE])?;
                tx.execute("INSERT INTO memory_vectors(mem_id, owner_hex, embedding) VALUES (?1, ?2, ?3)", params![tx.last_insert_rowid(), ALICE, m1])?;
                tx.execute("INSERT INTO memory(point_id, owner_hex, content, content_hash, ts, kind, status) VALUES ('p2', ?1, 'Ancien', 'c2', 1, 'fact', 'deleted')", [ALICE])?;
                tx.execute("INSERT INTO memory_vectors(mem_id, owner_hex, embedding) VALUES (?1, ?2, ?3)", params![tx.last_insert_rowid(), ALICE, m2])?;
                Ok(())
            })
            .await
            .unwrap();
    }

    async fn wipe(db: &Db) {
        db.writer()
            .transaction(|tx| {
                tx.execute_batch(
                    "DELETE FROM face_vectors; DELETE FROM faces; DELETE FROM chat_messages; DELETE FROM chat_reactions; DELETE FROM chat_prefs; \
                     DELETE FROM chat_groups; DELETE FROM kv WHERE scope IN ('chat_deleted', 'chat_cleared', 'life_pack'); DELETE FROM memory_vectors; DELETE FROM memory;",
                )?;
                Ok(())
            })
            .await
            .unwrap();
    }

    async fn snapshot(db: &Db) -> Vec<Vec<Line>> {
        db.readers().read(|c| Ok(Part::ALL.iter().map(|p| lines(c, ALICE, *p)).collect::<Result<Vec<_>, _>>()?)).await.unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn packs_written_then_base_wiped_then_read_back_give_the_same_life() {
        let dav = station("roundtrip").await;
        live(&dav.db).await;
        let before = snapshot(&dav.db).await;
        assert_eq!(before[0].len(), 1, "seul le visage nommé part, l'inconnu se recalcule");
        let r = sync(&dav).await.unwrap();
        assert_eq!((r.written, r.problems.len()), (3, 0), "{:?}", r.problems);
        let sizes = dav.db.readers().read(|c| Ok(sizes(c, ALICE)?)).await.unwrap();
        assert!(sizes.0 > 0 && sizes.1 > 0 && sizes.2 > 0, "{sizes:?}");
        // Rien n'a changé : pas de réécriture.
        assert_eq!(sync(&dav).await.unwrap().written, 0);

        // La station meurt : la base est perdue, les fichiers reviennent (restauration). Les paquets sont relus.
        wipe(&dav.db).await;
        let r = sync(&dav).await.unwrap();
        assert_eq!((r.read, r.written), (3, 0), "{:?}", r.problems);
        assert_eq!(snapshot(&dav.db).await, before);
        // Le message effacé reste effacé, le souvenir effacé aussi ; les vecteurs sont revenus.
        let (vecs, deleted): (i64, String) = dav
            .db
            .readers()
            .read(|c| Ok((c.query_row("SELECT (SELECT count(*) FROM face_vectors) + (SELECT count(*) FROM memory_vectors)", [], |r| r.get(0))?, c.query_row("SELECT status FROM memory WHERE point_id = 'p2'", [], |r| r.get(0))?)))
            .await
            .unwrap();
        assert_eq!((vecs, deleted.as_str()), (3, "deleted"));

        // Deux relectures n'en font pas doublon.
        for part in Part::ALL {
            let bytes = astro_dav::carddav::read_file(&dav, ALICE, &part.path()).await.unwrap();
            let lines = parse(&bytes, ALICE, part).unwrap();
            assert_eq!(import(&dav.db, ALICE, lines).await.unwrap(), 0, "{}", part.tag());
        }
        assert_eq!(snapshot(&dav.db).await, before);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_named_face_here_keeps_its_name_and_an_unknown_one_takes_the_pack_s() {
        let dav = station("merge").await;
        live(&dav.db).await;
        let bytes = dav.db.readers().read(|c| export(c, ALICE, Part::Faces)).await.unwrap().unwrap();
        dav.db.writer().execute("UPDATE faces SET name = 'Inconnu_aaaaaaaa', contact_uid = NULL WHERE embed_hash = 'h1'", vec![]).await.unwrap();
        assert_eq!(import(&dav.db, ALICE, parse(&bytes, ALICE, Part::Faces).unwrap()).await.unwrap(), 1);
        dav.db.writer().execute("UPDATE faces SET name = 'Léa Martin' WHERE embed_hash = 'h1'", vec![]).await.unwrap();
        assert_eq!(import(&dav.db, ALICE, parse(&bytes, ALICE, Part::Faces).unwrap()).await.unwrap(), 0);
        let name: String = dav.db.readers().read(|c| Ok(c.query_row("SELECT name FROM faces WHERE embed_hash = 'h1'", [], |r| r.get(0))?)).await.unwrap();
        assert_eq!(name, "Léa Martin");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_damaged_pack_is_refused_without_breaking_anything() {
        let dav = station("corrupt").await;
        live(&dav.db).await;
        let before = snapshot(&dav.db).await;
        let good = dav.db.readers().read(|c| export(c, ALICE, Part::Chat)).await.unwrap().unwrap();
        // Refus à la lecture : autre compte, autre sorte, ligne tronquée, valeur hors des contraintes, vecteur abîmé.
        assert!(parse(&good, PEER, Part::Chat).is_err());
        assert!(parse(&good, ALICE, Part::Memory).is_err());
        assert!(parse(&good[..good.len() - 10], ALICE, Part::Chat).is_err());
        assert!(parse(b"", ALICE, Part::Chat).is_err() && parse(b"{}\n", ALICE, Part::Chat).is_err());
        let bad_state = String::from_utf8(good.clone()).unwrap().replace("\"state\":\"ok\"", "\"state\":\"perdu\"");
        assert!(parse(bad_state.as_bytes(), ALICE, Part::Chat).is_err());
        let faces = String::from_utf8(dav.db.readers().read(|c| export(c, ALICE, Part::Faces)).await.unwrap().unwrap()).unwrap();
        let cut = faces.replace("\"v\":\"", "\"v\":\"AAAA");
        assert!(parse(cut.as_bytes(), ALICE, Part::Faces).is_err(), "vecteur de mauvaise dimension");

        // Un paquet abîmé posé dans l'uCloud : refusé, dit, et la base n'a pas bougé.
        astro_dav::ingest::replace(&dav, ALICE, Part::Chat.dir(), Part::Chat.name(), None, &b"{\"astroidx\":1,\"part\":\"chat\",\"owner\":\"x\"}\nzzz\n"[..]).await.unwrap();
        let r = sync(&dav).await.unwrap();
        assert_eq!(r.problems.len(), 1, "{:?}", r.problems);
        assert_eq!(snapshot(&dav.db).await, before);
        // Au tour suivant, il est remplacé par un paquet sain.
        let r = sync(&dav).await.unwrap();
        assert!(r.problems.is_empty() && r.written >= 1, "{r:?}");
        let bytes = astro_dav::carddav::read_file(&dav, ALICE, &Part::Chat.path()).await.unwrap();
        assert!(parse(&bytes, ALICE, Part::Chat).is_ok());
    }
}
