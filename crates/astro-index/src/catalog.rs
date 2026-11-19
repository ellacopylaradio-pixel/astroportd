//! Catalogue des modèles de la recherche multimédia : ce que `astroportd models add` sait installer.
//!
//! Un fichier n'est **jamais** accepté sans son SHA-256. Chaque fichier a une révision figée (commit Hugging Face) et une copie
//! dans IPFS (CID v1, épinglée par les stations UPlanet) : `models add` essaie la passerelle IPFS, puis Hugging Face. Les deux
//! donnent les mêmes octets, vérifiés de la même façon. Un modèle sans empreinte (`sha256: None`) serait listé, jamais installé.

use std::io::Read as _;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::{ModelTag, Task};

/// Un fichier d'un modèle.
#[derive(Debug, Clone, Copy)]
pub struct ModelFile {
    /// Nom dans le dossier des modèles.
    pub name: &'static str,
    /// Adresse de téléchargement (révision figée).
    pub url: Option<&'static str>,
    /// SHA-256 attendu.
    pub sha256: Option<&'static str>,
    /// Copie IPFS (CID v1).
    pub cid: Option<&'static str>,
    /// Taille exacte, en octets (aussi le plafond du téléchargement).
    pub bytes: u64,
}

/// Un modèle du catalogue.
#[derive(Debug, Clone, Copy)]
pub struct Model {
    /// Identifiant (`astroportd models add <id>`).
    pub id: &'static str,
    /// Ce qu'il fait.
    pub tasks: &'static [Task],
    /// Nom lisible.
    pub label: &'static str,
    /// Licence.
    pub licence: &'static str,
    /// Fichiers ; le premier est le modèle principal (son SHA-256 étiquette les résultats).
    pub files: &'static [ModelFile],
}

const fn pin(name: &'static str, url: &'static str, sha256: &'static str, cid: &'static str, bytes: u64) -> ModelFile {
    ModelFile { name, url: Some(url), sha256: Some(sha256), cid: Some(cid), bytes }
}

