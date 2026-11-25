//! **Fiche plante** d'une photo, comme la fiche d'un film de la Médiathèque : un document que le membre voit, corrige et
//! complète, rangé chiffré dans **son uCloud** (`/_/Plantes/<empreinte de la photo>.json`, le tiroir système). C'est la vérité :
//! elle suit le membre dans sa sauvegarde et revient avec elle. La base n'en garde qu'un cache rejouable (`plant_fiches`) et le
//! texte cherchable (`search_text`, `src = 'plant'`), relus quand le fichier change — comme `media_index` pour les fiches de films.
//!
//! La fiche se remplit de trois façons, toutes **modifiables** par le membre :
//! - l'espèce : choisie parmi les propositions de Pl@ntNet ([`crate::plantnet`], si la coopérative a une clé), ou tapée à la main
//!   (reconnaissance ratée, pas de clé) ;
//! - les rubriques (description, comestible ?, préparation, propriétés médicinales, dangers) : **préremplies** depuis Wikipédia
//!   dans la langue de l'interface — extraits des sections qui en parlent, rien d'inventé, source citée (CC BY-SA) — et
//!   seulement là où le membre n'a rien écrit ;
//! - ses notes personnelles.
//!
//! La liste embarquée des plantes très toxiques ([`crate::plantnet::danger_of`]) s'ajoute aux dangers. Une reconnaissance peut se
//! tromper : l'écran le dit toujours.

use std::sync::Arc;

use astro_dav::{BlobStore, DavState};
use astro_db::Db;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::fetch::{FetchLimits, fetch};
use crate::plantnet::{self, Match};

/// Dossier des fiches, dans le tiroir système du membre.
pub const DIR: &str = "/_/Plantes";
/// Taille d'une fiche au plus (lecture comme écriture).
pub const MAX_FICHE: usize = 64 << 10;
/// Caractères gardés au plus par rubrique.
pub const MAX_FIELD: usize = 4000;
/// Caractères préremplis au plus par rubrique depuis Wikipédia.
const WIKI_FIELD: usize = 1500;
/// Fiches relues au plus par synchronisation (le reste à la suivante).
const MAX_SYNC: usize = 200;

/// Espèce retenue par le membre.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Species {
    /// Nom latin (ou, saisi à la main, le nom que le membre connaît).
    pub scientific: String,
    /// Noms communs.
    #[serde(default)]
    pub common: Vec<String>,
    /// Famille.
    #[serde(default)]
    pub family: String,
    /// Identifiant GBIF.
    #[serde(default)]
    pub gbif: Option<String>,
}

/// Source citée d'un préremplissage.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Source {
    /// Titre de l'article.
    pub title: String,
    /// Adresse de l'article.
    pub url: String,
    /// Licence du texte repris.
    pub license: String,
}

/// La fiche, telle qu'écrite dans l'uCloud.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Fiche {
    /// Version du format.
    #[serde(default = "one")]
    pub v: u32,
    /// Empreinte de la photo décrite.
    pub sha: String,
    /// Chemin de la photo.
    pub photo: String,
    /// Espèce retenue.
    #[serde(default)]
    pub species: Option<Species>,
    /// Dernières propositions de Pl@ntNet.
    #[serde(default)]
    pub proposals: Vec<Match>,
    /// Résumé.
    #[serde(default)]
    pub description: String,
    /// Comestible ?
    #[serde(default)]
    pub edible: String,
    /// Préparation.
    #[serde(default)]
    pub preparation: String,
    /// Propriétés médicinales.
    #[serde(default)]
    pub medicinal: String,
    /// Dangers.
    #[serde(default)]
    pub dangers: String,
    /// Notes personnelles.
    #[serde(default)]
    pub notes: String,
    /// Source du préremplissage.
    #[serde(default)]
    pub source: Option<Source>,
    /// Dernière modification (secondes).
    #[serde(default)]
    pub updated_at: i64,
}

