//! `astroportd models` : modèles de la recherche multimédia (catalogue `astro_index::catalog`).
//!
//! Installation : chaque fichier est téléchargé (garde-fous de [`crate::fetch`]) dans `.<nom>.part`, haché au fil de l'eau, et
//! n'est renommé à sa place que si son SHA-256 est celui du catalogue. Un fichier déjà là et intact n'est pas retéléchargé.
//! Retrait : seuls les fichiers du catalogue sont supprimés, jamais un autre chemin.

use std::path::Path;

use astro_index::catalog::{CATALOG, Model, State, sha256_file};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::fetch::{FetchLimits, fetch};

/// Passerelle IPFS par défaut (celle d'`install.sh` ; `ASTRO_IPFS_GATEWAY` la remplace).
pub const DEFAULT_GATEWAY: &str = "https://ipfs.copylaradio.com";

/// Tableau des modèles et de leur état dans `dir`.
pub fn list(dir: &Path) -> String {
    let mut out = format!("Modèles de la recherche multimédia ({})\n", dir.display());
    for m in CATALOG {
        let state = match (m.state(dir), m.pinned()) {
            (State::Present, _) => "✔ installé",
            (State::Partial, _) => "⚠ incomplet",
            (State::Absent, true) => "· absent",
            (State::Absent, false) => "· révision pas encore figée",
        };
        let tasks = m.tasks.iter().map(|t| t.name()).collect::<Vec<_>>().join(", ");
        out += &format!("  {:<13} {:<28} ≈ {:>4} Mo  {:<21} {} ({})\n", m.id, state, m.mb(), tasks, m.label, m.licence);
    }
    out
}

/// Modèles installables (révision figée) dont aucun fichier n'est dans `dir` : une ligne `id|Mo|tâches|nom` chacun.
/// Format stable, lu par `install.sh` pour proposer ce qu'une nouvelle version a ajouté au catalogue.
pub fn absent_lines(dir: &Path) -> String {
    CATALOG
        .iter()
        .filter(|m| m.pinned() && m.state(dir) == State::Absent)
        .map(|m| {
            let tasks = m.tasks.iter().map(|t| t.name()).collect::<Vec<_>>().join(",");
            format!("{}|{}|{}|{}\n", m.id, m.mb(), tasks, m.label.replace('|', "/"))
        })
        .collect()
}

/// Compte rendu de [`update`].
#[derive(Debug, Default)]
pub struct UpdateReport {
    /// Texte à afficher, une ligne par modèle.
    pub text: String,
    /// Modèles dont au moins un fichier a été retéléchargé.
    pub changed: usize,
    /// Modèles qui n'ont pas pu être mis à jour.
    pub failed: usize,
}

/// Met à jour les modèles déjà installés (même en partie) : [`add`] ne retélécharge que les fichiers absents ou dont
/// l'empreinte n'est plus celle du catalogue de ce binaire. Les modèles absents sont seulement cités.
pub async fn update(dir: &Path, gateway: Option<&str>, limits: &FetchLimits) -> UpdateReport {
    let mut r = UpdateReport { text: format!("Modèles de la recherche multimédia ({})\n", dir.display()), ..UpdateReport::default() };
    let mut absent = Vec::new();
    for m in CATALOG.iter().filter(|m| m.pinned()) {
        if m.state(dir) == State::Absent {
            absent.push(m.id);
            continue;
        }
        match add(m, dir, gateway, limits).await {
            Ok(notes) if notes.iter().all(|n| n.ends_with("déjà là, intact")) => r.text += &format!("  ✔ {} : à jour\n", m.id),
            Ok(_) => {
                r.changed += 1;
                r.text += &format!("  ✔ {} : mis à jour, empreintes vérifiées\n", m.id);
            }
            Err(e) => {
                r.failed += 1;
                r.text += &format!("  ✘ {} : {e}\n", m.id);
            }
        }
    }
    if !absent.is_empty() {
        r.text += &format!("  Pas encore installés : {} (astroportd models add <nom>)\n", absent.join(", "));
    }
    r
}

