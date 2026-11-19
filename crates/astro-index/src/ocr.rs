//! OCR PP-OCRv6 : détection DB (carte de probabilité → boîtes) puis reconnaissance CTC (une ligne → texte).
//!
//! Les calculs (taille d'entrée, normalisation, boîtes, décodage, lignes) sont purs et testés sans ONNX Runtime ; le moteur
//! ([`OcrEngine`], fonction `onnx`) ne fait qu'enchaîner les deux modèles. Réglages : ceux des `inference.yml` publiés
//! (image BGR, moyenne/écart ImageNet pour la détection, `thresh` 0,2, `box_thresh` 0,45, `unclip_ratio` 1,4 ; hauteur 48 et
//! `(x/255 − 0,5)/0,5` pour la reconnaissance). Les lignes sont supposées à peu près horizontales (documents, panneaux).

use image::RgbImage;
use image::imageops::{FilterType, resize};

/// Côté le plus long donné à la détection (les petits caractères d'un A4 à 150 dpi restent lisibles).
pub const DET_MAX_SIDE: u32 = 1536;
/// Hauteur d'une ligne pour la reconnaissance.
pub const REC_HEIGHT: u32 = 48;
/// Largeur d'une ligne au plus.
pub const REC_MAX_WIDTH: u32 = 3200;
/// Lignes reconnues au plus par image.
pub const MAX_LINES: usize = 400;
const THRESH: f32 = 0.2;
const BOX_THRESH: f32 = 0.45;
const UNCLIP: f32 = 1.4;
/// Confiance minimale d'une ligne reconnue (en dessous : bruit, hachures, texture).
pub const MIN_LINE_SCORE: f32 = 0.5;

/// Dictionnaire de caractères d'`ocr-rec.yml` (`PostProcess.character_dict`), sans dépendance YAML : une entrée par ligne
/// `  - c`, éventuellement entre apostrophes (`''` = une apostrophe).
pub fn parse_dict(yml: &str) -> Vec<String> {
    let Some((_, rest)) = yml.split_once("character_dict:\n") else { return Vec::new() };
    rest.lines()
        .map_while(|l| l.strip_prefix("  - "))
        .map(|v| match v.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')) {
            Some(inner) => inner.replace("''", "'"),
            None => v.to_owned(),
        })
        .collect()
}

/// Taille d'entrée de la détection : côté long ≤ [`DET_MAX_SIDE`], multiples de 32, au moins 32.
pub fn det_size(w: u32, h: u32) -> (u32, u32) {
    let r = (f64::from(DET_MAX_SIDE) / f64::from(w.max(h).max(1))).min(1.0);
    let side = |v: u32| ((f64::from(v) * r / 32.0).round() as u32).max(1) * 32;
    (side(w), side(h))
}

/// Entrée NCHW de la détection (BGR, normalisation ImageNet).
pub fn det_input(img: &RgbImage) -> (Vec<f32>, u32, u32) {
    let (w, h) = det_size(img.width(), img.height());
    let small = resize(img, w, h, FilterType::Triangle);
    const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
    const STD: [f32; 3] = [0.229, 0.224, 0.225];
    let plane = (w * h) as usize;
    let mut out = vec![0f32; 3 * plane];
    for (i, p) in small.pixels().enumerate() {
        for c in 0..3 {
            out[c * plane + i] = (f32::from(p[2 - c]) / 255.0 - MEAN[c]) / STD[c];
        }
    }
    (out, w, h)
}

/// Boîte de texte dans le repère de l'image d'origine.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextBox {
    /// Gauche.
    pub x0: f32,
    /// Haut.
    pub y0: f32,
    /// Droite.
    pub x1: f32,
    /// Bas.
    pub y1: f32,
}

