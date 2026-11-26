//! Partage N¹ en chaîne, DAG de création/remix et répartition exacte Voie A en déci-Ẑen.
//!
//! Conception : `docs/design/PARTAGE_CASCADE.md`.
//!
//! - **Unité monétaire atomique** : `1 Ẑen = 0,10 Ğ1`, et la plus petite unité transférable sur Duniter v2s est `0,01 Ğ1`
//!   ([`Amount(1)`]). Un [`DeciZen`] vaut exactement `0,10 Ẑen = 10 centimes de Ẑen = 0,01 Ğ1 = Amount(1)`. Aucun flottant ;
//!   tout reste de division euclidienne revient à l'auteur principal (`author_pubkey`).
//! - **DAG de création** : une fiche `.media.json` porte un bloc optionnel [`CommonsBlock`] qui décrit ses droits (copie
//!   privée ou commun sous licence SPDX), sa tarification globale ([`PricingMode`]), son générique pondéré ([`Credit`]) et ses
//!   œuvres parentes ([`SourceRef`], remix rendu ou « recette » non destructive, par exemple une fiche botanique [`PlantData`]
//!   référençant la photo originale sans la dupliquer). [`flatten_dag`] aplatit le DAG en une table déterministe vérifiable.
//! - **Chaîne N¹** : [`ShareChain`] enchaîne les sauts signés ([`Hop`]) de proche en proche.
//! - **Voie A** : [`compute_payout_batch`] calcule, en un seul lot de virements définitifs, la part Création et la part
//!   Diffusion, en fusionnant les bénéficiaires identiques et en garantissant une conservation exacte.

use std::collections::{BTreeMap, BTreeSet};

use astro_core::Amount;
use astro_nostr_proto::{Event, UnsignedEvent, is_hex64, pubkey_of};
use serde::{Deserialize, Serialize};

/// 100 % exprimé en points de base (`100 bps = 1 %`).
pub const BPS_TOTAL: u16 = 10_000;
/// Nombre de déci-Ẑen dans 1 Ẑen entier (`1 Ẑen = 10 déci-Ẑen = 0,10 Ğ1`).
pub const DECI_PER_ZEN: u64 = 10;
/// Quantum minimal en centimes de Ẑen (`0,10 Ẑen = 0,01 Ğ1 = Amount(1)`).
pub const ZEN_QUANTUM_CENTS: u64 = 10;
/// Profondeur maximale du DAG de création (générations de remixes).
pub const MAX_DAG_DEPTH: u32 = 8;
/// Nombre maximal de sauts dans une chaîne de partage N¹.
pub const MAX_CHAIN_HOPS: usize = 64;
/// Nombre maximal d'entrées au générique d'une œuvre.
pub const MAX_CREDITS: usize = 32;
/// Nombre maximal de parents directs d'un remix.
pub const MAX_SOURCES: usize = 32;
/// Nombre maximal de pistes dans une recette non destructive.
pub const MAX_TRACKS: usize = 64;

/// Kind éphémère (jamais publié) utilisé pour signer un [`CommonsBlock`].
pub const COMMONS_SIG_KIND: u32 = 27_856;
/// Kind éphémère (jamais publié) utilisé pour signer un saut [`Hop`].
pub const HOP_SIG_KIND: u32 = 27_857;
/// Kind éphémère (jamais publié) utilisé pour la contresignature d'un co-auteur sur [`Credit`].
pub const CREDIT_SIG_KIND: u32 = 27_858;

const GENESIS_SIG: &str = "genesis";

/// Erreurs de validation, de signature, d'aplatissement du DAG ou de répartition Voie A.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CascadeError {
    /// Clé publique ou empreinte hexadécimale (64 minuscules) invalide.
    #[error("identifiant hex64 invalide pour {0}")]
    BadHex(&'static str),
    /// Chaîne ou texte vide ou mal formé.
    #[error("champ invalide : {0}")]
    InvalidField(&'static str),
    /// Limite structurelle dépassée (anti-DoS).
    #[error("limite dépassée : {0}")]
    LimitExceeded(&'static str),
    /// Clé secrète ou opération cryptographique impossible.
    #[error("échec de signature cryptographique")]
    Crypto,
    /// Signature absente ou invalide.
    #[error("signature invalide")]
    BadSignature,
    /// Règle de répartition Création / Diffusion dont la somme ne vaut pas 10 000 bps.
    #[error("règle de répartition invalide (somme != 10000 bps)")]
    BadSplit,
    /// Table de générique ou de répartition aplatie dont la somme ne vaut pas 10 000 bps.
    #[error("table de répartition création invalide (somme != 10000 bps)")]
    BadCreationTable,
    /// Œuvre non monétisable (copie privée, licence absente ou fiche non signée).
    #[error("œuvre non monétisable (copie privée ou fiche non signée)")]
    NotMonetizable,
    /// Parent introuvable pour aplatir ou vérifier le DAG.
    #[error("œuvre parente introuvable : {0}")]
    MissingParent(String),
    /// Parent qui interdit le remix (copie privée ou `allow_remix = false`).
    #[error("le parent interdit le remix : {0}")]
    RemixForbidden(String),
    /// Part reversée au parent inférieure à son plancher `remix_min_bps`.
    #[error("part reversée au parent inférieure au plancher exigé ({got} < {min} bps)")]
    BelowRemixFloor {
        /// Part proposée par l'enfant.
        got: u16,
        /// Plancher minimal exigé par le parent.
        min: u16,
    },
    /// Parent non signé ou incohérent (clé d'auteur ou empreinte discordante).
    #[error("parent invalide ou non signé : {0}")]
    InvalidParent(String),
    /// Table aplatie ou génération en désaccord avec les parents.
    #[error("table aplatie ou génération incohérente avec le DAG")]
    FlattenedMismatch,
    /// Auto-référence ou doublon dans les parents.
    #[error("source en doublon ou auto-référence")]
    DuplicateOrSelfSource,
    /// Chaîne de partage incohérente (rang, enchaînement des clés, auto-saut).
    #[error("chaîne de partage invalide : {0}")]
    InvalidChain(&'static str),
    /// La chaîne de partage ne correspond pas à la fiche (empreinte ou auteur).
    #[error("chaîne incompatible avec la fiche d'œuvre")]
    ChainWorkMismatch,
    /// Montant nul ou inférieur au prix/minimum exigé par la fiche.
    #[error("montant nul ou insuffisant")]
    InsufficientAmount,
}

/// Unité monétaire atomique de la cascade : **1 déci-Ẑen = 0,10 Ẑen = 0,01 Ğ1 = `Amount(1)`**.
///
/// Garantit par construction que tout virement est un multiple du pas indivisible de 10 centimes de Ẑen.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DeciZen(pub u64);

impl DeciZen {
    /// Montant nul.
    pub const ZERO: Self = Self(0);

    /// Construit un montant à partir de Ẑen entiers (`zen * 10`).
    pub const fn from_zen(zen: u64) -> Option<Self> {
        match zen.checked_mul(DECI_PER_ZEN) {
            Some(v) => Some(Self(v)),
            None => None,
        }
    }

    /// Conversion exacte vers les centimes de Ğ1 ([`Amount`]).
    pub const fn to_g1_amount(self) -> Amount {
        Amount(self.0)
    }

    /// Conversion exacte vers les centimes de Ẑen (toujours multiple de 10).
    pub const fn to_zen_cents(self) -> Option<u64> {
        self.0.checked_mul(ZEN_QUANTUM_CENTS)
    }
}

/// Points de base (`0..=10_000`, où `10_000 = 100 %`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Bps(pub u16);

impl Bps {
    /// 0 %.
    pub const ZERO: Self = Self(0);
    /// 100 % (`10_000 bps`).
    pub const FULL: Self = Self(BPS_TOTAL);

    /// `true` si la valeur tient dans `0..=10_000`.
    pub const fn is_valid(self) -> bool {
        self.0 <= BPS_TOTAL
    }
}

// ──────────────────────── 1. Économie & Licence (dans `.media.json`) ────────────────────────

/// Clé de répartition globale entre la création (auteur + équipe + parents du DAG) et la diffusion N¹.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SplitRule {
    /// Part dédiée au graphe de création (défaut : `8_000` = 80 %).
    pub creation_bps: Bps,
    /// Part partagée entre les relais uniques de la branche N¹ (défaut : `2_000` = 20 %).
    pub diffusion_bps: Bps,
}

impl Default for SplitRule {
    fn default() -> Self {
        Self { creation_bps: Bps(8_000), diffusion_bps: Bps(2_000) }
    }
}

impl SplitRule {
    /// Vérifie que `creation_bps + diffusion_bps == 10_000` sans dépassement.
    pub const fn is_valid(&self) -> bool {
        (self.creation_bps.0 as u32) + (self.diffusion_bps.0 as u32) == (BPS_TOTAL as u32)
    }
}

/// Régime de droits : sépare strictement la copie privée (non monétisable) des créations et communs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Rights {
    /// Copie privée (CPI L. 122-5) : partage N¹ uniquement, **jamais** de paiement ni de remix rémunéré.
    PrivateCopy,
    /// Commun ou création sous licence explicite (identifiant SPDX, ex. `"CC-BY-SA-4.0"`, `"LAL-1.3"`, `"CC0-1.0"`).
    OpenCommon {
        /// Identifiant de licence SPDX (non vide).
        spdx: String,
        /// Autorise un tiers à créer un remix référençant cette œuvre dans le DAG.
        #[serde(default = "default_true")]
        allow_remix: bool,
        /// Plancher minimal (en bps de la tranche création de l'œuvre dérivée) exigé en cas de remix.
        #[serde(default)]
        remix_min_bps: Bps,
    },
}

const fn default_true() -> bool {
    true
}

/// Mode de tarification d'une œuvre (prix global indépendant de la profondeur `n` de la chaîne).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum PricingMode {
    /// Gratuit (compteur et filiation seuls).
    Free,
    /// Prix libre / chapeau (recommandé en cercle N¹).
    Hat {
        /// Montant suggéré en déci-Ẑen.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        suggested_deci_zen: Option<DeciZen>,
        /// Plancher facultatif en déci-Ẑen.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min_deci_zen: Option<DeciZen>,
    },
    /// Prix global fixe en déci-Ẑen (ex. `25` = 2,50 Ẑen = 0,25 Ğ1).
    Fixed {
        /// Prix fixe exigé.
        price_deci_zen: DeciZen,
    },
}

