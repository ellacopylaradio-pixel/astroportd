//! Filtre des messages CDP (Chrome DevTools Protocol) entre un agent et le navigateur prêté.
//!
//! Le filtre retire les voies **directes** vers la session du membre (lecture et écriture des cookies, fichiers, traces du
//! système), borne la navigation explicite au site accordé et retire les en-têtes `Cookie` des événements réseau. Il ne
//! prétend pas davantage : un agent qui exécute du JavaScript dans la page lit ce que la page lit
//! (voir `docs/design/SESSION_AGENT.md`, § 2).

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde_json::{Value, json};

/// Commandes refusées, par nom exact.
const DENIED: &[&str] = &[
    "Storage.getCookies",
    "Storage.setCookies",
    "Storage.clearCookies",
    "Network.getCookies",
    "Network.getAllCookies",
    "Network.setCookie",
    "Network.setCookies",
    "Network.deleteCookies",
    "Network.clearBrowserCookies",
    "Browser.getBrowserCommandLine",
    "Target.exposeDevToolsProtocol",
];
/// Domaines CDP refusés en entier (`IO.read` lit des fichiers par descripteur, `SystemInfo` décrit la machine).
const DENIED_DOMAINS: &[&str] = &["IO", "SystemInfo", "Tracing"];
/// Clés retirées des événements réseau : elles portent les cookies ou les en-têtes bruts.
const SCRUBBED_KEYS: &[&str] = &["associatedCookies", "blockedCookies", "exemptedCookies", "headersText", "requestHeadersText", "cookiePartitionKey"];
/// Hôtes visités retenus au plus pour l'audit.
const MAX_HOSTS: usize = 50;

/// Ce que fait la porte d'un message de l'agent.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Transmis au navigateur (éventuellement réécrit).
    Forward(String),
    /// Refusé : cette réponse d'erreur est rendue à l'agent, rien ne part vers le navigateur.
    Deny(String),
    /// Ignoré (message mal formé, sans identifiant auquel répondre).
    Drop,
}

/// Comptes d'une session, écrits dans le journal d'audit à la fin.
#[derive(Debug, Default, Clone)]
pub struct Stats {
    /// Commandes transmises, par domaine CDP.
    pub calls: BTreeMap<String, u64>,
    /// Commandes refusées.
    pub denied: u64,
    /// Hôtes demandés par navigation explicite (acceptés ou non).
    pub hosts: BTreeSet<String>,
}

impl Stats {
    /// Résumé JSON pour l'audit.
    #[must_use]
    pub fn summary(&self) -> Value {
        json!({ "cdp_calls": self.calls, "denied_calls": self.denied, "hosts": self.hosts })
    }
}

/// Règles d'une session : le site accordé (et ses sous-domaines), les hôtes ajoutés par le membre, le dossier des
/// téléchargements (dans le profil jetable, effacé avec lui).
#[derive(Debug, Clone)]
pub struct Policy {
    site: String,
    extra: Vec<String>,
    downloads: PathBuf,
}

/// Hôte du site ou d'un de ses sous-domaines.
fn host_matches(host: &str, site: &str) -> bool {
    host == site || host.strip_suffix(site).is_some_and(|p| p.ends_with('.'))
}

/// Suffixes des noms qui désignent le réseau local : le navigateur de la station ne doit jamais y aller (services privés de la
/// station ou de la box).
const LOCAL_SUFFIXES: &[&str] = &[".local", ".localhost", ".internal", ".lan", ".home", ".home.arpa", ".localdomain", ".intranet", ".corp"];

/// Nom d'hôte public : au moins un point, et pas un nom du réseau local. Les adresses IP sont écartées avant (l'URL doit
/// porter un nom).
fn public_host(host: &str) -> bool {
    host.contains('.') && !LOCAL_SUFFIXES.iter().any(|s| host.ends_with(s))
}

impl Policy {
    /// Règles pour `site` (nom d'hôte déjà validé), avec les hôtes `extra`, téléchargements dans `downloads`.
    #[must_use]
    pub fn new(site: &str, extra: Vec<String>, downloads: PathBuf) -> Self {
        Self { site: site.to_ascii_lowercase(), extra: extra.into_iter().map(|h| h.to_ascii_lowercase()).collect(), downloads }
    }

    /// Règles ouvertes (co-surfing) : tout site **public** ; jamais le réseau local ni une adresse IP.
    #[must_use]
    pub fn open(downloads: PathBuf) -> Self {
        Self { site: String::new(), extra: Vec::new(), downloads }
    }

