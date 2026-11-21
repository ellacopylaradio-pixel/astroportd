//! Aperçu riche d'un lien du chat : titre, description, site et petite image (balises Open Graph, sinon `<title>`).
//!
//! C'est la **station** qui va chercher la page, jamais le navigateur (sa politique de contenu n'autorise que la station) :
//! l'adresse IP du membre ne part pas chez le site. Garde-fous de [`crate::fetch`] (adresses publiques seulement, redirections
//! revérifiées, HTTPS validé), page bornée à [`MAX_PAGE`], image à [`MAX_IMAGE`] et rendue en `data:` ; réponses gardées en
//! mémoire ([`CACHE_MAX`] liens, [`CACHE_SECS`]).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::Engine;
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;

use crate::fetch::{FetchLimits, fetch};

/// Octets de page lus au plus (les balises utiles sont dans l'en-tête).
pub const MAX_PAGE: u64 = 512 * 1024;
/// Octets d'image au plus.
pub const MAX_IMAGE: u64 = 300 * 1024;
const CACHE_MAX: usize = 500;
const CACHE_SECS: u64 = 6 * 3600;

/// Cache des aperçus (les échecs aussi : un lien mort n'est pas réessayé à chaque affichage).
#[derive(Default)]
pub struct Previews {
    cache: Mutex<HashMap<String, (Instant, Value)>>,
}

impl Previews {
    /// Aperçu de `url` (`{ url, title, description, site, image }`), ou `{ url, none: true }` si la page n'en donne pas.
    pub async fn get(&self, url: &str) -> Result<Value, String> {
        let parsed = url::Url::parse(url).map_err(|_| "adresse invalide".to_owned())?;
        if !matches!(parsed.scheme(), "http" | "https") || url.len() > 2000 {
            return Err("seules les adresses http(s) ont un aperçu".into());
        }
        if let Some((at, v)) = self.cache.lock().unwrap_or_else(|e| e.into_inner()).get(url) {
            if at.elapsed() < Duration::from_secs(CACHE_SECS) {
                return Ok(v.clone());
            }
        }
        let v = build(url).await.unwrap_or_else(|_| json!({ "url": url, "none": true }));
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if cache.len() >= CACHE_MAX {
            let old = cache.iter().min_by_key(|(_, (t, _))| *t).map(|(k, _)| k.clone());
            if let Some(k) = old {
                cache.remove(&k);
            }
        }
        cache.insert(url.to_owned(), (Instant::now(), v.clone()));
        Ok(v)
    }
}

fn limits(max: u64) -> FetchLimits {
    let mut l = FetchLimits::new(max);
    l.timeout = Duration::from_secs(6);
    l
}

async fn read(url: &str, max: u64) -> Result<(Vec<u8>, Option<String>, url::Url), String> {
    let got = fetch(url, &limits(max)).await.map_err(|e| e.to_string())?;
    let (ct, base) = (got.content_type.clone(), got.final_url.clone());
    let mut out = Vec::new();
    got.body.take(max).read_to_end(&mut out).await.map_err(|e| e.to_string())?;
    Ok((out, ct, base))
}

async fn build(url: &str) -> Result<Value, String> {
    let (page, ct, base) = read(url, MAX_PAGE).await?;
    if !ct.as_deref().unwrap_or("text/html").contains("html") {
        return Err("pas une page".into());
    }
    let tags = parse(&String::from_utf8_lossy(&page));
    let pick = |keys: &[&str]| keys.iter().find_map(|k| tags.get(*k)).map(|s| s.chars().take(300).collect::<String>());
    let title = pick(&["og:title", "twitter:title", "<title>"]);
    let Some(title) = title.filter(|t| !t.trim().is_empty()) else { return Ok(json!({ "url": url, "none": true })) };
    let description = pick(&["og:description", "twitter:description", "description"]);
    let site = pick(&["og:site_name"]).or_else(|| base.host_str().map(str::to_owned));
    let image = match pick(&["og:image", "twitter:image"]).and_then(|i| base.join(&i).ok()) {
        Some(u) if matches!(u.scheme(), "http" | "https") => image(u.as_str()).await,
        _ => None,
    };
    Ok(json!({ "url": url, "title": title.trim(), "description": description.map(|d| d.trim().to_owned()), "site": site, "image": image }))
}

