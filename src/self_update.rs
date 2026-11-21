//! Mise à jour de la station depuis l'interface du Capitaine (Ma station › Détails techniques).
//!
//! La station ne se remplace pas elle-même : elle lance **le même `install.sh`** que le Capitaine taperait (binaire vérifié par
//! SHA-256, modèles de recherche mis à jour et nouveaux modèles recommandés, plugins installés mis à jour, redémarrage), pris
//! à l'étiquette de la version visée. Il tourne dans une unité systemd **à part** (`systemd-run --user`), sinon le
//! redémarrage du service le tuerait avec lui (même cgroup). Son compte rendu est `install.log`, relu par l'écran.
//!
//! La dernière version n'est demandée à GitHub qu'à l'ouverture de l'écran, et gardée 6 h.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::io::AsyncReadExt;

use crate::fetch::{FetchLimits, fetch};

/// Dépôt des releases (celui d'`install.sh`).
pub const REPO: &str = "papiche/astroportd";
/// Unité systemd transitoire de la mise à jour.
pub const UNIT: &str = "astroportd-update";
const CACHE_FOR: Duration = Duration::from_secs(6 * 3600);

static LATEST: Mutex<Option<(Instant, String)>> = Mutex::new(None);

/// `(majeur, mineur, correctif, est une préversion)` d'une version `v1.7.0` ou `1.7.0-beta.2`.
fn parse(v: &str) -> Option<(u64, u64, u64, bool)> {
    let v = v.trim().trim_start_matches('v');
    let (core, pre) = v.split_once('-').map_or((v, false), |(c, _)| (c, true));
    let mut it = core.split('.').map(|n| n.parse::<u64>().ok());
    Some((it.next()??, it.next().flatten().unwrap_or(0), it.next().flatten().unwrap_or(0), pre))
}

/// `latest` est-elle plus récente que `current` ? (À numéro égal, la version finale passe devant la préversion.)
pub fn is_newer(latest: &str, current: &str) -> bool {
    match (parse(latest), parse(current)) {
        (Some((a, b, c, pa)), Some((x, y, z, px))) => (a, b, c) > (x, y, z) || ((a, b, c) == (x, y, z) && px && !pa),
        _ => false,
    }
}

/// Étiquette de la release la plus récente dans la réponse de `GET /repos/…/releases?per_page=1`.
pub fn tag_of(body: &str) -> Option<String> {
    let v: Value = serde_json::from_str(body).ok()?;
    let tag = v.get(0)?.get("tag_name")?.as_str()?;
    // Une étiquette sert dans une adresse et une ligne de commande : rien d'autre que des chiffres, lettres, points et tirets.
    (tag.len() <= 40 && tag.starts_with(|c: char| c == 'v' || c.is_ascii_digit()) && tag.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')).then(|| tag.to_owned())
}

async fn get_text(url: &str, max: u64) -> Result<String, String> {
    let mut got = fetch(url, &FetchLimits::new(max)).await.map_err(|e| e.to_string())?;
    let mut s = String::new();
    got.body.read_to_string(&mut s).await.map_err(|e| e.to_string())?;
    if s.len() as u64 > max {
        return Err(format!("réponse de plus de {max} octets"));
    }
    Ok(s)
}

/// Dernière version publiée (cache de 6 h ; `fresh` force la question).
pub async fn latest(fresh: bool) -> Result<String, String> {
    if !fresh && let Ok(g) = LATEST.lock() && let Some((at, tag)) = g.as_ref() && at.elapsed() < CACHE_FOR {
        return Ok(tag.clone());
    }
    let body = get_text(&format!("https://api.github.com/repos/{REPO}/releases?per_page=1"), 256 * 1024).await?;
    let tag = tag_of(&body).ok_or("aucune release lisible sur GitHub")?;
    if let Ok(mut g) = LATEST.lock() {
        *g = Some((Instant::now(), tag.clone()));
    }
    Ok(tag)
}

/// Une ligne `nom|installée|disponible|état` d'`install-plugins.sh --status` → objet JSON.
pub fn plugin_lines(out: &str) -> Vec<Value> {
    out.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split('|').collect();
            (f.len() == 4 && !f[0].is_empty()).then(|| json!({ "name": f[0], "installed": (!f[1].is_empty()).then_some(f[1]), "available": f[2], "state": f[3] }))
        })
        .collect()
}

/// Plugins clonés dans `~/.zen/astroportd-plugins` et leur état (sans réseau : `--no-pull`). Vide s'il n'y en a pas.
async fn plugins(home: &Path) -> Vec<Value> {
    let script = home.join(".zen/astroportd-plugins/install-plugins.sh");
    if !script.is_file() {
        return Vec::new();
    }
    let run = tokio::process::Command::new("bash").arg(&script).args(["--status", "--no-pull"]).output();
    match tokio::time::timeout(Duration::from_secs(10), run).await {
        Ok(Ok(o)) if o.status.success() => plugin_lines(&String::from_utf8_lossy(&o.stdout)),
        _ => Vec::new(),
    }
}

