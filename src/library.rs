//! Bibliothèque de photos et fichiers du compte : requêtes de lecture de l'interface (déjà là, souvenirs, chronologie,
//! recherche à filtres, doublons). Tout est calculé en SQL sur `ucloud_entries`, par propriétaire : rien ne sort de la station
//! et rien n'est comparé entre comptes (la présence d'un contenu chez un autre compte ne doit jamais se deviner).

use rusqlite::{Connection, ToSql, params, params_from_iter};
use serde_json::{Value, json};

/// Colonnes d'un élément affiché : chemin, type MIME, taille, mtime, date de prise de vue, date retenue, miniature, lat, lon, empreinte.
const COLS: &str = "e.path, e.mime, e.size, e.mtime, e.taken_at, COALESCE(e.taken_at, e.mtime) AS t, json_extract(e.data, '$.thumb'), e.lat, e.lon, e.phash, e.sha_plain";
/// Fichiers réels d'un compte (pas les dossiers, pas les entrées sans contenu), hors tiroir système `/_` (coffre, carnet,
/// agenda : ce ne sont ni des photos, ni des documents à retrouver dans la bibliothèque). Plage `['/_/', '/_0')` : `_` est
/// un joker de `LIKE`, et la plage suit l'index.
/// Fichiers du membre, hors tiroir système et hors médiathèque (fiches, affiches, pochettes, sous-titres : [`crate::media`]).
pub(crate) const FILES: &str = concat!("e.owner_hex = ?1 AND e.type = 'file' AND e.cid IS NOT NULL AND NOT (e.path >= '/_/' AND e.path < '/_0') AND ", crate::not_companion_sql!());
/// Plafond d'empreintes comparées pour les doublons visuels (borne la mémoire et le temps).
const MAX_PHASH: usize = 30_000;
/// Distance de Hamming maximale entre deux empreintes pour parler de « même image ».
const NEAR: u32 = 6;
/// Nombre de SHA-256 acceptés par appel de [`have`].
pub const MAX_HAVE: usize = 500;

fn thumb(json: Option<String>) -> Value {
    let Some(v) = json.and_then(|j| serde_json::from_str::<Value>(&j).ok()) else { return Value::Null };
    json!({ "w": v["w"], "h": v["h"], "ow": v["ow"], "oh": v["oh"] })
}

fn item(r: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    Ok(json!({
        "path": r.get::<_, String>(0)?, "mime": r.get::<_, Option<String>>(1)?, "size": r.get::<_, Option<i64>>(2)?,
        "mtime": r.get::<_, Option<i64>>(3)?, "taken_at": r.get::<_, Option<i64>>(4)?, "t": r.get::<_, Option<i64>>(5)?,
        "thumb": thumb(r.get::<_, Option<String>>(6)?), "lat": r.get::<_, Option<f64>>(7)?, "lon": r.get::<_, Option<f64>>(8)?,
    }))
}

/// Parmi `shas` (SHA-256 du contenu clair, en hexadécimal), ceux que ce compte possède déjà : `[{ sha, path }]`.
/// Un client qui sauvegarde ses photos n'envoie ainsi que ce qui manque.
pub fn have(c: &Connection, owner: &str, shas: &[String]) -> rusqlite::Result<Vec<Value>> {
    let shas: Vec<String> = shas.iter().filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())).map(|s| s.to_ascii_lowercase()).take(MAX_HAVE).collect();
    if shas.is_empty() {
        return Ok(Vec::new());
    }
    let marks = vec!["?"; shas.len()].join(",");
    let sql = format!("SELECT sha_plain, MIN(path) FROM ucloud_entries WHERE owner_hex = ? AND type = 'file' AND cid IS NOT NULL AND sha_plain IN ({marks}) GROUP BY sha_plain");
    let mut args: Vec<&dyn ToSql> = vec![&owner];
    args.extend(shas.iter().map(|s| s as &dyn ToSql));
    let mut stmt = c.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(args), |r| Ok(json!({ "sha": r.get::<_, String>(0)?, "path": r.get::<_, String>(1)? })))?.collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Jours depuis 1970-01-01 pour une date civile (algorithme de H. Hinnant).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468
}

/// `"2026-10"` → `[début, fin[` en secondes Unix (UTC). `None` si le mois est mal formé.
pub fn month_bounds(ym: &str) -> Option<(i64, i64)> {
    let (y, m) = ym.split_once('-')?;
    let (y, m): (i64, i64) = (y.parse().ok()?, m.parse().ok()?);
    if !(1970..=2200).contains(&y) || !(1..=12).contains(&m) || ym.len() != 7 {
        return None;
    }
    let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    Some((days_from_civil(y, m, 1) * 86_400, days_from_civil(ny, nm, 1) * 86_400))
}

