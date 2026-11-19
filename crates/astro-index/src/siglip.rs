//! SigLIP 2 (base, patch 16, 224) : une image et une phrase deviennent des vecteurs du **même** espace (768 dimensions) ;
//! chercher « des lumières de Noël », c'est chercher les photos dont le vecteur est le plus proche de celui de la phrase.
//!
//! Réglages de `siglip2-preprocessor.json` : image RVB redimensionnée en 224 × 224 (bilinéaire), `(x/255 − 0,5)/0,5`.
//! Texte : en minuscules (le modèle a appris ainsi), dans le gabarit [`TEMPLATE`], 64 jetons (le tokeniseur complète lui-même).
//! Le gabarit anglais l'emporte, même pour une requête en français : 6 bonnes réponses sur 7 contre 3 ou 4 pour les gabarits
//! français, sur le banc d'essai du lot 2. Les sorties `pooler_output` sont normalisées (cosinus = produit scalaire).
//!
//! **Chercher** demande deux corrections, mesurées sur un banc de 41 images (photos, fonds d'écran, une facture) et 22 requêtes en
//! français (15 avec une bonne réponse, 7 sans) :
//! - certaines images « attirent » toutes les requêtes (une page de texte surtout) : on retire de la requête la moyenne des
//!   phrases neutres [`NEUTRAL`] ([`debiased`]), ce qui revient à retirer à chaque image son attirance moyenne ;
//! - la proximité seule rend toujours « les plus proches », même sans rapport : on ne garde que les photos qui **sortent du lot**
//!   des photos du membre ([`stand_out`], écart d'au moins [`MIN_Z`] écarts-types).
//!
//! Ensemble : 12 bonnes réponses sur 15 et aucun intrus (le score brut au même seuil : 7 sur 15).

use image::RgbImage;
use image::imageops::{FilterType, resize};

/// Côté de l'image d'entrée.
pub const SIDE: u32 = 224;
/// Dimension des vecteurs.
pub const DIM: usize = 768;
/// Jetons d'une phrase.
pub const MAX_TOKENS: usize = 64;
/// Gabarit d'une requête.
pub const TEMPLATE: &str = "this is a photo of {}.";
/// Phrases neutres : leur moyenne est retirée de chaque requête ([`debiased`]).
pub const NEUTRAL: [&str; 16] = [
    "quelque chose", "une image", "une photo", "un objet", "un endroit", "des gens", "une scène", "du texte",
    "une couleur", "un dessin", "un paysage", "un animal", "de la nourriture", "un intérieur", "un extérieur", "un document",
];
/// Écart minimal (en écarts-types des photos du membre) pour qu'une photo soit trouvée.
pub const MIN_Z: f32 = 3.5;
/// Photos au moins pour juger ce qui sort du lot (en deçà, la recherche par l'image ne rend rien) : parmi N scores, aucun ne
/// peut dépasser √(N − 1) écarts-types, et [`MIN_Z`] = 3,5 demande donc au moins 14 photos.
pub const MIN_PHOTOS: usize = 14;

/// Entrée NCHW de la vision.
pub fn image_input(img: &RgbImage) -> Vec<f32> {
    let small = resize(img, SIDE, SIDE, FilterType::Triangle);
    let plane = (SIDE * SIDE) as usize;
    let mut out = vec![0f32; 3 * plane];
    for (i, p) in small.pixels().enumerate() {
        for c in 0..3 {
            out[c * plane + i] = (f32::from(p[c]) / 255.0 - 0.5) / 0.5;
        }
    }
    out
}

/// Phrase donnée au modèle pour la requête `q` (espaces resserrés, 200 caractères au plus).
pub fn query_text(q: &str) -> String {
    let q: String = q.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(200).collect();
    TEMPLATE.replace("{}", &q.to_lowercase())
}

/// Normalise `v` (norme 1) ; `None` pour un vecteur nul ou non fini.
pub fn normalized(mut v: Vec<f32>) -> Option<Vec<f32>> {
    let n = v.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>().sqrt();
    if !n.is_finite() || n < 1e-12 || v.len() != DIM {
        return None;
    }
    v.iter_mut().for_each(|x| *x = (f64::from(*x) / n) as f32);
    Some(v)
}