impl PricingMode {
    /// Vérifie qu'un montant `paid` satisfait le mode tarifaire (un don volontaire supérieur au prix fixe est admis).
    pub fn accepts(&self, paid: DeciZen) -> bool {
        if paid.0 == 0 {
            return false;
        }
        match self {
            Self::Free => true,
            Self::Hat { min_deci_zen, .. } => min_deci_zen.is_none_or(|m| paid >= m),
            Self::Fixed { price_deci_zen } => paid >= *price_deci_zen,
        }
    }
}

/// Un bénéficiaire direct au générique de l'œuvre (personne, co-auteur ou portefeuille d'un bien commun).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Credit {
    /// Clé publique Nostr hex64 du bénéficiaire (MULTIPASS ou bien commun).
    pub pubkey: String,
    /// Rôle lisible (ex. `"photo"`, `"identification-botanique"`, `"musique"`, `"studio-commun"`).
    pub role: String,
    /// Part en bps de la tranche création de cette œuvre.
    pub bps: Bps,
    /// Contresignature BIP-340 facultative du co-auteur attestant le générique.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attest_sig: Option<String>,
}

impl Credit {
    fn attest_event(&self, content_sha: &str) -> UnsignedEvent {
        UnsignedEvent {
            created_at: 0,
            kind: CREDIT_SIG_KIND,
            tags: vec![
                vec!["t".into(), "astro-credit-v1".into()],
                vec!["x".into(), content_sha.to_owned()],
                vec!["role".into(), self.role.clone()],
                vec!["bps".into(), self.bps.0.to_string()],
            ],
            content: String::new(),
        }
    }

    /// Contresigne cette ligne de générique avec la clé secrète du bénéficiaire (`self.pubkey`).
    pub fn attest(&mut self, secret: &[u8; 32], content_sha: &str) -> Result<(), CascadeError> {
        if !is_hex64(content_sha) {
            return Err(CascadeError::BadHex("content_sha"));
        }
        if !self.bps.is_valid() {
            return Err(CascadeError::BadCreationTable);
        }
        let pk = pubkey_of(secret).map_err(|_| CascadeError::Crypto)?;
        if pk != self.pubkey {
            return Err(CascadeError::BadSignature);
        }
        let ev = self.attest_event(content_sha).sign(secret).map_err(|_| CascadeError::Crypto)?;
        self.attest_sig = Some(ev.sig);
        Ok(())
    }

    /// Vérifie la contresignature BIP-340 du co-auteur pour l'œuvre `content_sha`.
    pub fn verify_attestation(&self, content_sha: &str) -> bool {
        let Some(sig) = self.attest_sig.as_deref() else {
            return false;
        };
        if sig.len() != 128 || !is_hex64(&self.pubkey) || !is_hex64(content_sha) {
            return false;
        }
        let u = self.attest_event(content_sha);
        let mut ev = Event {
            id: String::new(),
            pubkey: self.pubkey.clone(),
            created_at: u.created_at,
            kind: u.kind,
            tags: u.tags,
            content: u.content,
            sig: sig.to_owned(),
        };
        let Ok(id) = ev.compute_id() else {
            return false;
        };
        ev.id = id.iter().map(|b| format!("{b:02x}")).collect();
        ev.verify().is_ok()
    }
}

// ──────────────────────── 2. DAG de création & Remix ────────────────────────

/// Nature de l'apport d'une œuvre parente dans un remix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceRole {
    /// Média source principal (ex. photo originale de plante, rush vidéo).
    PrimaryMedia,
    /// Piste audio / bande son.
    AudioTrack,
    /// Piste vidéo.
    VideoTrack,
    /// Sous-titres ou traduction.
    Subtitles,
    /// Fiche de savoir / documentation / identification.
    Knowledge,
    /// Jeu de données ou corpus.
    Dataset,
    /// Autre rôle libre.
    Other(String),
}

/// Lien vers une œuvre parente du DAG.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRef {
    /// Empreinte SHA-256 du contenu clair parent (`x`).
    pub sha_plain: String,
    /// CID du blob parent (ou de sa fiche `.media.json` pour une dépendance purement documentaire).
    pub cid: String,
    /// Clé Nostr hex64 de l'auteur principal du parent.
    pub author_pubkey: String,
    /// Identifiant d'événement Nostr de la fiche parente, si publiée.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    /// Rôle du parent dans cette œuvre dérivée.
    pub role: SourceRole,
    /// Part en bps de la tranche création de cette œuvre reversée à la lignée de ce parent.
    /// Doit être `>= parent.rights.remix_min_bps`.
    pub share_bps: Bps,
}

/// Piste d'une composition non destructive (« remix recette », zéro duplication de blobs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompositionTrack {
    /// Référence le `sha_plain` d'un élément présent dans `sources` (ou du contenu propre).
    pub source_sha: String,
    /// Type de flux (`"video"`, `"audio"`, `"subtitles"`, `"image"`, `"annotation"`).
    pub stream: String,
    /// Début facultatif en millisecondes (découpe).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_ms: Option<u64>,
    /// Fin facultative en millisecondes (découpe).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_ms: Option<u64>,
    /// Décalage temporel en millisecondes dans la composition finale.
    #[serde(default)]
    pub offset_ms: u64,
    /// Langue ISO-639-1 facultative (doublage, sous-titres).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
}

/// Forme technique d'un remix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum RemixKind {
    /// Nouveau fichier encodé (montage, mashup, étalonnage) possédant son propre CID.
    Rendered,
    /// Recette non destructive : assemble des CID existants sans recopier les médias.
    /// Couvre aussi l'enrichissement documentaire (ex. fiche botanique de Bob sur photo d'Alice).
    Recipe {
        /// Pistes assemblées par la recette.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tracks: Vec<CompositionTrack>,
    },
}

/// Ligne aplatie (pré-résolue récursivement sur le DAG) prête pour la Voie A.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PayoutShare {
    /// Clé publique Nostr hex64 du bénéficiaire.
    pub pubkey: String,
    /// Part consolidée en points de base (`sum == 10_000`).
    pub bps: Bps,
}

