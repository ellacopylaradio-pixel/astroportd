//! PDF par poppler (`pdftotext`, `pdfinfo`, `pdftoppm`, paquet `poppler-utils`, facultatif) : **en flux**, entrée standard
//! et sortie standard, sans fichier temporaire. Un PDF né numérique rend son texte tel quel ; un PDF scanné est rendu en
//! images, page par page, pour l'OCR. Bornes : [`MAX_PAGES`] pages, côté de page, sortie et durée de chaque outil.

use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

use crate::{IndexError, MAX_TEXT};

/// Pages lues au plus (la suite d'un long document n'est pas indexée).
pub const MAX_PAGES: usize = 30;
/// Côté le plus long d'une page rendue, en pixels (≈ 170 dpi pour un A4).
const PAGE_SIDE: &str = "2000";
/// Image d'une page au plus.
const MAX_PAGE_BYTES: usize = 24 << 20;
/// Durée d'un appel au plus.
const TIMEOUT: Duration = Duration::from_secs(60);
/// Texte d'une page en dessous duquel le PDF est traité comme scanné.
const MIN_TEXT_PER_PAGE: usize = 20;

/// poppler est-il installé ?
pub fn available() -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join("pdftoppm").is_file() && d.join("pdftotext").is_file()))
}

async fn tool(cmd: &str, args: &[&str], input: &[u8], max_out: usize) -> Result<Vec<u8>, IndexError> {
    let mut child = Command::new(cmd)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| IndexError::Unavailable(format!("{cmd} : {e} (paquet poppler-utils)")))?;
    let (Some(mut stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else { return Err(IndexError::Backend(format!("{cmd} : flux indisponibles"))) };
    let input = input.to_vec();
    let feed = tokio::spawn(async move {
        let _ = stdin.write_all(&input).await; // l'outil peut s'arrêter avant d'avoir tout lu (page demandée atteinte)
    });
    let work = async {
        let mut out = Vec::new();
        stdout.take(max_out as u64 + 1).read_to_end(&mut out).await.map_err(|e| IndexError::Backend(e.to_string()))?;
        let status = child.wait().await.map_err(|e| IndexError::Backend(e.to_string()))?;
        Ok::<_, IndexError>((out, status))
    };
    let (out, status) = tokio::time::timeout(TIMEOUT, work).await.map_err(|_| IndexError::Backend(format!("{cmd} : délai dépassé")))??;
    feed.abort();
    if out.len() > max_out {
        return Err(IndexError::TooLarge(out.len()));
    }
    if !status.success() {
        return Err(IndexError::Invalid(format!("{cmd} : PDF illisible")));
    }
    Ok(out)
}

/// Nombre de pages (au plus [`MAX_PAGES`] retenues).
pub async fn pages(pdf: &[u8]) -> Result<usize, IndexError> {
    let info = String::from_utf8_lossy(&tool("pdfinfo", &["-"], pdf, 64 << 10).await?).into_owned();
    let n = info.lines().find_map(|l| l.strip_prefix("Pages:")).and_then(|v| v.trim().parse::<usize>().ok()).ok_or_else(|| IndexError::Invalid("pdfinfo : nombre de pages inconnu".into()))?;
    Ok(n.min(MAX_PAGES))
}

/// Texte d'un PDF né numérique ; `None` s'il n'en a pas assez (PDF scanné : passer à l'OCR).
pub async fn text(pdf: &[u8], pages: usize) -> Result<Option<String>, IndexError> {
    let last = pages.max(1).to_string();
    let raw = tool("pdftotext", &["-l", &last, "-enc", "UTF-8", "-", "-"], pdf, MAX_TEXT).await?;
    let t = String::from_utf8_lossy(&raw).replace('\u{c}', "\n");
    let solid = t.chars().filter(|c| !c.is_whitespace()).count();
    Ok((solid >= MIN_TEXT_PER_PAGE * pages.max(1)).then(|| t.trim().to_owned()))
}

/// Image PNG de la page `n` (à partir de 1).
pub async fn page_png(pdf: &[u8], n: usize) -> Result<Vec<u8>, IndexError> {
    let n = n.to_string();
    tool("pdftoppm", &["-f", &n, "-l", &n, "-scale-to", PAGE_SIDE, "-png", "-singlefile", "-"], pdf, MAX_PAGE_BYTES).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_scanned_pdf_has_no_text_layer_but_renders_to_png() {
        if !available() {
            return; // poppler absent : le module dit « indisponible », vérifié ailleurs
        }
        let pdf = include_bytes!("../tests/fixtures/ocr_scan.pdf");
        assert_eq!(pages(pdf).await.unwrap(), 1);
        assert_eq!(text(pdf, 1).await.unwrap(), None, "scanné : pas de couche texte");
        let png = page_png(pdf, 1).await.unwrap();
        assert_eq!(&png[..4], b"\x89PNG");
        assert!(matches!(pages(b"pas un pdf").await, Err(IndexError::Invalid(_))));
    }
}