/// Requête corrigée : `query` moins la moyenne des vecteurs neutres, normalisée.
pub fn debiased(query: &[f32], neutral: &[Vec<f32>]) -> Option<Vec<f32>> {
    if neutral.is_empty() {
        return normalized(query.to_vec());
    }
    let n = neutral.len() as f32;
    normalized(query.iter().enumerate().map(|(i, q)| q - neutral.iter().map(|v| v.get(i).copied().unwrap_or(0.0)).sum::<f32>() / n).collect())
}

/// Indices des scores qui sortent du lot (au moins [`MIN_Z`] écarts-types au-dessus de la moyenne), du meilleur au moins bon.
/// Rien sous [`MIN_PHOTOS`] scores, ni si tous se valent.
pub fn stand_out(scores: &[f32]) -> Vec<usize> {
    if scores.len() < MIN_PHOTOS {
        return Vec::new();
    }
    let n = scores.len() as f64;
    let mean = scores.iter().map(|&s| f64::from(s)).sum::<f64>() / n;
    let sd = (scores.iter().map(|&s| (f64::from(s) - mean).powi(2)).sum::<f64>() / n).sqrt();
    if !sd.is_finite() || sd < 1e-9 {
        return Vec::new();
    }
    let mut out: Vec<(usize, f64)> = scores.iter().enumerate().map(|(i, &s)| (i, (f64::from(s) - mean) / sd)).filter(|(_, z)| *z >= f64::from(MIN_Z)).collect();
    out.sort_by(|a, b| b.1.total_cmp(&a.1));
    out.into_iter().map(|(i, _)| i).collect()
}

#[cfg(feature = "onnx")]
pub use engine::SiglipEngine;

#[cfg(feature = "onnx")]
mod engine {
    use std::path::Path;
    use std::sync::Mutex;

    use image::RgbImage;
    use ort::session::Session;
    use ort::session::builder::GraphOptimizationLevel;
    use ort::value::Tensor;
    use tokenizers::{Tokenizer, TruncationParams};

    use super::{MAX_TOKENS, NEUTRAL, SIDE, debiased, image_input, normalized, query_text};
    use crate::IndexError;

    fn backend(e: impl std::fmt::Display) -> IndexError {
        IndexError::Backend(e.to_string())
    }

    /// Les deux tours de SigLIP 2 et le tokeniseur. Bloquant : appeler depuis `spawn_blocking`.
    pub struct SiglipEngine {
        vision: Mutex<Session>,
        text: Mutex<Session>,
        tok: Tokenizer,
        /// Vecteurs des phrases neutres, calculés à la première recherche (une seconde, une fois).
        neutral: std::sync::OnceLock<Vec<Vec<f32>>>,
    }

    impl SiglipEngine {
        /// Charge `siglip2-vision-int8.onnx`, `siglip2-text-int8.onnx` et `siglip2-tokenizer.json` depuis `dir`.
        pub fn load(dir: &Path, threads: usize) -> Result<Self, IndexError> {
            let open = |f: &str| -> Result<Session, IndexError> {
                let mut b = Session::builder().map_err(backend)?.with_optimization_level(GraphOptimizationLevel::Level3).map_err(backend)?;
                if threads > 0 {
                    b = b.with_intra_threads(threads).map_err(backend)?;
                }
                b.commit_from_file(dir.join(f)).map_err(|e| IndexError::Backend(format!("{f} : {e}")))
            };
            let mut tok = Tokenizer::from_file(dir.join("siglip2-tokenizer.json")).map_err(|e| IndexError::Backend(format!("siglip2-tokenizer.json : {e}")))?;
            tok.with_truncation(Some(TruncationParams { max_length: MAX_TOKENS, ..TruncationParams::default() })).map_err(backend)?;
            Ok(Self { vision: Mutex::new(open("siglip2-vision-int8.onnx")?), text: Mutex::new(open("siglip2-text-int8.onnx")?), tok, neutral: std::sync::OnceLock::new() })
        }