    /// L'adresse `url` est-elle une page du site accordé (ou `about:blank`) ? En règles ouvertes : une page d'un site public.
    #[must_use]
    pub fn allows_url(&self, url: &str) -> bool {
        if url == "about:blank" {
            return true;
        }
        let Ok(u) = url::Url::parse(url) else { return false };
        if !matches!(u.scheme(), "https" | "http") || !u.username().is_empty() || u.password().is_some() {
            return false;
        }
        let Some(url::Host::Domain(host)) = u.host() else { return false };
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        if !public_host(&host) {
            return false;
        }
        self.site.is_empty() || host_matches(&host, &self.site) || self.extra.iter().any(|e| host_matches(&host, e))
    }

    /// Décide d'un message de l'agent.
    pub fn inbound(&self, raw: &str, stats: &mut Stats) -> Verdict {
        let Ok(mut msg) = serde_json::from_str::<Value>(raw) else { return Verdict::Drop };
        let (Some(id), Some(method)) = (msg.get("id").filter(|v| v.is_u64()).cloned(), msg.get("method").and_then(Value::as_str).map(str::to_owned)) else {
            return Verdict::Drop;
        };
        let domain = method.split_once('.').map_or(method.as_str(), |(d, _)| d).to_owned();
        let deny = |why: &str, stats: &mut Stats| {
            stats.denied += 1;
            let mut reply = json!({ "id": id, "error": { "code": -32000, "message": format!("refusé par la station : {why}") } });
            if let Some(s) = msg.get("sessionId") {
                reply["sessionId"] = s.clone();
            }
            Verdict::Deny(reply.to_string())
        };
        if DENIED.contains(&method.as_str()) || DENIED_DOMAINS.contains(&domain.as_str()) {
            return deny("la session du membre ne se lit pas", stats);
        }
        match method.as_str() {
            "Page.navigate" | "Target.createTarget" => {
                let url = msg.pointer("/params/url").and_then(Value::as_str).unwrap_or_default();
                if let Ok(u) = url::Url::parse(url) {
                    if let Some(h) = u.host_str() {
                        if stats.hosts.len() < MAX_HOSTS {
                            stats.hosts.insert(h.to_ascii_lowercase());
                        }
                    }
                }
                if !self.allows_url(url) {
                    return deny("adresse hors du site accordé", stats);
                }
            }
            // Les téléchargements restent dans le profil jetable, quel que soit le dossier demandé.
            "Browser.setDownloadBehavior" | "Page.setDownloadBehavior" => {
                if let Some(p) = msg.get_mut("params").and_then(Value::as_object_mut) {
                    if p.contains_key("downloadPath") {
                        p.insert("downloadPath".into(), Value::String(self.downloads.to_string_lossy().into_owned()));
                    }
                }
                *stats.calls.entry(domain).or_default() += 1;
                return Verdict::Forward(msg.to_string());
            }
            _ => {}
        }
        *stats.calls.entry(domain).or_default() += 1;
        Verdict::Forward(raw.to_owned())
    }

    /// Nettoie un message du navigateur avant de le rendre à l'agent : en-têtes `Cookie` / `Set-Cookie` et listes de cookies
    /// retirés. Les messages sans cookie passent sans être relus.
    #[must_use]
    pub fn outbound(raw: String) -> String {
        if !raw.contains("ookie") {
            return raw;
        }
        let Ok(mut v) = serde_json::from_str::<Value>(&raw) else { return raw };
        scrub(&mut v);
        v.to_string()
    }
}

