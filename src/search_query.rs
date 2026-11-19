//! Ce que le membre a voulu dire en tapant sa recherche : « Léa plage mars 2024 » = une personne reconnue, un mot à chercher
//! partout, un mois et une année. Chaque groupe de mots devient un [`Term`] ; un terme est trouvé s'il est trouvé **quelque
//! part** (nom, description, texte lu, visage nommé, lieu, date) : rien n'est retiré de la recherche par erreur, une
//! interprétation ne fait qu'**ajouter** des façons d'être trouvé (« orange » cherche le mot, et les photos prises à Orange).
//!
//! - **personnes** : les noms donnés aux visages dans FaceCloud, sans casse ni accents ; un prénom seul suffit (« lea » →
//!   « Léa Martin ») ;
//! - **lieux** : les villes de plus de 15 000 habitants (GeoNames, CC BY 4.0, embarquées : aucun service de carte n'est
//!   appelé), retenues seulement si le membre a des photos géolocalisées autour ;
//! - **dates** : une année (`2024`) ou un mois en français, anglais ou espagnol (`mars`, `march`, `marzo`).
//!
//! Les mots qui ne sont ni une personne, ni un lieu, ni une date restent des mots « à voir » : la recherche par l'image ne
//! reçoit qu'eux (« Léa plage » cherche une plage, pas Léa, sur les photos).

use std::io::Read as _;
use std::sync::OnceLock;

use astro_nostr_proto::filter::fold;
use rusqlite::{Connection, params};
use serde_json::{Value, json};

/// Mots lus au plus dans une recherche.
pub const MAX_WORDS: usize = 8;
/// Demi-côté de la zone d'un lieu, en degrés de latitude (≈ 15 km).
const PLACE_HALF: f64 = 0.15;
/// Villes homonymes gardées au plus (Paris en France, Paris au Texas…).
const MAX_PLACES: usize = 5;
/// Noms de personnes lus au plus.
const MAX_NAMES: i64 = 5_000;

/// Un groupe de mots de la recherche et ses interprétations.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Term {
    /// Tel que tapé.
    pub text: String,
    /// Ses mots (cherchés dans le nom, la description et le texte lu).
    pub words: Vec<String>,
    /// Personnes reconnues (`faces.name` exact) que ces mots désignent.
    pub people: Vec<String>,
    /// Zones `(sud, ouest, nord, est)` des villes de ce nom où le membre a des photos.
    pub places: Vec<(f64, f64, f64, f64)>,
    /// Année.
    pub year: Option<i32>,
    /// Mois (1 à 12).
    pub month: Option<u32>,
}

impl Term {
    /// Ni personne, ni lieu, ni date : un mot « à voir ».
    pub fn plain(&self) -> bool {
        self.people.is_empty() && self.places.is_empty() && self.year.is_none() && self.month.is_none()
    }

    /// Pour l'interface : ce que la station a compris de ce terme (`null` : un simple mot).
    pub fn view(&self) -> Value {
        let mut as_ = Vec::new();
        if !self.people.is_empty() {
            as_.push("person");
        }
        if !self.places.is_empty() {
            as_.push("place");
        }
        if self.year.is_some() {
            as_.push("year");
        }
        if self.month.is_some() {
            as_.push("month");
        }
        json!({ "text": self.text, "as": as_, "people": self.people, "month": self.month })
    }
}