/// Chronologie : un compteur par mois (du plus récent au plus ancien) ; l'interface charge ensuite un mois à la demande.
pub fn timeline_buckets(c: &Connection, owner: &str) -> rusqlite::Result<Vec<Value>> {
    let sql = format!("SELECT strftime('%Y-%m', COALESCE(e.taken_at, e.mtime), 'unixepoch') AS ym, count(*) FROM ucloud_entries e WHERE {FILES} AND (e.mime LIKE 'image/%' OR e.mime LIKE 'video/%') AND COALESCE(e.taken_at, e.mtime) IS NOT NULL GROUP BY ym ORDER BY ym DESC");
    let mut stmt = c.prepare_cached(&sql)?;
    let rows = stmt.query_map([owner], |r| Ok(json!({ "ym": r.get::<_, String>(0)?, "count": r.get::<_, i64>(1)? })))?.collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Les photos et vidéos d'un mois (`"2026-10"`), de la plus récente à la plus ancienne.
pub fn timeline_month(c: &Connection, owner: &str, ym: &str) -> rusqlite::Result<Option<Vec<Value>>> {
    let Some((start, end)) = month_bounds(ym) else { return Ok(None) };
    let sql = format!("SELECT {COLS} FROM ucloud_entries e WHERE {FILES} AND (e.mime LIKE 'image/%' OR e.mime LIKE 'video/%') AND COALESCE(e.taken_at, e.mtime) >= ?2 AND COALESCE(e.taken_at, e.mtime) < ?3 ORDER BY t DESC, e.path LIMIT 2000");
    let mut stmt = c.prepare_cached(&sql)?;
    let rows = stmt.query_map(params![owner, start, end], item)?.collect::<Result<Vec<_>, _>>()?;
    Ok(Some(rows))
}

/// « Ce jour-là » : les photos et vidéos prises le même jour (`"10-31"`) les années précédentes, groupées par année décroissante.
/// `taken_at` est une heure murale (voir `ExifMeta::taken_at`) : `strftime` donne directement la date civile du membre, quel
/// que soit son fuseau ou celui de l'appareil.
pub fn memories(c: &Connection, owner: &str, month_day: &str, this_year: i64) -> rusqlite::Result<Vec<Value>> {
    if month_day.len() != 5 || !month_day.bytes().enumerate().all(|(i, b)| if i == 2 { b == b'-' } else { b.is_ascii_digit() }) {
        return Ok(Vec::new());
    }
    let sql = format!(
        "SELECT CAST(strftime('%Y', COALESCE(e.taken_at, e.mtime), 'unixepoch') AS INTEGER) AS y, {COLS} FROM ucloud_entries e \
         WHERE {FILES} AND (e.mime LIKE 'image/%' OR e.mime LIKE 'video/%') AND strftime('%m-%d', COALESCE(e.taken_at, e.mtime), 'unixepoch') = ?2 \
         AND e.taken_at IS NOT NULL ORDER BY y DESC, t, e.path"
    );
    let mut stmt = c.prepare_cached(&sql)?;
    let mut years: Vec<(i64, Vec<Value>)> = Vec::new();
    let mut rows = stmt.query(params![owner, month_day])?;
    while let Some(r) = rows.next()? {
        let y: i64 = r.get(0)?;
        // `item` lit les colonnes à partir de la 0 : on les relit décalées d'une.
        let it = json!({
            "path": r.get::<_, String>(1)?, "mime": r.get::<_, Option<String>>(2)?, "size": r.get::<_, Option<i64>>(3)?,
            "mtime": r.get::<_, Option<i64>>(4)?, "taken_at": r.get::<_, Option<i64>>(5)?, "t": r.get::<_, Option<i64>>(6)?,
            "thumb": thumb(r.get::<_, Option<String>>(7)?), "lat": r.get::<_, Option<f64>>(8)?, "lon": r.get::<_, Option<f64>>(9)?,
        });
        if y >= this_year {
            continue;
        }
        match years.last_mut() {
            Some((ly, items)) if *ly == y => {
                if items.len() < 12 {
                    items.push(it);
                }
            }
            _ => years.push((y, vec![it])),
        }
    }
    Ok(years.into_iter().map(|(y, items)| json!({ "year": y, "ago": this_year - y, "items": items })).collect())
}

/// Critères de [`search`].
#[derive(Debug, Default, Clone)]
pub struct Search {
    /// Ce que le membre a tapé : chaque groupe de mots ([`crate::search_query::Term`]) doit être trouvé **quelque part** —
    /// nom, description, texte lu (OCR, fiche de la Médiathèque : sans accents, début de mot accepté), visage nommé, lieu,
    /// mois ou année.
    pub q: Option<String>,
    /// Nom exact d'une personne reconnue (FaceCloud) présente sur la photo.
    pub person: Option<String>,
    /// Bornes de date (secondes Unix, `from` inclus, `to` exclu).
    pub from: Option<i64>,
    /// Fin de période.
    pub to: Option<i64>,
    /// Zone `(sud, ouest, nord, est)` en degrés.
    pub bbox: Option<(f64, f64, f64, f64)>,
    /// `image`, `video` ou `other`.
    pub kind: Option<String>,
    /// Curseur : `(date, chemin)` du dernier résultat de la page précédente.
    pub after: Option<(i64, String)>,
    /// Résultats par page (1 à 200).
    pub limit: usize,
    /// Photos et vidéos qui montrent ce que disent les mots « à voir » (`(empreinte, instant en ms de la meilleure image)`,
    /// la meilleure d'abord, rendus par [`seen`]) : elles valent pour tous ces mots à la fois.
    pub seen: Vec<(String, i64)>,
    /// Les plus proches de la requête même si aucune ne sort du lot : proposées en « Peut-être » quand rien n'est trouvé.
    pub near: Vec<(String, i64)>,
}

/// Photos trouvées au plus par l'image.
pub const MAX_SEEN: usize = 200;
/// Photos proposées au plus en « Peut-être ».
pub const MAX_NEAR: usize = 4;
/// Meilleurs résultats montrés en tête, et « Peut-être » au plus.
pub const MAX_BEST: usize = 8;
/// Résultats classés au plus pour trouver les meilleurs.
const MAX_RANKED: i64 = 300;

/// Ce que la recherche par l'image a trouvé : celles qui sortent nettement du lot, et les plus proches quoi qu'il arrive.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Seen {
    /// Sortent du lot ([`astro_index::siglip::stand_out`]), la meilleure d'abord.
    pub strict: Vec<(String, i64)>,
    /// Les [`MAX_NEAR`] plus proches, sans seuil.
    pub near: Vec<(String, i64)>,
}

/// Photos et vidéos du compte qui montrent ce que dit la requête, avec l'instant de l'image la plus proche (0 pour une photo,
/// l'image clé pour une vidéo) : `query` est le vecteur de la requête (`Task::VisionQuery`, déjà corrigé), comparé à
/// **toutes** les photos du compte dans la base (calcul fait par sqlite-vec). Aucun réseau.
pub fn seen(c: &Connection, owner: &str, query: &[f32]) -> rusqlite::Result<Seen> {
    let Ok(blob) = astro_db::VectorTable::SearchVision.encode(query) else { return Ok(Seen::default()) };
    let mut stmt = c.prepare_cached(
        "SELECT f.sha, f.t_ms, 1.0 - vec_distance_cosine(v.embedding, ?1) FROM search_vision_vectors v JOIN search_frames f ON f.vis_id = v.vis_id WHERE v.owner_hex = ?2",
    )?;
    let rows: Vec<(String, i64, f64)> = stmt.query_map(params![blob, owner], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<Result<_, _>>()?;
    let scores: Vec<f32> = rows.iter().map(|(_, _, s)| *s as f32).collect();
    let distinct = |order: &mut dyn Iterator<Item = usize>, max: usize| {
        let mut out: Vec<(String, i64)> = Vec::new();
        for i in order {
            if !out.iter().any(|(sha, _)| *sha == rows[i].0) {
                out.push((rows[i].0.clone(), rows[i].1));
            }
            if out.len() >= max {
                break;
            }
        }
        out
    };
    let mut by_score: Vec<usize> = (0..rows.len()).collect();
    by_score.sort_by(|&a, &b| scores[b].total_cmp(&scores[a]));
    Ok(Seen { strict: distinct(&mut astro_index::siglip::stand_out(&scores).into_iter(), MAX_SEEN), near: distinct(&mut by_score.into_iter(), MAX_NEAR) })
}


/// Requête FTS5 sûre : chaque mot entre guillemets (guillemets internes doublés), **mot entier** au pluriel près (comme
/// [`astro_db::has_word`] : « chat » ne trouve pas « achat »), tous requis. `None` si aucun mot n'a de lettre ni de chiffre.
pub fn fts_query(q: &str) -> Option<String> {
    fts_words(q, " ")
}

fn fts_words(q: &str, sep: &str) -> Option<String> {
    let quote = |w: &str| format!("\"{}\"", w.chars().take(64).collect::<String>().replace('"', "\"\""));
    let groups: Vec<String> = q
        .split_whitespace()
        .take(8)
        .flat_map(|w| astro_db::words_of(w).collect::<Vec<_>>())
        .map(|w| {
            // Le mot, sa forme sans pluriel, et ses pluriels : l'index FTS est déjà sans accents ni majuscules.
            let stem = astro_db::word_stem(&w);
            let mut forms = vec![w, stem.clone(), format!("{stem}s"), format!("{stem}x")];
            forms.sort();
            forms.dedup();
            format!("({})", forms.iter().map(|f| quote(f)).collect::<Vec<_>>().join(" OR "))
        })
        .collect();
    (!groups.is_empty()).then(|| groups.join(sep))
}

/// Quelques mots autour du premier mot cherché trouvé dans `text` (la description d'une photo) : pourquoi elle est là.
fn excerpt_of(text: &str, words: &[String]) -> Option<String> {
    let toks: Vec<&str> = text.split_whitespace().collect();
    let i = toks.iter().position(|t| words.iter().any(|w| astro_db::has_word(t, w)))?;
    let (a, b) = (i.saturating_sub(5), (i + 6).min(toks.len()));
    Some(format!("{}{}{}", if a > 0 { "…" } else { "" }, toks[a..b].join(" "), if b < toks.len() { "…" } else { "" }))
}

/// Empreintes sûres pour la requête (lettres et chiffres seulement, rien d'autre ne passe) : `e.sha_plain IN (…)`.
fn sha_list(list: &[(String, i64)]) -> Vec<&(String, i64)> {
    list.iter().filter(|(x, _)| !x.is_empty() && x.len() <= 128 && x.bytes().all(|b| b.is_ascii_alphanumeric())).collect()
}

/// Façon de lire les résultats.
#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// Tous les termes, par date, page par page.
    Page,
    /// Tous les termes, les résultats « forts » (texte lu, vus par l'image) d'abord : pour classer les meilleurs.
    Rank,
    /// Un terme au moins, ou parmi les photos les plus proches : « Peut-être ».
    Loose,
}