/// Installe `m` dans `dir` ; rend une ligne par fichier. Sources, dans l'ordre : la passerelle IPFS `gateway` (copie UPlanet),
/// puis la révision figée sur Hugging Face. Chaque fichier est borné à sa taille exacte et vérifié par son SHA-256.
pub async fn add(m: &Model, dir: &Path, gateway: Option<&str>, limits: &FetchLimits) -> Result<Vec<String>, String> {
    if !m.pinned() {
        return Err(format!("{} : révision pas encore figée dans ce binaire (adresse et SHA-256 à choisir) : rien n'est installé", m.id));
    }
    tokio::fs::create_dir_all(dir).await.map_err(|e| format!("{} : {e}", dir.display()))?;
    let mut notes = Vec::new();
    for f in m.files {
        let Some(want) = f.sha256 else { continue };
        let dest = dir.join(f.name);
        if dest.is_file() && sha256_file(&dest).is_ok_and(|h| h == want) {
            notes.push(format!("{} : déjà là, intact", f.name));
            continue;
        }
        let ipfs = gateway.zip(f.cid).map(|(g, c)| (format!("{}/ipfs/{c}", g.trim_end_matches('/')), "IPFS"));
        let sources = ipfs.into_iter().chain(f.url.map(|u| (u.to_owned(), "Hugging Face")));
        let part = dir.join(format!(".{}.part", f.name));
        let mut tried = Vec::new();
        let mut one = limits.clone();
        one.max_bytes = f.bytes;
        for (url, from) in sources {
            match download(&url, &part, &one).await {
                Ok(h) if h == want => {
                    tokio::fs::rename(&part, &dest).await.map_err(|e| format!("{} : {e}", dest.display()))?;
                    notes.push(format!("{} : installé depuis {from}, SHA-256 vérifié", f.name));
                    tried.clear();
                    break;
                }
                Ok(h) => tried.push(format!("{from} : SHA-256 {h} ≠ attendu, rejeté")),
                Err(e) => tried.push(format!("{from} : {e}")),
            }
            let _ = tokio::fs::remove_file(&part).await;
        }
        if !tried.is_empty() {
            return Err(format!("{} : {}", f.name, tried.join(" ; ")));
        }
    }
    Ok(notes)
}

async fn download(url: &str, part: &Path, limits: &FetchLimits) -> Result<String, String> {
    let got = fetch(url, limits).await.map_err(|e| e.to_string())?;
    let mut body = got.body;
    let mut file = tokio::fs::File::create(part).await.map_err(|e| e.to_string())?;
    let (mut hash, mut total, mut buf) = (Sha256::new(), 0u64, vec![0u8; 1 << 16]);
    loop {
        let n = body.read(&mut buf).await.map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > limits.max_bytes {
            return Err(format!("plus de {} octets", limits.max_bytes));
        }
        hash.update(&buf[..n]);
        file.write_all(&buf[..n]).await.map_err(|e| e.to_string())?;
    }
    file.sync_all().await.map_err(|e| e.to_string())?;
    Ok(hex::encode(hash.finalize()))
}

/// Retire les fichiers de `m` de `dir` ; rend le nombre de fichiers supprimés.
pub fn remove(m: &Model, dir: &Path) -> Result<usize, String> {
    let mut n = 0;
    for f in m.files {
        match std::fs::remove_file(dir.join(f.name)) {
            Ok(()) => n += 1,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("{} : {e}", f.name)),
        }
    }
    Ok(n)
}

/// Une mesure de [`bench`] : une tâche, sa durée moyenne et le nombre d'essais.
#[derive(Debug, Clone, PartialEq)]
pub struct Measure {
    /// Tâche.
    pub task: astro_index::Task,
    /// Durée moyenne d'un essai (ms), après un premier essai d'échauffement non compté.
    pub ms: f64,
    /// Essais comptés.
    pub runs: usize,
}

/// Photo d'essai fabriquée (2000 × 1500, dégradés et aplats), quand on ne donne pas de fichier : la durée dépend surtout de la
/// taille, pas du sujet.
pub fn sample_photo() -> Vec<u8> {
    let img = image::RgbImage::from_fn(2000, 1500, |x, y| {
        let (a, b) = (u8::try_from(x * 255 / 2000).unwrap_or(0), u8::try_from(y * 255 / 1500).unwrap_or(0));
        if (x / 200 + y / 150) % 3 == 0 { image::Rgb([a, b, 128]) } else { image::Rgb([b, 200, a]) }
    });
    let mut out = std::io::Cursor::new(Vec::new());
    let _ = img.write_to(&mut out, image::ImageFormat::Jpeg);
    out.into_inner()
}