/// Sans casse ni accents, tirets et apostrophes changés en espaces, espaces simples.
pub fn fold_str(s: &str) -> String {
    let s: String = s.chars().map(|c| if matches!(c, '-' | '\'' | '’' | '.' | '_') { ' ' } else { fold(c) }).collect();
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

const MONTHS: [&[&str]; 12] = [
    &["janvier", "january", "enero", "janv"],
    &["fevrier", "february", "febrero", "fevr"],
    &["mars", "march", "marzo"],
    &["avril", "april", "abril"],
    &["mai", "may", "mayo"],
    &["juin", "june", "junio"],
    &["juillet", "july", "julio", "juil"],
    &["aout", "august", "agosto"],
    &["septembre", "september", "septiembre", "sept"],
    &["octobre", "october", "octubre"],
    &["novembre", "november", "noviembre"],
    &["decembre", "december", "diciembre"],
];

/// Mois désigné par un mot déjà replié (`"aout"` → 8).
pub fn month_of(w: &str) -> Option<u32> {
    MONTHS.iter().position(|names| names.contains(&w)).and_then(|i| u32::try_from(i + 1).ok())
}

/// Année désignée par un mot (`"2024"`), de 1900 à 2100.
pub fn year_of(w: &str) -> Option<i32> {
    (w.len() == 4 && w.bytes().all(|b| b.is_ascii_digit())).then(|| w.parse().ok()).flatten().filter(|y| (1900..=2100).contains(y))
}

/// Lignes `nom<TAB>lat<TAB>lon` des villes, triées par nom (la plus peuplée d'abord à nom égal) : décompressées une fois, à
/// la première recherche.
fn cities() -> &'static [&'static str] {
    static TEXT: OnceLock<String> = OnceLock::new();
    static LINES: OnceLock<Vec<&'static str>> = OnceLock::new();
    LINES.get_or_init(|| {
        let text = TEXT.get_or_init(|| {
            let mut s = String::new();
            // Données embarquées et vérifiées par un test : une erreur laisse simplement la liste vide.
            let _ = flate2::read::GzDecoder::new(&include_bytes!("data/cities.tsv.gz")[..]).read_to_string(&mut s);
            s
        });
        let mut lines: Vec<&'static str> = text.lines().collect();
        lines.sort_by_key(|l| l.split('\t').next().unwrap_or("")); // tri stable : la plus peuplée reste devant
        lines
    })
}

/// Centres des villes nommées `name` (déjà replié), la plus peuplée d'abord.
pub fn city(name: &str) -> Vec<(f64, f64)> {
    let all = cities();
    let start = all.partition_point(|l| l.split('\t').next().unwrap_or("") < name);
    all[start..]
        .iter()
        .take_while(|l| l.split('\t').next() == Some(name))
        .filter_map(|l| {
            let mut f = l.split('\t').skip(1);
            Some((f.next()?.parse().ok()?, f.next()?.parse().ok()?))
        })
        .take(MAX_PLACES)
        .collect()
}

/// Zone `(sud, ouest, nord, est)` autour d'un point (≈ 15 km de chaque côté).
pub fn zone((lat, lon): (f64, f64)) -> (f64, f64, f64, f64) {
    let dlon = PLACE_HALF / lat.to_radians().cos().max(0.2);
    ((lat - PLACE_HALF).max(-90.0), (lon - dlon).max(-180.0), (lat + PLACE_HALF).min(90.0), (lon + dlon).min(180.0))
}