/// Condition SQL d'un terme : trouvé par **tous** ses mots (nom ou dossier, description, texte lu), ou par ce qu'il désigne
/// (personne, lieu, année, mois). Un mot se cherche entier ([`astro_db::has_word`]).
fn term_sql(t: &crate::search_query::Term, args: &mut Vec<rusqlite::types::Value>) -> String {
    let words: Vec<String> = t
        .words
        .iter()
        .map(|w| {
            args.extend([w.clone().into(), w.clone().into()]);
            let mut alt = "has_word(e.path, ?) OR has_word(e.desc_text, ?)".to_owned();
            if let Some(f) = fts_query(w) {
                alt += " OR e.sha_plain IN (SELECT sha FROM search_text WHERE search_text MATCH ? AND owner_hex = ?1)";
                args.push(f.into());
            }
            format!("({alt})")
        })
        .collect();
    let mut any = vec![format!("({})", words.join(" AND "))];
    if !t.people.is_empty() {
        any.push(format!("EXISTS (SELECT 1 FROM faces f WHERE f.owner_hex = e.owner_hex AND f.ucloud_path = e.path AND f.name IN ({}))", vec!["?"; t.people.len()].join(",")));
        args.extend(t.people.iter().map(|p| p.clone().into()));
    }
    for (s, w, n, e) in &t.places {
        any.push("(e.lat BETWEEN ? AND ? AND e.lon BETWEEN ? AND ?)".into());
        args.extend([(*s).into(), (*n).into(), (*w).into(), (*e).into()]);
    }
    if let Some(y) = t.year {
        any.push("(COALESCE(e.taken_at, e.mtime) >= ? AND COALESCE(e.taken_at, e.mtime) < ?)".into());
        args.extend([(days_from_civil(i64::from(y), 1, 1) * 86_400).into(), (days_from_civil(i64::from(y) + 1, 1, 1) * 86_400).into()]);
    }
    if let Some(m) = t.month {
        any.push("strftime('%m', COALESCE(e.taken_at, e.mtime), 'unixepoch') = ?".into());
        args.push(format!("{m:02}").into());
    }
    format!("({})", any.join(" OR "))
}

/// Une lecture de la recherche (voir [`Mode`]) : `(élément, empreinte)` ; une ligne de plus que `limit` signale une page suivante.
fn select(c: &Connection, owner: &str, s: &Search, terms: &[crate::search_query::Term], mode: Mode, limit: usize) -> rusqlite::Result<Vec<(Value, String)>> {
    let words: Vec<String> = terms.iter().flat_map(|t| t.words.iter().cloned()).collect();
    let all_words = words.join(" ");
    // Extrait : n'importe lequel des mots (chacun peut avoir été trouvé dans une source différente).
    let fts = fts_words(&all_words, " OR ");
    let shown = sha_list(if mode == Mode::Loose { &s.near } else { &s.seen });
    let seen_sql = (!shown.is_empty()).then(|| format!("e.sha_plain IN ({})", shown.iter().map(|(x, _)| format!("'{x}'")).collect::<Vec<_>>().join(",")));
    // Pourquoi c'est trouvé : la source (`ocr`, `fiche` ; `vision` = vue par l'image, avec l'instant ; `desc` = sa description
    // dit l'un des mots) et un extrait, séparés par l'octet 0x1F. L'extrait d'une description se fait plus bas, en Rust.
    let by_image = if shown.is_empty() { "NULL".to_owned() } else { format!("CASE e.sha_plain {} END", shown.iter().map(|(x, t)| format!("WHEN '{x}' THEN 'vision' || char(31) || '{t}'")).collect::<Vec<_>>().join(" ")) };
    // Numérotés explicitement (après ?1 le compte et ?2 la requête FTS) : un `?` anonyme placé avant tout `?1` dans le texte
    // prendrait le numéro 1.
    let first = if fts.is_some() { 3 } else { 2 };
    let by_desc = if words.is_empty() {
        "NULL".to_owned()
    } else {
        format!("CASE WHEN {} THEN 'desc' || char(31) END", (0..words.len()).map(|i| format!("has_word(e.desc_text, ?{})", first + i)).collect::<Vec<_>>().join(" OR "))
    };
    let why = if fts.is_some() {
        format!("COALESCE((SELECT src || char(31) || snippet(search_text, 0, '', '', '…', 10) FROM search_text WHERE search_text MATCH ?2 AND owner_hex = ?1 AND sha = e.sha_plain LIMIT 1), {by_image}, {by_desc})")
    } else {
        format!("COALESCE({by_image}, {by_desc})")
    };
    // Film ou musique : un son, ou un fichier qui a sa fiche de la Médiathèque à côté (`<nom>.media.json`).
    let media = "(e.mime LIKE 'audio/%' OR EXISTS (SELECT 1 FROM ucloud_entries f WHERE f.owner_hex = e.owner_hex AND f.parent = e.parent AND f.path GLOB '*.media.json' \
                 AND substr(e.path, 1, length(f.path) - 10) = substr(f.path, 1, length(f.path) - 10)))";
    let mut sql = format!("SELECT {COLS}, {why} AS why, {media}, e.desc_text FROM ucloud_entries e WHERE {FILES}");
    // Paramètres dans l'ordre du texte : compte (?1), requête FTS (?2), mots de la description (raison), puis les filtres.
    let mut args: Vec<rusqlite::types::Value> = vec![owner.to_owned().into()];
    if let Some(f) = &fts {
        args.push(f.clone().into());
    }
    args.extend(words.iter().map(|w| w.clone().into()));
    if !terms.is_empty() {
        let (plain, named): (Vec<_>, Vec<_>) = terms.iter().partition(|t| t.plain());
        let named: Vec<String> = named.iter().map(|t| term_sql(t, &mut args)).collect();
        let plain: Vec<String> = plain.iter().map(|t| term_sql(t, &mut args)).collect();
        if mode == Mode::Loose {
            let any: Vec<String> = named.into_iter().chain(plain).chain(seen_sql).collect();
            sql += &format!(" AND ({})", any.join(" OR "));
        } else {
            for n in named {
                sql += &format!(" AND {n}");
            }
            // Les mots « à voir » : tous trouvés quelque part, ou la photo les montre (la recherche par l'image les a reçus ensemble).
            match (plain.is_empty(), seen_sql) {
                (true, _) => {}
                (false, Some(img)) => sql += &format!(" AND (({}) OR {img})", plain.join(" AND ")),
                (false, None) => sql += &format!(" AND {}", plain.join(" AND ")),
            }
        }
    }
    match s.kind.as_deref() {
        Some("image") => sql += " AND e.mime LIKE 'image/%'",
        Some("video") => sql += " AND e.mime LIKE 'video/%'",
        Some("other") => sql += " AND (e.mime IS NULL OR (e.mime NOT LIKE 'image/%' AND e.mime NOT LIKE 'video/%'))",
        _ => {}
    }
    if let Some(p) = s.person.as_deref().filter(|p| !p.is_empty()) {
        sql += " AND EXISTS (SELECT 1 FROM faces f WHERE f.owner_hex = e.owner_hex AND f.ucloud_path = e.path AND f.name = ?)";
        args.push(p.to_owned().into());
    }
    if let Some(from) = s.from {
        sql += " AND COALESCE(e.taken_at, e.mtime) >= ?";
        args.push(from.into());
    }
    if let Some(to) = s.to {
        sql += " AND COALESCE(e.taken_at, e.mtime) < ?";
        args.push(to.into());
    }
    if let Some((south, west, north, east)) = s.bbox {
        sql += " AND e.lat BETWEEN ? AND ?";
        args.extend([south.into(), north.into()]);
        // Une zone qui franchit l'antiméridien (ouest > est) se coupe en deux intervalles.
        if west <= east {
            sql += " AND e.lon BETWEEN ? AND ?";
        } else {
            sql += " AND (e.lon >= ? OR e.lon <= ?)";
        }
        args.extend([west.into(), east.into()]);
    }
    if let (Some((t, path)), Mode::Page) = (&s.after, mode) {
        sql += " AND (COALESCE(e.taken_at, e.mtime) < ? OR (COALESCE(e.taken_at, e.mtime) = ? AND e.path > ?))";
        args.extend([(*t).into(), (*t).into(), path.clone().into()]);
    }
    sql += if mode == Mode::Page { " ORDER BY t DESC, e.path ASC LIMIT ?" } else { " ORDER BY why IS NULL, t DESC, e.path ASC LIMIT ?" };
    args.push(i64::try_from(limit + 1).unwrap_or(MAX_RANKED).into());
    let mut stmt = c.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(args.iter()), |r| {
        let mut v = item(r)?;
        if let Some((src, x)) = r.get::<_, Option<String>>(11)?.as_deref().and_then(|s| s.split_once('\u{1f}')) {
            v["found"] = json!(match src {
                "vision" => "seen",
                "fiche" => "fiche",
                "desc" => "desc",
                _ => "text",
            });
            match src {
                "vision" => v["at_ms"] = json!(x.parse::<i64>().unwrap_or(0)),
                "desc" => v["excerpt"] = json!(r.get::<_, Option<String>>(13)?.as_deref().and_then(|d| excerpt_of(d, &words))),
                _ => v["excerpt"] = json!(x),
            }
        }
        if r.get::<_, bool>(12)? {
            v["media"] = json!(true);
        }
        Ok((v, r.get::<_, Option<String>>(10)?.unwrap_or_default()))
    })?;
    rows.collect()
}

