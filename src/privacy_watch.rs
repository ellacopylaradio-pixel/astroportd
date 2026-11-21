//! Vigie « vie privée » du dossier `/Public` : ce qui y est publié est lisible par tout le monde (`public_site`). Les
//! **photos** n'y sont plus un souci de métadonnées : JPEG, PNG et WebP partent nettoyés de leur position GPS et de l'identité
//! de l'appareil (`public_clean`, l'original reste intact dans l'uCloud), HEIC, AVIF et TIFF ne sont pas servis. La vigie
//! **prévient seulement**, elle ne modifie rien :
//!
//! - **photo non servie** : HEIC, AVIF ou TIFF (d'après le type enregistré), que le site public refuse — le membre croit
//!   l'avoir publiée, elle n'est pas visible ;
//! - **fichier sensible** : coffre de mots de passe, fiche de contact, agenda, clé ou secret — rien de cela n'a sa
//!   place sur une page publique ;
//! - **papiers** : une image ou un PDF dont le **texte lu** (OCR, `search_text`) contient une pièce d'identité (bande MRZ,
//!   « carte nationale d'identité », passeport, permis, titre de séjour), un IBAN ou un numéro de sécurité sociale — les deux
//!   derniers validés par leur clé (modulo 97), pas seulement par leur forme.
//!
//! Elle tourne chaque nuit à 20h12 (rapport au Capitaine, message à chaque membre concerné) et répond en direct à
//! l'accueil du membre (`/api/v1/me/footprint`, champ `privacy`) : l'alerte disparaît dès que le fichier est retiré.
//! Tout se décide sur ce que la base sait déjà : aucun fichier n'est relu ni déchiffré.

use astro_db::{Db, DbError};
use serde::Serialize;

/// Extensions qui trahissent un fichier sensible (comparées en minuscules).
const SENSITIVE_EXT: [&str; 12] = ["kdbx", "kdb", "vcf", "ics", "key", "pem", "p12", "pfx", "dunikey", "ssss", "env", "gpg"];
/// Noms de fichiers de clés privées courants.
const SENSITIVE_NAMES: [&str; 4] = ["id_ed25519", "id_rsa", "id_ecdsa", "secret.june"];

/// Ce que la vigie a trouvé chez un membre.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Findings {
    /// Photos de `/Public` dans un format que le site public ne sert pas (HEIC, AVIF, TIFF).
    pub unserved: Vec<String>,
    /// Fichiers publics sensibles (mots de passe, contacts, agenda, clés).
    pub sensitive: Vec<String>,
    /// Images et PDF publics où se lit une pièce d'identité, un IBAN ou un numéro de sécurité sociale.
    pub papers: Vec<String>,
}

impl Findings {
    /// Rien à signaler ?
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.unserved.is_empty() && self.sensitive.is_empty() && self.papers.is_empty()
    }

    /// Phrase pour le membre.
    #[must_use]
    pub fn message(&self) -> String {
        let mut parts = Vec::new();
        if !self.sensitive.is_empty() {
            parts.push(format!("{} fichier(s) sensible(s) (mot de passe, contact, agenda ou clé) y sont publiés", self.sensitive.len()));
        }
        if !self.papers.is_empty() {
            parts.push(format!("{} document(s) laissent lire une pièce d'identité, un IBAN ou un numéro de sécurité sociale", self.papers.len()));
        }
        let mut text = if parts.is_empty() { String::new() } else { format!("Vie privée — votre dossier Public, lisible par tous : {}. Retirez-les de Public.", parts.join(" ; ")) };
        if !self.unserved.is_empty() {
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(&format!("{} photo(s) de Public (HEIC, AVIF ou TIFF) ne sont pas montrées aux visiteurs : la station ne sait pas en retirer le lieu et l'appareil. Publiez-les en JPEG.", self.unserved.len()));
        }
        text
    }
}

/// Longueur de l'IBAN par pays (ceux qu'on croise ici) : la longueur fixe et la clé écartent les suites de chiffres banales.
const IBAN_LEN: [(&str, usize); 14] = [("FR", 27), ("MC", 27), ("BE", 16), ("LU", 20), ("CH", 21), ("DE", 22), ("ES", 24), ("IT", 27), ("PT", 25), ("NL", 18), ("GB", 22), ("IE", 22), ("AT", 20), ("PL", 28)];

fn mod97(digits: impl Iterator<Item = u32>) -> u32 {
    digits.fold(0, |r, d| (r * if d > 9 { 100 } else { 10 } + d) % 97)
}