fn scrub(v: &mut Value) {
    match v {
        Value::Object(map) => {
            for k in SCRUBBED_KEYS {
                map.remove(*k);
            }
            for (k, child) in map.iter_mut() {
                if matches!(k.as_str(), "headers" | "requestHeaders") {
                    if let Value::Object(h) = child {
                        h.retain(|name, _| !name.eq_ignore_ascii_case("cookie") && !name.eq_ignore_ascii_case("set-cookie"));
                    }
                }
                scrub(child);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(scrub),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::panic)]
    use super::*;

    fn policy() -> Policy {
        Policy::new("linkedin.com", vec!["accounts.example.net".into()], PathBuf::from("/profil/dl"))
    }

    #[test]
    fn only_the_granted_site_and_its_subdomains_are_reachable() {
        let p = policy();
        for ok in ["https://linkedin.com/feed", "https://www.linkedin.com/", "http://a.b.linkedin.com", "about:blank", "https://accounts.example.net/o"] {
            assert!(p.allows_url(ok), "{ok}");
        }
        for bad in ["https://evillinkedin.com", "https://linkedin.com.evil.org", "file:///etc/passwd", "javascript:alert(1)", "chrome://settings", "data:text/html,x", "https://user@linkedin.com", "https://127.0.0.1", "", "https://example.net"] {
            assert!(!p.allows_url(bad), "{bad}");
        }
    }

    #[test]
    fn open_rules_allow_public_sites_but_never_the_local_network() {
        let p = Policy::open(PathBuf::from("/p"));
        for ok in ["https://example.org/a", "https://www.service-public.fr", "about:blank"] {
            assert!(p.allows_url(ok), "{ok}");
        }
        for bad in ["http://localhost:12345", "http://127.0.0.1", "http://[::1]/", "http://192.168.1.1/", "http://box.lan/", "http://nas.local", "http://router.home.arpa", "http://intranet", "file:///etc/passwd", "http://10.0.0.1"] {
            assert!(!p.allows_url(bad), "{bad}");
        }
        assert!(!policy().allows_url("http://linkedin.com.local"), "même en règles strictes");
    }

    #[test]
    fn cookie_commands_files_and_foreign_navigation_are_refused() {
        let p = policy();
        let mut s = Stats::default();
        for m in ["Storage.getCookies", "Network.getAllCookies", "Network.setCookie", "IO.read", "SystemInfo.getInfo", "Browser.getBrowserCommandLine"] {
            let raw = json!({ "id": 7, "method": m, "sessionId": "S1", "params": {} }).to_string();
            let Verdict::Deny(r) = p.inbound(&raw, &mut s) else { panic!("{m} doit être refusé") };
            let r: Value = serde_json::from_str(&r).unwrap();
            assert_eq!((r["id"].as_u64(), r["sessionId"].as_str()), (Some(7), Some("S1")), "réponse routable vers la bonne session");
            assert!(r["error"]["message"].as_str().unwrap().contains("refusé"));
        }
        let nav = json!({ "id": 8, "method": "Page.navigate", "params": { "url": "https://evil.org/" } }).to_string();
        assert!(matches!(p.inbound(&nav, &mut s), Verdict::Deny(_)));
        let blank = json!({ "id": 9, "method": "Target.createTarget", "params": { "url": "about:blank" } }).to_string();
        assert!(matches!(p.inbound(&blank, &mut s), Verdict::Forward(_)));
        assert_eq!(s.denied, 7);
        assert!(s.hosts.contains("evil.org"), "l'audit garde la trace de la tentative");
        // Mal formé ou sans identifiant : ignoré.
        assert_eq!(p.inbound("pas du json", &mut s), Verdict::Drop);
        assert_eq!(p.inbound(r#"{"method":"Page.navigate"}"#, &mut s), Verdict::Drop);
    }

    #[test]
    fn downloads_are_kept_inside_the_disposable_profile() {
        let mut s = Stats::default();
        let raw = json!({ "id": 1, "method": "Browser.setDownloadBehavior", "params": { "behavior": "allowAndName", "downloadPath": "/home/fred/.ssh" } }).to_string();
        let Verdict::Forward(out) = policy().inbound(&raw, &mut s) else { panic!() };
        assert!(out.contains("/profil/dl") && !out.contains(".ssh"), "{out}");
    }

    #[test]
    fn cookie_headers_are_scrubbed_from_network_events() {
        let ev = json!({ "method": "Network.requestWillBeSentExtraInfo", "params": {
            "headers": { "Cookie": "li_at=SECRET", "accept": "*/*" },
            "associatedCookies": [{ "cookie": { "name": "li_at", "value": "SECRET" } }],
            "headersText": "Cookie: li_at=SECRET" } });
        let out = Policy::outbound(ev.to_string());
        assert!(!out.contains("SECRET") && out.contains("accept"), "{out}");
        let resp = json!({ "method": "Network.responseReceived", "params": { "response": { "headers": { "set-cookie": "a=SECRET", "content-type": "text/html" }, "requestHeaders": { "COOKIE": "b=SECRET" } } } });
        let out = Policy::outbound(resp.to_string());
        assert!(!out.contains("SECRET") && out.contains("text/html"), "{out}");
        let plain = r#"{"id":1,"result":{}}"#.to_owned();
        assert_eq!(Policy::outbound(plain.clone()), plain);
    }
}