/// Boîtes de la carte de probabilité `prob` (`pw × ph`) ramenées à l'image `w × h` : composantes connexes au-dessus du seuil,
/// gardées si leur probabilité moyenne passe `box_thresh`, élargies (« unclip ») comme le fait PaddleOCR.
pub fn boxes(prob: &[f32], pw: usize, ph: usize, w: u32, h: u32) -> Vec<TextBox> {
    let mut seen = vec![false; pw * ph];
    let mut stack = Vec::new();
    let mut out = Vec::new();
    let (sx, sy) = (w as f32 / pw as f32, h as f32 / ph as f32);
    for start in 0..pw * ph {
        if seen[start] || prob.get(start).is_none_or(|p| *p <= THRESH) {
            continue;
        }
        seen[start] = true;
        stack.push(start);
        let (mut x0, mut y0, mut x1, mut y1, mut sum, mut n) = (usize::MAX, usize::MAX, 0, 0, 0f32, 0usize);
        while let Some(i) = stack.pop() {
            let (x, y) = (i % pw, i / pw);
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1));
            sum += prob[i];
            n += 1;
            let mut push = |j: usize| {
                if !seen[j] && prob[j] > THRESH {
                    seen[j] = true;
                    stack.push(j);
                }
            };
            if x > 0 {
                push(i - 1);
            }
            if x + 1 < pw {
                push(i + 1);
            }
            if y > 0 {
                push(i - pw);
            }
            if y + 1 < ph {
                push(i + pw);
            }
        }
        let (bw, bh) = ((x1 - x0) as f32, (y1 - y0) as f32);
        if bw.min(bh) < 3.0 || sum / (n as f32) < BOX_THRESH {
            continue;
        }
        let d = bw * bh * UNCLIP / (2.0 * (bw + bh));
        out.push(TextBox {
            x0: ((x0 as f32 - d) * sx).max(0.0),
            y0: ((y0 as f32 - d) * sy).max(0.0),
            x1: ((x1 as f32 + d) * sx).min(w as f32),
            y1: ((y1 as f32 + d) * sy).min(h as f32),
        });
        if out.len() >= MAX_LINES {
            break;
        }
    }
    out
}

/// Entrée NCHW de la reconnaissance pour la zone `b` : hauteur 48, largeur proportionnelle, BGR, `(x/255 − 0,5)/0,5`.
/// `None` pour une zone vide.
pub fn rec_input(img: &RgbImage, b: &TextBox) -> Option<(Vec<f32>, u32)> {
    let (x0, y0) = (b.x0.floor() as u32, b.y0.floor() as u32);
    let (x1, y1) = ((b.x1.ceil() as u32).min(img.width()), (b.y1.ceil() as u32).min(img.height()));
    if x1 <= x0 + 1 || y1 <= y0 + 1 {
        return None;
    }
    let crop = image::imageops::crop_imm(img, x0, y0, x1 - x0, y1 - y0).to_image();
    let w = ((f64::from(REC_HEIGHT) * f64::from(crop.width()) / f64::from(crop.height())).round() as u32).clamp(16, REC_MAX_WIDTH);
    let line = resize(&crop, w, REC_HEIGHT, FilterType::Triangle);
    let plane = (w * REC_HEIGHT) as usize;
    let mut out = vec![0f32; 3 * plane];
    for (i, p) in line.pixels().enumerate() {
        for c in 0..3 {
            out[c * plane + i] = (f32::from(p[2 - c]) / 255.0 - 0.5) / 0.5;
        }
    }
    Some((out, w))
}

/// Décodage CTC glouton de `probs` (`steps × classes`, déjà normalisé) : classe 0 = vide, puis le dictionnaire, puis l'espace.
/// Rend le texte et sa confiance moyenne.
pub fn ctc(probs: &[f32], classes: usize, chars: &[String]) -> (String, f32) {
    let (mut text, mut score, mut n, mut last) = (String::new(), 0f32, 0usize, 0usize);
    for step in probs.chunks_exact(classes.max(1)) {
        let (k, p) = step.iter().enumerate().fold((0, f32::MIN), |m, (i, &v)| if v > m.1 { (i, v) } else { m });
        if k != 0 && k != last {
            match k {
                k if k <= chars.len() => text.push_str(&chars[k - 1]),
                _ => text.push(' '),
            }
            score += p;
            n += 1;
        }
        last = k;
    }
    (text.trim().to_owned(), if n == 0 { 0.0 } else { score / n as f32 })
}