/// Les meilleurs résultats d'abord : les mots dans le nom du fichier (mots entiers), puis le texte lu, puis la description de
/// la photo, puis la photo qui montre le mieux la requête ; à égalité, le plus récent. Seuls les résultats qui ont une raison
/// forte d'être là sont gardés ; moins de deux : rien (la liste par date suffit).
fn best(rows: Vec<(Value, String)>, terms: &[crate::search_query::Term], seen: &[(String, i64)]) -> Vec<Value> {
    let words: Vec<&String> = terms.iter().flat_map(|t| t.words.iter()).filter(|w| astro_db::words_of(w).next().is_some()).collect();
    let mut scored: Vec<(f64, Value)> = rows
        .into_iter()
        .map(|(v, sha)| {
            let name = v["path"].as_str().unwrap_or("").rsplit('/').next().unwrap_or("");
            let in_name = if words.is_empty() { 0.0 } else { words.iter().filter(|w| astro_db::has_word(name, w)).count() as f64 / words.len() as f64 };
            let text = match v["found"].as_str() {
                Some("text" | "fiche") => 2.5,
                Some("desc") => 2.2,
                _ => 0.0,
            };
            let image = seen.iter().position(|(x, _)| *x == sha).map_or(0.0, |p| 2.0 - 1.5 * p as f64 / seen.len().max(1) as f64);
            (3.0 * in_name + text + image, v)
        })
        .filter(|(s, _)| *s > 0.0)
        .collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    if scored.len() < 2 {
        return Vec::new();
    }
    scored.into_iter().take(MAX_BEST).map(|(_, v)| v).collect()
}

/// Recherche à filtres combinés, paginée par curseur `(date, chemin)` (stable même si des fichiers arrivent pendant la lecture).
/// Rend `{ items, next }` ; `next` est `null` à la dernière page. Un élément trouvé par le texte lu dedans porte `found: "text"`
/// (lu dans le fichier) ou `"fiche"` (fiche de la Médiathèque) avec un extrait (`excerpt`), ou `"seen"` (la photo montre ce que
/// dit la requête : [`Search::seen`], sans extrait, avec `at_ms` : l'instant de l'image, pour une vidéo) ; `media: true` pour
/// un film ou un morceau.
///
/// À la première page d'une recherche par mots, s'y ajoutent `understood` (ce que la station a compris de chaque groupe de
/// mots : personne, lieu, année, mois), `best` (les meilleurs résultats, s'il y a de quoi classer) et, si rien n'est trouvé,
/// `maybe` : ce qui répond à **un** des mots, ou les photos les plus proches de la requête ([`Search::near`]).
pub fn search(c: &Connection, owner: &str, s: &Search) -> rusqlite::Result<Value> {
    let terms = match s.q.as_deref() {
        Some(q) => crate::search_query::understand(c, owner, q)?,
        None => Vec::new(),
    };
    let limit = s.limit.clamp(1, 200);
    let mut rows = select(c, owner, s, &terms, Mode::Page, limit)?;
    let next = if rows.len() > limit {
        rows.truncate(limit);
        rows.last().map(|(l, _)| json!({ "t": l["t"], "path": l["path"] }))
    } else {
        None
    };
    let empty = rows.is_empty();
    let mut out = json!({ "items": rows.into_iter().map(|(v, _)| v).collect::<Vec<_>>(), "next": next });
    if !terms.is_empty() && s.after.is_none() {
        out["understood"] = json!(terms.iter().filter(|t| !t.plain()).map(crate::search_query::Term::view).collect::<Vec<_>>());
        if empty {
            let maybe = select(c, owner, s, &terms, Mode::Loose, MAX_BEST)?;
            out["maybe"] = json!(maybe.into_iter().take(MAX_BEST).map(|(v, _)| v).collect::<Vec<_>>());
        } else {
            let ranked = select(c, owner, s, &terms, Mode::Rank, usize::try_from(MAX_RANKED).unwrap_or(300))?;
            out["best"] = json!(best(ranked, &terms, &s.seen));
        }
    }
    Ok(out)
}

/// Texte lu dans un fichier du compte (OCR des images et des PDF) : `{ text }`, `null` s'il n'y en a pas (pas encore lu, ou rien
/// à lire). Pour « Copier le texte » de la visionneuse.
pub fn text(c: &Connection, owner: &str, path: &str) -> rusqlite::Result<Value> {
    let body: Option<String> = rusqlite::OptionalExtension::optional(c.query_row(
        "SELECT t.body FROM ucloud_entries e JOIN search_text t ON t.sha = e.sha_plain WHERE e.owner_hex = ?1 AND e.path = ?2 AND e.type = 'file' \
         AND t.owner_hex = ?1 AND t.src = 'ocr' LIMIT 1",
        params![owner, path],
        |r| r.get(0),
    ))?;
    Ok(json!({ "text": body }))
}

/// Zone et période d'une demande de carte.
#[derive(Debug, Clone, Default)]
pub struct MapArea {
    /// Début de période (secondes Unix, inclus).
    pub from: Option<i64>,
    /// Fin de période (exclue).
    pub to: Option<i64>,
    /// Sud, ouest, nord, est (degrés) ; tout le globe si absent.
    pub bbox: Option<(f64, f64, f64, f64)>,
    /// Côté d'une case de regroupement, en degrés.
    pub cell: f64,
    /// Rend les éléments de la zone au lieu des regroupements.
    pub items: bool,
}