/// Mesure ce que sait faire `ix` sur `photos` (JPEG ou PNG) : regarder une photo, transformer une recherche en vecteur, lire le
/// texte d'une image. Chaque tâche : un essai d'échauffement, puis `runs` essais comptés (en tournant sur les photos).
pub async fn bench<I: astro_index::Indexer>(ix: &I, photos: &[(String, bytes::Bytes)], runs: usize) -> Result<Vec<Measure>, String> {
    use astro_index::{Input, Task};
    let have = ix.tasks();
    let query = |i: usize| ["un gâteau d'anniversaire", "coucher de soleil sur la mer", "un vélo rouge devant la mairie"][i % 3];
    let mut out = Vec::new();
    for task in [Task::Vision, Task::VisionQuery, Task::Ocr].into_iter().filter(|t| have.contains(t)) {
        let input = |i: usize| match task {
            Task::VisionQuery => Input { mime: "text/plain".into(), bytes: bytes::Bytes::from(query(i)) },
            _ => {
                let (mime, b) = &photos[i % photos.len().max(1)];
                Input { mime: mime.clone(), bytes: b.clone() }
            }
        };
        if photos.is_empty() && task != Task::VisionQuery {
            continue;
        }
        ix.index(task, input(0)).await.map_err(|e| format!("{} : {e}", task.name()))?;
        let start = std::time::Instant::now();
        for i in 0..runs.max(1) {
            ix.index(task, input(i + 1)).await.map_err(|e| format!("{} : {e}", task.name()))?;
        }
        out.push(Measure { task, ms: start.elapsed().as_secs_f64() * 1000.0 / runs.max(1) as f64, runs: runs.max(1) });
    }
    Ok(out)
}

/// Mémoire au plus haut du processus (Mo), lue dans `/proc/self/status` (Linux).
pub fn peak_mb() -> Option<u64> {
    let s = std::fs::read_to_string("/proc/self/status").ok()?;
    s.lines().find_map(|l| l.strip_prefix("VmHWM:")).and_then(|v| v.trim().trim_end_matches("kB").trim().parse::<u64>().ok()).map(|kb| kb / 1024)
}

fn duration_words(secs: f64) -> String {
    match secs {
        s if s < 90.0 => format!("{} s", s.round()),
        s if s < 5400.0 => format!("{} min", (s / 60.0).round()),
        s => format!("{:.1} h", s / 3600.0).replace('.', ","),
    }
}

