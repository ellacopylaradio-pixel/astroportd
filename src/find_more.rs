//! Recherche transversale d'un membre, à côté de ses fichiers (`library::search`) : ses conversations, ses notes et articles
//! et ceux de ses amis réciproques, son carnet d'adresses. Une seule case (`m-find.js`) retrouve donc « ce qu'on m'a écrit »
//! aussi bien que « ce que j'ai rangé ».
//!
//! Tout se lit dans la base de la station, sans réseau :
//! - messages : `chat_messages` (déjà déchiffrés pour le titulaire), chaque mot présent, sans casse ;
//! - notes et articles (kinds 1 et 30023, plus ses brouillons 30024) : index plein texte `nostr_fts` (celui de NIP-50), auteurs =
//!   le membre et ses amis réciproques (`social_graph`, niveau ≤ 1) ; un article réservé (NIP-70, en clair dans la base) n'est
//!   montré qu'à son auteur et à ses destinataires (`p`) ; les conversations chiffrées sont cherchées déchiffrées (`chat_messages`) ;
//! - carnet : `contact_cards.search`, comparé comme l'écran Carnet (`vcard::name_key`).

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

/// Résultats au plus, par source.
pub const MAX: usize = 8;
/// Messages relus au plus, les plus récents d'abord.
const SCAN: i64 = 20_000;

/// Mots de la requête, sans casse ni accents (la forme de `contact_cards.search`).
fn words(q: &str) -> Vec<String> {
    astro_dav::vcard::name_key(q).split_whitespace().filter(|w| w.chars().any(char::is_alphanumeric)).take(8).map(str::to_owned).collect()
}

/// Requête FTS5 : chaque mot doit commencer un mot du texte (comme NIP-50 sur le relai).
fn fts(words: &[String]) -> String {
    words.iter().map(|w| format!("\"{}\"*", w.replace('"', "\"\""))).collect::<Vec<_>>().join(" ")
}

/// Extrait de `text` autour du premier mot trouvé (≈ 140 caractères), pour dire pourquoi c'est là.
pub fn excerpt(text: &str, words: &[String]) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let chars: Vec<char> = flat.chars().collect();
    let folded: Vec<String> = chars.iter().map(|c| astro_dav::vcard::name_key(&c.to_string())).collect();
    let hit = words.first().and_then(|w| {
        let w: Vec<char> = w.chars().collect();
        (0..chars.len()).find(|&i| {
            let mut s = String::new();
            for f in folded.iter().skip(i) {
                s.push_str(if f.is_empty() { " " } else { f });
                if s.chars().count() >= w.len() {
                    break;
                }
            }
            s.chars().take(w.len()).eq(w.iter().copied())
        })
    });
    let start = hit.map_or(0, |i| i.saturating_sub(50));
    let end = (start + 140).min(chars.len());
    let mut out: String = chars[start..end].iter().collect();
    if start > 0 {
        out.insert(0, '…');
    }
    if end < chars.len() {
        out.push('…');
    }
    out
}

/// Nom affichable d'une clé : la fiche du carnet qui lui est liée, sinon son profil Nostr, sinon le début de la clé.
pub(crate) fn name_of(c: &Connection, owner: &str, pubkey: &str) -> rusqlite::Result<String> {
    let card: Option<String> = c.query_row("SELECT name FROM identity_links WHERE owner_hex = ?1 AND pubkey = ?2 LIMIT 1", params![owner, pubkey], |r| r.get(0)).optional()?;
    if let Some(n) = card.filter(|n| !n.trim().is_empty()) {
        return Ok(n);
    }
    let profile: Option<String> = c.query_row("SELECT content FROM nostr_events WHERE kind = 0 AND pubkey = ?1 ORDER BY created_at DESC LIMIT 1", [pubkey], |r| r.get(0)).optional()?;
    let named = profile.and_then(|p| serde_json::from_str::<Value>(&p).ok()).and_then(|v| {
        ["display_name", "name"].iter().find_map(|k| v[*k].as_str().filter(|s| !s.trim().is_empty()).map(|s| s.chars().take(60).collect::<String>()))
    });
    Ok(named.unwrap_or_else(|| pubkey.chars().take(8).collect()))
}

