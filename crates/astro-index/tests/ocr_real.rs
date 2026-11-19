//! OCR avec les VRAIS modèles PP-OCRv6 small. Ignoré tant que les fichiers et la bibliothèque ne sont pas fournis :
//! ASTRO_OCR_DIR=/chemin   # dossier où `astroportd models add ppocrv6-small` les a mis
//! cargo test -p astro-index --features onnx --test ocr_real -- --ignored
#![cfg(feature = "onnx")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use astro_index::{Indexer, Input, LocalIndexer, Task};
use bytes::Bytes;

fn engine() -> LocalIndexer {
    engine_with(&[Task::Ocr])
}

fn engine_with(want: &[Task]) -> LocalIndexer {
    let dir = std::path::PathBuf::from(std::env::var("ASTRO_OCR_DIR").expect("ASTRO_OCR_DIR"));
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let lib = astro_facecloud::onnx::find_library(std::env::var("ORT_DYLIB_PATH").ok().as_deref(), home.as_deref());
    let (ix, problems) = LocalIndexer::load(&dir, lib.as_deref(), 0);
    assert!(problems.is_empty(), "{problems:?}");
    assert!(want.iter().all(|t| ix.tasks().contains(t)), "{:?}", ix.tasks());
    ix
}

#[tokio::test]
#[ignore = "demande les modèles PP-OCRv6 (ASTRO_OCR_DIR) et libonnxruntime"]
async fn a_french_invoice_is_read_with_its_accents() {
    let ix = engine();
    let png = Bytes::from_static(include_bytes!("fixtures/ocr_facture.png"));
    let out = ix.index(Task::Ocr, Input { mime: "image/png".into(), bytes: png }).await.unwrap();
    let text = out.text.unwrap();
    for want in ["FACTURE", "Électricité du hameau des Écluses", "84,50 €", "Échéance : 15 décembre 2026"] {
        assert!(text.contains(want), "« {want} » absent de :\n{text}");
    }
    assert_eq!(text.lines().count(), 5, "{text}");
    assert_eq!(out.model.id, "ppocrv6-small");
}

#[tokio::test]
#[ignore = "demande les modèles PP-OCRv6 (ASTRO_OCR_DIR) et libonnxruntime"]
async fn a_big_jpeg_photo_of_an_invoice_is_still_read_after_reduced_decoding() {
    let ix = engine();
    let png = image::load_from_memory(include_bytes!("fixtures/ocr_facture.png")).unwrap().to_rgb8();
    let (w, h) = png.dimensions();
    let big = image::imageops::resize(&png, w * 3, h * 3, image::imageops::FilterType::CatmullRom);
    let mut jpeg = std::io::Cursor::new(Vec::new());
    big.write_to(&mut jpeg, image::ImageFormat::Jpeg).unwrap();
    let text = ix.index(Task::Ocr, Input { mime: "image/jpeg".into(), bytes: Bytes::from(jpeg.into_inner()) }).await.unwrap().text.unwrap();
    assert!(text.contains("Électricité") && text.contains("84,50"), "{text}");
}

#[tokio::test]
#[ignore = "demande les modèles PP-OCRv6 (ASTRO_OCR_DIR), libonnxruntime et poppler-utils"]
async fn a_scanned_pdf_is_rendered_then_read() {
    let ix = engine();
    let pdf = Bytes::from_static(include_bytes!("fixtures/ocr_scan.pdf"));
    let text = ix.index(Task::Ocr, Input { mime: "application/pdf".into(), bytes: pdf }).await.unwrap().text.unwrap();
    assert!(text.contains("Montant à régler : 84,50 €"), "{text}");
}

#[tokio::test]
#[ignore = "demande les modèles PP-OCRv6 et SigLIP 2 (ASTRO_OCR_DIR) et libonnxruntime"]
async fn a_photo_of_an_invoice_is_closer_to_its_description_than_to_a_mountain() {
    let ix = engine_with(&[Task::Ocr, Task::Vision, Task::VisionQuery]);
    let png = Bytes::from_static(include_bytes!("fixtures/ocr_facture.png"));
    let img = ix.index(Task::Vision, Input { mime: "image/png".into(), bytes: png }).await.unwrap();
    assert_eq!((img.model.id.as_str(), img.vector.as_ref().map(Vec::len)), ("siglip2-base", Some(768)));
    let score = async |q: &'static str| {
        let v = ix.index(Task::VisionQuery, Input { mime: "text/plain".into(), bytes: Bytes::from_static(q.as_bytes()) }).await.unwrap().vector.unwrap();
        v.iter().zip(img.vector.as_ref().unwrap()).map(|(a, b)| a * b).sum::<f32>()
    };
    let (facture, montagne) = (score("une facture").await, score("une montagne enneigée").await);
    assert!(facture > montagne + 0.02, "facture {facture} montagne {montagne}");
}