/// Modèles de la recherche : installés et pas encore installés (catalogue de ce binaire).
fn models(dir: &Path) -> Value {
    use astro_index::catalog::{CATALOG, State};
    let list: Vec<Value> = CATALOG
        .iter()
        .filter(|m| m.pinned())
        .map(|m| json!({ "id": m.id, "label": m.label, "mb": m.mb(), "installed": m.state(dir) != State::Absent, "tasks": m.tasks.iter().map(|t| t.name()).collect::<Vec<_>>() }))
        .collect();
    json!(list)
}

/// L'unité de mise à jour tourne-t-elle ?
async fn running() -> bool {
    let st = tokio::process::Command::new("systemctl").args(["--user", "is-active", "--quiet", UNIT]).status();
    matches!(tokio::time::timeout(Duration::from_secs(5), st).await, Ok(Ok(s)) if s.success())
}

/// Dernières lignes du compte rendu d'`install.sh`.
fn log_tail(data: &Path, n: usize) -> Vec<String> {
    let Ok(s) = std::fs::read_to_string(data.join("install.log")) else { return Vec::new() };
    let lines: Vec<&str> = s.lines().collect();
    lines[lines.len().saturating_sub(n)..].iter().map(|l| l.chars().take(300).collect()).collect()
}

/// Vue de l'écran : version en place, dernière version, modèles, plugins, mise à jour en cours et son journal.
pub async fn view(data: &Path, home: &Path, fresh: bool) -> Value {
    let current = env!("CARGO_PKG_VERSION");
    let (latest, error) = match latest(fresh).await {
        Ok(t) => (Some(t), None),
        Err(e) => (None, Some(format!("dernière version inconnue : {e}"))),
    };
    let newer = latest.as_deref().is_some_and(|l| is_newer(l, current));
    let run = running().await;
    json!({
        "current": current, "latest": latest, "newer": newer, "error": error,
        "models": models(&data.join("models")), "plugins": plugins(home).await,
        "running": run, "log": if run { log_tail(data, 40) } else { log_tail(data, 12) },
        "can_run": crate::station_domain::restart_available(),
        "command": format!("curl -fsSL https://raw.githubusercontent.com/{REPO}/main/install.sh | bash"),
    })
}

/// Lance `install.sh --yes` de la version la plus récente dans l'unité transitoire [`UNIT`].
pub async fn start(data: &Path) -> Result<Value, String> {
    if !crate::station_domain::restart_available() {
        return Err("la station ne tourne pas sous systemd : lancez la mise à jour sur la machine (install.sh)".into());
    }
    if running().await {
        return Err("une mise à jour est déjà en cours".into());
    }
    let tag = latest(true).await?;
    let script = get_text(&format!("https://raw.githubusercontent.com/{REPO}/{tag}/install.sh"), 512 * 1024).await?;
    if !script.starts_with("#!") || !script.contains("astroportd") {
        return Err("install.sh reçu illisible : mise à jour abandonnée".into());
    }
    let path: PathBuf = data.join("install-ui.sh");
    std::fs::write(&path, script).map_err(|e| format!("{} : {e}", path.display()))?;
    let out = tokio::process::Command::new("systemd-run")
        .args(["--user", "--unit", UNIT, "--collect", "--quiet", "--setenv=ASTRO_SKIP_DISKTEST=1", "bash"])
        .arg(&path)
        .args(["--yes", "--version", &tag])
        .output();
    match tokio::time::timeout(Duration::from_secs(10), out).await {
        Ok(Ok(o)) if o.status.success() => Ok(json!({ "started": true, "version": tag })),
        Ok(Ok(o)) => Err(format!("systemd-run a refusé : {}", String::from_utf8_lossy(&o.stderr).trim())),
        Ok(Err(e)) => Err(format!("systemd-run introuvable : {e}")),
        Err(_) => Err("systemd-run ne répond pas".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_like_a_captain_expects() {
        assert!(is_newer("v1.7.0", "1.6.0") && is_newer("v1.10.0", "1.9.3") && is_newer("1.7.0", "1.7.0-beta.1"));
        assert!(!is_newer("v1.6.0", "1.6.0") && !is_newer("v1.6.0", "1.7.0") && !is_newer("v1.7.0-beta.1", "1.7.0") && !is_newer("n'importe quoi", "1.0.0"));
    }

    #[test]
    fn a_tag_is_only_taken_when_it_cannot_inject_anything() {
        assert_eq!(tag_of(r#"[{"tag_name":"v1.7.0"}]"#).as_deref(), Some("v1.7.0"));
        assert_eq!(tag_of(r#"[{"tag_name":"v1.7.0; rm -rf ~"}]"#), None);
        assert_eq!(tag_of(r#"[{"tag_name":"../../main"}]"#), None);
        assert_eq!(tag_of("[]"), None);
    }

    #[test]
    fn plugin_status_lines_become_objects() {
        let v = plugin_lines("media|0.2.0|0.3.0|maj\nenergy||0.1.0|nouveau\nbroken line\n");
        assert_eq!(v.len(), 2);
        assert_eq!((v[0]["state"].as_str(), v[0]["installed"].as_str()), (Some("maj"), Some("0.2.0")));
        assert!(v[1]["installed"].is_null());
    }
}