/// Photos et vidéos géolocalisées. Sans `items` : des **regroupements** par case (`{ lat, lon, n, t0, t1 }`, au plus 1500), un
/// histogramme par mois de **toutes** les photos géolocalisées (pour choisir la période) et le total. Avec `items` : les éléments
/// de la zone, du plus récent au plus ancien (au plus 120). Tout est calculé ici : la carte ne reçoit jamais 30 000 points.
pub fn map(c: &Connection, owner: &str, a: &MapArea) -> rusqlite::Result<Value> {
    let media = "(e.mime LIKE 'image/%' OR e.mime LIKE 'video/%')";
    let (south, west, north, east) = a.bbox.unwrap_or((-90.0, -180.0, 90.0, 180.0));
    let (from, to) = (a.from.unwrap_or(i64::MIN), a.to.unwrap_or(i64::MAX));
    let zone = format!("{FILES} AND {media} AND e.lat IS NOT NULL AND e.lon IS NOT NULL AND e.lat BETWEEN ?2 AND ?3 AND e.lon BETWEEN ?4 AND ?5 AND COALESCE(e.taken_at, e.mtime, 0) >= ?6 AND COALESCE(e.taken_at, e.mtime, 0) < ?7");
    if a.items {
        let sql = format!("SELECT {COLS} FROM ucloud_entries e WHERE {zone} ORDER BY t DESC, e.path LIMIT 120");
        let mut stmt = c.prepare_cached(&sql)?;
        let items = stmt.query_map(params![owner, south, north, west, east, from, to], item)?.collect::<Result<Vec<_>, _>>()?;
        return Ok(json!({ "items": items }));
    }
    let cell = if a.cell.is_finite() { a.cell.clamp(0.01, 90.0) } else { 5.0 };
    let sql = format!(
        "SELECT AVG(e.lat), AVG(e.lon), count(*), MIN(COALESCE(e.taken_at, e.mtime)), MAX(COALESCE(e.taken_at, e.mtime)) FROM ucloud_entries e WHERE {zone} \
         GROUP BY (CAST(e.lat / {cell} AS INTEGER) - (e.lat / {cell} < CAST(e.lat / {cell} AS INTEGER))), (CAST(e.lon / {cell} AS INTEGER) - (e.lon / {cell} < CAST(e.lon / {cell} AS INTEGER))) ORDER BY count(*) DESC LIMIT 1500"
    );
    let mut stmt = c.prepare_cached(&sql)?;
    let points = stmt
        .query_map(params![owner, south, north, west, east, from, to], |r| Ok(json!({ "lat": r.get::<_, f64>(0)?, "lon": r.get::<_, f64>(1)?, "n": r.get::<_, i64>(2)?, "t0": r.get::<_, Option<i64>>(3)?, "t1": r.get::<_, Option<i64>>(4)? })))?
        .collect::<Result<Vec<_>, _>>()?;
    let sql = format!("SELECT strftime('%Y-%m', COALESCE(e.taken_at, e.mtime), 'unixepoch') AS ym, count(*) FROM ucloud_entries e WHERE {FILES} AND {media} AND e.lat IS NOT NULL AND e.lon IS NOT NULL AND COALESCE(e.taken_at, e.mtime) IS NOT NULL GROUP BY ym ORDER BY ym");
    let mut stmt = c.prepare_cached(&sql)?;
    let months = stmt.query_map([owner], |r| Ok(json!({ "ym": r.get::<_, String>(0)?, "n": r.get::<_, i64>(1)? })))?.collect::<Result<Vec<_>, _>>()?;
    let total: i64 = months.iter().map(|m| m["n"].as_i64().unwrap_or(0)).sum();
    let all: i64 = c.query_row(&format!("SELECT count(*) FROM ucloud_entries e WHERE {FILES} AND {media}"), [owner], |r| r.get(0))?;
    Ok(json!({ "points": points, "months": months, "geotagged": total, "photos": all }))
}

/// Un doublon candidat.
struct Dup {
    path: String,
    sha: Option<String>,
    phash: i64,
    size: i64,
    scored: i64,
}

fn find(parent: &mut [usize], mut i: usize) -> usize {
    while parent[i] != i {
        parent[i] = parent[parent[i]];
        i = parent[i];
    }
    i
}

