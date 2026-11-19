//! Indexeur local : ONNX Runtime dans le processus. Un moteur n'est chargé que si ses modèles sont installés ; chaque lot
//! en ajoute un (OCR au lot 1, SigLIP au lot 2, Whisper au lot 4) et allonge [`LocalIndexer::tasks`].

use std::path::{Path, PathBuf};
#[cfg(feature = "onnx")]
use std::sync::Arc;

use crate::catalog::{CATALOG, Model, State};
use crate::{IndexError, Indexer, Input, Output, Task};

/// Indexeur local.
#[derive(Clone)]
pub struct LocalIndexer {
    dir: PathBuf,
    #[cfg(feature = "onnx")]
    ocr: Option<Arc<crate::ocr::OcrEngine>>,
    #[cfg(feature = "onnx")]
    siglip: Option<Arc<crate::siglip::SiglipEngine>>,
}

/// Décode une image pour un moteur qui n'a besoin que de `need` pixels sur le côté que `side` choisit (petit ou grand) : un
/// JPEG est décodé **directement réduit** (IDCT au 1/2, 1/4 ou 1/8, `astro_dav::jpeg_scale`), sans passer par la pleine
/// résolution. Mesuré sur une photo de 5 Mo (5K) : la majeure partie du temps d'analyse partait dans le décodage. Tout autre
/// format, ou un JPEG que ce décodeur refuse, passe par le décodeur général.
pub fn decode_for(bytes: &[u8], need: u32, side: fn(u32, u32) -> u32) -> Result<image::RgbImage, IndexError> {
    let max = astro_facecloud::imageio::MAX_SIDE;
    if let Ok(s) = astro_dav::jpeg_scale::decode_scaled(bytes, max, |w, h| astro_dav::jpeg_scale::divisor_for(side(w, h), need)) {
        return Ok(s.image.to_rgb8());
    }
    astro_facecloud::imageio::decode_oriented(bytes).map_err(|e| IndexError::Invalid(format!("image illisible : {e}")))
}

/// Côté utile pour regarder une photo (SigLIP la ramène à 224 × 224 : le petit côté compte), avec une marge de deux.
pub const VISION_NEED: u32 = 448;
/// Côté utile pour lire le texte (la détection travaille au plus sur 1536 px : le grand côté compte), avec de la marge.
pub const OCR_NEED: u32 = 2000;

impl LocalIndexer {
    /// Indexeur sans moteur (savoir ce qui est installé).
    pub fn new(dir: &Path) -> Self {
        Self {
            dir: dir.to_owned(),
            #[cfg(feature = "onnx")]
            ocr: None,
            #[cfg(feature = "onnx")]
            siglip: None,
        }
    }

    /// Charge les moteurs dont les modèles sont installés dans `dir`. `lib` : bibliothèque ONNX Runtime (sinon celle du système) ;
    /// `threads` : fils de calcul par modèle (0 = au choix d'ORT). Rend aussi ce qui n'a pas pu être chargé, pour le dire.
    /// `ort` panique sans `libonnxruntime` : la panique est rattrapée et rendue en message.
    #[cfg(feature = "onnx")]
    pub fn load(dir: &Path, lib: Option<&Path>, threads: usize) -> (Self, Vec<String>) {
        let mut me = Self::new(dir);
        let mut problems = Vec::new();
        let ready = |id: &str| Model::find(id).is_some_and(|m| m.state(dir) == State::Present);
        if ready("ppocrv6-small") {
            let loaded = std::panic::catch_unwind(|| {
                if let Some(lib) = lib {
                    ort::init_from(lib.to_string_lossy()).commit().map_err(|e| IndexError::Backend(format!("ONNX Runtime ({}) : {e}", lib.display())))?;
                }
                crate::ocr::OcrEngine::load(dir, threads)
            });
            match loaded {
                Ok(Ok(e)) => me.ocr = Some(Arc::new(e)),
                Ok(Err(e)) => problems.push(format!("OCR : {e}")),
                Err(_) => problems.push("OCR : ONNX Runtime introuvable ou illisible (ORT_DYLIB_PATH, ou pip install onnxruntime)".into()),
            }
        }
        if ready("siglip2-base") && problems.is_empty() {
            match std::panic::catch_unwind(|| crate::siglip::SiglipEngine::load(dir, threads)) {
                Ok(Ok(e)) => me.siglip = Some(Arc::new(e)),
                Ok(Err(e)) => problems.push(format!("photos : {e}")),
                Err(_) => problems.push("photos : ONNX Runtime introuvable ou illisible".into()),
            }
        }
        (me, problems)
    }