        fn pooled(outputs: &ort::session::SessionOutputs<'_>) -> Result<Vec<f32>, IndexError> {
            let (_, data) = outputs["pooler_output"].try_extract_tensor::<f32>().map_err(backend)?;
            normalized(data.to_vec()).ok_or_else(|| backend("vecteur nul ou de mauvaise dimension"))
        }

        /// Vecteur d'une image.
        pub fn image(&self, img: &RgbImage) -> Result<Vec<f32>, IndexError> {
            let side = SIDE as usize;
            let input = Tensor::from_array(([1usize, 3, side, side], image_input(img))).map_err(backend)?;
            let mut s = self.vision.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            Self::pooled(&s.run(ort::inputs!["pixel_values" => input]).map_err(backend)?)
        }

        /// Vecteur d'une requête (dans le gabarit, en minuscules), corrigé de l'attirance des images ([`debiased`]).
        pub fn text(&self, q: &str) -> Result<Vec<f32>, IndexError> {
            if self.neutral.get().is_none() {
                let v = NEUTRAL.iter().map(|n| self.sentence(&query_text(n))).collect::<Result<Vec<_>, _>>()?;
                let _ = self.neutral.set(v);
            }
            let neutral = self.neutral.get().map_or(&[][..], Vec::as_slice);
            debiased(&self.sentence(&query_text(q))?, neutral).ok_or_else(|| backend("requête sans vecteur"))
        }

        fn sentence(&self, s: &str) -> Result<Vec<f32>, IndexError> {
            let enc = self.tok.encode(s, true).map_err(backend)?;
            let ids: Vec<i64> = enc.get_ids().iter().map(|&i| i64::from(i)).collect();
            let input = Tensor::from_array(([1usize, ids.len()], ids)).map_err(backend)?;
            let mut s = self.text.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            Self::pooled(&s.run(ort::inputs!["input_ids" => input]).map_err(backend)?)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queries_are_lowercased_and_wrapped() {
        assert_eq!(query_text("  Lumières   de NOËL "), "this is a photo of lumières de noël.");
        assert!(query_text(&"x".repeat(500)).len() < 240);
    }

    #[test]
    fn images_become_224_square_rgb_planes_in_minus_one_one() {
        let img = RgbImage::from_fn(40, 20, |x, _| if x < 20 { image::Rgb([255, 0, 0]) } else { image::Rgb([0, 0, 255]) });
        let v = image_input(&img);
        let plane = (SIDE * SIDE) as usize;
        assert_eq!(v.len(), 3 * plane);
        assert!((v[0] - 1.0).abs() < 1e-3 && (v[plane] + 1.0).abs() < 1e-3, "rouge à gauche : R = 1, G = −1");
        assert!(v.iter().all(|x| (-1.0..=1.0).contains(x)));
    }

    #[test]
    fn the_pull_of_neutral_sentences_is_removed_from_a_query() {
        let unit = |i: usize| (0..DIM).map(|j| if j == i { 1.0 } else { 0.0 }).collect::<Vec<f32>>();
        let mut q = unit(0);
        q[1] = 1.0;
        let d = debiased(&q, &[unit(1), unit(1)]).unwrap();
        assert!((d[0] - 1.0).abs() < 1e-5 && d[1].abs() < 1e-5, "la direction neutre disparaît");
        assert_eq!(debiased(&unit(3), &[]).unwrap(), unit(3));
    }

    #[test]
    fn only_photos_that_clearly_stand_out_are_found_best_first() {
        let mut s = vec![0.05_f32; 100];
        s.iter_mut().enumerate().for_each(|(i, x)| *x += (i % 5) as f32 * 0.002);
        s[7] = 0.20;
        s[30] = 0.17;
        assert_eq!(stand_out(&s), [7, 30]);
        assert!(stand_out(&[0.05; 40]).is_empty(), "tout se vaut : rien");
        assert!(stand_out(&s[..MIN_PHOTOS - 1]).is_empty(), "trop peu de photos pour juger");
    }

    #[test]
    fn vectors_are_unit_or_refused() {
        let v = normalized(vec![2.0; DIM]).unwrap();
        assert!((v.iter().map(|x| x * x).sum::<f32>() - 1.0).abs() < 1e-4);
        assert!(normalized(vec![0.0; DIM]).is_none() && normalized(vec![1.0; 3]).is_none());
    }
}