/// Les modèles connus. OCR : PP-OCRv6 *small* plutôt que *tiny* (reconnaissance 81,3 % contre 73,5 % pour 30 Mo au lieu de 6) ;
/// le dictionnaire de caractères est dans `ocr-rec.yml`. Tous les modèles sont quantifiés int8, comme bge-m3.
pub const CATALOG: &[Model] = &[
    Model {
        id: "ppocrv6-small",
        tasks: &[Task::Ocr],
        label: "PP-OCRv6 small (50 langues, dont le français)",
        licence: "Apache-2.0",
        files: &[
            pin("ocr-det.onnx", "https://huggingface.co/PaddlePaddle/PP-OCRv6_small_det_onnx/resolve/28fe5895c24fd108c19eb3e8479f4ab385fbfc62/inference.onnx", "d73e0058b7a8086bbd57f3d10b8bcd4ff95363f67e06e2762b5e814fe9c9410e", "bafybeidcgt7hpqpaf5qjj376ujin7yywbn466qseo7eqcm67ojfw3lzczy", 9880512),
            pin("ocr-det.yml", "https://huggingface.co/PaddlePaddle/PP-OCRv6_small_det_onnx/resolve/28fe5895c24fd108c19eb3e8479f4ab385fbfc62/inference.yml", "193f435274bf9f0b5f71a929bbfbcf148282df7e633b34e7c373e8f44741b516", "bafkreiazh5bve5f7t4fv64njfg57xtyuqkbn67tdhm2opq3t5d2eoqnvcy", 885),
            pin("ocr-rec.onnx", "https://huggingface.co/PaddlePaddle/PP-OCRv6_small_rec_onnx/resolve/b8f84f0b80c529de40b4fbb3544b84fa7233a513/inference.onnx", "5435fd747c9e0efe15a96d0b378d5bd157e9492ed8fd80edf08f30d02fa24634", "bafybeifseus2flcniwe7df4yxowxoqpqyqghjfavytygzyukax7gy3cu5a", 21159378),
            pin("ocr-rec.yml", "https://huggingface.co/PaddlePaddle/PP-OCRv6_small_rec_onnx/resolve/b8f84f0b80c529de40b4fbb3544b84fa7233a513/inference.yml", "ab078671bb49f06228eadccd34f1bb501e157f7a047095ffb943ba81512c77d1", "bafkreifla6dhdo2j6brcr2w4zu2pdo2qdykx66qeock77okdxkavcldx2e", 150579),
        ],
    },
    Model {
        id: "siglip2-base",
        tasks: &[Task::Vision, Task::VisionQuery],
        label: "SigLIP 2 base patch16-224 (int8)",
        licence: "Apache-2.0",
        files: &[
            pin("siglip2-vision-int8.onnx", "https://huggingface.co/onnx-community/siglip2-base-patch16-224-ONNX/resolve/ba1f3b0843f24bc5417d38e19c37b287d719b2f4/onnx/vision_model_int8.onnx", "0dd31785a2713f1113ef2272472165c69d580473dae38d7b47568ac587795e70", "bafybeihu6geopjl4yn2ah7ykxnkyo4vifktqkfipq72eech5p2upxfof3e", 94553333),
            pin("siglip2-text-int8.onnx", "https://huggingface.co/onnx-community/siglip2-base-patch16-224-ONNX/resolve/ba1f3b0843f24bc5417d38e19c37b287d719b2f4/onnx/text_model_int8.onnx", "3a0603d3a00c05a80a6ded4743c16aaac7b1e62cdcc7e362e7ce418659b96400", "bafybeig6lxejtz5ugfwxsj2x6xdup6rkjpudct5642lozpnpwamz66nlly", 283438275),
            pin("siglip2-tokenizer.json", "https://huggingface.co/onnx-community/siglip2-base-patch16-224-ONNX/resolve/ba1f3b0843f24bc5417d38e19c37b287d719b2f4/tokenizer.json", "cb9140fae3ac5122c972d37adf83e1248471a38147ad76f8215c8872c6fd8322", "bafybeihmlihqojetohpcro73ei5ybqukiffezdspiuxxkuo4hcgewiyt7u", 34363039),
            pin("siglip2-preprocessor.json", "https://huggingface.co/onnx-community/siglip2-base-patch16-224-ONNX/resolve/ba1f3b0843f24bc5417d38e19c37b287d719b2f4/preprocessor_config.json", "9b36b57ebaf20f09bf4c22100ccc21877ea6bfe5aead0c00c59f8af8ccefacfc", "bafkreie3g22x5oxsb4e36tbccagmyimhp2tl7znovugabrm7rl4mz35m7q", 394),
        ],
    },
    Model {
        id: "e5-small",
        tasks: &[Task::Text],
        label: "Multilingual E5 Small (int8)",
        licence: "MIT",
        files: &[
            pin("e5-small-int8.onnx", "https://huggingface.co/Xenova/multilingual-e5-small/resolve/761b726dd34fb83930e26aab4e9ac3899aa1fa78/onnx/model_int8.onnx", "4d24e2bc01a447951524466ef533e52944bf48509e6552810bcee1a2711cb02c", "bafybeic5o2rsfsvkwhaqb2bt43ycj2azui6f4jif42ylroncra3ccd2kxy", 118054593),
            pin("e5-small-tokenizer.json", "https://huggingface.co/Xenova/multilingual-e5-small/resolve/761b726dd34fb83930e26aab4e9ac3899aa1fa78/tokenizer.json", "0b44a9d7b51c3c62626640cda0e2c2f70fdacdc25bbbd68038369d14ebdf4c39", "bafybeicaihcs4mud3xu6zq4s57uucuiqx4cbqvt4n32d64qpqvmk7qytem", 17082730),
        ],
    },
    Model {
        id: "whisper-base",
        tasks: &[Task::Speech],
        label: "Whisper base multilingue (int8)",
        licence: "Apache-2.0",
        files: &[
            pin("whisper-base-encoder-int8.onnx", "https://huggingface.co/onnx-community/whisper-base/resolve/27a07b4b52b3a4c5886134bb4320e93e1a4bbc7c/onnx/encoder_model_int8.onnx", "ca6177401f86a2c6b4dc5f7fc02fbca680678906bd0c22f6d89f0b80f124253f", "bafybeif5l5gk2ilrexvq2plg4nmkm6nm364kdmgoadokcok3kv2yn3obt4", 23201297),
            pin("whisper-base-decoder-int8.onnx", "https://huggingface.co/onnx-community/whisper-base/resolve/27a07b4b52b3a4c5886134bb4320e93e1a4bbc7c/onnx/decoder_model_merged_int8.onnx", "fa3ef9902734ce5ae6f9ef2bdb2ba9a6c4b5785b09f4f420ce036573dc9d090b", "bafybeibagbd6cmdnu3trj7vvpc7pp4i7vxbxkyfwd3ovizy4o4fsfqpgha", 53693315),
            pin("whisper-base-tokenizer.json", "https://huggingface.co/onnx-community/whisper-base/resolve/27a07b4b52b3a4c5886134bb4320e93e1a4bbc7c/tokenizer.json", "27fc476bfe7f17299480be2273fc0608e4d5a99aba2ab5dec5374b4482d1a566", "bafybeihrvzexytbwzbcafktad7iadt2up6toei4p7rtc25y4fthxfpkyum", 2480466),
            pin("whisper-base-generation.json", "https://huggingface.co/onnx-community/whisper-base/resolve/27a07b4b52b3a4c5886134bb4320e93e1a4bbc7c/generation_config.json", "61070cf8de25b1e9256e8e102ded49d8d24a8369ed36ef84fdf21549e68125a0", "bafkreidba4gprxrfwhusk3uocaw62soy2jfig2png3xyj7pscve6najfua", 3832),
            pin("whisper-base-preprocessor.json", "https://huggingface.co/onnx-community/whisper-base/resolve/27a07b4b52b3a4c5886134bb4320e93e1a4bbc7c/preprocessor_config.json", "a6a76d28c93edb273669eb9e0b0636a2bddbb1272c3261e47b7ca6dfdbac1b8d", "bafkreifgu5wsrsj63mttm2pltyfqmnvcxxn3cjzmgjq6i634u3p5xla3ru", 339),
        ],
    },
];