/// Un IBAN valide (pays connu, longueur, clé modulo 97) dans `compact` (lettres et chiffres seuls, majuscules).
fn has_iban(compact: &[char]) -> bool {
    (0..compact.len()).any(|i| {
        IBAN_LEN.iter().any(|(cc, n)| {
            let Some(w) = compact.get(i..i + n) else { return false };
            let ok_head = w[0] == cc.chars().next().unwrap_or(' ') && w[1] == cc.chars().nth(1).unwrap_or(' ') && w[2].is_ascii_digit() && w[3].is_ascii_digit();
            ok_head && w.iter().all(char::is_ascii_alphanumeric) && mod97(w[4..].iter().chain(&w[..4]).map(|c| c.to_digit(36).unwrap_or(0))) == 1
        })
    })
}

/// Un numéro de sécurité sociale français valide (sexe 1/2, mois 01-12 ou 20+, clé = 97 − numéro mod 97) parmi les chiffres.
fn has_nir(digits: &str) -> bool {
    let b = digits.as_bytes();
    (0..b.len().saturating_sub(14)).any(|i| {
        let w = &digits[i..i + 15];
        let month: u32 = w[3..5].parse().unwrap_or(0);
        let (Ok(num), Ok(key)) = (w[..13].parse::<u64>(), w[13..].parse::<u64>()) else { return false };
        matches!(b[i], b'1' | b'2') && (1..=12).contains(&month) && key == 97 - num % 97
    })
}

/// Le texte lu dans un document trahit-il des papiers personnels ?
pub fn looks_like_papers(text: &str) -> bool {
    let up = text.to_uppercase();
    let words = ["CARTE NATIONALE D'IDENTIT", "CARTE NATIONALE D’IDENTIT", "PASSEPORT", "PASSPORT", "PERMIS DE CONDUIRE", "TITRE DE SÉJOUR", "TITRE DE SEJOUR"];
    if up.contains("<<<<<") || words.iter().any(|w| up.contains(w)) {
        return true;
    }
    let compact: Vec<char> = up.chars().filter(char::is_ascii_alphanumeric).collect();
    // Les chiffres d'un même bloc, espaces et points compris (« 1 85 05 78 006 084 36 ») ; une lettre ou un retour à la ligne coupe.
    let digits: String = up.split(|c: char| !(c.is_ascii_digit() || c == ' ' || c == '.')).map(|s| s.replace([' ', '.'], "")).filter(|s| s.len() >= 15).collect::<Vec<_>>().join("|");
    has_iban(&compact) || digits.split('|').any(has_nir)
}

fn is_sensitive(path: &str, mime: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or("").to_ascii_lowercase();
    let ext = name.rsplit_once('.').map_or("", |(_, e)| e);
    SENSITIVE_EXT.contains(&ext) || SENSITIVE_NAMES.contains(&name.as_str()) || matches!(mime, "application/x-keepass2" | "text/vcard" | "text/calendar")
}

/// Fichier de `/Public` : propriétaire, chemin, type, texte lu dedans.
type Row = (String, String, String, Option<String>);

/// Photo que le site public refuse de servir (voir `public_clean`).
fn unserved(mime: &str) -> bool {
    matches!(mime, "image/heic" | "image/heif" | "image/heic-sequence" | "image/heif-sequence" | "image/avif" | "image/tiff")
}

