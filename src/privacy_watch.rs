//! Vigie « vie privée » du dossier `/Public` : ce qui y est publié est servi **tel quel** à tout le monde
//! (`public_site`), métadonnées comprises. La vigie **prévient seulement**, elle ne modifie rien :
//!
//! - **photo géolocalisée** : la position GPS lue à l'envoi (`ucloud_entries.lat`) révèle où elle a été prise, souvent
//!   le domicile ;
//! - **fichier sensible** : coffre de mots de passe, fiche de contact, agenda, clé ou secret — rien de cela n'a sa
//!   place sur une page publique.
//!
//! Elle tourne chaque nuit à 20h12 (rapport au Capitaine, message à chaque membre concerné) et répond en direct à
//! l'accueil du membre (`/api/v1/me/footprint`, champ `privacy`) : l'alerte disparaît dès que le fichier est retiré.
//! Tout se décide en SQL, sur ce que la base sait déjà : aucun fichier n'est relu ni déchiffré.

use astro_db::{Db, DbError};
use serde::Serialize;

/// Extensions qui trahissent un fichier sensible (comparées en minuscules).
const SENSITIVE_EXT: [&str; 12] = ["kdbx", "kdb", "vcf", "ics", "key", "pem", "p12", "pfx", "dunikey", "ssss", "env", "gpg"];
/// Noms de fichiers de clés privées courants.
const SENSITIVE_NAMES: [&str; 4] = ["id_ed25519", "id_rsa", "id_ecdsa", "secret.june"];

/// Ce que la vigie a trouvé chez un membre.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Findings {
    /// Photos publiques qui portent une position GPS.
    pub gps: Vec<String>,
    /// Fichiers publics sensibles (mots de passe, contacts, agenda, clés).
    pub sensitive: Vec<String>,
}

impl Findings {
    /// Rien à signaler ?
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.gps.is_empty() && self.sensitive.is_empty()
    }

    /// Phrase pour le membre.
    #[must_use]
    pub fn message(&self) -> String {
        let mut parts = Vec::new();
        if !self.gps.is_empty() {
            parts.push(format!("{} photo(s) révèlent où elles ont été prises (position GPS)", self.gps.len()));
        }
        if !self.sensitive.is_empty() {
            parts.push(format!("{} fichier(s) sensible(s) (mot de passe, contact, agenda ou clé) y sont publiés", self.sensitive.len()));
        }
        format!("Vie privée — votre dossier Public, lisible par tous : {}. Retirez-les de Public, ou republiez les photos sans leur position.", parts.join(" ; "))
    }
}

fn is_sensitive(path: &str, mime: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or("").to_ascii_lowercase();
    let ext = name.rsplit_once('.').map_or("", |(_, e)| e);
    SENSITIVE_EXT.contains(&ext) || SENSITIVE_NAMES.contains(&name.as_str()) || matches!(mime, "application/x-keepass2" | "text/vcard" | "text/calendar")
}

/// Fichiers de `/Public` (plage d'index `['/Public/', '/Public0')`) : `(propriétaire, chemin, type, géolocalisé)`.
fn public_files(c: &rusqlite::Connection, owner: Option<&str>) -> rusqlite::Result<Vec<(String, String, String, bool)>> {
    let mut stmt = c.prepare(
        "SELECT owner_hex, path, COALESCE(mime, ''), lat IS NOT NULL AND lon IS NOT NULL FROM ucloud_entries \
         WHERE type = 'file' AND path >= '/Public/' AND path < '/Public0' AND (?1 IS NULL OR owner_hex = ?1) ORDER BY owner_hex, path",
    )?;
    stmt.query_map([owner], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect()
}

fn sort(rows: Vec<(String, String, String, bool)>) -> Vec<(String, Findings)> {
    let mut out: Vec<(String, Findings)> = Vec::new();
    for (owner, path, mime, geo) in rows {
        let (sensitive, gps) = (is_sensitive(&path, &mime), geo && mime.starts_with("image/"));
        if !sensitive && !gps {
            continue;
        }
        if out.last().is_none_or(|(o, _)| *o != owner) {
            out.push((owner.clone(), Findings::default()));
        }
        if let Some((_, f)) = out.last_mut() {
            if sensitive {
                f.sensitive.push(path);
            } else {
                f.gps.push(path);
            }
        }
    }
    out
}

/// Ce que la vigie trouve chez `owner`, en direct.
pub async fn check(db: &Db, owner: &str) -> Result<Findings, DbError> {
    let o = owner.to_owned();
    let rows = db.readers().read(move |c| Ok(public_files(c, Some(&o))?)).await?;
    Ok(sort(rows).into_iter().next().map(|(_, f)| f).unwrap_or_default())
}

/// Passage de toute la station (nuit de 20h12) : les membres concernés et ce qui a été trouvé chez chacun.
pub async fn sweep(db: &Db) -> Result<Vec<(String, Findings)>, DbError> {
    let rows = db.readers().read(|c| Ok(public_files(c, None)?)).await?;
    Ok(sort(rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sensitive_files_are_told_by_their_name_or_their_type() {
        for (p, m) in [("/Public/perso.KDBX", ""), ("/Public/a/Léa.vcf", ""), ("/Public/x", "text/calendar"), ("/Public/id_ed25519", ""), ("/Public/.env", ""), ("/Public/n.dunikey", "")] {
            assert!(is_sensitive(p, m), "{p}");
        }
        for (p, m) in [("/Public/index.html", "text/html"), ("/Public/photo.jpg", "image/jpeg"), ("/Public/keys.txt", "text/plain")] {
            assert!(!is_sensitive(p, m), "{p}");
        }
    }

    #[tokio::test]
    async fn the_watch_finds_located_photos_and_secrets_in_public_and_nothing_elsewhere() {
        let dir = std::env::temp_dir().join(format!("astro-privacy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("u.db"), 1).unwrap();
        for (owner, path, mime, lat) in [
            ("aa", "/Public/plage.jpg", "image/jpeg", Some(43.6)),
            ("aa", "/Public/logo.png", "image/png", None),
            ("aa", "/Public/perso.kdbx", "application/x-keepass2", None),
            ("aa", "/Photos/maison.jpg", "image/jpeg", Some(43.6)),
            ("aa", "/Publication/x.jpg", "image/jpeg", Some(1.0)),
            ("bb", "/Public/index.html", "text/html", None),
            ("cc", "/Public/sub/Léa.vcf", "text/vcard", None),
        ] {
            db.writer()
                .execute(
                    "INSERT INTO ucloud_entries(owner_hex, path, parent, type, cid, mime, size, mtime, lat, lon, data) VALUES (?1, ?2, '/Public', 'file', 'c', ?3, 1, 1, ?4, ?4, '{}')",
                    vec![owner.to_owned().into(), path.to_owned().into(), mime.to_owned().into(), lat.map_or(rusqlite::types::Value::Null, rusqlite::types::Value::Real)],
                )
                .await
                .unwrap();
        }
        let aa = check(&db, "aa").await.unwrap();
        assert_eq!(aa, Findings { gps: vec!["/Public/plage.jpg".into()], sensitive: vec!["/Public/perso.kdbx".into()] }, "ni /Photos, ni /Publication");
        assert!(aa.message().contains("1 photo(s)") && aa.message().contains("1 fichier(s) sensible(s)"));
        assert!(check(&db, "bb").await.unwrap().is_empty(), "une page publique ordinaire ne dit rien");
        let all = sweep(&db).await.unwrap();
        assert_eq!(all.iter().map(|(o, _)| o.as_str()).collect::<Vec<_>>(), ["aa", "cc"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