async fn image(url: &str) -> Option<String> {
    let (bytes, ct, _) = read(url, MAX_IMAGE).await.ok()?;
    let ct = ct?.split(';').next()?.trim().to_ascii_lowercase();
    // Jamais de SVG (il peut porter du script) : seulement des images matricielles.
    if !matches!(ct.as_str(), "image/jpeg" | "image/png" | "image/webp" | "image/gif") || bytes.len() as u64 >= MAX_IMAGE {
        return None;
    }
    Some(format!("data:{ct};base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes)))
}

/// Balises `<meta property|name=… content=…>` et `<title>` de l'en-tête (clés en minuscules, entités courantes décodées).
fn parse(html: &str) -> HashMap<String, String> {
    let head = html.split("</head>").next().unwrap_or(html);
    let lower = head.to_ascii_lowercase();
    let mut out = HashMap::new();
    let mut at = 0;
    while let Some(i) = lower[at..].find("<meta") {
        let start = at + i;
        let Some(end) = lower[start..].find('>').map(|e| start + e) else { break };
        let tag = &head[start..end];
        let key = attr(tag, "property").or_else(|| attr(tag, "name")).map(|k| k.to_ascii_lowercase());
        if let (Some(k), Some(v)) = (key, attr(tag, "content")) {
            out.entry(k).or_insert(decode(&v));
        }
        at = end;
    }
    if let (Some(s), Some(e)) = (lower.find("<title"), lower.find("</title>")) {
        if let Some(gt) = lower[s..].find('>').map(|g| s + g + 1).filter(|g| *g <= e) {
            out.insert("<title>".into(), decode(head[gt..e].trim()));
        }
    }
    out
}

/// Valeur d'un attribut (`name="…"`, `name='…'`) d'une balise.
fn attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(i) = lower[from..].find(name) {
        let p = from + i;
        let before_ok = p == 0 || lower.as_bytes()[p - 1].is_ascii_whitespace();
        let rest = lower[p + name.len()..].trim_start();
        if before_ok && rest.starts_with('=') {
            let off = tag.len() - rest.len() + 1;
            let v = tag[off..].trim_start();
            let q = v.chars().next()?;
            return if q == '"' || q == '\'' { v[1..].split(q).next().map(str::to_owned) } else { v.split(|c: char| c.is_whitespace() || c == '/').next().map(str::to_owned) };
        }
        from = p + name.len();
    }
    None
}

fn decode(s: &str) -> String {
    s.replace("&amp;", "&").replace("&quot;", "\"").replace("&#39;", "'").replace("&#x27;", "'").replace("&lt;", "<").replace("&gt;", ">").replace("&nbsp;", " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_graph_and_title_are_read_from_the_head() {
        let t = parse(r#"<html><head><title> Ma page </title><meta property="og:title" content="Titre &amp; co"><meta name='description' content='Une page'><META PROPERTY="og:image" CONTENT="/i.png"></head><body><meta property="og:title" content="faux"></body>"#);
        assert_eq!(t.get("og:title").map(String::as_str), Some("Titre & co"));
        assert_eq!(t.get("description").map(String::as_str), Some("Une page"));
        assert_eq!(t.get("og:image").map(String::as_str), Some("/i.png"));
        assert_eq!(t.get("<title>").map(String::as_str), Some("Ma page"));
        assert_eq!(attr(r#"<meta data-name="x" name="y" content="z""#, "name").as_deref(), Some("y"), "data-name n'est pas name");
    }

    #[tokio::test]
    async fn private_and_odd_addresses_get_no_preview() {
        let p = Previews::default();
        assert!(p.get("ftp://exemple.org/").await.is_err());
        assert_eq!(p.get("http://127.0.0.1/").await.unwrap()["none"], true, "SSRF : jamais d'adresse locale");
    }
}