/// Fichiers de `/Public` (plage d'index `['/Public/', '/Public0')`).
fn public_files(c: &rusqlite::Connection, owner: Option<&str>) -> rusqlite::Result<Vec<Row>> {
    let mut stmt = c.prepare(
        "SELECT e.owner_hex, e.path, COALESCE(e.mime, ''), \
         (SELECT group_concat(t.body, ' ') FROM search_text t WHERE t.owner_hex = e.owner_hex AND t.sha = e.sha_plain AND t.src = 'ocr') FROM ucloud_entries e \
         WHERE e.type = 'file' AND e.path >= '/Public/' AND e.path < '/Public0' AND (?1 IS NULL OR e.owner_hex = ?1) ORDER BY e.owner_hex, e.path",
    )?;
    stmt.query_map([owner], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect()
}

fn sort(rows: Vec<Row>) -> Vec<(String, Findings)> {
    let mut out: Vec<(String, Findings)> = Vec::new();
    for (owner, path, mime, text) in rows {
        // Position et appareil d'une photo ne sont plus signalés : ils sont retirés à la sortie (ou la photo n'est pas servie).
        let (sensitive, unserved) = (is_sensitive(&path, &mime), unserved(&mime));
        let papers = text.as_deref().is_some_and(looks_like_papers);
        if !sensitive && !unserved && !papers {
            continue;
        }
        if out.last().is_none_or(|(o, _)| *o != owner) {
            out.push((owner.clone(), Findings::default()));
        }
        if let Some((_, f)) = out.last_mut() {
            if papers {
                f.papers.push(path);
            } else if sensitive {
                f.sensitive.push(path);
            } else {
                f.unserved.push(path);
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
    fn papers_are_told_by_a_checked_iban_or_social_security_number_or_an_id_document() {
        assert!(looks_like_papers("IBAN : FR76 3000 6000 0112 3456 7890 189"), "IBAN valide");
        assert!(!looks_like_papers("IBAN : FR76 3000 6000 0112 3456 7890 188"), "mauvaise clé");
        assert!(looks_like_papers("N° de sécurité sociale 1 85 05 78 006 084 91"), "NIR valide");
        assert!(!looks_like_papers("Commande 1 85 05 78 006 084 90"), "mauvaise clé");
        assert!(looks_like_papers("IDFRADUPONT<<<<<<<<<<<<<<<<<<<<<"), "bande MRZ");
        assert!(looks_like_papers("RÉPUBLIQUE FRANÇAISE — Carte nationale d'identité"));
        assert!(!looks_like_papers("Facture n° 2026-117, total 84,50 €, tél. 06 12 34 56 78"));
    }

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
    async fn the_watch_tells_secrets_and_unserved_photos_in_public_but_no_longer_located_ones() {
        let dir = std::env::temp_dir().join(format!("astro-privacy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("u.db"), 1).unwrap();
        for (owner, path, mime, lat) in [
            ("aa", "/Public/plage.jpg", "image/jpeg", Some(43.6)),
            ("aa", "/Public/signée.jpg", "image/jpeg", None),
            ("aa", "/Public/logo.png", "image/png", None),
            ("aa", "/Public/iphone.heic", "image/heic", Some(43.6)),
            ("aa", "/Public/perso.kdbx", "application/x-keepass2", None),
            ("aa", "/Photos/maison.jpg", "image/jpeg", Some(43.6)),
            ("aa", "/Publication/x.jpg", "image/jpeg", Some(1.0)),
            ("bb", "/Public/index.html", "text/html", None),
            ("cc", "/Public/sub/Léa.vcf", "text/vcard", None),
        ] {
            db.writer()
                .execute(
                    "INSERT INTO ucloud_entries(owner_hex, path, parent, type, cid, mime, size, mtime, lat, lon, data) VALUES (?1, ?2, '/Public', 'file', 'c', ?3, 1, 1, ?4, ?4, CASE WHEN ?2 LIKE '%signée%' THEN json_object('exif_ident', 1) ELSE '{}' END)",
                    vec![owner.to_owned().into(), path.to_owned().into(), mime.to_owned().into(), lat.map_or(rusqlite::types::Value::Null, rusqlite::types::Value::Real)],
                )
                .await
                .unwrap();
        }
        let aa = check(&db, "aa").await.unwrap();
        // plage.jpg (GPS) et signée.jpg (numéro de série) partent nettoyés : plus d'alarme ; la photo HEIC, elle, n'est pas servie.
        assert_eq!(aa, Findings { unserved: vec!["/Public/iphone.heic".into()], sensitive: vec!["/Public/perso.kdbx".into()], papers: vec![] }, "ni /Photos, ni /Publication");
        assert!(aa.message().contains("1 photo(s) de Public (HEIC") && aa.message().contains("1 fichier(s) sensible(s)") && !aa.message().contains("GPS"));
        assert!(check(&db, "bb").await.unwrap().is_empty(), "une page publique ordinaire ne dit rien");
        let all = sweep(&db).await.unwrap();
        assert_eq!(all.iter().map(|(o, _)| o.as_str()).collect::<Vec<_>>(), ["aa", "cc"]);
        // Un scan publié par erreur : la vigie le reconnaît au texte lu dedans (OCR), pas à son nom.
        db.writer().execute("UPDATE ucloud_entries SET sha_plain = 'rib' WHERE path = '/Public/logo.png'", vec![]).await.unwrap();
        db.writer().execute("INSERT INTO search_text(body, owner_hex, sha, src, t_ms) VALUES ('Relevé d''identité bancaire IBAN FR76 3000 6000 0112 3456 7890 189', 'aa', 'rib', 'ocr', 0)", vec![]).await.unwrap();
        let aa = check(&db, "aa").await.unwrap();
        assert_eq!(aa.papers, ["/Public/logo.png"]);
        assert!(aa.message().contains("IBAN"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