/// Messages du titulaire qui contiennent chaque mot.
fn messages(c: &Connection, owner: &str, words: &[String]) -> rusqlite::Result<Vec<Value>> {
    // Comparaison sans accents ni casse, faite ici (SQLite ne sait pas plier les accents) sur les messages les plus récents.
    let mut s = c.prepare("SELECT peer, id, at, text, mine FROM chat_messages WHERE owner_hex = ?1 ORDER BY at DESC LIMIT ?2")?;
    let rows = s.query_map(params![owner, SCAN], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?, r.get::<_, String>(3)?, r.get::<_, i64>(4)? == 1)))?;
    let mut out = Vec::new();
    for row in rows {
        let (peer, id, at, text, mine) = row?;
        let key = astro_dav::vcard::name_key(&text);
        if !words.iter().all(|w| key.contains(w.as_str())) {
            continue;
        }
        // Un groupe sans nom : `name` vide, l'écran dit « Groupe ».
        let group: Option<String> = c.query_row("SELECT COALESCE(name, '') FROM chat_groups WHERE owner_hex = ?1 AND id = ?2", params![owner, peer], |r| r.get(0)).optional()?;
        let name = match &group {
            Some(g) => g.clone(),
            None => name_of(c, owner, &peer)?,
        };
        out.push(json!({ "peer": peer, "id": id, "at": at, "mine": mine, "group": group.is_some(), "name": name, "excerpt": excerpt(&text, words) }));
        if out.len() >= MAX {
            break;
        }
    }
    Ok(out)
}

/// Notes et articles du membre et de ses amis réciproques.
fn notes(c: &Connection, owner: &str, words: &[String], now: i64) -> rusqlite::Result<Vec<Value>> {
    let mut s = c.prepare(
        "SELECT e.pubkey, e.kind, e.created_at, e.content, e.d_tag, e.tags, e.id FROM nostr_fts f JOIN nostr_events e ON e.seq = f.rowid \
         WHERE nostr_fts MATCH ?1 AND (e.kind IN (1, 30023) OR (e.kind = 30024 AND e.pubkey = ?2)) \
           AND (e.pubkey = ?2 OR e.pubkey IN (SELECT pubkey FROM social_graph WHERE owner_hex = ?2 AND level <= 1)) \
           AND (e.expires_at IS NULL OR e.expires_at > ?3) \
           AND (e.pubkey = ?2 OR instr(e.tags, '[\"-\"]') = 0 OR EXISTS (SELECT 1 FROM nostr_tags t WHERE t.event_id = e.seq AND t.name = 'p' AND t.value = ?2)) \
         ORDER BY f.rank, e.created_at DESC LIMIT ?4",
    )?;
    let rows = s.query_map(params![fts(words), owner, now, MAX as i64], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, r.get::<_, String>(3)?, r.get::<_, Option<String>>(4)?, r.get::<_, String>(5)?, r.get::<_, String>(6)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (pubkey, kind, at, content, d, tags, id) = row?;
        let tags: Vec<Vec<String>> = serde_json::from_str(&tags).unwrap_or_default();
        let tag = |k: &str| tags.iter().find(|t| t.first().is_some_and(|x| x == k)).and_then(|t| t.get(1)).cloned();
        let at = if kind >= 30023 { tag("published_at").and_then(|p| p.parse().ok()).unwrap_or(at) } else { at };
        let mine = pubkey == owner;
        let name = if mine { String::new() } else { name_of(c, owner, &pubkey)? };
        out.push(json!({ "id": id, "author": pubkey, "mine": mine, "name": name, "article": kind >= 30023, "draft": kind == 30024, "d": d, "title": tag("title"), "at": at, "excerpt": excerpt(&content, words) }));
    }
    Ok(out)
}