/// Bloc `commons` (optionnel, rétrocompatible) embarqué dans `.media.json` (`media/v1`).
///
/// Sans `signature` valide de `author_pubkey`, l'œuvre reste en Lots 1–2 (compteur et filiation seuls, jamais d'argent).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommonsBlock {
    /// Version du bloc (`1`).
    #[serde(default = "one")]
    pub v: u32,
    /// Clé Nostr hex64 de l'auteur principal (signataire de la fiche et destinataire du reste de division).
    pub author_pubkey: String,
    /// Empreinte SHA-256 du contenu couvert (fichier média ou recette canonique).
    pub content_sha: String,
    /// Régime de droits et licence.
    pub rights: Rights,
    /// Tarification globale.
    pub pricing: PricingMode,
    /// Répartition création / diffusion.
    #[serde(default)]
    pub split: SplitRule,
    /// Générique propre à cette œuvre (`sum(credits) + sum(sources) == 10_000`).
    #[serde(default)]
    pub credits: Vec<Credit>,
    /// Parents du DAG (vide pour une œuvre racine ; 1..N pour un remix ou une fiche dérivée).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<SourceRef>,
    /// Profondeur dans le DAG de création (`0` = œuvre originale, `max(parents) + 1` = remix).
    #[serde(default)]
    pub generation: u32,
    /// Forme du remix si `sources` est non vide.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remix: Option<RemixKind>,
    /// Table de répartition création aplatie (diamants fusionnés par `pubkey`, somme = `10_000`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flattened_payouts: Vec<PayoutShare>,
    /// Signature BIP-340 (128 hex) par `author_pubkey` sur le bloc canonique (hors `signature`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

const fn one() -> u32 {
    1
}

impl CommonsBlock {
    /// Événement éphémère canonique reconstruit pour signer ou vérifier `self` (sans `signature` ni contresignatures `attest_sig`).
    fn canonical_event(&self) -> Result<UnsignedEvent, CascadeError> {
        let mut unsigned = self.clone();
        unsigned.signature = None;
        for c in &mut unsigned.credits {
            c.attest_sig = None;
        }
        let content = serde_json::to_string(&unsigned).map_err(|_| CascadeError::Crypto)?;
        Ok(UnsignedEvent {
            created_at: 0,
            kind: COMMONS_SIG_KIND,
            tags: vec![vec!["t".into(), "astro-commons-v1".into()], vec!["x".into(), self.content_sha.clone()]],
            content,
        })
    }

    /// Vérifie la validité structurelle du bloc (hors parents externes et signature).
    pub fn validate_structure(&self) -> Result<(), CascadeError> {
        if self.v != 1 {
            return Err(CascadeError::InvalidField("version"));
        }
        if !is_hex64(&self.author_pubkey) {
            return Err(CascadeError::BadHex("author_pubkey"));
        }
        if !is_hex64(&self.content_sha) {
            return Err(CascadeError::BadHex("content_sha"));
        }
        if !self.split.is_valid() {
            return Err(CascadeError::BadSplit);
        }
        if let Rights::OpenCommon { spdx, remix_min_bps, .. } = &self.rights {
            if spdx.trim().is_empty() || spdx.len() > 64 {
                return Err(CascadeError::InvalidField("spdx"));
            }
            if !remix_min_bps.is_valid() {
                return Err(CascadeError::BadCreationTable);
            }
        }
        if self.credits.len() > MAX_CREDITS {
            return Err(CascadeError::LimitExceeded("credits"));
        }
        if self.sources.len() > MAX_SOURCES {
            return Err(CascadeError::LimitExceeded("sources"));
        }
        if let Some(RemixKind::Recipe { tracks }) = &self.remix
            && tracks.len() > MAX_TRACKS
        {
            return Err(CascadeError::LimitExceeded("tracks"));
        }
        if self.generation > MAX_DAG_DEPTH {
            return Err(CascadeError::LimitExceeded("generation"));
        }
        for c in &self.credits {
            if !is_hex64(&c.pubkey) {
                return Err(CascadeError::BadHex("credit.pubkey"));
            }
            if c.role.trim().is_empty() || c.role.len() > 64 {
                return Err(CascadeError::InvalidField("credit.role"));
            }
            if !c.bps.is_valid() {
                return Err(CascadeError::BadCreationTable);
            }
        }
        let mut seen_sources = BTreeSet::new();
        for s in &self.sources {
            if !is_hex64(&s.sha_plain) {
                return Err(CascadeError::BadHex("source.sha_plain"));
            }
            if !is_hex64(&s.author_pubkey) {
                return Err(CascadeError::BadHex("source.author_pubkey"));
            }
            if s.cid.trim().is_empty() || s.cid.len() > 128 {
                return Err(CascadeError::InvalidField("source.cid"));
            }
            if s.sha_plain == self.content_sha || !seen_sources.insert(s.sha_plain.as_str()) {
                return Err(CascadeError::DuplicateOrSelfSource);
            }
            if !s.share_bps.is_valid() {
                return Err(CascadeError::BadCreationTable);
            }
        }
        Ok(())
    }

    /// Signe le bloc avec la clé secrète de `author_pubkey` (BIP-340).
    pub fn sign(&mut self, secret: &[u8; 32]) -> Result<(), CascadeError> {
        let pk = pubkey_of(secret).map_err(|_| CascadeError::Crypto)?;
        if pk != self.author_pubkey {
            return Err(CascadeError::BadSignature);
        }
        self.validate_structure()?;
        let ev = self.canonical_event()?.sign(secret).map_err(|_| CascadeError::Crypto)?;
        self.signature = Some(ev.sig);
        Ok(())
    }

    /// Vérifie la signature BIP-340 du bloc contre `author_pubkey`.
    pub fn verify_signature(&self) -> bool {
        let Some(sig) = self.signature.as_deref() else {
            return false;
        };
        if sig.len() != 128 || !is_hex64(&self.author_pubkey) || !is_hex64(&self.content_sha) {
            return false;
        }
        let Ok(u) = self.canonical_event() else {
            return false;
        };
        let mut ev = Event {
            id: String::new(),
            pubkey: self.author_pubkey.clone(),
            created_at: u.created_at,
            kind: u.kind,
            tags: u.tags,
            content: u.content,
            sig: sig.to_owned(),
        };
        let Ok(id) = ev.compute_id() else {
            return false;
        };
        ev.id = id.iter().map(|b| format!("{b:02x}")).collect();
        ev.verify().is_ok()
    }
}