/// Rapport lisible : durées, et ce qu'elles veulent dire pour une bibliothèque de 10 000 photos et 1 000 documents.
pub fn report(load_ms: f64, measures: &[Measure], peak: Option<u64>) -> String {
    use astro_index::Task;
    let mut r = format!("Chargement des moteurs : {}\n", duration_words(load_ms / 1000.0));
    if measures.is_empty() {
        r += "Aucun moteur chargé : `astroportd models add ppocrv6-small` (texte) ou `siglip2-base` (photos), puis `doctor`.\n";
    }
    for m in measures {
        let (what, scale, of) = match m.task {
            Task::Vision => ("regarder une photo", 10_000.0, "10 000 photos"),
            Task::VisionQuery => ("transformer une recherche", 0.0, ""),
            Task::Ocr => ("lire le texte d'une image", 1_000.0, "1 000 documents"),
            _ => ("autre", 0.0, ""),
        };
        r += &format!("  {what:<28} {:>7.0} ms  ({} essais)", m.ms, m.runs);
        if scale > 0.0 {
            r += &format!("  → {of} ≈ {}", duration_words(m.ms * scale / 1000.0));
        }
        r += "\n";
    }
    if let Some(mb) = peak {
        r += &format!("Mémoire au plus haut : {mb} Mo\n");
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use astro_index::Task;
    use astro_index::catalog::ModelFile;

    fn leak(s: String) -> &'static str {
        Box::leak(s.into_boxed_str())
    }

    struct Slow;
    impl astro_index::Indexer for Slow {
        fn tasks(&self) -> Vec<Task> {
            vec![Task::Vision, Task::VisionQuery]
        }
        async fn index(&self, task: Task, input: astro_index::Input) -> Result<astro_index::Output, astro_index::IndexError> {
            input.check(task)?;
            tokio::time::sleep(std::time::Duration::from_millis(if task == Task::Vision { 20 } else { 2 })).await;
            Ok(astro_index::Output { model: astro_index::ModelTag { id: "t".into(), sha256: "t".into() }, text: None, vector: Some(vec![1.0]) })
        }
    }

    #[tokio::test]
    async fn the_bench_measures_each_available_task_and_says_what_it_means_for_a_library() {
        let photos = vec![("image/jpeg".to_owned(), bytes::Bytes::from(sample_photo()))];
        assert!(photos[0].1.starts_with(&[0xFF, 0xD8]), "une vraie photo JPEG");
        let m = bench(&Slow, &photos, 3).await.unwrap();
        assert_eq!(m.iter().map(|m| m.task).collect::<Vec<_>>(), [Task::Vision, Task::VisionQuery], "seulement ce que le moteur sait faire");
        assert!(m[0].ms >= 19.0 && m[0].runs == 3);
        let r = report(1500.0, &m, Some(900));
        assert!(r.contains("regarder une photo") && r.contains("10 000 photos ≈ ") && r.contains("900 Mo"), "{r}");
        assert!(report(10.0, &[], None).contains("models add"), "rien de chargé : on dit quoi faire");
        assert_eq!((duration_words(42.0), duration_words(600.0), duration_words(9000.0)), ("42 s".to_owned(), "10 min".to_owned(), "2,5 h".to_owned()));
    }

    #[tokio::test]
    async fn install_keeps_only_files_whose_digest_matches_then_remove_cleans_up() {
        use axum::routing::get;
        let app = axum::Router::new().route("/ok", get(|| async { "abc" })).route("/bad", get(|| async { "abd" })).route("/ipfs/bafok", get(|| async { "abc" }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let file = |name, path: &str| ModelFile { name, url: Some(leak(format!("{base}{path}"))), sha256: Some(abc), cid: Some("bafabsent"), bytes: 3 };
        let good: &'static [ModelFile] = Box::leak(vec![file("a.onnx", "/ok")].into_boxed_slice());
        let bad: &'static [ModelFile] = Box::leak(vec![file("b.onnx", "/bad")].into_boxed_slice());
        let model = |files| Model { id: "t", tasks: &[Task::Text], label: "test", licence: "MIT", files };
        let dir = std::env::temp_dir().join(format!("astroportd-models-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut limits = FetchLimits::new(1024);
        limits.allow_private = true;

        // La passerelle n'a pas le fichier (404) : Hugging Face prend le relais.
        assert_eq!(add(&model(good), &dir, Some(&base), &limits).await.unwrap(), vec!["a.onnx : installé depuis Hugging Face, SHA-256 vérifié"]);
        assert_eq!(add(&model(good), &dir, None, &limits).await.unwrap(), vec!["a.onnx : déjà là, intact"]);
        std::fs::remove_file(dir.join("a.onnx")).unwrap();
        let via_ipfs = |name| ModelFile { name, url: Some(leak(format!("{base}/bad"))), sha256: Some(abc), cid: Some("bafok"), bytes: 3 };
        let ipfs_first: &'static [ModelFile] = Box::leak(vec![via_ipfs("a.onnx")].into_boxed_slice());
        assert_eq!(add(&model(ipfs_first), &dir, Some(&base), &limits).await.unwrap(), vec!["a.onnx : installé depuis IPFS, SHA-256 vérifié"]);
        let e = add(&model(bad), &dir, None, &limits).await.unwrap_err();
        assert!(e.contains("rejeté") && !dir.join("b.onnx").exists() && !dir.join(".b.onnx.part").exists(), "{e}");
        let big: &'static [ModelFile] = Box::leak(vec![ModelFile { bytes: 2, ..file("c.onnx", "/ok") }].into_boxed_slice());
        assert!(add(&model(big), &dir, None, &limits).await.unwrap_err().contains("octets"), "plafond = taille exacte");
        std::fs::write(dir.join("a.onnx"), b"abc").unwrap();
        assert_eq!((remove(&model(good), &dir).unwrap(), remove(&model(good), &dir).unwrap()), (1, 0));
        assert!(list(&dir).contains("ppocrv6-small"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn update_only_touches_installed_models_and_names_the_new_ones() {
        let dir = std::env::temp_dir().join(format!("astroportd-models-upd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Rien d'installé : rien n'est téléchargé (aucune adresse joignable ici), tout est cité comme nouveau.
        let r = update(&dir, None, &FetchLimits::new(1)).await;
        assert_eq!((r.changed, r.failed), (0, 0), "{}", r.text);
        assert!(r.text.contains("Pas encore installés") && r.text.contains("whisper-base"), "{}", r.text);
        let lines = absent_lines(&dir);
        assert_eq!(lines.lines().count(), CATALOG.len(), "{lines}");
        assert!(lines.lines().all(|l| l.split('|').count() == 4 && l.split('|').nth(1).is_some_and(|mb| mb.parse::<u64>().is_ok())), "{lines}");
        // Un fichier d'un modèle suffit à le tenir pour installé (une mise à jour le complète) : il sort de la liste des absents.
        std::fs::write(dir.join("ocr-det.yml"), b"x").unwrap();
        assert!(!absent_lines(&dir).contains("ppocrv6-small"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
