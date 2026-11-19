//! Images clés d'une vidéo par `ffmpeg` (lot 3) : quelques images réparties sur la durée, en PNG, regardées ensuite comme des
//! photos (`Task::Vision`). La vidéo n'est **jamais** écrite en clair sur le disque : ffmpeg la lit à une adresse HTTP en boucle
//! locale que l'appelant sert en déchiffrant à la volée, avec accès aléatoire (un MP4 a souvent son index à la fin).
//!
//! Garde-fous : le format est **imposé** d'après le type MIME (`-f mov` ou `-f matroska`), donc une « vidéo » qui serait en fait
//! une liste de lecture (HLS, concat) ne peut pas faire lire autre chose à ffmpeg ; protocoles limités à `http,tcp` ; adresse
//! `127.0.0.1` seulement ; délai borné, processus tué s'il est abandonné ; sortie bornée.

use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::process::Command;

use crate::IndexError;

/// Images au plus par vidéo.
pub const MAX_FRAMES: usize = 12;
/// Écart minimal entre deux images (une vidéo courte n'en donne que quelques-unes).
pub const MIN_GAP_MS: u64 = 3_000;
/// Durée maximale d'une extraction.
const TIMEOUT: Duration = Duration::from_secs(180);
/// Sortie maximale de ffmpeg (12 PNG de 448 px de large).
const MAX_OUT: u64 = 48 << 20;

/// ffmpeg et ffprobe sont-ils là ?
pub fn available() -> bool {
    let Some(path) = std::env::var_os("PATH") else { return false };
    let has = |b: &str| std::env::split_paths(&path).any(|d| d.join(b).is_file());
    has("ffmpeg") && has("ffprobe")
}

/// Format imposé à ffmpeg pour ce type MIME ; `None` : vidéo non prise en charge.
pub fn demuxer(mime: &str) -> Option<&'static str> {
    match mime.split(';').next().unwrap_or("").trim().to_ascii_lowercase().as_str() {
        "video/mp4" | "video/quicktime" | "video/x-m4v" | "video/3gpp" => Some("mov"),
        "video/webm" | "video/x-matroska" => Some("matroska"),
        _ => None,
    }
}

/// Écart entre deux images pour une vidéo de `secs` secondes : la durée répartie en [`MAX_FRAMES`], au moins [`MIN_GAP_MS`] ;
/// 10 s si la durée est inconnue.
pub fn gap_ms(secs: Option<f64>) -> u64 {
    match secs.filter(|s| s.is_finite() && *s > 0.0) {
        Some(s) => ((s * 1000.0 / MAX_FRAMES as f64) as u64).max(MIN_GAP_MS),
        None => 10_000,
    }
}

/// Découpe une suite de PNG collés (sortie `image2pipe`) : chacun finit par son bloc `IEND` et son CRC.
pub fn split_png(mut bytes: &[u8]) -> Vec<Vec<u8>> {
    const END: &[u8] = b"IEND\xAE\x42\x60\x82";
    let mut out = Vec::new();
    while let Some(at) = bytes.windows(END.len()).position(|w| w == END) {
        let (png, rest) = bytes.split_at(at + END.len());
        if png.starts_with(b"\x89PNG") {
            out.push(png.to_vec());
        }
        bytes = rest;
    }
    out
}

fn check_url(url: &str) -> Result<(), IndexError> {
    if url.starts_with("http://127.0.0.1:") { Ok(()) } else { Err(IndexError::Invalid("adresse de lecture hors de la station".into())) }
}