/// État d'un modèle dans un dossier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Aucun fichier.
    Absent,
    /// Une partie des fichiers.
    Partial,
    /// Tous les fichiers (présence seulement : l'empreinte est vérifiée à l'installation).
    Present,
}

impl Model {
    /// Un modèle du catalogue.
    pub fn find(id: &str) -> Option<&'static Self> {
        CATALOG.iter().find(|m| m.id == id)
    }

    /// Révision figée (adresse et empreinte pour chaque fichier) : installable.
    pub fn pinned(&self) -> bool {
        self.files.iter().all(|f| f.url.is_some() && f.sha256.is_some())
    }

    /// Taille totale, en Mo (arrondie au supérieur).
    pub fn mb(&self) -> u64 {
        self.files.iter().map(|f| f.bytes).sum::<u64>().div_ceil(1_000_000)
    }

    /// Présence des fichiers dans `dir`.
    pub fn state(&self, dir: &Path) -> State {
        match self.files.iter().filter(|f| dir.join(f.name).is_file()).count() {
            0 => State::Absent,
            n if n == self.files.len() => State::Present,
            _ => State::Partial,
        }
    }

    /// Étiquette des résultats de ce modèle (`None` tant que la révision n'est pas figée).
    pub fn tag(&self) -> Option<ModelTag> {
        Some(ModelTag { id: self.id.to_owned(), sha256: self.files.first()?.sha256?.to_owned() })
    }
}

/// Dossier des modèles : `<données>/models`, sinon `~/.zen/astroportd/models` (celui d'`install.sh`).
pub fn models_dir(data_dir: Option<&Path>, home: Option<&Path>) -> Option<PathBuf> {
    data_dir.map(|d| d.join("models")).or_else(|| home.map(|h| h.join(".zen/astroportd/models")))
}

/// SHA-256 d'un fichier, en hexadécimal, lu par blocs.
pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        match f.read(&mut buf)? {
            0 => return Ok(hex::encode(h.finalize())),
            n => h.update(&buf[..n]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_is_coherent_and_never_pinned_without_a_digest() {
        let mut names = std::collections::HashSet::new();
        for m in CATALOG {
            assert!(Model::find(m.id).is_some() && !m.files.is_empty() && !m.tasks.is_empty());
            assert!(m.files.iter().all(|f| names.insert(f.name)), "{} : nom de fichier en double", m.id);
            assert_eq!(m.pinned(), m.tag().is_some() && m.files.iter().all(|f| f.url.is_some()));
            assert!(m.pinned(), "{} : révision figée", m.id);
            assert!(m.files.iter().all(|f| f.sha256.is_some_and(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))));
            assert!(m.files.iter().all(|f| f.cid.is_some_and(|c| c.starts_with("baf")) && f.bytes > 0 && f.url.is_some_and(|u| u.starts_with("https://huggingface.co/"))));
        }
        for t in Task::ALL {
            assert!(CATALOG.iter().any(|m| m.tasks.contains(&t)), "{} : aucun modèle", t.name());
        }
    }

    #[test]
    fn state_and_digest_follow_the_files() {
        let dir = std::env::temp_dir().join(format!("astro-index-cat-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let m = Model::find("e5-small").unwrap();
        assert_eq!(m.state(&dir), State::Absent);
        std::fs::write(dir.join(m.files[0].name), b"abc").unwrap();
        assert_eq!(m.state(&dir), State::Partial);
        std::fs::write(dir.join(m.files[1].name), b"").unwrap();
        assert_eq!(m.state(&dir), State::Present);
        assert_eq!(sha256_file(&dir.join(m.files[0].name)).unwrap(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