/// Résout récursivement le DAG (parents directs déjà vérifiés) en `(generation, flattened_payouts)`.
///
/// - Vérifie que `sum(credits.bps) + sum(sources.share_bps) == 10_000`.
/// - Vérifie chaque parent : signé, `OpenCommon { allow_remix: true, remix_min_bps }`, `share_bps >= remix_min_bps`,
///   et génération bornée par [`MAX_DAG_DEPTH`].
/// - Fusionne les diamants par `pubkey` dans un `BTreeMap` déterministe.
/// - Attribue tout reste d'arrondi de `bps` à `author_pubkey` afin que `sum(flattened_payouts) == 10_000`.
pub fn flatten_dag(
    author_pubkey: &str,
    content_sha: &str,
    credits: &[Credit],
    sources: &[SourceRef],
    parents: &BTreeMap<String, CommonsBlock>,
) -> Result<(u32, Vec<PayoutShare>), CascadeError> {
    if !is_hex64(author_pubkey) {
        return Err(CascadeError::BadHex("author_pubkey"));
    }
    if !is_hex64(content_sha) {
        return Err(CascadeError::BadHex("content_sha"));
    }
    if credits.len() > MAX_CREDITS {
        return Err(CascadeError::LimitExceeded("credits"));
    }
    if sources.len() > MAX_SOURCES {
        return Err(CascadeError::LimitExceeded("sources"));
    }

    let mut local_sum: u32 = 0;
    for c in credits {
        if !is_hex64(&c.pubkey) {
            return Err(CascadeError::BadHex("credit.pubkey"));
        }
        local_sum = local_sum.saturating_add(u32::from(c.bps.0));
    }

    let mut seen = BTreeSet::new();
    for s in sources {
        if !is_hex64(&s.sha_plain) {
            return Err(CascadeError::BadHex("source.sha_plain"));
        }
        if !is_hex64(&s.author_pubkey) {
            return Err(CascadeError::BadHex("source.author_pubkey"));
        }
        if s.sha_plain == content_sha || !seen.insert(s.sha_plain.as_str()) {
            return Err(CascadeError::DuplicateOrSelfSource);
        }
        local_sum = local_sum.saturating_add(u32::from(s.share_bps.0));
    }
    if local_sum != u32::from(BPS_TOTAL) {
        return Err(CascadeError::BadCreationTable);
    }

    let mut merged: BTreeMap<String, u32> = BTreeMap::new();
    for c in credits {
        if c.bps.0 > 0 {
            *merged.entry(c.pubkey.clone()).or_insert(0) += u32::from(c.bps.0);
        }
    }

    let mut max_parent_gen: Option<u32> = None;
    for s in sources {
        let parent = parents.get(&s.sha_plain).ok_or_else(|| CascadeError::MissingParent(s.sha_plain.clone()))?;
        if parent.content_sha != s.sha_plain || parent.author_pubkey != s.author_pubkey || !parent.verify_signature() {
            return Err(CascadeError::InvalidParent(s.sha_plain.clone()));
        }
        match &parent.rights {
            Rights::PrivateCopy => return Err(CascadeError::RemixForbidden(s.sha_plain.clone())),
            Rights::OpenCommon { allow_remix: false, .. } => return Err(CascadeError::RemixForbidden(s.sha_plain.clone())),
            Rights::OpenCommon { allow_remix: true, remix_min_bps, .. } => {
                if s.share_bps.0 < remix_min_bps.0 {
                    return Err(CascadeError::BelowRemixFloor { got: s.share_bps.0, min: remix_min_bps.0 });
                }
            }
        }
        max_parent_gen = Some(max_parent_gen.map_or(parent.generation, |g| g.max(parent.generation)));

        let parent_table: Vec<(&str, u16)> = if !parent.flattened_payouts.is_empty() {
            parent.flattened_payouts.iter().map(|p| (p.pubkey.as_str(), p.bps.0)).collect()
        } else {
            parent.credits.iter().map(|c| (c.pubkey.as_str(), c.bps.0)).collect()
        };
        let parent_sum: u32 = parent_table.iter().map(|(_, b)| u32::from(*b)).sum();
        if parent_sum != u32::from(BPS_TOTAL) {
            return Err(CascadeError::InvalidParent(s.sha_plain.clone()));
        }
        for (pk, pbps) in parent_table {
            let eff = (u32::from(s.share_bps.0) * u32::from(pbps)) / u32::from(BPS_TOTAL);
            if eff > 0 {
                *merged.entry(pk.to_owned()).or_insert(0) += eff;
            }
        }
    }

    let generation = match max_parent_gen {
        None => 0,
        Some(g) => {
            let next = g.checked_add(1).ok_or(CascadeError::LimitExceeded("generation"))?;
            if next > MAX_DAG_DEPTH {
                return Err(CascadeError::LimitExceeded("generation"));
            }
            next
        }
    };

    let allocated: u32 = merged.values().copied().sum();
    let remainder = u32::from(BPS_TOTAL).saturating_sub(allocated);
    if remainder > 0 {
        *merged.entry(author_pubkey.to_owned()).or_insert(0) += remainder;
    }

    let table: Vec<PayoutShare> = merged
        .into_iter()
        .filter(|(_, v)| *v > 0)
        .map(|(pubkey, v)| PayoutShare { pubkey, bps: Bps(u16::try_from(v).unwrap_or(BPS_TOTAL)) })
        .collect();

    Ok((generation, table))
}

/// Vérifie qu'un [`CommonsBlock`] respecte ses parents (planchers, licence, génération et table aplatie exacte).
pub fn verify_flattened_dag(block: &CommonsBlock, parents: &BTreeMap<String, CommonsBlock>) -> Result<(), CascadeError> {
    block.validate_structure()?;
    if !block.verify_signature() {
        return Err(CascadeError::BadSignature);
    }
    let (expected_gen, expected_table) =
        flatten_dag(&block.author_pubkey, &block.content_sha, &block.credits, &block.sources, parents)?;
    if block.generation != expected_gen || block.flattened_payouts != expected_table {
        return Err(CascadeError::FlattenedMismatch);
    }
    Ok(())
}

// ──────────────────────── 3. Spécialisation Botanique (base commune) ────────────────────────

/// Bloc `plant` (optionnel) dans `.media.json` pour `type = "plant"` ou `"photo"`.
///
/// Reprend les champs botaniques de `src/plant_fiche.rs` sans jamais exposer le GPS exact (maille UMAP uniquement) ni les
/// notes privées du membre.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlantData {
    /// Nom scientifique (ex. `"Urtica dioica"`).
    pub scientific: String,
    /// Noms communs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub common: Vec<String>,
    /// Famille botanique.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub family: String,
    /// Identifiant GBIF.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gbif: Option<String>,
    /// Maille géographique UMAP 0,01° (ex. `"_43.61_1.44"`), jamais de coordonnées fines.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub umap: Option<String>,
    /// Date d'observation arrondie (secondes UNIX).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<i64>,
    /// Résumé botanique.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Comestibilité.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub edible: String,
    /// Préparation.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub preparation: String,
    /// Propriétés médicinales.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub medicinal: String,
    /// Dangers et confusions possibles.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub dangers: String,
    /// Source citée (ex. article Wikipédia CC BY-SA).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wiki_source_url: Option<String>,
}

// ──────────────────────── 4. Jeton de chaîne N¹ (Lot 1 — Voie A) ────────────────────────

/// Un saut signé de proche en proche (`from_pubkey` partage à `to_pubkey`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hop {
    /// Rang du saut (`1` = origine → premier proche, `2` = proche → suivant…).
    pub rank: u32,
    /// Clé Nostr hex64 du partageur.
    pub from_pubkey: String,
    /// Clé Nostr hex64 du destinataire (empêche le rejeu vers un tiers).
    pub to_pubkey: String,
    /// Horodatage UNIX en secondes.
    pub at: i64,
    /// Signature BIP-340 (128 hex) de `from_pubkey` sur le saut chaîné au précédent.
    pub sig: String,
}

fn hop_event(work_sha: &str, rank: u32, from_pubkey: &str, to_pubkey: &str, at: i64, prev_sig: &str) -> UnsignedEvent {
    let created_at = u64::try_from(at).unwrap_or(0);
    UnsignedEvent {
        created_at,
        kind: HOP_SIG_KIND,
        tags: vec![
            vec!["t".into(), "astro-hop-v1".into()],
            vec!["x".into(), work_sha.to_owned()],
            vec!["rank".into(), rank.to_string()],
            vec!["from".into(), from_pubkey.to_owned()],
            vec!["p".into(), to_pubkey.to_owned()],
            vec!["prev".into(), prev_sig.to_owned()],
        ],
        content: String::new(),
    }
}

/// Jeton de lignée N¹ joint au partage chiffré (NIP-59).
///
/// En Voie A, le spectateur connaît sa branche complète de l'origine jusqu'à lui, jamais les autres branches de l'arbre.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareChain {
    /// Version du format (`1`).
    #[serde(default = "one")]
    pub v: u32,
    /// Empreinte SHA-256 (`x`) de l'œuvre diffusée (identique sur toute la branche).
    pub work_sha: String,
    /// CID du blob diffusé.
    pub work_cid: String,
    /// CID de la fiche `.media.json` associée (porte le bloc `commons` signé).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fiche_cid: Option<String>,
    /// Clé Nostr hex64 de la source (rang 0).
    pub origin_pubkey: String,
    /// Sauts successifs ordonnés par `rank = 1..=k`.
    pub hops: Vec<Hop>,
}

impl ShareChain {
    /// Crée une racine de chaîne (0 saut) prête à être étendue lors du premier partage.
    pub fn new_root(
        work_sha: impl Into<String>,
        work_cid: impl Into<String>,
        fiche_cid: Option<String>,
        origin_pubkey: impl Into<String>,
    ) -> Result<Self, CascadeError> {
        let work_sha = work_sha.into();
        let work_cid = work_cid.into();
        let origin_pubkey = origin_pubkey.into();
        if !is_hex64(&work_sha) {
            return Err(CascadeError::BadHex("work_sha"));
        }
        if !is_hex64(&origin_pubkey) {
            return Err(CascadeError::BadHex("origin_pubkey"));
        }
        if work_cid.trim().is_empty() || work_cid.len() > 128 {
            return Err(CascadeError::InvalidField("work_cid"));
        }
        Ok(Self { v: 1, work_sha, work_cid, fiche_cid, origin_pubkey, hops: Vec::new() })
    }