/// Un morceau de texte reconnu et sa place.
type Piece = (TextBox, String);

/// Range les morceaux reconnus en lignes de lecture : même ligne si leurs hauteurs se recouvrent de moitié, de gauche à droite.
pub fn lines(mut parts: Vec<Piece>) -> String {
    parts.sort_by(|a, b| (a.0.y0 + a.0.y1).total_cmp(&(b.0.y0 + b.0.y1)));
    let mut rows: Vec<(f32, f32, Vec<Piece>)> = Vec::new();
    for (b, t) in parts {
        match rows.last_mut() {
            Some((top, bottom, row)) if (bottom.min(b.y1) - top.max(b.y0)) >= 0.5 * (b.y1 - b.y0).min(*bottom - *top) => {
                (*top, *bottom) = (top.min(b.y0), bottom.max(b.y1));
                row.push((b, t));
            }
            _ => rows.push((b.y0, b.y1, vec![(b, t)])),
        }
    }
    rows.into_iter()
        .map(|(_, _, mut row)| {
            row.sort_by(|a, b| a.0.x0.total_cmp(&b.0.x0));
            row.into_iter().map(|(_, t)| t).collect::<Vec<_>>().join(" ")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(feature = "onnx")]
pub use engine::OcrEngine;

#[cfg(feature = "onnx")]
mod engine {
    use std::path::Path;
    use std::sync::Mutex;

    use image::RgbImage;
    use ort::session::Session;
    use ort::session::builder::GraphOptimizationLevel;
    use ort::value::Tensor;

    use super::{MIN_LINE_SCORE, REC_HEIGHT, boxes, ctc, det_input, lines, parse_dict, rec_input};
    use crate::IndexError;

    fn backend(e: impl std::fmt::Display) -> IndexError {
        IndexError::Backend(e.to_string())
    }

    /// Les deux modèles PP-OCRv6 et le dictionnaire, chargés une fois. Bloquant : appeler depuis `spawn_blocking`.
    pub struct OcrEngine {
        det: Mutex<Session>,
        rec: Mutex<Session>,
        chars: Vec<String>,
    }

    impl OcrEngine {
        /// Charge `ocr-det.onnx`, `ocr-rec.onnx` et le dictionnaire d'`ocr-rec.yml` depuis `dir`.
        pub fn load(dir: &Path, threads: usize) -> Result<Self, IndexError> {
            let open = |f: &str| -> Result<Session, IndexError> {
                let mut b = Session::builder().map_err(backend)?.with_optimization_level(GraphOptimizationLevel::Level3).map_err(backend)?;
                if threads > 0 {
                    b = b.with_intra_threads(threads).map_err(backend)?;
                }
                b.commit_from_file(dir.join(f)).map_err(|e| IndexError::Backend(format!("{f} : {e}")))
            };
            let yml = std::fs::read_to_string(dir.join("ocr-rec.yml")).map_err(|e| IndexError::Backend(format!("ocr-rec.yml : {e}")))?;
            let chars = parse_dict(&yml);
            if chars.len() < 100 {
                return Err(IndexError::Backend("ocr-rec.yml : dictionnaire illisible".into()));
            }
            Ok(Self { det: Mutex::new(open("ocr-det.onnx")?), rec: Mutex::new(open("ocr-rec.onnx")?), chars })
        }

        fn run(session: &Mutex<Session>, input: Vec<f32>, shape: [usize; 4]) -> Result<(Vec<usize>, Vec<f32>), IndexError> {
            let mut s = session.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let out = s.outputs.first().map(|o| o.name.clone()).ok_or_else(|| backend("modèle sans sortie"))?;
            let outputs = s.run(ort::inputs!["x" => Tensor::from_array((shape, input)).map_err(backend)?]).map_err(backend)?;
            let (dims, data) = outputs[out.as_str()].try_extract_tensor::<f32>().map_err(backend)?;
            Ok((dims.iter().map(|d| usize::try_from(*d).unwrap_or(0)).collect(), data.to_vec()))
        }

        /// Texte de l'image, ligne par ligne (vide s'il n'y en a pas).
        pub fn read(&self, img: &RgbImage) -> Result<String, IndexError> {
            let (input, pw, ph) = det_input(img);
            let (dims, prob) = Self::run(&self.det, input, [1, 3, ph as usize, pw as usize])?;
            if dims.len() != 4 || dims[2] * dims[3] != prob.len() {
                return Err(backend(format!("détection : sortie {dims:?} inattendue")));
            }
            let mut parts = Vec::new();
            for b in boxes(&prob, dims[3], dims[2], img.width(), img.height()) {
                let Some((input, w)) = rec_input(img, &b) else { continue };
                let (dims, probs) = Self::run(&self.rec, input, [1, 3, REC_HEIGHT as usize, w as usize])?;
                let classes = dims.last().copied().unwrap_or(0);
                if classes != self.chars.len() + 2 {
                    return Err(backend(format!("reconnaissance : {classes} classes pour un dictionnaire de {}", self.chars.len())));
                }
                let (text, score) = ctc(&probs, classes, &self.chars);
                if score >= MIN_LINE_SCORE && !text.is_empty() {
                    parts.push((b, text));
                }
            }
            Ok(lines(parts))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dictionary_reads_quoted_and_plain_entries() {
        let yml = "PostProcess:\n  name: CTCLabelDecode\n  character_dict:\n  - '!'\n  - ''''\n  - a\n  - é\n  - €\nPreProcess:\n  - x\n";
        assert_eq!(parse_dict(yml), ["!", "'", "a", "é", "€"]);
        assert!(parse_dict("rien").is_empty());
    }

    #[test]
    fn detection_size_is_bounded_and_aligned() {
        assert_eq!(det_size(1100, 620), (1088, 608));
        let (w, h) = det_size(4000, 3000);
        assert!(w <= DET_MAX_SIDE + 16 && w % 32 == 0 && h % 32 == 0);
        assert_eq!(det_size(5, 5), (32, 32));
    }

    #[test]
    fn components_become_boxes_in_the_original_frame() {
        // Carte 20 × 10 : un trait de texte (x 2..12, y 3..6) et un point isolé, trop petit.
        let (pw, ph) = (20, 10);
        let mut p = vec![0f32; pw * ph];
        for y in 3..6 {
            for x in 2..12 {
                p[y * pw + x] = 0.9;
            }
        }
        p[9 * pw + 19] = 0.9;
        let b = boxes(&p, pw, ph, 200, 100);
        assert_eq!(b.len(), 1);
        assert!(b[0].x0 < 20.0 && b[0].x1 > 120.0 && b[0].y0 < 30.0 && b[0].y1 > 60.0, "{:?}", b[0]);
        assert!(boxes(&vec![0.3; pw * ph], pw, ph, 200, 100).is_empty(), "probabilité moyenne sous box_thresh");
    }

    #[test]
    fn ctc_merges_repeats_and_skips_blanks() {
        let chars: Vec<String> = ["a", "b"].map(String::from).to_vec();
        // classes : 0 vide, 1 a, 2 b, 3 espace ; suite a a vide a espace b → « aa b ».
        let step = |k: usize| (0..4).map(|i| if i == k { 0.9 } else { 0.03 }).collect::<Vec<f32>>();
        let probs: Vec<f32> = [1, 1, 0, 1, 3, 2].iter().flat_map(|k| step(*k)).collect();
        let (t, s) = ctc(&probs, 4, &chars);
        assert_eq!(t, "aa b");
        assert!((s - 0.9).abs() < 1e-5);
    }

    #[test]
    fn pieces_are_read_line_by_line_left_to_right() {
        let b = |x0, y0, x1, y1| TextBox { x0, y0, x1, y1 };
        let parts = vec![(b(300.0, 12.0, 400.0, 40.0), "2026".into()), (b(10.0, 60.0, 200.0, 90.0), "Montant".into()), (b(10.0, 10.0, 280.0, 42.0), "Facture".into())];
        assert_eq!(lines(parts), "Facture 2026\nMontant");
    }
}