async fn run(cmd: &mut Command) -> Result<Vec<u8>, IndexError> {
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true).spawn().map_err(|e| IndexError::Unavailable(format!("ffmpeg : {e}")))?;
    let mut out = Vec::new();
    let stdout = child.stdout.take().ok_or_else(|| IndexError::Backend("ffmpeg : sortie absente".into()))?;
    let read = async {
        stdout.take(MAX_OUT).read_to_end(&mut out).await.map_err(|e| IndexError::Backend(format!("ffmpeg : {e}")))?;
        child.wait().await.map_err(|e| IndexError::Backend(format!("ffmpeg : {e}")))
    };
    let status = tokio::time::timeout(TIMEOUT, read).await.map_err(|_| IndexError::Backend("vidéo : délai dépassé".into()))??;
    if !status.success() && out.is_empty() {
        return Err(IndexError::Invalid("vidéo illisible".into()));
    }
    Ok(out)
}

/// Durée de la vidéo (secondes), si ffprobe la trouve.
pub async fn duration(url: &str, mime: &str) -> Result<Option<f64>, IndexError> {
    check_url(url)?;
    let f = demuxer(mime).ok_or_else(|| IndexError::Invalid(format!("vidéo « {mime} » non prise en charge")))?;
    let out = run(Command::new("ffprobe").args(["-v", "error", "-protocol_whitelist", "http,tcp", "-f", f, "-show_entries", "format=duration", "-of", "csv=p=0", url])).await?;
    Ok(String::from_utf8_lossy(&out).trim().parse::<f64>().ok())
}

/// Images clés : `(instant en ms, PNG)`, au plus [`MAX_FRAMES`], réparties sur la durée.
pub async fn frames(url: &str, mime: &str) -> Result<Vec<(i64, Vec<u8>)>, IndexError> {
    let gap = gap_ms(duration(url, mime).await?);
    let f = demuxer(mime).ok_or_else(|| IndexError::Invalid(format!("vidéo « {mime} » non prise en charge")))?;
    let filter = format!("fps=1000/{gap},scale=448:-2");
    let max = MAX_FRAMES.to_string();
    let out = run(Command::new("ffmpeg").args([
        "-nostdin", "-v", "error", "-protocol_whitelist", "http,tcp", "-f", f, "-i", url, "-vf", &filter, "-frames:v", &max, "-f", "image2pipe", "-c:v", "png", "-",
    ]))
    .await?;
    let gap = i64::try_from(gap).unwrap_or(i64::MAX);
    Ok(split_png(&out).into_iter().take(MAX_FRAMES).enumerate().map(|(i, png)| (i64::try_from(i).unwrap_or(0).saturating_mul(gap), png)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_format_is_imposed_from_the_type_never_guessed() {
        assert_eq!((demuxer("video/mp4"), demuxer("video/QuickTime"), demuxer("video/webm; codecs=vp9")), (Some("mov"), Some("mov"), Some("matroska")));
        assert_eq!((demuxer("application/vnd.apple.mpegurl"), demuxer("video/x-msvideo"), demuxer("")), (None, None, None));
    }

    #[test]
    fn frames_are_spread_over_the_length_but_not_too_close() {
        assert_eq!(gap_ms(Some(120.0)), 10_000);
        assert_eq!(gap_ms(Some(6.0)), MIN_GAP_MS, "vidéo courte : quelques images seulement");
        assert_eq!((gap_ms(None), gap_ms(Some(f64::NAN)), gap_ms(Some(-1.0))), (10_000, 10_000, 10_000));
    }

    #[test]
    fn glued_pngs_are_split_and_junk_is_dropped() {
        let png = |x: u8| [&b"\x89PNG\r\n\x1a\n"[..], &[x; 5], b"IEND\xAE\x42\x60\x82"].concat();
        let glued = [png(1), b"bruit IEND\xAE\x42\x60\x82".to_vec(), png(2), b"reste".to_vec()].concat();
        assert_eq!(split_png(&glued), [png(1), png(2)]);
    }

    #[tokio::test]
    async fn only_a_loopback_address_is_ever_read() {
        assert!(matches!(frames("file:///etc/passwd", "video/mp4").await, Err(IndexError::Invalid(_))));
        assert!(matches!(frames("http://192.168.1.1/v.mp4", "video/mp4").await, Err(IndexError::Invalid(_))));
    }
}