    /// Fabrique la chaîne suivante en ajoutant un saut signé par `from_secret` vers `to_pubkey`.
    pub fn extend(&self, from_secret: &[u8; 32], to_pubkey: &str, at: i64) -> Result<Self, CascadeError> {
        self.verify()?;
        if self.hops.len() >= MAX_CHAIN_HOPS {
            return Err(CascadeError::LimitExceeded("hops"));
        }
        if at < 0 {
            return Err(CascadeError::InvalidChain("horodatage négatif"));
        }
        if !is_hex64(to_pubkey) {
            return Err(CascadeError::BadHex("to_pubkey"));
        }
        let from_pubkey = pubkey_of(from_secret).map_err(|_| CascadeError::Crypto)?;
        if from_pubkey == to_pubkey {
            return Err(CascadeError::InvalidChain("auto-partage interdit"));
        }
        let expected_from = self.hops.last().map_or(self.origin_pubkey.as_str(), |h| h.to_pubkey.as_str());
        if from_pubkey != expected_from {
            return Err(CascadeError::InvalidChain("le partageur n'est pas le dernier destinataire"));
        }
        let prev_sig = self.hops.last().map_or(GENESIS_SIG, |h| h.sig.as_str());
        let rank = u32::try_from(self.hops.len() + 1).map_err(|_| CascadeError::LimitExceeded("hops"))?;
        let ev = hop_event(&self.work_sha, rank, &from_pubkey, to_pubkey, at, prev_sig)
            .sign(from_secret)
            .map_err(|_| CascadeError::Crypto)?;
        let mut next = self.clone();
        next.hops.push(Hop { rank, from_pubkey, to_pubkey: to_pubkey.to_owned(), at, sig: ev.sig });
        Ok(next)
    }

    /// Vérifie l'intégrité structurelle et toutes les signatures BIP-340 de la branche.
    pub fn verify(&self) -> Result<(), CascadeError> {
        if self.v != 1 {
            return Err(CascadeError::InvalidChain("version inconnue"));
        }
        if !is_hex64(&self.work_sha) {
            return Err(CascadeError::BadHex("work_sha"));
        }
        if !is_hex64(&self.origin_pubkey) {
            return Err(CascadeError::BadHex("origin_pubkey"));
        }
        if self.work_cid.trim().is_empty() || self.work_cid.len() > 128 {
            return Err(CascadeError::InvalidField("work_cid"));
        }
        if self.hops.len() > MAX_CHAIN_HOPS {
            return Err(CascadeError::LimitExceeded("hops"));
        }

        let mut expected_from = self.origin_pubkey.as_str();
        let mut prev_sig = GENESIS_SIG;
        for (idx, hop) in self.hops.iter().enumerate() {
            let expected_rank = u32::try_from(idx + 1).map_err(|_| CascadeError::LimitExceeded("hops"))?;
            if hop.rank != expected_rank {
                return Err(CascadeError::InvalidChain("rang discontinu"));
            }
            if hop.at < 0 {
                return Err(CascadeError::InvalidChain("horodatage négatif"));
            }
            if !is_hex64(&hop.from_pubkey) || !is_hex64(&hop.to_pubkey) {
                return Err(CascadeError::BadHex("hop"));
            }
            if hop.from_pubkey != expected_from {
                return Err(CascadeError::InvalidChain("enchaînement des clés rompu"));
            }
            if hop.from_pubkey == hop.to_pubkey {
                return Err(CascadeError::InvalidChain("auto-saut"));
            }
            if hop.sig.len() != 128 {
                return Err(CascadeError::BadSignature);
            }
            let u = hop_event(&self.work_sha, hop.rank, &hop.from_pubkey, &hop.to_pubkey, hop.at, prev_sig);
            let mut ev = Event {
                id: String::new(),
                pubkey: hop.from_pubkey.clone(),
                created_at: u.created_at,
                kind: u.kind,
                tags: u.tags,
                content: u.content,
                sig: hop.sig.clone(),
            };
            let id = ev.compute_id().map_err(|_| CascadeError::BadSignature)?;
            ev.id = id.iter().map(|b| format!("{b:02x}")).collect();
            ev.verify().map_err(|_| CascadeError::BadSignature)?;

            expected_from = hop.to_pubkey.as_str();
            prev_sig = hop.sig.as_str();
        }
        Ok(())
    }

    /// Profondeur courante de la branche (= nombre de sauts).
    pub fn depth(&self) -> u32 {
        u32::try_from(self.hops.len()).unwrap_or(u32::MAX)
    }

    /// Relais uniques de la branche (hors auteur principal, qui touche déjà la tranche création).
    /// Conserve l'ordre de passage sans doublon (neutralise une boucle `B → C → B`).
    pub fn unique_relays<'a>(&'a self, author_pubkey: &str) -> Vec<&'a str> {
        let mut out: Vec<&'a str> = Vec::new();
        for hop in &self.hops {
            let p = hop.from_pubkey.as_str();
            if p != author_pubkey && !out.contains(&p) {
                out.push(p);
            }
        }
        out
    }
}

// ──────────────────────── 5. Lot de virements Voie A (calcul exact u64, Lot 3) ────────────────────────

/// Une ligne du lot unique de virements définitifs émis par la station du spectateur.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferLine {
    /// Clé publique Nostr hex64 du bénéficiaire.
    pub to_pubkey: String,
    /// Montant exact en déci-Ẑen (`1 DeciZen = 0,10 Ẑen = 0,01 Ğ1 = Amount(1)`).
    pub amount: DeciZen,
}

/// Résultat déterministe du calcul de répartition Voie A (`sum(lines) == total`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PayoutBatch {
    /// Empreinte de l'œuvre payée.
    pub work_sha: String,
    /// Montant total versé par le spectateur.
    pub total: DeciZen,
    /// Virements à émettre en un lot (une seule ligne par bénéficiaire, montants > 0).
    pub lines: Vec<TransferLine>,
    /// Relais présents dans la chaîne mais écartés car hors de l'ensemble éligible (ex. hors toile de confiance WoT).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filtered_relays: Vec<String>,
}

/// Calcule le lot unique de virements (Voie A) sans filtrage de relais (tous les relais uniques de la chaîne sont éligibles).
pub fn compute_payout_batch(
    total: DeciZen,
    commons: &CommonsBlock,
    chain: &ShareChain,
) -> Result<PayoutBatch, CascadeError> {
    compute_payout_batch_with_eligible(total, commons, chain, None)
}