/// Noms donnés aux visages du membre : `(replié, exact)`.
fn people(c: &Connection, owner: &str) -> rusqlite::Result<Vec<(String, String)>> {
    let mut stmt = c.prepare_cached("SELECT DISTINCT name FROM faces WHERE owner_hex = ?1 AND name IS NOT NULL AND name <> '' AND name NOT LIKE 'Inconnu\\_%' ESCAPE '\\' LIMIT ?2")?;
    let names = stmt.query_map(params![owner, MAX_NAMES], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
    Ok(names.into_iter().map(|n| (fold_str(&n), n)).collect())
}

/// Le membre a-t-il des fichiers géolocalisés dans cette zone ?
fn has_photos_in(c: &Connection, owner: &str, (s, w, n, e): (f64, f64, f64, f64)) -> rusqlite::Result<bool> {
    c.prepare_cached("SELECT EXISTS (SELECT 1 FROM ucloud_entries WHERE owner_hex = ?1 AND type = 'file' AND lat BETWEEN ?2 AND ?3 AND lon BETWEEN ?4 AND ?5)")?.query_row(params![owner, s, n, w, e], |r| r.get(0))
}

/// Découpe et comprend la recherche `q` de `owner`. Les groupes de deux ou trois mots sont essayés d'abord (« Saint Étienne »,
/// « Léa Martin »), puis chaque mot seul.
pub fn understand(c: &Connection, owner: &str, q: &str) -> rusqlite::Result<Vec<Term>> {
    let raw: Vec<String> = q.split_whitespace().take(MAX_WORDS).map(|w| w.chars().take(64).collect()).collect();
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    let names = people(c, owner)?;
    let who = |key: &str, alone: bool| -> Vec<String> { names.iter().filter(|(f, _)| f == key || (alone && key.len() >= 3 && f.split(' ').any(|w| w == key))).map(|(_, n)| n.clone()).collect() };
    let places = |key: &str| -> rusqlite::Result<Vec<(f64, f64, f64, f64)>> {
        if key.len() < 3 || key.bytes().all(|b| b.is_ascii_digit()) {
            return Ok(Vec::new());
        }
        city(key).into_iter().map(zone).filter_map(|z| has_photos_in(c, owner, z).map(|ok| ok.then_some(z)).transpose()).collect()
    };
    let mut out = Vec::new();
    let mut i = 0;
    while i < raw.len() {
        let mut found = None;
        for n in (2..=3.min(raw.len() - i)).rev() {
            let key = fold_str(&raw[i..i + n].join(" "));
            let (people, places) = (who(&key, false), places(&key)?);
            if !people.is_empty() || !places.is_empty() {
                found = Some((n, Term { text: raw[i..i + n].join(" "), words: raw[i..i + n].to_vec(), people, places, year: None, month: None }));
                break;
            }
        }
        let (n, term) = match found {
            Some(f) => f,
            None => {
                let key = fold_str(&raw[i]);
                (1, Term { text: raw[i].clone(), words: vec![raw[i].clone()], people: who(&key, true), places: places(&key)?, year: year_of(&key), month: month_of(&key) })
            }
        };
        out.push(term);
        i += n;
    }
    Ok(out)
}

/// Les mots « à voir » de la recherche, pour la recherche par l'image (vide : rien à regarder).
pub fn to_see(terms: &[Term]) -> String {
    terms.iter().filter(|t| t.plain()).map(|t| t.text.as_str()).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn months_years_and_folding() {
        assert_eq!((month_of("mars"), month_of("aout"), month_of("diciembre"), month_of("plage")), (Some(3), Some(8), Some(12), None));
        assert_eq!((year_of("2024"), year_of("1850"), year_of("20245"), year_of("abcd")), (Some(2024), None, None, None));
        assert_eq!(fold_str("  Saint-Étienne  l’Île "), "saint etienne l ile");
    }

    #[test]
    fn embedded_cities_are_found_by_their_folded_name_biggest_first() {
        let lyon = city("lyon");
        assert!(!lyon.is_empty() && (lyon[0].0 - 45.75).abs() < 0.1 && (lyon[0].1 - 4.85).abs() < 0.1, "{lyon:?}");
        assert!(!city("saint etienne").is_empty() && !city("londres").is_empty(), "noms composés et autres noms des grandes villes");
        assert!(city("pas une ville du tout").is_empty());
        let (s, w, n, e) = zone((45.75, 4.85));
        assert!(s < 45.75 && n > 45.75 && w < 4.85 && e > 4.85 && (n - s - 0.3).abs() < 1e-9);
    }

    #[test]
    fn people_places_and_dates_are_understood_only_when_the_member_has_them() {
        let c = astro_db::open_in_memory().unwrap();
        c.execute_batch(
            "INSERT INTO faces(owner_hex, name, embed_hash, ts, ucloud_path) VALUES ('me', 'Léa Martin', 'h1', 0, '/a.jpg'), ('me', 'Inconnu_12345678', 'h2', 0, '/a.jpg'), ('bob', 'Paul', 'h3', 0, '/b.jpg'); \
             INSERT INTO ucloud_entries(owner_hex, path, parent, type, cid, mtime, lat, lon, data) VALUES ('me', '/lyon.jpg', '/', 'file', 'c', 0, 45.76, 4.84, '{}')",
        )
        .unwrap();
        let t = understand(&c, "me", "lea plage Lyon mars 2024 Paul Nice").unwrap();
        assert_eq!(t.iter().map(|t| t.text.as_str()).collect::<Vec<_>>(), ["lea", "plage", "Lyon", "mars", "2024", "Paul", "Nice"]);
        assert_eq!(t[0].people, ["Léa Martin"], "un prénom seul, sans accent");
        assert!(t[1].plain());
        assert_eq!(t[2].places.len(), 1, "le membre a une photo à Lyon");
        assert_eq!((t[3].month, t[4].year), (Some(3), Some(2024)));
        assert!(t[5].plain(), "Paul est un visage de Bob, pas du membre");
        assert!(t[6].plain(), "aucune photo du membre à Nice");
        assert_eq!(to_see(&t), "plage Paul Nice");
        let full = understand(&c, "me", "Léa Martin").unwrap();
        assert_eq!((full.len(), full[0].people.len()), (1, 1), "nom complet sur deux mots");
        assert!(understand(&c, "me", "   ").unwrap().is_empty());
    }
}