    /// Modèles complets dans le dossier.
    pub fn installed(&self) -> Vec<&'static Model> {
        CATALOG.iter().filter(|m| m.state(&self.dir) == State::Present).collect()
    }

    #[cfg(feature = "onnx")]
    async fn ocr(&self, engine: Arc<crate::ocr::OcrEngine>, input: Input) -> Result<String, IndexError> {
        let read = |engine: Arc<crate::ocr::OcrEngine>, bytes: Vec<u8>| async move {
            tokio::task::spawn_blocking(move || {
                let img = decode_for(&bytes, OCR_NEED, u32::max)?;
                engine.read(&img)
            })
            .await
            .map_err(|e| IndexError::Backend(format!("OCR interrompu : {e}")))?
        };
        if !input.mime.starts_with("application/pdf") {
            return read(engine, input.bytes.to_vec()).await;
        }
        let pages = crate::pdf::pages(&input.bytes).await?;
        if let Some(text) = crate::pdf::text(&input.bytes, pages).await? {
            return Ok(text);
        }
        let mut out = Vec::new();
        for n in 1..=pages {
            let png = crate::pdf::page_png(&input.bytes, n).await?;
            let text = read(engine.clone(), png).await?;
            if !text.is_empty() {
                out.push(text);
            }
        }
        Ok(out.join("\n\n"))
    }
}

impl Indexer for LocalIndexer {
    fn tasks(&self) -> Vec<Task> {
        #[allow(unused_mut)]
        let mut t = Vec::new();
        #[cfg(feature = "onnx")]
        if self.ocr.is_some() {
            t.push(Task::Ocr);
        }
        #[cfg(feature = "onnx")]
        if self.siglip.is_some() {
            t.extend([Task::Vision, Task::VisionQuery]);
        }
        t
    }

    async fn index(&self, task: Task, input: Input) -> Result<Output, IndexError> {
        input.check(task)?;
        #[cfg(feature = "onnx")]
        if let (Task::Ocr, Some(engine), Some(model)) = (task, self.ocr.clone(), Model::find("ppocrv6-small").and_then(Model::tag)) {
            let mut text = self.ocr(engine, input).await?;
            if text.len() > crate::MAX_TEXT {
                let mut cut = crate::MAX_TEXT;
                while !text.is_char_boundary(cut) {
                    cut -= 1;
                }
                text.truncate(cut);
            }
            return Ok(Output { model, text: Some(text), vector: None });
        }
        #[cfg(feature = "onnx")]
        if let (Task::Vision | Task::VisionQuery, Some(engine), Some(model)) = (task, self.siglip.clone(), Model::find("siglip2-base").and_then(Model::tag)) {
            let vector = tokio::task::spawn_blocking(move || {
                if task == Task::VisionQuery {
                    return engine.text(&String::from_utf8_lossy(&input.bytes));
                }
                let img = decode_for(&input.bytes, VISION_NEED, u32::min)?;
                engine.image(&img)
            })
            .await
            .map_err(|e| IndexError::Backend(format!("analyse interrompue : {e}")))??;
            return Ok(Output { model, text: None, vector: Some(vector) });
        }
        Err(IndexError::Unavailable(format!("{} : pas de moteur local", task.name())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(img: &image::RgbImage, fmt: image::ImageFormat) -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, fmt).unwrap();
        out.into_inner()
    }

    #[test]
    fn a_big_jpeg_is_decoded_already_reduced_but_never_below_what_the_engine_needs() {
        let big = image::RgbImage::from_fn(4000, 3000, |x, y| image::Rgb([(x % 256) as u8, (y % 256) as u8, 90]));
        let photo = decode_for(&encode(&big, image::ImageFormat::Jpeg), VISION_NEED, u32::min).unwrap();
        assert_eq!(photo.dimensions(), (1000, 750), "1/4 : le petit côté reste ≥ 448");
        let doc = decode_for(&encode(&big, image::ImageFormat::Jpeg), OCR_NEED, u32::max).unwrap();
        assert_eq!(doc.dimensions(), (2000, 1500), "1/2 seulement pour lire le texte");
        let small = image::RgbImage::new(300, 200);
        assert_eq!(decode_for(&encode(&small, image::ImageFormat::Png), VISION_NEED, u32::min).unwrap().dimensions(), (300, 200), "autre format : décodeur général");
        assert!(matches!(decode_for(b"pas une image", VISION_NEED, u32::min), Err(IndexError::Invalid(_))));
    }
}