/// Fiches du carnet qui contiennent chaque mot.
fn contacts(c: &Connection, owner: &str, words: &[String]) -> rusqlite::Result<Vec<Value>> {
    let mut s = c.prepare("SELECT path, json_extract(fields, '$.name'), json_extract(fields, '$.org'), search FROM contact_cards WHERE owner_hex = ?1 AND instr(search, ?2) > 0 LIMIT 200")?;
    let first = words.first().cloned().unwrap_or_default();
    let rows = s.query_map(params![owner, first], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, Option<String>>(2)?, r.get::<_, String>(3)?)))?;
    let mut out = Vec::new();
    for row in rows {
        let (path, name, org, search) = row?;
        if words.iter().all(|w| search.contains(w.as_str())) {
            out.push(json!({ "path": path, "name": name.unwrap_or_default(), "org": org }));
        }
        if out.len() >= MAX {
            break;
        }
    }
    Ok(out)
}

/// Tout ce que la case de recherche trouve en dehors des fichiers. Moins de 2 caractères utiles : rien.
pub fn others(c: &Connection, owner: &str, q: &str, now: i64) -> rusqlite::Result<Value> {
    let words = words(q);
    if words.iter().map(|w| w.chars().count()).sum::<usize>() < 2 {
        return Ok(json!({ "messages": [], "notes": [], "contacts": [] }));
    }
    Ok(json!({ "messages": messages(c, owner, &words)?, "notes": notes(c, owner, &words, now)?, "contacts": contacts(c, owner, &words)? }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excerpt_centres_on_the_word_without_accents_or_case() {
        let w = words("Éléphant");
        let long = format!("{} un ÉLÉPHANT rose {}", "bla ".repeat(40), "fin ".repeat(40));
        let e = excerpt(&long, &w);
        assert!(e.starts_with('…') && e.ends_with('…') && e.contains("ÉLÉPHANT"), "{e}");
        assert_eq!(excerpt("court", &w), "court");
    }

    #[test]
    fn hostile_queries_stay_inert() {
        // Les opérateurs FTS5 deviennent des mots entre guillemets : jamais une syntaxe.
        assert_eq!(fts(&words("\" OR * NEAR( -- ;")), "\"or\"* \"near\"*");
        assert!(words("\" * -- ;").is_empty());
        assert_eq!(fts(&words("vélo \"rouge\"")), "\"velo\"* \"rouge\"*");
        assert_eq!(words(&"mot ".repeat(50)).len(), 8, "8 mots au plus");
    }

    #[test]
    fn one_box_finds_messages_notes_of_friends_and_contacts_but_nothing_from_strangers() {
        let c = astro_db::open_in_memory().unwrap();
        let (me, lea, inconnu, groupe) = ("aa".repeat(32), "bb".repeat(32), "cc".repeat(32), "dd".repeat(32));
        let msg = |id: &str, peer: &str, at: i64, text: &str| {
            c.execute("INSERT INTO chat_messages(owner_hex, id, peer, mine, at, text) VALUES (?1, ?2, ?3, 0, ?4, ?5)", params![me, id, peer, at, text]).unwrap();
        };
        msg("m1", &lea, 10, "Rendez-vous au Café des Écluses à 18 h");
        msg("m2", &groupe, 20, "Le café est fermé lundi");
        msg("m3", &lea, 30, "rien à voir");
        c.execute("INSERT INTO chat_groups(owner_hex, id, name, members) VALUES (?1, ?2, NULL, '[]')", params![me, groupe]).unwrap();
        c.execute("INSERT INTO identity_links(owner_hex, path, cid, name, name_key, pubkey, updated_at) VALUES (?1, '/Contacts/lea.vcf', 'x', 'Léa Martin', 'lea martin', ?2, 0)", params![me, lea]).unwrap();
        c.execute("INSERT INTO contact_cards(owner_hex, path, fields, search) VALUES (?1, '/Contacts/cafe.vcf', '{\"name\":\"Café des Écluses\",\"org\":\"SCOP\"}', 'cafe des ecluses scop 0612345678')", params![me]).unwrap();
        c.execute("INSERT INTO social_graph(owner_hex, pubkey, level, updated_at) VALUES (?1, ?2, 1, 0), (?1, ?3, 2, 0)", params![me, lea, inconnu]).unwrap();
        let ev = |id: &str, pk: &str, kind: i64, content: &str, tags: &str, d: Option<&str>| {
            c.execute("INSERT INTO nostr_events(id, pubkey, kind, created_at, content, sig, tags, d_tag) VALUES (?1, ?2, ?3, 100, ?4, 'x', ?5, ?6)", params![id, pk, kind, content, tags, d]).unwrap();
        };
        ev("e1", &lea, 30023, "Notre visite du café associatif", r#"[["d","visite"],["title","Le café du coin"],["published_at","90"]]"#, Some("visite"));
        ev("e2", &me, 1, "Je cherche un café ouvert le dimanche", "[]", None);
        ev("e3", &inconnu, 1, "Café gratuit, cliquez ici", "[]", None);
        ev("e4", &lea, 4, "café en message chiffré", "[]", None);
        // Réservé (NIP-70) par Léa à quelqu'un d'autre : jamais montré ; réservé pour moi : montré.
        ev("e5", &lea, 30023, "Le café secret", &format!(r#"[["d","secret"],["-"],["p","{inconnu}"]]"#), Some("secret"));
        ev("e6", &lea, 30023, "Un café rien que pour toi", &format!(r#"[["d","pourtoi"],["-"],["p","{me}"]]"#), Some("pourtoi"));
        for (id, p) in [("e5", &inconnu), ("e6", &me)] {
            c.execute("INSERT INTO nostr_tags(event_id, name, value) SELECT seq, 'p', ?2 FROM nostr_events WHERE id = ?1", params![id, p]).unwrap();
        }

        let r = others(&c, &me, "cafe", 1000).unwrap();
        let m = r["messages"].as_array().unwrap();
        assert_eq!(m.len(), 2, "sans accents ni casse : {m:?}");
        assert_eq!((m[0]["name"].as_str(), m[0]["group"].as_bool()), (Some(""), Some(true)), "groupe sans nom : l'écran dit « Groupe »");
        assert_eq!(m[1]["name"], "Léa Martin", "le nom vient du carnet");
        let n = r["notes"].as_array().unwrap();
        assert_eq!(n.len(), 3, "mes notes, celles de mes amis réciproques et ce qu'ils m'ont réservé ; jamais d'un inconnu, d'un kind privé ni réservé à un autre : {n:?}");
        assert!(n.iter().all(|x| x["d"] != "secret"));
        let article = n.iter().find(|x| x["d"] == "visite").unwrap();
        assert_eq!((article["title"].as_str(), article["d"].as_str(), article["at"].as_i64(), article["name"].as_str()), (Some("Le café du coin"), Some("visite"), Some(90), Some("Léa Martin")));
        assert!(n.iter().any(|x| x["mine"] == true));
        assert_eq!(r["contacts"][0]["name"], "Café des Écluses");
        assert_eq!(others(&c, &me, "café écluses", 1000).unwrap()["messages"].as_array().unwrap().len(), 1, "tous les mots");
        assert_eq!(others(&c, &me, "a", 1000).unwrap()["notes"].as_array().unwrap().len(), 0, "trop court : rien");
        assert_eq!(others(&c, &inconnu, "cafe", 1000).unwrap()["messages"].as_array().unwrap().len(), 0, "les messages d'un autre compte restent les siens");
    }
}
