//! Recherche multimédia de la station : texte des images et des PDF (OCR), vecteurs d'images (SigLIP 2), vecteurs de texte,
//! transcription de la parole. Ce crate ne fait **que** l'inférence ; le stockage (`search_text`, `search_frames`,
//! `search_vision_vectors`) et la file de travail sont dans le binaire.
//!
//! **Cerveau partagé** : une petite station (Raspberry Pi) peut confier l'inférence à la station d'un Capitaine ami par le
//! tunnel DRAGON `ai-index`. [`Indexer`] masque la différence : [`LocalIndexer`] (ONNX Runtime ici) ou [`RemoteIndexer`]
//! (HTTP vers l'adresse locale d'un `forward` ouvert sur l'ami). L'ami n'écrit rien sur son disque ([`server`]).
//! Envoyer un fichier en clair à un ami n'est fait **que** si son propriétaire l'a accepté (table `index_delegation`) :
//! cette décision appartient à l'appelant, pas à ce crate.

#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod catalog;
mod local;
pub mod ocr;
pub mod pdf;
mod remote;
pub mod sched;
pub mod server;
pub mod siglip;
pub mod video;

use std::future::Future;

use bytes::Bytes;
use serde::{Deserialize, Serialize};

pub use local::{LocalIndexer, OCR_NEED, VISION_NEED, decode_for};
pub use remote::RemoteIndexer;

/// Texte rendu au plus (OCR d'un long PDF, transcription d'une heure de parole).
pub const MAX_TEXT: usize = 1 << 20;
/// Dimension d'un vecteur au plus.
pub const MAX_DIM: usize = 4096;

/// Ce qu'on demande à un indexeur.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Task {
    /// Image ou PDF → texte.
    Ocr,
    /// Image → vecteur (espace image-texte de SigLIP).
    Vision,
    /// Texte d'une recherche → vecteur dans le **même** espace que [`Task::Vision`].
    VisionQuery,
    /// Texte → vecteur sémantique (documents, descriptions).
    Text,
    /// Audio → texte (transcription).
    Speech,
}

impl Task {
    /// Toutes les tâches.
    pub const ALL: [Self; 5] = [Self::Ocr, Self::Vision, Self::VisionQuery, Self::Text, Self::Speech];

    /// Nom dans l'URL et dans la base.
    pub fn name(self) -> &'static str {
        match self {
            Self::Ocr => "ocr",
            Self::Vision => "vision",
            Self::VisionQuery => "vision-query",
            Self::Text => "text",
            Self::Speech => "speech",
        }
    }

    /// Tâche d'un nom.
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.name() == s)
    }

    /// Octets d'entrée au plus.
    pub fn max_input(self) -> usize {
        match self {
            Self::Ocr | Self::Vision => 32 << 20,
            Self::Speech => 64 << 20,
            Self::VisionQuery | Self::Text => 64 << 10,
        }
    }

    /// Type MIME accepté.
    pub fn accepts(self, mime: &str) -> bool {
        let m = mime.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
        match self {
            Self::Ocr => m.starts_with("image/") || m == "application/pdf",
            Self::Vision => m.starts_with("image/"),
            Self::VisionQuery | Self::Text => m == "text/plain",
            Self::Speech => m.starts_with("audio/"),
        }
    }

    /// La tâche rend-elle un vecteur (sinon du texte) ?
    pub fn gives_vector(self) -> bool {
        matches!(self, Self::Vision | Self::VisionQuery | Self::Text)
    }
}

/// Un contenu à indexer, gardé en mémoire.
#[derive(Debug, Clone)]
pub struct Input {
    /// Type MIME.
    pub mime: String,
    /// Octets (déchiffrés).
    pub bytes: Bytes,
}

impl Input {
    /// Refuse ce qu'une tâche ne doit pas recevoir (type, taille, texte non UTF-8).
    pub fn check(&self, task: Task) -> Result<(), IndexError> {
        if self.bytes.len() > task.max_input() {
            return Err(IndexError::TooLarge(self.bytes.len()));
        }
        if !task.accepts(&self.mime) {
            return Err(IndexError::Invalid(format!("« {} » refusé pour {}", self.mime, task.name())));
        }
        if matches!(task, Task::VisionQuery | Task::Text) && std::str::from_utf8(&self.bytes).is_err() {
            return Err(IndexError::Invalid("texte non UTF-8".into()));
        }
        Ok(())
    }
}

/// Modèle qui a produit un résultat : un vecteur ne se compare qu'à un vecteur du même modèle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelTag {
    /// Identifiant du catalogue (`siglip2-base`).
    pub id: String,
    /// SHA-256 du fichier modèle principal.
    pub sha256: String,
}

/// Résultat d'une indexation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Output {
    /// Modèle utilisé.
    pub model: ModelTag,
    /// Texte (OCR, parole).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Vecteur normalisé.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vector: Option<Vec<f32>>,
}