fn one() -> u32 {
    1
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

fn clip(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let cut: String = s.chars().take(max.saturating_sub(1)).collect();
    // Fin propre : la dernière phrase entière, sinon le dernier mot.
    match cut.rfind(". ").filter(|i| *i > max / 2) {
        Some(i) => cut[..=i].to_owned(),
        None => format!("{}…", cut.rsplit_once(' ').map_or(cut.as_str(), |(a, _)| a)),
    }
}

impl Fiche {
    /// Texte cherchable : « plante », l'espèce, les noms, et les rubriques remplies avec leur nom (« comestible »,
    /// « médicinale », « toxique »…), pour que le membre retrouve la photo par ce qu'il sait de la plante.
    pub fn search_body(&self) -> String {
        // Mots repères en français, anglais et espagnol : le membre cherche dans la langue de son interface.
        let mut parts = vec!["plante plant planta".to_owned()];
        if let Some(s) = &self.species {
            parts.push(format!("{} {} {}", s.scientific, s.common.join(" "), s.family));
        }
        for (label, text) in [("", &self.description), ("comestible edible", &self.edible), ("préparation preparation preparación", &self.preparation), ("médicinale medicinal", &self.medicinal), ("dangers toxique toxic tóxica", &self.dangers), ("", &self.notes)] {
            if !text.trim().is_empty() {
                parts.push(format!("{label} {}", text.trim()));
            }
        }
        parts.join("\n")
    }

    /// Avertissement de la liste embarquée pour l'espèce retenue, ajouté aux dangers s'il n'y est pas déjà.
    fn add_known_danger(&mut self) {
        if let Some(why) = self.species.as_ref().and_then(|s| plantnet::danger_of(&s.scientific))
            && !self.dangers.contains(why)
        {
            self.dangers = if self.dangers.trim().is_empty() { why.to_owned() } else { format!("{why}\n\n{}", self.dangers.trim()) };
        }
    }
}

/// Nom du fichier de la fiche d'une photo d'empreinte `sha`.
pub fn file_of(sha: &str) -> String {
    format!("{DIR}/{sha}.json")
}

// ── Wikipédia ────────────────────────────────────────────────────────────────────────────────────────────────────────────

/// Ce que Wikipédia dit d'une plante, rubrique par rubrique (vide = rien trouvé).
#[derive(Debug, Default, PartialEq)]
pub struct WikiFacts {
    /// Résumé (introduction de l'article).
    pub description: String,
    /// Phrases qui disent si elle se mange.
    pub edible: String,
    /// Sections culinaires.
    pub preparation: String,
    /// Sections médicinales.
    pub medicinal: String,
    /// Sections sur la toxicité et les confusions.
    pub dangers: String,
    /// Article lu.
    pub source: Option<Source>,
}

/// Minuscules sans accents, pour comparer les titres de section.
fn fold(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| match c {
            'à' | 'â' | 'ä' | 'á' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'î' | 'ï' | 'í' => 'i',
            'ô' | 'ö' | 'ó' => 'o',
            'û' | 'ü' | 'ù' | 'ú' => 'u',
            'ç' => 'c',
            'ñ' => 'n',
            c => c,
        })
        .collect()
}

const COOK: &[&str] = &["alimentation humaine", "cuisine", "culinar", "gastronom", "comestib", "food", "edib", "alimentaci", "cocina", "usage alimentaire", "consommation"];
const HEAL: &[&str] = &["medicin", "therapeut", "phytotherap", "pharmaco", "herbal", "proprietes"];
const HARM: &[&str] = &["toxic", "poison", "danger", "confusion", "contre-indication", "contraindica", "peligro", "similarity", "risque"];
const EAT: &[&str] = &["comestible", "edible", "se mange", "se consomme", "are eaten", "is eaten"];
/// Sections générales d'usages : seulement quand aucune section culinaire n'existe.
const USES: &[&str] = &["uses", "usos", "utilisation", "usages"];
/// Phrases qui disent un danger, quand aucune section n'en parle.
const TOXIC: &[&str] = &["toxique", "toxic", "toxica", "mortel", "poison", "venenos"];