/// Doublons du compte : `exact` (même contenu, même SHA-256 du clair) et `similar` (images presque identiques, distance de Hamming
/// des empreintes perceptuelles ≤ 6, contenus différents). Chaque groupe propose un `keep` : le plus gros fichier, puis celui qui a le plus
/// de métadonnées (date, position). **Rien n'est supprimé ici** : l'interface demande confirmation.
pub fn duplicates(c: &Connection, owner: &str, all: bool) -> rusqlite::Result<Value> {
    // Exacts : par défaut seulement les photos et vidéos (la bibliothèque est celle des médias) ; `all` y ajoute tous les fichiers.
    let media = if all { "" } else { " AND (mime LIKE 'image/%' OR mime LIKE 'video/%')" };
    let media_e = if all { "" } else { " AND (e.mime LIKE 'image/%' OR e.mime LIKE 'video/%')" };
    let sql = format!(
        "SELECT {COLS} FROM ucloud_entries e WHERE {FILES}{media_e} AND e.sha_plain IN (SELECT sha_plain FROM ucloud_entries WHERE owner_hex = ?1 AND type = 'file' AND cid IS NOT NULL AND sha_plain IS NOT NULL{media} GROUP BY sha_plain HAVING count(*) > 1) \
         ORDER BY e.sha_plain, e.size DESC, e.path LIMIT 5000"
    );
    let mut stmt = c.prepare(&sql)?;
    let mut exact: Vec<(String, Vec<Value>)> = Vec::new();
    let mut rows = stmt.query([owner])?;
    while let Some(r) = rows.next()? {
        let sha: String = r.get(10)?;
        let it = item(r)?;
        match exact.last_mut() {
            Some((s, items)) if *s == sha => items.push(it),
            _ => exact.push((sha, vec![it])),
        }
    }
    let exact: Vec<Value> = exact.into_iter().map(|(sha, items)| json!({ "sha": sha, "items": items, "keep": 0 })).collect();

    // Similaires : bandes de bits (au plus 6 bits diffèrent donc au moins une des 7 bandes de 9 bits est identique).
    let mut stmt = c.prepare(
        "SELECT path, sha_plain, phash, COALESCE(size, 0), (lat IS NOT NULL) + (taken_at IS NOT NULL) FROM ucloud_entries \
         WHERE owner_hex = ?1 AND type = 'file' AND cid IS NOT NULL AND phash IS NOT NULL AND mime LIKE 'image/%' LIMIT ?2",
    )?;
    let dups: Vec<Dup> = stmt
        .query_map(params![owner, i64::try_from(MAX_PHASH).unwrap_or(i64::MAX)], |r| Ok(Dup { path: r.get(0)?, sha: r.get(1)?, phash: r.get(2)?, size: r.get(3)?, scored: r.get(4)? }))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut parent: Vec<usize> = (0..dups.len()).collect();
    let mut buckets: std::collections::HashMap<(u8, u16), Vec<usize>> = std::collections::HashMap::new();
    for (i, d) in dups.iter().enumerate() {
        let bits = u64::from_ne_bytes(d.phash.to_ne_bytes());
        for band in 0..7u8 {
            buckets.entry((band, ((bits >> (u32::from(band) * 9)) & 0x1ff) as u16)).or_default().push(i);
        }
    }
    for members in buckets.values().filter(|m| m.len() > 1 && m.len() <= 2000) {
        for (a, &i) in members.iter().enumerate() {
            for &j in &members[a + 1..] {
                if (dups[i].phash ^ dups[j].phash).count_ones() <= NEAR {
                    let (ri, rj) = (find(&mut parent, i), find(&mut parent, j));
                    if ri != rj {
                        parent[ri] = rj;
                    }
                }
            }
        }
    }
    let mut groups: std::collections::BTreeMap<usize, Vec<usize>> = std::collections::BTreeMap::new();
    for i in 0..dups.len() {
        let root = find(&mut parent, i);
        groups.entry(root).or_default().push(i);
    }
    let mut similar = Vec::new();
    for members in groups.into_values().filter(|m| m.len() > 1) {
        // Un groupe dont tous les membres ont le même contenu est déjà dans `exact`.
        let shas: std::collections::BTreeSet<_> = members.iter().map(|&i| dups[i].sha.as_deref().unwrap_or("")).collect();
        if shas.len() < 2 {
            continue;
        }
        let keep = members.iter().enumerate().max_by_key(|&(_, &i)| (dups[i].size, dups[i].scored)).map_or(0, |(k, _)| k);
        let mut paths: Vec<&str> = members.iter().map(|&i| dups[i].path.as_str()).collect();
        let keep_path = paths[keep].to_owned();
        paths.sort_unstable();
        let items = paths
            .iter()
            .map(|p| c.query_row(&format!("SELECT {COLS} FROM ucloud_entries e WHERE e.owner_hex = ?1 AND e.path = ?2"), params![owner, p], item))
            .collect::<Result<Vec<_>, _>>()?;
        let keep = paths.iter().position(|p| **p == keep_path).unwrap_or(0);
        similar.push(json!({ "items": items, "keep": keep }));
        if similar.len() >= 500 {
            break;
        }
    }
    Ok(json!({ "exact": exact, "similar": similar }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: &str = "aa";
    const SHA_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn db() -> Connection {
        astro_db::open_in_memory().unwrap()
    }

    #[allow(clippy::too_many_arguments)]
    fn put(c: &Connection, owner: &str, path: &str, mime: &str, size: i64, taken: Option<i64>, sha: &str, phash: Option<i64>, desc: Option<&str>) {
        c.execute(
            "INSERT INTO ucloud_entries(owner_hex, path, parent, type, cid, mime, size, mtime, taken_at, sha_plain, phash, desc_text, data) VALUES (?1, ?2, '/', 'file', ?3, ?4, ?5, 1000, ?6, ?7, ?8, ?9, '{}')",
            params![owner, path, format!("cid-{path}"), mime, size, taken, sha, phash, desc],
        )
        .unwrap();
    }

    // 2024-05-17 12:00 UTC et 2026-05-17 12:00 UTC.
    const T2024: i64 = 1_715_947_200;
    const T2026: i64 = 1_779_019_200;

    #[test]
    fn have_reports_only_what_this_account_owns() {
        let c = db();
        put(&c, ME, "/a.jpg", "image/jpeg", 10, None, SHA_A, None, None);
        put(&c, "bb", "/autre.jpg", "image/jpeg", 10, None, &"bb".repeat(32), None, None);
        let r = have(&c, ME, &[SHA_A.to_uppercase(), "bb".repeat(32), "pas-un-sha".into()]).unwrap();
        assert_eq!(r, vec![json!({ "sha": SHA_A, "path": "/a.jpg" })], "le contenu d'un autre compte ne se devine pas");
        assert!(have(&c, ME, &[]).unwrap().is_empty());
    }

    #[test]
    fn months_and_memories() {
        assert_eq!(month_bounds("2026-05"), Some((1_777_593_600, 1_780_272_000)));
        assert_eq!(month_bounds("2026-12").map(|b| b.1 - b.0), Some(31 * 86_400));
        assert_eq!(month_bounds("2026-13"), None);
        assert_eq!(month_bounds("26-05"), None);
        let c = db();
        put(&c, ME, "/2024.jpg", "image/jpeg", 1, Some(T2024), "", None, None);
        put(&c, ME, "/2026.jpg", "image/jpeg", 1, Some(T2026), "", None, None);
        put(&c, ME, "/doc.pdf", "application/pdf", 1, Some(T2024), "", None, None);
        let b = timeline_buckets(&c, ME).unwrap();
        assert_eq!(b, vec![json!({ "ym": "2026-05", "count": 1 }), json!({ "ym": "2024-05", "count": 1 })]);
        let m = timeline_month(&c, ME, "2024-05").unwrap().unwrap();
        assert_eq!(m.len(), 1);
        assert_eq!(m[0]["path"], "/2024.jpg");
        assert!(timeline_month(&c, ME, "n'importe quoi").unwrap().is_none());
        let mem = memories(&c, ME, "05-17", 2026).unwrap();
        assert_eq!(mem.len(), 1, "l'année en cours est exclue");
        assert_eq!((mem[0]["year"].as_i64(), mem[0]["ago"].as_i64(), mem[0]["items"][0]["path"].as_str()), (Some(2024), Some(2), Some("/2024.jpg")));
        assert!(memories(&c, ME, "05/17", 2026).unwrap().is_empty());
    }

    /// Relevé sur une vraie station : « chat » trouvait « achat », « 2025 » des photos de 2026 dont le nom portait l'heure
    /// 20:25, et une photo trouvée par sa description (« un chat tricolore ») n'était ni dans les meilleurs ni expliquée.
    #[test]
    fn a_word_is_found_whole_and_a_description_counts_among_the_best_with_its_excerpt() {
        let c = db();
        put(&c, "alice", "/Images/IMG_1.jpg", "image/jpeg", 10, Some(1_780_000_000), "d1", None, None);
        put(&c, "alice", "/Images/IMG_2.jpg", "image/jpeg", 10, Some(1_780_000_100), "d2", None, None);
        put(&c, "alice", "/Docs/ticket.jpg", "image/jpeg", 10, Some(1_780_000_200), "d3", None, None);
        put(&c, "alice", "/Images/2026/10/IMG_20260621_202533.jpg", "image/jpeg", 10, Some(1_782_000_000), "d4", None, None);
        put(&c, "alice", "/Images/2025/plage.jpg", "image/jpeg", 10, Some(1_740_000_000), "d5", None, None);
        for (path, desc) in [
            ("/Images/IMG_1.jpg", "un adulte assis tenant un chat tricolore sur ses genoux, lumière du soir"),
            ("/Images/IMG_2.jpg", "deux chats dorment au soleil"),
            ("/Docs/ticket.jpg", "un ticket de caisse : achat de pain"),
        ] {
            c.execute("UPDATE ucloud_entries SET desc_text = ?1 WHERE path = ?2", [desc, path]).unwrap();
        }
        let res = search(&c, "alice", &Search { q: Some("chat".into()), limit: 10, ..Search::default() }).unwrap();
        let paths: Vec<&str> = res["items"].as_array().unwrap().iter().map(|h| h["path"].as_str().unwrap()).collect();
        assert_eq!(paths, ["/Images/IMG_2.jpg", "/Images/IMG_1.jpg"], "mot entier, pluriel compris, jamais « achat »");
        let best = res["best"].as_array().unwrap();
        assert_eq!(best.len(), 2, "trouvées par leur description : parmi les meilleures");
        let one = best.iter().find(|h| h["path"] == "/Images/IMG_1.jpg").unwrap();
        assert_eq!(one["found"], "desc");
        assert_eq!(one["excerpt"], "un adulte assis tenant un chat tricolore sur ses genoux, lumière…", "l'extrait dit pourquoi");
        // « 2025 » : l'année (prise de vue) ou le dossier, jamais l'heure dans le nom d'une photo de 2026.
        let y: Vec<String> = search(&c, "alice", &Search { q: Some("2025".into()), limit: 10, ..Search::default() }).unwrap()["items"].as_array().unwrap().iter().map(|h| h["path"].as_str().unwrap().to_owned()).collect();
        assert_eq!(y, ["/Images/2025/plage.jpg"]);
    }

    #[test]
    fn words_read_inside_a_file_find_it_with_an_excerpt_and_only_for_its_owner() {
        let c = db();
        put(&c, "alice", "/Scans/scan001.jpg", "image/jpeg", 10, None, "s1", None, None);
        put(&c, "alice", "/Photos/facture-vacances.jpg", "image/jpeg", 10, None, "s2", None, None);
        put(&c, "bob", "/Scans/b.jpg", "image/jpeg", 10, None, "s3", None, None);
        c.execute_batch(
            "INSERT INTO search_text(body, owner_hex, sha, src, t_ms) VALUES ('FACTURE N° 117\nÉlectricité du hameau des Écluses', 'alice', 's1', 'ocr', 0), \
             ('Électricité facture', 'bob', 's3', 'ocr', 0)",
        )
        .unwrap();
        let find = |owner: &str, q: &str| search(&c, owner, &Search { q: Some(q.into()), limit: 10, ..Search::default() }).unwrap()["items"].as_array().unwrap().clone();
        let hits = find("alice", "electricite ecluse");
        assert_eq!(hits.len(), 1, "sans accents, au pluriel près");
        assert_eq!((hits[0]["path"].as_str(), hits[0]["found"].as_str()), (Some("/Scans/scan001.jpg"), Some("text")));
        assert!(hits[0]["excerpt"].as_str().unwrap().contains("Électricité"));
        let both: Vec<_> = find("alice", "facture").iter().map(|h| h["path"].as_str().unwrap().to_owned()).collect();
        assert_eq!(both.len(), 2, "par le nom ou par le texte : {both:?}");
        assert!(find("alice", "\"\" ** ()").is_empty() || fts_query("\"\" ** ()").is_none(), "ponctuation seule : pas d'erreur FTS");
        assert_eq!(text(&c, "alice", "/Scans/scan001.jpg").unwrap()["text"].as_str().map(|t| t.starts_with("FACTURE")), Some(true));
        assert!(text(&c, "bob", "/Scans/scan001.jpg").unwrap()["text"].is_null(), "pas le fichier de bob");
        assert!(text(&c, "alice", "/Photos/facture-vacances.jpg").unwrap()["text"].is_null());
    }

    #[test]
    fn a_photo_that_clearly_shows_the_query_is_found_by_the_image_and_only_for_its_owner() {
        let c = db();
        let unit = |i: usize, noise: usize| (0..768).map(|j| if j == i { 1.0 } else if j == 700 + noise % 60 { 0.3 } else { 0.0 }).collect::<Vec<f32>>();
        let add = |owner: &str, sha: &str, v: &[f32]| {
            put(&c, owner, &format!("/Photos/{sha}.jpg"), "image/jpeg", 10, None, sha, None, None);
            c.execute("INSERT INTO search_frames(owner_hex, sha, t_ms) VALUES (?1, ?2, 0)", params![owner, sha]).unwrap();
            c.execute("INSERT INTO search_vision_vectors(vis_id, owner_hex, embedding) VALUES (?1, ?2, ?3)", params![c.last_insert_rowid(), owner, astro_db::VectorTable::SearchVision.encode(v).unwrap()]).unwrap();
        };
        for n in 0..20 {
            add("alice", &format!("p{n}"), &unit(n, n));
        }
        add("alice", "velo", &unit(500, 0));
        add("bob", "velobob", &unit(500, 0));
        let found = seen(&c, "alice", &unit(500, 59)).unwrap();
        let hits = found.strict.clone();
        assert_eq!(hits, [("velo".to_owned(), 0)], "la seule photo qui ressemble à la requête, pas celle de Bob");
        assert_eq!((found.near.len(), found.near[0].0.as_str()), (MAX_NEAR, "velo"), "les plus proches, la meilleure d'abord");
        let nothing = seen(&c, "alice", &unit(600, 59)).unwrap();
        assert!(nothing.strict.is_empty() && nothing.near.len() == MAX_NEAR, "rien ne ressort : rien de sûr, mais des « peut-être »");
        assert!(seen(&c, "bob", &unit(500, 1)).unwrap().strict.is_empty(), "une seule photo : pas de quoi juger");
        let r = search(&c, "alice", &Search { q: Some("bicyclette".into()), seen: hits, limit: 10, ..Search::default() }).unwrap();
        let items = r["items"].as_array().unwrap();
        assert_eq!((items.len(), items[0]["path"].as_str(), items[0]["found"].as_str(), items[0].get("excerpt"), items[0]["at_ms"].as_i64()), (1, Some("/Photos/velo.jpg"), Some("seen"), None, Some(0)));
        let r = search(&c, "alice", &Search { q: Some("x".into()), seen: vec![("velo') OR 1=1 --".into(), 0)], limit: 10, ..Search::default() }).unwrap();
        assert!(r["items"].as_array().unwrap().is_empty(), "une empreinte douteuse est ignorée");
    }

    #[test]
    fn search_combines_filters_and_pages_by_cursor() {
        let c = db();
        put(&c, ME, "/Vacances/plage 100%.jpg", "image/jpeg", 1, Some(T2024), "", None, Some("Coucher de soleil"));
        put(&c, ME, "/Vacances/montagne.jpg", "image/jpeg", 1, Some(T2024 + 10), "", None, None);
        put(&c, ME, "/Docs/contrat.pdf", "application/pdf", 1, Some(T2026), "", None, None);
        put(&c, "bb", "/Vacances/autre.jpg", "image/jpeg", 1, Some(T2024), "", None, None);
        c.execute("UPDATE ucloud_entries SET lat = 45.7, lon = 4.8 WHERE path = '/Vacances/montagne.jpg'", []).unwrap();
        let q = |s: Search| search(&c, ME, &s).unwrap();
        let paths = |v: &Value| v["items"].as_array().unwrap().iter().map(|i| i["path"].as_str().unwrap().to_owned()).collect::<Vec<_>>();
        assert_eq!(paths(&q(Search { q: Some("VACANCES".into()), limit: 50, ..Search::default() })), ["/Vacances/montagne.jpg", "/Vacances/plage 100%.jpg"], "sans casse, et pas le compte voisin");
        assert_eq!(paths(&q(Search { q: Some("soleil".into()), limit: 50, ..Search::default() })), ["/Vacances/plage 100%.jpg"], "description");
        assert_eq!(paths(&q(Search { q: Some("100%".into()), limit: 50, ..Search::default() })).len(), 1, "le % est littéral");
        assert_eq!(paths(&q(Search { kind: Some("other".into()), limit: 50, ..Search::default() })), ["/Docs/contrat.pdf"]);
        assert_eq!(paths(&q(Search { bbox: Some((45.0, 4.0, 46.0, 5.0)), limit: 50, ..Search::default() })), ["/Vacances/montagne.jpg"]);
        assert_eq!(paths(&q(Search { from: Some(T2026 - 1), to: Some(T2026 + 1), limit: 50, ..Search::default() })), ["/Docs/contrat.pdf"]);
        // Curseur : deux pages d'un résultat.
        let p1 = q(Search { limit: 2, ..Search::default() });
        assert_eq!(paths(&p1).len(), 2);
        let next = &p1["next"];
        let p2 = q(Search { limit: 2, after: Some((next["t"].as_i64().unwrap(), next["path"].as_str().unwrap().to_owned())), ..Search::default() });
        assert_eq!(paths(&p2), ["/Vacances/plage 100%.jpg"]);
        assert!(p2["next"].is_null());
    }

    #[test]
    fn each_word_may_be_found_in_a_different_place_and_people_places_dates_are_understood() {
        let c = db();
        put(&c, ME, "/Docs/facture.pdf", "application/pdf", 1, Some(T2024), "f1", None, None);
        put(&c, ME, "/Photos/IMG_1.jpg", "image/jpeg", 1, Some(T2024), "p1", None, None);
        put(&c, ME, "/Photos/IMG_2.jpg", "image/jpeg", 1, Some(T2026), "p2", None, None);
        put(&c, ME, "/Photos/plage.jpg", "image/jpeg", 1, Some(T2024), "p3", None, None);
        put(&c, ME, "/Films/Le Voyage.mkv", "video/x-matroska", 1, None, "m1", None, None);
        put(&c, ME, "/Films/Le Voyage.media.json", "application/json", 1, None, "m2", None, None);
        c.execute_batch(
            "INSERT INTO search_text(body, owner_hex, sha, src, t_ms) VALUES ('EDF Électricité', 'aa', 'f1', 'ocr', 0); \
             INSERT INTO faces(owner_hex, name, embed_hash, ts, ucloud_path) VALUES ('aa', 'Léa', 'h1', 0, '/Photos/IMG_1.jpg'), ('aa', 'Léa', 'h2', 0, '/Photos/IMG_2.jpg'); \
             UPDATE ucloud_entries SET lat = 45.76, lon = 4.84 WHERE path = '/Photos/IMG_2.jpg'",
        )
        .unwrap();
        let q = |text: &str| search(&c, ME, &Search { q: Some(text.into()), limit: 50, ..Search::default() }).unwrap();
        let paths = |v: &Value, k: &str| v[k].as_array().unwrap().iter().map(|i| i["path"].as_str().unwrap().to_owned()).collect::<Vec<_>>();
        assert_eq!(paths(&q("facture edf"), "items"), ["/Docs/facture.pdf"], "un mot dans le nom, l'autre dans le texte lu");
        let r = q("lea 2024");
        assert_eq!(paths(&r, "items"), ["/Photos/IMG_1.jpg"], "Léa reconnue, prise en 2024");
        assert_eq!((r["understood"][0]["as"][0].as_str(), r["understood"][1]["as"][0].as_str()), (Some("person"), Some("year")));
        assert_eq!(paths(&q("Lyon"), "items"), ["/Photos/IMG_2.jpg"], "prise à Lyon, sans service de carte");
        assert_eq!(paths(&q("mai"), "items").len(), 4, "les fichiers de mai, toutes années");
        let r = q("voyage");
        assert_eq!((r["items"][0]["media"].as_bool(), r["items"].as_array().unwrap().len()), (Some(true), 1), "le film, pas sa fiche");
        // Rien de sûr : ce qui répond à un des mots.
        let r = q("plage zanzibar");
        assert!(r["items"].as_array().unwrap().is_empty());
        assert_eq!(paths(&r, "maybe"), ["/Photos/plage.jpg"]);
        assert!(r.get("best").is_none());
        // Pages suivantes : ni compréhension, ni meilleurs.
        let p2 = search(&c, ME, &Search { q: Some("lea".into()), after: Some((T2026 + 1, String::new())), limit: 50, ..Search::default() }).unwrap();
        assert!(p2.get("understood").is_none() && p2.get("best").is_none());
    }

    #[test]
    fn the_best_results_come_first_name_then_text_then_image() {
        let c = db();
        put(&c, ME, "/a/recu.jpg", "image/jpeg", 1, Some(T2026), "r1", None, None);
        put(&c, ME, "/a/scan.jpg", "image/jpeg", 1, Some(T2026 - 10), "r2", None, None);
        put(&c, ME, "/a/velo rouge.jpg", "image/jpeg", 1, Some(T2024), "r3", None, None);
        c.execute_batch("INSERT INTO search_text(body, owner_hex, sha, src, t_ms) VALUES ('un velo rouge', 'aa', 'r2', 'ocr', 0)").unwrap();
        let r = search(&c, ME, &Search { q: Some("velo".into()), seen: vec![("r1".into(), 0)], limit: 50, ..Search::default() }).unwrap();
        let order = |k: &str| r[k].as_array().unwrap().iter().map(|i| i["path"].as_str().unwrap().to_owned()).collect::<Vec<_>>();
        assert_eq!(order("items"), ["/a/recu.jpg", "/a/scan.jpg", "/a/velo rouge.jpg"], "page : par date");
        assert_eq!(order("best"), ["/a/velo rouge.jpg", "/a/scan.jpg", "/a/recu.jpg"], "meilleurs : nom, texte lu, image");
    }

    #[test]
    fn duplicates_group_exact_copies_and_near_images_without_touching_anything() {
        let c = db();
        let h: i64 = 0x0F0F_0F0F_0F0F_0F0F;
        put(&c, ME, "/a.jpg", "image/jpeg", 500, Some(T2024), SHA_A, Some(h), None);
        put(&c, ME, "/copie de a.jpg", "image/jpeg", 500, None, SHA_A, Some(h), None);
        // Même image recompressée : empreinte à 3 bits, contenu différent, plus gros.
        put(&c, ME, "/a-grande.jpg", "image/jpeg", 900, Some(T2024), &"cc".repeat(32), Some(h ^ 0b10101), None);
        // Image sans rapport.
        put(&c, ME, "/autre.jpg", "image/jpeg", 100, None, &"dd".repeat(32), Some(!h), None);
        // Un autre compte avec le même contenu : jamais mélangé.
        put(&c, "bb", "/x.jpg", "image/jpeg", 500, None, SHA_A, Some(h), None);
        let d = duplicates(&c, ME, false).unwrap();
        assert_eq!(d["exact"].as_array().unwrap().len(), 1);
        assert_eq!(d["exact"][0]["items"].as_array().unwrap().len(), 2);
        let sim = d["similar"].as_array().unwrap();
        assert_eq!(sim.len(), 1);
        let items = sim[0]["items"].as_array().unwrap();
        assert_eq!(items.len(), 3, "les deux copies et la version recompressée");
        assert_eq!(items[sim[0]["keep"].as_u64().unwrap() as usize]["path"], "/a-grande.jpg", "on garde le plus gros");
        let n: i64 = c.query_row("SELECT count(*) FROM ucloud_entries", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 5, "rien n'est supprimé");
    }

    #[test]
    fn the_map_groups_by_cell_by_period_and_by_zone_for_one_account_only() {
        let c = db();
        let geo = |c: &Connection, owner: &str, path: &str, t: i64, lat: f64, lon: f64| {
            c.execute("INSERT INTO ucloud_entries(owner_hex, path, parent, type, cid, mime, size, mtime, taken_at, lat, lon, data) VALUES (?1, ?2, '/', 'file', ?2, 'image/jpeg', 1, ?3, ?3, ?4, ?5, '{}')", params![owner, path, t, lat, lon]).unwrap();
        };
        geo(&c, ME, "/toulouse1.jpg", T2024, 43.60, 1.44);
        geo(&c, ME, "/toulouse2.jpg", T2024 + 100, 43.61, 1.45);
        geo(&c, ME, "/paris.jpg", T2024 + 200, 48.85, 2.35);
        geo(&c, ME, "/tokyo.jpg", T2024 + 10_000_000, 35.68, 139.69);
        geo(&c, "bb", "/autre.jpg", T2024, 43.6, 1.44);
        let world = MapArea { cell: 1.0, ..MapArea::default() };
        let r = map(&c, ME, &world).unwrap();
        assert_eq!((r["points"].as_array().unwrap().len(), r["geotagged"].as_i64()), (3, Some(4)), "Toulouse regroupe ses deux photos ; l'autre compte n'existe pas");
        assert_eq!(r["points"][0]["n"], 2);
        let europe = MapArea { bbox: Some((35.0, -10.0, 60.0, 30.0)), cell: 1.0, ..MapArea::default() };
        assert_eq!(map(&c, ME, &europe).unwrap()["points"].as_array().unwrap().len(), 2);
        let early = MapArea { to: Some(T2024 + 1_000), cell: 1.0, ..MapArea::default() };
        assert_eq!(map(&c, ME, &early).unwrap()["points"].as_array().unwrap().len(), 2, "Tokyo est plus tard");
        let zone = MapArea { bbox: Some((43.0, 1.0, 44.0, 2.0)), items: true, ..MapArea::default() };
        let items = map(&c, ME, &zone).unwrap();
        assert_eq!(items["items"].as_array().unwrap().len(), 2);
        assert_eq!(items["items"][0]["path"], "/toulouse2.jpg", "du plus récent au plus ancien");
        assert_eq!(map(&c, ME, &world).unwrap()["months"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn exact_duplicates_are_photos_and_videos_unless_every_file_is_asked_for() {
        let c = db();
        put(&c, ME, "/a.jpg", "image/jpeg", 500, None, SHA_A, None, None);
        put(&c, ME, "/b.jpg", "image/jpeg", 500, None, SHA_A, None, None);
        let sha_t = "ee".repeat(32);
        put(&c, ME, "/notes.txt", "text/plain", 50, None, &sha_t, None, None);
        put(&c, ME, "/copie.txt", "text/plain", 50, None, &sha_t, None, None);
        assert_eq!(duplicates(&c, ME, false).unwrap()["exact"].as_array().unwrap().len(), 1, "les documents ne sont pas des doublons de photos");
        assert_eq!(duplicates(&c, ME, true).unwrap()["exact"].as_array().unwrap().len(), 2, "tous les fichiers sur demande");
    }
}