impl Output {
    /// Vérifie qu'un résultat a la forme attendue pour `task` (côté serveur avant l'envoi, côté client à la réception).
    pub fn check(&self, task: Task) -> Result<(), IndexError> {
        let bad = |w: &str| Err(IndexError::Invalid(format!("résultat {} : {w}", task.name())));
        match (&self.text, &self.vector, task.gives_vector()) {
            (_, Some(v), true) if v.is_empty() || v.len() > MAX_DIM => bad("dimension hors bornes"),
            (_, Some(v), true) if v.iter().any(|x| !x.is_finite()) => bad("valeur non finie"),
            (_, Some(v), true) if v.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>() < 1e-12 => bad("vecteur nul"),
            (None, Some(_), true) => Ok(()),
            (Some(t), None, false) if t.len() <= MAX_TEXT => Ok(()),
            (Some(_), None, false) => bad("texte trop long"),
            _ => bad("forme inattendue"),
        }
    }
}

/// Erreurs d'indexation.
#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    /// Tâche ou modèle indisponible ici (ou chez l'ami).
    #[error("indisponible : {0}")]
    Unavailable(String),
    /// Entrée trop grosse.
    #[error("contenu de {0} octets au-delà du plafond")]
    TooLarge(usize),
    /// Entrée ou résultat refusé.
    #[error("refusé : {0}")]
    Invalid(String),
    /// L'indexeur est occupé : revenir dans ce nombre de secondes.
    #[error("occupé, revenir dans {0} s")]
    Busy(u32),
    /// Transport ou calcul.
    #[error("échec : {0}")]
    Backend(String),
}

/// Un indexeur, local ou distant.
pub trait Indexer: Send + Sync {
    /// Tâches que cet indexeur sait faire maintenant.
    fn tasks(&self) -> Vec<Task>;
    /// Indexe `input` pour `task`. Le résultat est déjà vérifié ([`Output::check`]).
    fn index(&self, task: Task, input: Input) -> impl Future<Output = Result<Output, IndexError>> + Send;
}

/// Indexeur choisi pour un propriétaire : ici, ou chez l'ami qu'il a accepté.
pub enum AnyIndexer {
    /// Dans le processus.
    Local(LocalIndexer),
    /// Chez un Capitaine ami, par le tunnel `ai-index`.
    Remote(Box<RemoteIndexer>),
}

impl Indexer for AnyIndexer {
    fn tasks(&self) -> Vec<Task> {
        match self {
            Self::Local(i) => i.tasks(),
            Self::Remote(i) => i.tasks(),
        }
    }

    async fn index(&self, task: Task, input: Input) -> Result<Output, IndexError> {
        match self {
            Self::Local(i) => i.index(task, input).await,
            Self::Remote(i) => i.index(task, input).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tag() -> ModelTag {
        ModelTag { id: "m".into(), sha256: "00".into() }
    }

    #[test]
    fn inputs_are_bounded_by_task() {
        let img = |n: usize| Input { mime: "image/jpeg".into(), bytes: Bytes::from(vec![0u8; n]) };
        assert!(img(10).check(Task::Vision).is_ok());
        assert!(matches!(img(Task::Vision.max_input() + 1).check(Task::Vision), Err(IndexError::TooLarge(_))));
        assert!(img(10).check(Task::Speech).is_err(), "une image n'est pas de l'audio");
        let pdf = Input { mime: "application/pdf; x=1".into(), bytes: Bytes::from_static(b"%PDF") };
        assert!(pdf.check(Task::Ocr).is_ok() && pdf.check(Task::Vision).is_err());
        let bad = Input { mime: "text/plain".into(), bytes: Bytes::from_static(&[0xff, 0xfe]) };
        assert!(bad.check(Task::Text).is_err());
        assert!(Input { mime: "text/plain".into(), bytes: Bytes::from_static("chien à la plage".as_bytes()) }.check(Task::VisionQuery).is_ok());
    }

    #[test]
    fn outputs_must_match_their_task() {
        let v = |vector: Vec<f32>| Output { model: tag(), text: None, vector: Some(vector) };
        assert!(v(vec![0.6, 0.8]).check(Task::Vision).is_ok());
        assert!(v(vec![0.0; 3]).check(Task::Vision).is_err(), "nul");
        assert!(v(vec![f32::NAN, 1.0]).check(Task::Text).is_err(), "non fini");
        assert!(v(vec![1.0; MAX_DIM + 1]).check(Task::Text).is_err(), "trop grand");
        assert!(v(vec![1.0]).check(Task::Ocr).is_err(), "l'OCR rend du texte");
        let t = |text: String| Output { model: tag(), text: Some(text), vector: None };
        assert!(t("Facture".into()).check(Task::Ocr).is_ok());
        assert!(t("x".repeat(MAX_TEXT + 1)).check(Task::Speech).is_err());
        assert!(t("x".into()).check(Task::Vision).is_err());
        for task in Task::ALL {
            assert_eq!(Task::parse(task.name()), Some(task));
        }
    }
}