/// Découpe le texte brut d'un article (`== Titre ==`) en `(niveau, titre plié, texte)`.
fn sections(extract: &str) -> (String, Vec<(usize, String, String)>) {
    let mut intro = String::new();
    let mut out: Vec<(usize, String, String)> = Vec::new();
    for line in extract.lines() {
        let t = line.trim();
        let level = t.chars().take_while(|c| *c == '=').count();
        if level >= 2 && t.ends_with('=') && t.len() > 2 * level {
            out.push((level, t.trim_matches('=').trim().to_owned(), String::new()));
        } else if let Some(last) = out.last_mut() {
            if !t.is_empty() {
                last.2.push_str(t);
                last.2.push('\n');
            }
        } else if !t.is_empty() {
            intro.push_str(t);
            intro.push('\n');
        }
    }
    (intro, out)
}

/// Texte des sections dont le titre contient un des mots, sous-sections comprises (titres gardés comme repères).
fn pick(secs: &[(usize, String, String)], words: &[&str]) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < secs.len() {
        let (level, title, _) = &secs[i];
        if words.iter().any(|w| fold(title).contains(w)) {
            let mut j = i;
            while j < secs.len() && (j == i || secs[j].0 > *level) {
                let (_, t, body) = &secs[j];
                if !body.trim().is_empty() {
                    out.push_str(&format!("{t} — {}\n\n", body.trim()));
                }
                j += 1;
            }
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

/// Les phrases de l'article qui contiennent un des mots (trois au plus).
fn sentences(text: &str, words: &[&str]) -> String {
    text.split_inclusive(['.', '\n'])
        .map(str::trim)
        .filter(|s| s.len() > 12 && words.iter().any(|w| fold(s).contains(w)))
        .take(3)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Lit la réponse de `action=query&prop=extracts|info` (format 2) et la range en rubriques.
pub fn wiki_facts(body: &[u8]) -> Result<WikiFacts, String> {
    let v: Value = serde_json::from_slice(body).map_err(|e| format!("réponse de Wikipédia illisible : {e}"))?;
    let page = v.pointer("/query/pages/0").ok_or("réponse de Wikipédia sans page")?;
    if page.get("missing").is_some() {
        return Ok(WikiFacts::default());
    }
    let extract = page.get("extract").and_then(Value::as_str).unwrap_or_default();
    let (intro, secs) = sections(extract);
    let all = format!("{intro}\n{}", secs.iter().map(|s| s.2.as_str()).collect::<Vec<_>>().join("\n"));
    let url = page.get("fullurl").and_then(Value::as_str).unwrap_or_default();
    Ok(WikiFacts {
        description: clip(&intro, 800),
        edible: clip(&sentences(&all, EAT), WIKI_FIELD),
        preparation: clip(&Some(pick(&secs, COOK)).filter(|t| !t.is_empty()).unwrap_or_else(|| pick(&secs, USES)), WIKI_FIELD),
        medicinal: clip(&pick(&secs, HEAL), WIKI_FIELD),
        dangers: clip(&Some(pick(&secs, HARM)).filter(|t| !t.is_empty()).unwrap_or_else(|| sentences(&all, TOXIC)), WIKI_FIELD),
        source: url.starts_with("https://").then(|| Source { title: page.get("title").and_then(Value::as_str).unwrap_or_default().to_owned(), url: url.to_owned(), license: "CC BY-SA 4.0".into() }),
    })
}

/// Adresse de l'API de Wikipédia dans la langue `lang` (deux lettres, sinon français) : hôte fixe `xx.wikipedia.org`.
pub fn wiki_url(lang: &str, title: &str) -> String {
    let lang = if lang.len() == 2 && lang.bytes().all(|c| c.is_ascii_lowercase()) { lang } else { "fr" };
    let Ok(mut u) = url::Url::parse(&format!("https://{lang}.wikipedia.org/w/api.php")) else { return String::new() };
    u.query_pairs_mut()
        .append_pair("action", "query")
        .append_pair("prop", "extracts|info")
        .append_pair("inprop", "url")
        .append_pair("explaintext", "1")
        .append_pair("redirects", "1")
        .append_pair("format", "json")
        .append_pair("formatversion", "2")
        .append_pair("titles", title);
    u.to_string()
}

/// Lit l'article de `title` à `url` (réponse bornée, adresses publiques seulement).
pub async fn wiki_read(url: &str, limits: &FetchLimits) -> Result<WikiFacts, String> {
    let got = fetch(url, limits).await.map_err(|e| format!("Wikipédia injoignable : {e}"))?;
    let mut raw = Vec::new();
    tokio::io::AsyncReadExt::read_to_end(&mut { got.body }, &mut raw).await.map_err(|e| format!("Wikipédia : {e}"))?;
    if raw.len() as u64 > limits.max_bytes {
        return Err("article de Wikipédia trop long".into());
    }
    wiki_facts(&raw)
}

fn wiki_limits() -> FetchLimits {
    let mut l = FetchLimits::new(2 << 20);
    l.timeout = std::time::Duration::from_secs(15);
    l
}

/// Remplit les rubriques **vides** de la fiche avec `facts` ; rend `true` si quelque chose a changé.
pub fn prefill(f: &mut Fiche, facts: WikiFacts) -> bool {
    let mut changed = false;
    for (field, text) in [(&mut f.description, facts.description), (&mut f.edible, facts.edible), (&mut f.preparation, facts.preparation), (&mut f.medicinal, facts.medicinal), (&mut f.dangers, facts.dangers)] {
        if field.trim().is_empty() && !text.is_empty() {
            *field = text;
            changed = true;
        }
    }
    if changed && f.source.is_none() {
        f.source = facts.source;
    }
    changed
}

// ── Fichier et cache ─────────────────────────────────────────────────────────────────────────────────────────────────────

/// Relit les fiches du membre dont le fichier a changé (cache `plant_fiches` + texte cherchable) ; oublie celles qui n'existent
/// plus. Rend le nombre de fiches relues.
pub async fn sync<B: BlobStore>(dav: &Arc<DavState<B>>, owner: &str) -> Result<usize, String> {
    let o = owner.to_owned();
    let files: Vec<(String, String, i64, Option<String>)> = dav
        .db
        .readers()
        .read(move |c| {
            let mut st = c.prepare_cached(
                "SELECT e.path, e.cid, COALESCE(e.size, 0), p.seen FROM ucloud_entries e LEFT JOIN plant_fiches p ON p.owner_hex = e.owner_hex AND p.path = e.path \
                 WHERE e.owner_hex = ?1 AND e.parent = ?2 AND e.type = 'file' AND e.cid IS NOT NULL AND e.path GLOB '*.json'",
            )?;
            let rows = st.query_map(rusqlite::params![o, DIR], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .await
        .map_err(|e| e.to_string())?;
    let present: Vec<String> = files.iter().map(|f| f.0.clone()).collect();
    let mut fresh: Vec<(String, String, Fiche)> = Vec::new();
    for (path, cid, size, _) in files.into_iter().filter(|f| f.3.as_deref() != Some(f.1.as_str())).take(MAX_SYNC) {
        if usize::try_from(size).unwrap_or(usize::MAX) > MAX_FICHE {
            continue;
        }
        if let Some(f) = astro_dav::carddav::read_file(dav, owner, &path).await.and_then(|raw| serde_json::from_slice::<Fiche>(&raw).ok()) {
            fresh.push((path, cid, f));
        }
    }
    let n = fresh.len();
    let o = owner.to_owned();
    dav.db
        .writer()
        .transaction(move |tx| {
            let known: Vec<String> = tx.prepare("SELECT path FROM plant_fiches WHERE owner_hex = ?1")?.query_map([&o], |r| r.get(0))?.collect::<Result<_, _>>()?;
            for gone in known.iter().filter(|p| !present.contains(p)) {
                tx.execute("DELETE FROM plant_fiches WHERE owner_hex = ?1 AND path = ?2", rusqlite::params![o, gone])?;
                tx.execute("DELETE FROM search_text WHERE owner_hex = ?1 AND src = 'plant' AND ref = ?2", rusqlite::params![o, gone])?;
            }
            for (path, cid, f) in &fresh {
                index_tx(tx, &o, path, cid, f)?;
            }
            Ok(())
        })
        .await
        .map_err(|e| e.to_string())?;
    Ok(n)
}

fn index_tx(tx: &rusqlite::Transaction<'_>, owner: &str, path: &str, cid: &str, f: &Fiche) -> rusqlite::Result<()> {
    let json = serde_json::to_string(f).unwrap_or_default();
    tx.execute(
        "INSERT INTO plant_fiches(owner_hex, path, seen, sha, photo, fiche) VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
         ON CONFLICT(owner_hex, path) DO UPDATE SET seen = excluded.seen, sha = excluded.sha, photo = excluded.photo, fiche = excluded.fiche",
        rusqlite::params![owner, path, cid, f.sha, f.photo, json],
    )?;
    tx.execute("DELETE FROM search_text WHERE owner_hex = ?1 AND src = 'plant' AND ref = ?2", rusqlite::params![owner, path])?;
    tx.execute("INSERT INTO search_text(body, owner_hex, sha, src, t_ms, ref) VALUES (?1, ?2, ?3, 'plant', 0, ?4)", rusqlite::params![f.search_body(), owner, f.sha, path])?;
    Ok(())
}

/// Écrit la fiche dans l'uCloud (la vérité) puis met le cache et la recherche à jour.
pub async fn save<B: BlobStore>(dav: &Arc<DavState<B>>, owner: &str, f: &Fiche) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(f).map_err(|e| e.to_string())?;
    if bytes.len() > MAX_FICHE {
        return Err("fiche trop longue".into());
    }
    let name = format!("{}.json", f.sha);
    let done = astro_dav::ingest::replace(dav, owner, DIR, &name, Some("application/json"), &bytes[..]).await.map_err(|e| format!("écriture de la fiche : {e}"))?;
    let (o, path, f) = (owner.to_owned(), done.path, f.clone());
    dav.db
        .writer()
        .transaction(move |tx| {
            // CID du fichier tout juste écrit : la prochaine synchronisation saura qu'il est déjà lu.
            let cid: String = tx.query_row("SELECT COALESCE(cid, '') FROM ucloud_entries WHERE owner_hex = ?1 AND path = ?2", rusqlite::params![o, path], |r| r.get(0))?;
            Ok(index_tx(tx, &o, &path, &cid, &f)?)
        })
        .await
        .map_err(|e| e.to_string())
}

/// Fiche de la photo d'empreinte `sha` et de chemin `photo` (retrouvée par son chemin si la photo a été tournée).
async fn find(db: &Db, owner: &str, sha: &str, photo: &str) -> Result<Option<Fiche>, String> {
    let (o, s, p) = (owner.to_owned(), sha.to_owned(), photo.to_owned());
    let raw: Option<String> = db
        .readers()
        .read(move |c| Ok(rusqlite::OptionalExtension::optional(c.query_row("SELECT fiche FROM plant_fiches WHERE owner_hex = ?1 AND (sha = ?2 OR photo = ?3) ORDER BY sha = ?2 DESC LIMIT 1", [&o, &s, &p], |r| r.get(0)))?))
        .await
        .map_err(|e| e.to_string())?;
    Ok(raw.and_then(|r| serde_json::from_str(&r).ok()))
}

/// Fiche de la photo `path` (une vide si elle n'en a pas encore) et si la reconnaissance automatique est proposée.
pub async fn view<B: BlobStore>(dav: &Arc<DavState<B>>, uplanet_name: &str, owner: &str, path: &str) -> Result<Value, String> {
    let (sha, _, _) = plantnet::photo(&dav.db, owner, path).await?;
    sync(dav, owner).await?;
    let fiche = find(&dav.db, owner, &sha, path).await?;
    let exists = fiche.is_some();
    let fiche = fiche.unwrap_or_else(|| Fiche { v: 1, sha: sha.clone(), photo: path.to_owned(), ..Fiche::default() });
    Ok(json!({ "available": plantnet::api_key(&dav.db, uplanet_name).await.is_some(), "exists": exists, "fiche": fiche }))
}

/// La rubrique « Plantes » est-elle proposée au membre ? Oui si la coopérative a une clé Pl@ntNet, ou s'il a déjà des fiches.
pub async fn offered(db: &Db, uplanet_name: &str, owner: &str) -> bool {
    if plantnet::api_key(db, uplanet_name).await.is_some() {
        return true;
    }
    let o = owner.to_owned();
    db.readers()
        .read(move |c| Ok(c.query_row("SELECT EXISTS (SELECT 1 FROM ucloud_entries WHERE owner_hex = ?1 AND parent = ?2 AND type = 'file')", rusqlite::params![o, DIR], |r| r.get::<_, bool>(0))?))
        .await
        .unwrap_or(false)
}

/// Toutes les fiches du membre dont la photo existe encore, la plus récente d'abord : de quoi faire la liste « Mes plantes »
/// (nom, rubriques remplies, plante de la liste des très toxiques).
pub async fn list<B: BlobStore>(dav: &Arc<DavState<B>>, uplanet_name: &str, owner: &str) -> Result<Value, String> {
    sync(dav, owner).await?;
    let o = owner.to_owned();
    let rows: Vec<(String, String)> = dav
        .db
        .readers()
        .read(move |c| {
            // La photo est retrouvée par son empreinte, sinon par son chemin (photo tournée depuis).
            let mut st = c.prepare(
                "SELECT COALESCE((SELECT e.path FROM ucloud_entries e WHERE e.owner_hex = p.owner_hex AND e.sha_plain = p.sha AND e.type = 'file' LIMIT 1),                                  (SELECT e.path FROM ucloud_entries e WHERE e.owner_hex = p.owner_hex AND e.path = p.photo AND e.type = 'file')), p.fiche                  FROM plant_fiches p WHERE p.owner_hex = ?1",
            )?;
            let rows = st.query_map([&o], |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, String>(1)?)))?.filter_map(|r| r.ok()).filter_map(|(p, f)| p.map(|p| (p, f))).collect();
            Ok(rows)
        })
        .await
        .map_err(|e| e.to_string())?;
    let mut items: Vec<(i64, Value)> = rows
        .into_iter()
        .filter_map(|(photo, raw)| serde_json::from_str::<Fiche>(&raw).ok().map(|f| (photo, f)))
        .map(|(photo, f)| {
            let sp = f.species.clone().unwrap_or_default();
            let filled = |s: &str| !s.trim().is_empty();
            (f.updated_at, json!({
                "photo": photo,
                "name": sp.common.first().cloned().unwrap_or_else(|| sp.scientific.clone()),
                "scientific": sp.scientific,
                "edible": filled(&f.edible) || filled(&f.preparation),
                "medicinal": filled(&f.medicinal),
                "toxic": plantnet::danger_of(&sp.scientific).is_some(),
                "updated_at": f.updated_at,
            }))
        })
        .collect();
    items.sort_by(|a, b| b.0.cmp(&a.0));
    Ok(json!({ "available": plantnet::api_key(&dav.db, uplanet_name).await.is_some(), "items": items.into_iter().map(|i| i.1).collect::<Vec<_>>() }))
}

/// Ce que le membre envoie depuis l'écran de la fiche : l'espèce retenue et les rubriques ; `wiki` demande de préremplir les
/// rubriques vides depuis Wikipédia.
#[derive(Debug, Deserialize)]
pub struct Edit {
    /// Espèce retenue (`None` : aucune).
    pub species: Option<Species>,
    /// Résumé.
    #[serde(default)]
    pub description: String,
    /// Comestible ?
    #[serde(default)]
    pub edible: String,
    /// Préparation.
    #[serde(default)]
    pub preparation: String,
    /// Propriétés médicinales.
    #[serde(default)]
    pub medicinal: String,
    /// Dangers.
    #[serde(default)]
    pub dangers: String,
    /// Notes personnelles.
    #[serde(default)]
    pub notes: String,
    /// Préremplir depuis Wikipédia ?
    #[serde(default)]
    pub wiki: bool,
}

/// Applique l'édition du membre (bornée), préremplit si demandé, et enregistre.
pub async fn edit<B: BlobStore>(dav: &Arc<DavState<B>>, owner: &str, path: &str, e: Edit, lang: &str) -> Result<Value, String> {
    let (sha, _, _) = plantnet::photo(&dav.db, owner, path).await?;
    sync(dav, owner).await?;
    let mut f = find(&dav.db, owner, &sha, path).await?.unwrap_or_default();
    let species = e.species.map(|s| Species {
        scientific: clip(&s.scientific, 120),
        common: s.common.iter().map(|c| clip(c, 80)).filter(|c| !c.is_empty()).take(6).collect(),
        family: clip(&s.family, 80),
        gbif: s.gbif.filter(|g| g.len() <= 20 && g.bytes().all(|c| c.is_ascii_digit())),
    });
    let renamed = species.as_ref().map(|s| &s.scientific) != f.species.as_ref().map(|s| &s.scientific);
    f.species = species.filter(|s| !s.scientific.is_empty());
    (f.description, f.edible, f.preparation, f.medicinal, f.dangers, f.notes) = (clip(&e.description, MAX_FIELD), clip(&e.edible, MAX_FIELD), clip(&e.preparation, MAX_FIELD), clip(&e.medicinal, MAX_FIELD), clip(&e.dangers, MAX_FIELD), clip(&e.notes, MAX_FIELD));
    if renamed {
        f.source = None; // une autre plante : l'ancienne source ne vaut plus
    }
    let mut warn = None;
    if e.wiki
        && let Some(name) = f.species.as_ref().map(|s| s.scientific.clone())
    {
        match wiki_read(&wiki_url(lang, &name), &wiki_limits()).await {
            Ok(facts) if facts.source.is_some() => {
                prefill(&mut f, facts);
            }
            Ok(_) => warn = Some(format!("Wikipédia n'a pas d'article « {name} »")),
            Err(err) => warn = Some(err),
        }
    }
    f.add_known_danger();
    (f.v, f.sha, f.photo, f.updated_at) = (1, sha, path.to_owned(), now());
    save(dav, owner, &f).await?;
    Ok(json!({ "fiche": f, "warning": warn }))
}

/// Reconnaissance par Pl@ntNet, à la demande du membre : les propositions entrent dans la fiche ; la plus probable devient
/// l'espèce si aucune n'était retenue, et les rubriques vides sont préremplies depuis Wikipédia.
pub async fn recognize<B: BlobStore>(dav: &Arc<DavState<B>>, uplanet_name: &str, owner: &str, path: &str, lang: &str) -> Result<Value, String> {
    let key = plantnet::api_key(&dav.db, uplanet_name).await.ok_or("la coopérative n'a pas activé la reconnaissance des plantes")?;
    let (sha, _, size) = plantnet::photo(&dav.db, owner, path).await?;
    if u64::try_from(size).unwrap_or(u64::MAX) > plantnet::MAX_PHOTO {
        return Err("photo trop lourde".into());
    }
    let bytes = astro_dav::carddav::read_file(dav, owner, path).await.ok_or("photo illisible")?;
    let jpeg = tokio::task::spawn_blocking(move || plantnet::prepare(&bytes)).await.map_err(|e| e.to_string())??;
    let matches = plantnet::identify(plantnet::API, &key, lang, &jpeg, &plantnet::limits()).await?;
    sync(dav, owner).await?;
    let mut f = find(&dav.db, owner, &sha, path).await?.unwrap_or_default();
    let mut warn = None;
    if f.species.is_none()
        && let Some(best) = matches.first()
    {
        f.species = Some(Species { scientific: best.scientific.clone(), common: best.common.clone(), family: best.family.clone(), gbif: best.gbif.clone() });
        match wiki_read(&wiki_url(lang, &best.scientific), &wiki_limits()).await {
            Ok(facts) => {
                prefill(&mut f, facts);
            }
            Err(err) => warn = Some(err),
        }
    }
    f.proposals = matches;
    f.add_known_danger();
    (f.v, f.sha, f.photo, f.updated_at) = (1, sha, path.to_owned(), now());
    save(dav, owner, &f).await?;
    Ok(json!({ "fiche": f, "warning": warn }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ARTICLE: &str = r#"{"batchcomplete":true,"query":{"redirects":[{"from":"Allium ursinum","to":"Ail des ours"}],"pages":[{"pageid":1,"ns":0,"title":"Ail des ours","fullurl":"https://fr.wikipedia.org/wiki/Ail_des_ours",
      "extract":"L'ail des ours est une plante herbacée vivace. Ses feuilles sont comestibles et très appréciées au printemps.\n\n\n== Description ==\nFeuilles larges.\n\n\n== Utilisations ==\n\n\n=== Alimentation humaine ===\nLes feuilles se consomment crues en salade ou en pesto.\n\n\n=== Thérapeutique ===\nUtilisé contre l'hypertension.\n\n\n=== Confusions possibles avec d'autres plantes ===\nLe muguet et le colchique, très toxiques, lui ressemblent.\n\n\n== Notes et références ==\nRéf."}]}}"#;

    #[test]
    fn wikipedia_sections_fill_the_right_rubrics_and_nothing_is_made_up() {
        let w = wiki_facts(ARTICLE.as_bytes()).unwrap();
        assert!(w.description.starts_with("L'ail des ours est une plante"));
        assert!(w.edible.contains("comestibles"), "{}", w.edible);
        assert!(w.preparation.contains("pesto") && !w.preparation.contains("hypertension"), "{}", w.preparation);
        assert!(w.medicinal.contains("hypertension"));
        assert!(w.dangers.contains("colchique"));
        assert_eq!(w.source.as_ref().unwrap().url, "https://fr.wikipedia.org/wiki/Ail_des_ours");
        let missing = wiki_facts(br#"{"query":{"pages":[{"ns":0,"title":"Zz","missing":true}]}}"#).unwrap();
        assert_eq!(missing, WikiFacts::default(), "pas d'article : rien n'est rempli");
        assert!(wiki_facts(b"<html>").is_err());
    }

    #[test]
    fn prefill_never_overwrites_what_the_member_wrote() {
        let mut f = Fiche { edible: "Oui, cueillie au bois".into(), ..Fiche::default() };
        assert!(prefill(&mut f, wiki_facts(ARTICLE.as_bytes()).unwrap()));
        assert_eq!(f.edible, "Oui, cueillie au bois");
        assert!(f.preparation.contains("pesto") && f.source.is_some());
    }

    #[test]
    fn the_api_address_keeps_a_fixed_host_whatever_the_language_or_name() {
        assert!(wiki_url("en", "Allium ursinum").starts_with("https://en.wikipedia.org/w/api.php?"));
        for bad in ["evil.com/", "e.", "FR", "../", "fra"] {
            assert!(wiki_url(bad, "x").starts_with("https://fr.wikipedia.org/w/api.php?"), "{bad}");
        }
        let u = wiki_url("fr", "a&action=edit#x");
        assert!(u.contains("titles=a%26action%3Dedit%23x") && u.matches("action=").count() == 1, "{u}");
    }

    #[test]
    fn the_search_finds_names_and_what_the_plant_is_good_for() {
        let mut f = Fiche { species: Some(Species { scientific: "Colchicum autumnale".into(), common: vec!["Colchique".into()], ..Species::default() }), medicinal: "goutte".into(), edible: "non".into(), ..Fiche::default() };
        f.add_known_danger();
        f.add_known_danger();
        assert_eq!(f.dangers.matches("Colchique : mortelle").count(), 1, "l'avertissement n'est ajouté qu'une fois");
        let b = f.search_body();
        for w in ["plante", "Colchicum autumnale", "Colchique", "médicinale", "comestible", "toxique"] {
            assert!(b.contains(w), "{w}");
        }
    }

    #[test]
    fn long_texts_are_cut_on_a_sentence() {
        let s = "Une phrase assez longue pour compter. ".repeat(100);
        let c = clip(&s, 200);
        assert!(c.chars().count() <= 200 && c.ends_with('.'), "{c}");
    }

    /// Essai réel, à la main : `cargo test -p astroportd --lib plant_fiche::tests::real -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore = "appel réel à Wikipédia"]
    async fn real_wikipedia_call() {
        for (lang, name) in [("fr", "Allium ursinum"), ("en", "Allium ursinum"), ("es", "Urtica dioica"), ("fr", "Digitalis purpurea")] {
            let w = wiki_read(&wiki_url(lang, name), &wiki_limits()).await.unwrap();
            eprintln!("── {lang} {name} : {:?}\nCOMESTIBLE {}\nPRÉPARATION {}\nMÉDICINAL {}\nDANGERS {}\n", w.source, w.edible, &w.preparation.chars().take(200).collect::<String>(), &w.medicinal.chars().take(200).collect::<String>(), &w.dangers.chars().take(200).collect::<String>());
            assert!(w.source.is_some() && !w.description.is_empty());
        }
    }
}