/// Calcule le lot unique de virements (Voie A) en arithmétique entière exacte (`u64` / `u128`), en filtrant
/// éventuellement les relais par un ensemble de clés éligibles (`eligible_relays`, typiquement les membres de la toile
/// de confiance `Wot::members` pour neutraliser les attaques Sybil intra-diffusion).
///
/// Invariants garantis :
/// - aucun flottant ;
/// - la fiche est signée, en `OpenCommon`, conforme au mode tarifaire, et la chaîne remonte à son auteur ;
/// - chaque ligne est un multiple entier de `1 DeciZen` (`0,10 Ẑen = 0,01 Ğ1 = Amount(1)`) ;
/// - une seule ligne par `pubkey` (fusion création + diffusion / diamants) ;
/// - si `eligible_relays` est fourni, un relais hors ensemble est listé dans `filtered_relays` et reçoit `0` ; s'il ne
///   reste aucun relais éligible, 100 % va à la tranche Création ;
/// - **tout reste de division euclidienne (poussière) va toujours à `commons.author_pubkey`** ;
/// - `lines.iter().map(|l| l.amount.0).sum::<u64>() == total.0`.
pub fn compute_payout_batch_with_eligible(
    total: DeciZen,
    commons: &CommonsBlock,
    chain: &ShareChain,
    eligible_relays: Option<&BTreeSet<String>>,
) -> Result<PayoutBatch, CascadeError> {
    commons.validate_structure()?;
    if matches!(commons.rights, Rights::PrivateCopy) || !commons.verify_signature() {
        return Err(CascadeError::NotMonetizable);
    }
    if !commons.pricing.accepts(total) {
        return Err(CascadeError::InsufficientAmount);
    }
    chain.verify()?;
    if chain.work_sha != commons.content_sha || chain.origin_pubkey != commons.author_pubkey {
        return Err(CascadeError::ChainWorkMismatch);
    }

    let creation_shares: Vec<(&str, u16)> = if !commons.flattened_payouts.is_empty() {
        commons.flattened_payouts.iter().map(|p| (p.pubkey.as_str(), p.bps.0)).collect()
    } else {
        commons.credits.iter().map(|c| (c.pubkey.as_str(), c.bps.0)).collect()
    };

    let sum_bps: u32 = creation_shares.iter().map(|(_, b)| u32::from(*b)).sum();
    if sum_bps != u32::from(BPS_TOTAL) {
        return Err(CascadeError::BadCreationTable);
    }

    let mut acc: BTreeMap<String, u64> = BTreeMap::new();
    let total_u128 = u128::from(total.0);

    // 1. Tranche Diffusion : filtre les relais par la toile de confiance si demandée (anti-Sybil intra-diffusion).
    let all_relays = chain.unique_relays(&commons.author_pubkey);
    let (relays, filtered_relays): (Vec<&str>, Vec<String>) = match eligible_relays {
        Some(allowed) => {
            let mut ok = Vec::new();
            let mut out = Vec::new();
            for pk in all_relays {
                if allowed.contains(pk) {
                    ok.push(pk);
                } else {
                    out.push(pk.to_owned());
                }
            }
            (ok, out)
        }
        None => (all_relays, Vec::new()),
    };

    let diffusion_pool: u64 = if relays.is_empty() {
        0
    } else {
        ((total_u128 * u128::from(commons.split.diffusion_bps.0)) / u128::from(BPS_TOTAL)) as u64
    };
    let creation_pool: u64 = total.0.saturating_sub(diffusion_pool);

    // 2. Distribution de la tranche Création (selon la table aplatie du DAG).
    let mut distributed: u64 = 0;
    for (pk, bps) in creation_shares {
        if !is_hex64(pk) {
            return Err(CascadeError::BadHex("payout.pubkey"));
        }
        let part = ((u128::from(creation_pool) * u128::from(bps)) / u128::from(BPS_TOTAL)) as u64;
        if part > 0 {
            *acc.entry(pk.to_owned()).or_insert(0) += part;
            distributed += part;
        }
    }

    // 3. Distribution équitable de la tranche Diffusion entre les relais uniques éligibles.
    if !relays.is_empty() && diffusion_pool > 0 {
        let per_relay = diffusion_pool / (relays.len() as u64);
        if per_relay > 0 {
            for pk in relays {
                *acc.entry(pk.to_owned()).or_insert(0) += per_relay;
                distributed += per_relay;
            }
        }
    }

    // 4. Toute poussière (restes de divisions création + diffusion) revient à l'auteur principal.
    let remainder = total.0.saturating_sub(distributed);
    if remainder > 0 {
        *acc.entry(commons.author_pubkey.clone()).or_insert(0) += remainder;
    }

    let lines = acc
        .into_iter()
        .filter(|(_, v)| *v > 0)
        .map(|(to_pubkey, v)| TransferLine { to_pubkey, amount: DeciZen(v) })
        .collect();

    Ok(PayoutBatch { work_sha: commons.content_sha.clone(), total, lines, filtered_relays })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALICE_SK: [u8; 32] = [0xA1; 32];
    const BOB_SK: [u8; 32] = [0xB2; 32];
    const CHARLIE_SK: [u8; 32] = [0xC3; 32];
    const DAVID_SK: [u8; 32] = [0xD4; 32];
    const EVE_SK: [u8; 32] = [0xE5; 32];
    const FRANK_SK: [u8; 32] = [0xF6; 32];

    fn sha(byte: u8) -> String {
        hex::encode([byte; 32])
    }

    fn pk(sk: &[u8; 32]) -> String {
        pubkey_of(sk).unwrap()
    }

    fn make_alice_photo() -> CommonsBlock {
        let alice = pk(&ALICE_SK);
        let mut blk = CommonsBlock {
            v: 1,
            author_pubkey: alice.clone(),
            content_sha: sha(0x11),
            rights: Rights::OpenCommon { spdx: "CC-BY-SA-4.0".into(), allow_remix: true, remix_min_bps: Bps(4_000) },
            pricing: PricingMode::Hat { suggested_deci_zen: Some(DeciZen(10)), min_deci_zen: None },
            split: SplitRule::default(),
            credits: vec![Credit { pubkey: alice.clone(), role: "photo".into(), bps: Bps::FULL, attest_sig: None }],
            sources: Vec::new(),
            generation: 0,
            remix: None,
            flattened_payouts: vec![PayoutShare { pubkey: alice, bps: Bps::FULL }],
            signature: None,
        };
        blk.sign(&ALICE_SK).unwrap();
        blk
    }

    fn make_bob_botanical_remix(alice_blk: &CommonsBlock) -> CommonsBlock {
        let bob = pk(&BOB_SK);
        let content_sha = sha(0x22);
        let credits = vec![Credit { pubkey: bob.clone(), role: "identification-botanique".into(), bps: Bps(6_000), attest_sig: None }];
        let sources = vec![SourceRef {
            sha_plain: alice_blk.content_sha.clone(),
            cid: "bafkreialicephoto".into(),
            author_pubkey: alice_blk.author_pubkey.clone(),
            event_id: None,
            role: SourceRole::PrimaryMedia,
            share_bps: Bps(4_000),
        }];
        let parents = BTreeMap::from([(alice_blk.content_sha.clone(), alice_blk.clone())]);
        let (generation, flattened_payouts) = flatten_dag(&bob, &content_sha, &credits, &sources, &parents).unwrap();
        let mut blk = CommonsBlock {
            v: 1,
            author_pubkey: bob,
            content_sha,
            rights: Rights::OpenCommon { spdx: "CC-BY-SA-4.0".into(), allow_remix: true, remix_min_bps: Bps(3_000) },
            pricing: PricingMode::Fixed { price_deci_zen: DeciZen(25) },
            split: SplitRule::default(),
            credits,
            sources,
            generation,
            remix: Some(RemixKind::Recipe {
                tracks: vec![CompositionTrack {
                    source_sha: alice_blk.content_sha.clone(),
                    stream: "image".into(),
                    start_ms: None,
                    end_ms: None,
                    offset_ms: 0,
                    lang: None,
                }],
            }),
            flattened_payouts,
            signature: None,
        };
        blk.sign(&BOB_SK).unwrap();
        blk
    }

    #[test]
    fn decizen_units_align_with_g1_cents_and_zen_cents() {
        assert_eq!(DeciZen::from_zen(1), Some(DeciZen(10)));
        assert_eq!(DeciZen(1).to_g1_amount(), Amount(1));
        assert_eq!(DeciZen(1).to_zen_cents(), Some(10));
        assert_eq!(DeciZen(25).to_g1_amount(), Amount(25));
        assert_eq!(DeciZen(25).to_zen_cents(), Some(250));
        assert_eq!(DeciZen::from_zen(u64::MAX), None);
        assert_eq!(DeciZen(u64::MAX).to_zen_cents(), None);
    }

    #[test]
    fn botanical_remix_alice_bob_charlie_david_exact_payout() {
        let alice_blk = make_alice_photo();
        let bob_blk = make_bob_botanical_remix(&alice_blk);
        let parents = BTreeMap::from([(alice_blk.content_sha.clone(), alice_blk.clone())]);
        verify_flattened_dag(&bob_blk, &parents).unwrap();
        assert_eq!(bob_blk.generation, 1);

        // Bob → Charlie → David
        let c0 = ShareChain::new_root(&bob_blk.content_sha, "bafkreibobfiche", Some("bafkreibobfiche".into()), &bob_blk.author_pubkey).unwrap();
        let c1 = c0.extend(&BOB_SK, &pk(&CHARLIE_SK), 1_790_000_001).unwrap();
        let c2 = c1.extend(&CHARLIE_SK, &pk(&DAVID_SK), 1_790_000_002).unwrap();
        assert_eq!(c2.depth(), 2);
        c2.verify().unwrap();

        let batch = compute_payout_batch(DeciZen(25), &bob_blk, &c2).unwrap();
        let by_pk: BTreeMap<&str, u64> = batch.lines.iter().map(|l| (l.to_pubkey.as_str(), l.amount.0)).collect();
        assert_eq!(by_pk.get(pk(&ALICE_SK).as_str()), Some(&8));
        assert_eq!(by_pk.get(pk(&BOB_SK).as_str()), Some(&12));
        assert_eq!(by_pk.get(pk(&CHARLIE_SK).as_str()), Some(&5));
        assert_eq!(batch.lines.iter().map(|l| l.amount.0).sum::<u64>(), 25);
    }

    #[test]
    fn remainder_from_multiple_relays_goes_to_main_author_with_exact_conservation() {
        let alice_blk = make_alice_photo();
        let bob_blk = make_bob_botanical_remix(&alice_blk);

        // Bob → Charlie → Eve → Frank → David (3 relais : Charlie, Eve, Frank ; 5 / 3 = 1 chacun, reste 2 → Bob)
        let c = ShareChain::new_root(&bob_blk.content_sha, "bafkreibobfiche", None, &bob_blk.author_pubkey)
            .unwrap()
            .extend(&BOB_SK, &pk(&CHARLIE_SK), 100)
            .unwrap()
            .extend(&CHARLIE_SK, &pk(&EVE_SK), 101)
            .unwrap()
            .extend(&EVE_SK, &pk(&FRANK_SK), 102)
            .unwrap()
            .extend(&FRANK_SK, &pk(&DAVID_SK), 103)
            .unwrap();
        assert_eq!(c.depth(), 4);

        let batch = compute_payout_batch(DeciZen(25), &bob_blk, &c).unwrap();
        let by_pk: BTreeMap<&str, u64> = batch.lines.iter().map(|l| (l.to_pubkey.as_str(), l.amount.0)).collect();
        assert_eq!(by_pk.get(pk(&ALICE_SK).as_str()), Some(&8));
        assert_eq!(by_pk.get(pk(&BOB_SK).as_str()), Some(&14), "12 + 2 de reste");
        assert_eq!(by_pk.get(pk(&CHARLIE_SK).as_str()), Some(&1));
        assert_eq!(by_pk.get(pk(&EVE_SK).as_str()), Some(&1));
        assert_eq!(by_pk.get(pk(&FRANK_SK).as_str()), Some(&1));
        assert_eq!(batch.lines.iter().map(|l| l.amount.0).sum::<u64>(), 25);
    }

    #[test]
    fn diamond_dag_merges_ancestor_and_conserves_10000_bps() {
        let alice_blk = make_alice_photo(); // Alice 100 %
        let bob_blk = make_bob_botanical_remix(&alice_blk); // Bob 60 %, Alice 40 %

        // Charlie fait un autre remix d'Alice (Charlie 50 %, Alice 50 %)
        let charlie = pk(&CHARLIE_SK);
        let c_sha = sha(0x33);
        let c_credits = vec![Credit { pubkey: charlie.clone(), role: "audio".into(), bps: Bps(5_000), attest_sig: None }];
        let c_sources = vec![SourceRef {
            sha_plain: alice_blk.content_sha.clone(),
            cid: "bafkreialicephoto".into(),
            author_pubkey: alice_blk.author_pubkey.clone(),
            event_id: None,
            role: SourceRole::PrimaryMedia,
            share_bps: Bps(5_000),
        }];
        let p_alice = BTreeMap::from([(alice_blk.content_sha.clone(), alice_blk.clone())]);
        let (c_gen, c_flat) = flatten_dag(&charlie, &c_sha, &c_credits, &c_sources, &p_alice).unwrap();
        let mut charlie_blk = CommonsBlock {
            v: 1,
            author_pubkey: charlie,
            content_sha: c_sha,
            rights: Rights::OpenCommon { spdx: "CC-BY-SA-4.0".into(), allow_remix: true, remix_min_bps: Bps(2_000) },
            pricing: PricingMode::Free,
            split: SplitRule::default(),
            credits: c_credits,
            sources: c_sources,
            generation: c_gen,
            remix: Some(RemixKind::Rendered),
            flattened_payouts: c_flat,
            signature: None,
        };
        charlie_blk.sign(&CHARLIE_SK).unwrap();

        // David combine Bob (30 %) et Charlie (30 %) et garde 40 % → diamant sur Alice !
        // Alice = 30% * 40% (1200) + 30% * 50% (1500) = 2700 bps ; Bob = 1800 ; Charlie = 1500 ; David = 4000.
        let david = pk(&DAVID_SK);
        let d_sha = sha(0x44);
        let d_credits = vec![Credit { pubkey: david.clone(), role: "synthese".into(), bps: Bps(4_000), attest_sig: None }];
        let d_sources = vec![
            SourceRef {
                sha_plain: bob_blk.content_sha.clone(),
                cid: "bafkreibob".into(),
                author_pubkey: bob_blk.author_pubkey.clone(),
                event_id: None,
                role: SourceRole::Knowledge,
                share_bps: Bps(3_000),
            },
            SourceRef {
                sha_plain: charlie_blk.content_sha.clone(),
                cid: "bafkreicharlie".into(),
                author_pubkey: charlie_blk.author_pubkey.clone(),
                event_id: None,
                role: SourceRole::AudioTrack,
                share_bps: Bps(3_000),
            },
        ];
        let parents = BTreeMap::from([(bob_blk.content_sha.clone(), bob_blk), (charlie_blk.content_sha.clone(), charlie_blk)]);
        let (d_gen, d_flat) = flatten_dag(&david, &d_sha, &d_credits, &d_sources, &parents).unwrap();
        assert_eq!(d_gen, 2);
        let by_pk: BTreeMap<&str, u16> = d_flat.iter().map(|p| (p.pubkey.as_str(), p.bps.0)).collect();
        assert_eq!(by_pk.get(pk(&ALICE_SK).as_str()), Some(&2_700));
        assert_eq!(by_pk.get(pk(&BOB_SK).as_str()), Some(&1_800));
        assert_eq!(by_pk.get(pk(&CHARLIE_SK).as_str()), Some(&1_500));
        assert_eq!(by_pk.get(pk(&DAVID_SK).as_str()), Some(&4_000));
        assert_eq!(d_flat.iter().map(|p| u32::from(p.bps.0)).sum::<u32>(), u32::from(BPS_TOTAL));
    }

    #[test]
    fn serde_round_trip_covers_commons_plant_and_chain() {
        let alice_blk = make_alice_photo();
        let bob_blk = make_bob_botanical_remix(&alice_blk);
        let plant = PlantData {
            scientific: "Urtica dioica".into(),
            common: vec!["Grande ortie".into()],
            family: "Urticaceae".into(),
            gbif: Some("5361880".into()),
            umap: Some("_43.61_1.44".into()),
            observed_at: Some(1_790_000_000),
            edible: "Jeunes pousses cuites.".into(),
            ..Default::default()
        };
        let chain = ShareChain::new_root(&bob_blk.content_sha, "bafkreibob", None, &bob_blk.author_pubkey)
            .unwrap()
            .extend(&BOB_SK, &pk(&CHARLIE_SK), 1_790_000_001)
            .unwrap();

        let blk2: CommonsBlock = serde_json::from_str(&serde_json::to_string(&bob_blk).unwrap()).unwrap();
        assert_eq!(blk2, bob_blk);
        assert!(blk2.verify_signature());

        let plant2: PlantData = serde_json::from_str(&serde_json::to_string(&plant).unwrap()).unwrap();
        assert_eq!(plant2, plant);

        let chain2: ShareChain = serde_json::from_str(&serde_json::to_string(&chain).unwrap()).unwrap();
        assert_eq!(chain2, chain);
        chain2.verify().unwrap();
    }

    #[test]
    fn hostile_tampered_block_or_private_copy_is_refused() {
        let mut alice_blk = make_alice_photo();
        // Modification après signature
        alice_blk.pricing = PricingMode::Fixed { price_deci_zen: DeciZen(999) };
        assert!(!alice_blk.verify_signature());
        let c = ShareChain::new_root(&alice_blk.content_sha, "bafkrei1", None, &alice_blk.author_pubkey).unwrap();
        assert_eq!(compute_payout_batch(DeciZen(999), &alice_blk, &c), Err(CascadeError::NotMonetizable));

        // Copie privée : jamais monétisable ni remixable
        let mut priv_blk = make_alice_photo();
        priv_blk.rights = Rights::PrivateCopy;
        priv_blk.sign(&ALICE_SK).unwrap();
        assert_eq!(compute_payout_batch(DeciZen(10), &priv_blk, &c), Err(CascadeError::NotMonetizable));
        let parents = BTreeMap::from([(priv_blk.content_sha.clone(), priv_blk.clone())]);
        let res = flatten_dag(
            &pk(&BOB_SK),
            &sha(0x99),
            &[Credit { pubkey: pk(&BOB_SK), role: "edit".into(), bps: Bps(5_000), attest_sig: None }],
            &[SourceRef {
                sha_plain: priv_blk.content_sha.clone(),
                cid: "bafkrei1".into(),
                author_pubkey: priv_blk.author_pubkey.clone(),
                event_id: None,
                role: SourceRole::PrimaryMedia,
                share_bps: Bps(5_000),
            }],
            &parents,
        );
        assert!(matches!(res, Err(CascadeError::RemixForbidden(_))));
    }

    #[test]
    fn hostile_below_remix_floor_and_forged_flattened_table_are_refused() {
        let alice_blk = make_alice_photo(); // remix_min_bps = 4000
        let parents = BTreeMap::from([(alice_blk.content_sha.clone(), alice_blk.clone())]);
        let res = flatten_dag(
            &pk(&BOB_SK),
            &sha(0x77),
            &[Credit { pubkey: pk(&BOB_SK), role: "edit".into(), bps: Bps(7_000), attest_sig: None }],
            &[SourceRef {
                sha_plain: alice_blk.content_sha.clone(),
                cid: "bafkrei1".into(),
                author_pubkey: alice_blk.author_pubkey.clone(),
                event_id: None,
                role: SourceRole::PrimaryMedia,
                share_bps: Bps(3_000),
            }],
            &parents,
        );
        assert_eq!(res, Err(CascadeError::BelowRemixFloor { got: 3_000, min: 4_000 }));

        // Table aplatie falsifiée même si re-signée par Bob
        let mut forged = make_bob_botanical_remix(&alice_blk);
        forged.flattened_payouts = vec![PayoutShare { pubkey: pk(&BOB_SK), bps: Bps::FULL }];
        forged.sign(&BOB_SK).unwrap();
        assert_eq!(verify_flattened_dag(&forged, &parents), Err(CascadeError::FlattenedMismatch));
    }

    #[test]
    fn hostile_chain_tampering_replay_self_hop_and_mismatch_are_refused() {
        let alice_blk = make_alice_photo();
        let root = ShareChain::new_root(&alice_blk.content_sha, "bafkrei1", None, &alice_blk.author_pubkey).unwrap();

        // Auto-partage refusé
        assert!(matches!(root.extend(&ALICE_SK, &pk(&ALICE_SK), 10), Err(CascadeError::InvalidChain(_))));
        // Mauvais signataire (Bob essaie de signer à la place d'Alice)
        assert!(matches!(root.extend(&BOB_SK, &pk(&CHARLIE_SK), 10), Err(CascadeError::InvalidChain(_))));

        let c1 = root.extend(&ALICE_SK, &pk(&BOB_SK), 10).unwrap();
        let mut bad_sig = c1.clone();
        bad_sig.hops[0].to_pubkey = pk(&EVE_SK);
        assert_eq!(bad_sig.verify(), Err(CascadeError::BadSignature));

        let mut bad_rank = c1.clone();
        bad_rank.hops[0].rank = 2;
        assert!(matches!(bad_rank.verify(), Err(CascadeError::InvalidChain(_))));

        // Chaîne valide pour une autre œuvre ou un autre auteur → refusée au paiement
        let wrong_origin = ShareChain::new_root(&alice_blk.content_sha, "bafkrei1", None, pk(&EVE_SK)).unwrap();
        assert_eq!(compute_payout_batch(DeciZen(10), &alice_blk, &wrong_origin), Err(CascadeError::ChainWorkMismatch));
    }

    #[test]
    fn hostile_overflow_zero_and_fixed_price_underpayment_are_safe() {
        let alice_blk = make_alice_photo();
        let bob_blk = make_bob_botanical_remix(&alice_blk); // Fixed 25 DeciZen
        let c = ShareChain::new_root(&bob_blk.content_sha, "bafkreibob", None, &bob_blk.author_pubkey)
            .unwrap()
            .extend(&BOB_SK, &pk(&CHARLIE_SK), 10)
            .unwrap();

        assert_eq!(compute_payout_batch(DeciZen::ZERO, &bob_blk, &c), Err(CascadeError::InsufficientAmount));
        assert_eq!(compute_payout_batch(DeciZen(24), &bob_blk, &c), Err(CascadeError::InsufficientAmount));

        // u64::MAX sans débordement ni panique, conservation exacte
        let max_batch = compute_payout_batch(DeciZen(u64::MAX), &bob_blk, &c).unwrap();
        let sum: u128 = max_batch.lines.iter().map(|l| u128::from(l.amount.0)).sum();
        assert_eq!(sum, u128::from(u64::MAX));
    }

    #[test]
    fn credit_attestation_proves_coauthor_consent_and_rejects_tampering() {
        let work = sha(0x55);
        let mut c = Credit { pubkey: pk(&BOB_SK), role: "musique".into(), bps: Bps(3_000), attest_sig: None };
        assert!(!c.verify_attestation(&work));
        assert_eq!(c.attest(&ALICE_SK, &work), Err(CascadeError::BadSignature));
        c.attest(&BOB_SK, &work).unwrap();
        assert!(c.verify_attestation(&work));
        assert!(!c.verify_attestation(&sha(0x56)), "autre œuvre");
        let mut forged = c.clone();
        forged.bps = Bps(2_000);
        assert!(!forged.verify_attestation(&work), "part modifiée");
    }

    #[test]
    fn sybil_relays_outside_wot_are_excluded_from_diffusion_pool() {
        let alice_blk = make_alice_photo();
        let bob_blk = make_bob_botanical_remix(&alice_blk); // 60 % Bob, 40 % Alice ; 80/20 création/diffusion ; Fixed 25
        // Bob → Charlie (WoT) → Eve (Sybil) → Frank (Sybil) → David
        let c = ShareChain::new_root(&bob_blk.content_sha, "bafkreibob", None, &bob_blk.author_pubkey)
            .unwrap()
            .extend(&BOB_SK, &pk(&CHARLIE_SK), 1)
            .unwrap()
            .extend(&CHARLIE_SK, &pk(&EVE_SK), 2)
            .unwrap()
            .extend(&EVE_SK, &pk(&FRANK_SK), 3)
            .unwrap()
            .extend(&FRANK_SK, &pk(&DAVID_SK), 4)
            .unwrap();

        // Seul Charlie appartient à la WoT : les Sybils Eve et Frank touchent 0, Charlie touche toute la diffusion (5).
        let wot = BTreeSet::from([pk(&CHARLIE_SK)]);
        let batch = compute_payout_batch_with_eligible(DeciZen(25), &bob_blk, &c, Some(&wot)).unwrap();
        let by_pk: BTreeMap<&str, u64> = batch.lines.iter().map(|l| (l.to_pubkey.as_str(), l.amount.0)).collect();
        assert_eq!(by_pk.get(pk(&ALICE_SK).as_str()), Some(&8));
        assert_eq!(by_pk.get(pk(&BOB_SK).as_str()), Some(&12));
        assert_eq!(by_pk.get(pk(&CHARLIE_SK).as_str()), Some(&5));
        assert_eq!(by_pk.get(pk(&EVE_SK).as_str()), None);
        assert_eq!(by_pk.get(pk(&FRANK_SK).as_str()), None);
        assert_eq!(batch.filtered_relays.len(), 2);
        assert_eq!(batch.lines.iter().map(|l| l.amount.0).sum::<u64>(), 25);

        // Si aucun relais n'est dans la WoT : 100 % va à la Création (10 Alice + 15 Bob = 25).
        let empty = BTreeSet::new();
        let all_creation = compute_payout_batch_with_eligible(DeciZen(25), &bob_blk, &c, Some(&empty)).unwrap();
        let by_pk2: BTreeMap<&str, u64> = all_creation.lines.iter().map(|l| (l.to_pubkey.as_str(), l.amount.0)).collect();
        assert_eq!(by_pk2.get(pk(&ALICE_SK).as_str()), Some(&10));
        assert_eq!(by_pk2.get(pk(&BOB_SK).as_str()), Some(&15));
        assert_eq!(all_creation.filtered_relays.len(), 3);
        assert_eq!(all_creation.lines.iter().map(|l| l.amount.0).sum::<u64>(), 25);
    }
}
