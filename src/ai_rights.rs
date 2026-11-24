//! **Droits d'IA des membres**, décidés par le Capitaine (écran *Membres*, conception `docs/design/IA.md`).
//!
//! - **Partage** : le Capitaine prête son IA (celle qu'il a déclarée dans sa propre carte, [`crate::member_ai`]) à ses
//!   membres. Sa clé reste chiffrée pour lui : la station s'en sert pour eux sans jamais la leur montrer.
//! - **Par membre, et par défaut** : le mode ([`Mode`]), un quota de jetons par semaine (0 : sans limite) et un prix en Ẑen par
//!   semaine (0 : compris dans l'hébergement), prélevé avec l'hébergement (lot 4).
//! - **Liens de parrainage** : le lien d'inscription de la coopérative chez un fournisseur (MixRoute…), proposé aux membres qui
//!   n'ont pas de compte, et dit comme tel.
//!
//! Stockage : `kv`, portée `ai_rights` (état local de la station, comme `hosting_price`) : `default`, `<hex>`, `share`
//! (le Capitaine qui partage) et `referral:<fournisseur>`.

use std::sync::Arc;

use astro_db::Db;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::ai_client::PRESETS;

/// Portée `kv`.
pub const KV_SCOPE: &str = "ai_rights";
/// Prix maximal de l'IA, en Ẑen par semaine (même plafond que l'hébergement).
pub const MAX_WEEKLY_ZEN: u64 = 10_000;
/// Quota maximal, en jetons par semaine.
pub const MAX_WEEKLY_TOKENS: u64 = 1_000_000_000;

/// Ce qu'un membre peut utiliser.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Aucune IA : BRO reste masqué.
    Off,
    /// L'IA que le Capitaine partage (défaut).
    #[default]
    Station,
    /// L'IA de la station, et en plus sa propre clé ou un service de la station qu'il choisit.
    Own,
}

impl Mode {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "off" => Some(Self::Off),
            "station" => Some(Self::Station),
            "own" => Some(Self::Own),
            _ => None,
        }
    }
}

/// Droits d'un membre.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Rights {
    /// Ce qu'il peut utiliser.
    #[serde(default)]
    pub mode: Mode,
    /// Jetons par semaine (0 : sans limite).
    #[serde(default)]
    pub quota_tokens_week: u64,
    /// Ẑen par semaine pour l'IA (0 : compris).
    #[serde(default)]
    pub price_zen_week: u64,
}

impl Rights {
    /// Vue JSON.
    #[must_use]
    pub fn view(&self) -> Value {
        json!({ "mode": self.mode, "quota_tokens_week": self.quota_tokens_week, "price_zen_week": self.price_zen_week })
    }

    /// Applique un correctif `{ mode?, quota_tokens_week?, price_zen_week? }` (champ absent : inchangé).
    fn patched(mut self, patch: &Value) -> Result<Self, String> {
        if let Some(m) = patch.get("mode") {
            self.mode = m.as_str().and_then(Mode::parse).ok_or("mode : off, station ou own")?;
        }
        if let Some(q) = patch.get("quota_tokens_week") {
            self.quota_tokens_week = q.as_u64().filter(|q| *q <= MAX_WEEKLY_TOKENS).ok_or(format!("quota : nombre entier de jetons, {MAX_WEEKLY_TOKENS} au plus"))?;
        }
        if let Some(p) = patch.get("price_zen_week") {
            self.price_zen_week = p.as_u64().filter(|p| *p <= MAX_WEEKLY_ZEN).ok_or(format!("prix : nombre entier de Ẑen par semaine, {MAX_WEEKLY_ZEN} au plus"))?;
        }
        Ok(self)
    }
}

/// Liens de parrainage **par défaut**, ceux de la coopérative (tête de la chaîne d'affiliation) : proposés par toute station
/// dont le Capitaine n'a pas posé le sien ; le sien les remplace pour ses membres.
pub const DEFAULT_REFERRALS: [(&str, &str); 1] = [("mixroute", "https://console.mixroute.ai/invite/lsAE4ss3")];

/// Domaines où un lien de parrainage peut mener, par fournisseur (le lien vient du Capitaine, mais il est montré aux membres :
/// il ne doit pas pouvoir les envoyer ailleurs).
fn referral_domains(provider: &str) -> &'static [&'static str] {
    match provider {
        "mixroute" => &["mixroute.ai"],
        "openrouter" => &["openrouter.ai"],
        "mistral" => &["mistral.ai"],
        "gemini" => &["google.com"],
        "anthropic" => &["anthropic.com", "claude.com"],
        _ => &[],
    }
}

/// Un lien de parrainage valide pour `provider` : HTTPS, sans identifiants, sur un domaine du fournisseur.
fn valid_referral(provider: &str, raw: &str) -> Result<String, String> {
    let raw = raw.trim();
    let u = url::Url::parse(raw).map_err(|_| "lien illisible".to_owned())?;
    let host = u.host_str().unwrap_or_default().to_lowercase();
    let ok_host = referral_domains(provider).iter().any(|d| host == *d || host.ends_with(&format!(".{d}")));
    if u.scheme() != "https" || !u.username().is_empty() || u.password().is_some() || !ok_host || raw.len() > 512 {
        return Err(format!("lien de parrainage : une adresse https du site du fournisseur ({})", referral_domains(provider).join(", ")));
    }
    Ok(raw.to_owned())
}

/// Échec d'une opération sur les droits.
#[derive(Debug, PartialEq, Eq)]
pub enum RightsError {
    /// Saisie refusée.
    Invalid(String),
    /// Panne de la base.
    Internal(String),
}

/// Service des droits d'IA.
pub struct AiRights {
    db: Arc<Db>,
}

impl AiRights {
    /// Service sur `db`.
    #[must_use]
    pub fn new(db: Arc<Db>) -> Self {
        Self { db }
    }

    async fn get_kv(&self, key: &str) -> Result<Option<String>, RightsError> {
        let k = key.to_owned();
        self.db
            .readers()
            .read(move |c| Ok(rusqlite::OptionalExtension::optional(c.query_row("SELECT value FROM kv WHERE scope = ?1 AND key = ?2", rusqlite::params![KV_SCOPE, k], |r| r.get(0)))?))
            .await
            .map_err(|e| RightsError::Internal(e.to_string()))
    }

    async fn put_kv(&self, key: &str, value: Option<String>) -> Result<(), RightsError> {
        let at = i64::try_from(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())).unwrap_or(0);
        let r = match value {
            Some(v) => self.db.writer().execute("INSERT INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at", vec![KV_SCOPE.to_owned().into(), key.to_owned().into(), v.into(), at.into()]).await,
            None => self.db.writer().execute("DELETE FROM kv WHERE scope = ?1 AND key = ?2", vec![KV_SCOPE.to_owned().into(), key.to_owned().into()]).await,
        };
        r.map(|_| ()).map_err(|e| RightsError::Internal(e.to_string()))
    }

    /// Droits par défaut (ceux des membres que le Capitaine n'a pas réglés un par un).
    ///
    /// # Errors
    /// Base indisponible.
    pub async fn default_rights(&self) -> Result<Rights, RightsError> {
        Ok(self.get_kv("default").await?.and_then(|v| serde_json::from_str(&v).ok()).unwrap_or_default())
    }

    /// Droits de `hex` et s'ils lui sont propres (`false` : ceux par défaut).
    ///
    /// # Errors
    /// Base indisponible.
    pub async fn of(&self, hex: &str) -> Result<(Rights, bool), RightsError> {
        match self.get_kv(hex).await?.and_then(|v| serde_json::from_str(&v).ok()) {
            Some(r) => Ok((r, true)),
            None => Ok((self.default_rights().await?, false)),
        }
    }

    /// Change les droits par défaut.
    ///
    /// # Errors
    /// Saisie invalide, base indisponible.
    pub async fn set_default(&self, patch: &Value) -> Result<Rights, RightsError> {
        let r = self.default_rights().await?.patched(patch).map_err(RightsError::Invalid)?;
        self.put_kv("default", Some(json!(r).to_string())).await?;
        Ok(r)
    }

    /// Change les droits propres de `hex` (`patch` nul : retour aux droits par défaut).
    ///
    /// # Errors
    /// Compte invalide, saisie invalide, base indisponible.
    pub async fn set_member(&self, hex: &str, patch: Option<&Value>) -> Result<(Rights, bool), RightsError> {
        if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(RightsError::Invalid("compte inconnu".into()));
        }
        let Some(patch) = patch else {
            self.put_kv(hex, None).await?;
            return self.of(hex).await;
        };
        let r = self.of(hex).await?.0.patched(patch).map_err(RightsError::Invalid)?;
        self.put_kv(hex, Some(json!(r).to_string())).await?;
        Ok((r, true))
    }

    /// Le Capitaine qui partage son IA, s'il le fait.
    ///
    /// # Errors
    /// Base indisponible.
    pub async fn shared_by(&self) -> Result<Option<String>, RightsError> {
        self.get_kv("share").await
    }

    /// Le Capitaine `hex` partage (ou cesse de partager) son IA avec les membres.
    ///
    /// # Errors
    /// Base indisponible.
    pub async fn share(&self, hex: &str, on: bool) -> Result<(), RightsError> {
        self.put_kv("share", on.then(|| hex.to_owned())).await
    }

    /// Liens de parrainage, par fournisseur : celui du Capitaine, sinon celui de la coopérative ([`DEFAULT_REFERRALS`]).
    ///
    /// # Errors
    /// Base indisponible.
    pub async fn referrals(&self) -> Result<serde_json::Map<String, Value>, RightsError> {
        let mut out = serde_json::Map::new();
        for p in PRESETS {
            let own = self.get_kv(&format!("referral:{}", p.0)).await?;
            if let Some(url) = own.or_else(|| DEFAULT_REFERRALS.iter().find(|d| d.0 == p.0).map(|d| d.1.to_owned())) {
                out.insert(p.0.to_owned(), Value::String(url));
            }
        }
        Ok(out)
    }

    /// Pose (ou retire, `url` nul : retour au lien de la coopérative s'il y en a un) le lien de parrainage de `provider`.
    ///
    /// # Errors
    /// Fournisseur inconnu, lien hors du site du fournisseur, base indisponible.
    pub async fn set_referral(&self, provider: &str, url: Option<&str>) -> Result<(), RightsError> {
        if !PRESETS.iter().any(|p| p.0 == provider) {
            return Err(RightsError::Invalid(format!("fournisseur inconnu : {provider}")));
        }
        let url = url.map(str::trim).filter(|u| !u.is_empty()).map(|u| valid_referral(provider, u)).transpose().map_err(RightsError::Invalid)?;
        self.put_kv(&format!("referral:{provider}"), url).await
    }

    /// Réglages de chaque membre qui en a de propres : `hex → droits`.
    ///
    /// # Errors
    /// Base indisponible.
    pub async fn members(&self) -> Result<serde_json::Map<String, Value>, RightsError> {
        let rows: Vec<(String, String)> = self
            .db
            .readers()
            .read(|c| Ok(c.prepare("SELECT key, value FROM kv WHERE scope = ?1 AND length(key) = 64")?.query_map([KV_SCOPE], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?))
            .await
            .map_err(|e| RightsError::Internal(e.to_string()))?;
        Ok(rows.into_iter().filter_map(|(k, v)| serde_json::from_str::<Rights>(&v).ok().map(|r| (k, r.view()))).collect())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn rig(tag: &str) -> (AiRights, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("astroportd-airights-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        (AiRights::new(Arc::new(Db::open(&dir.join("u.db"), 2).unwrap())), dir)
    }

    #[tokio::test]
    async fn a_member_follows_the_default_until_set_one_by_one_and_can_go_back() {
        let (r, dir) = rig("rights");
        let alice = "a1".repeat(32);
        assert_eq!(r.of(&alice).await.unwrap(), (Rights::default(), false), "défaut : l'IA de la station, sans limite, comprise");
        r.set_default(&json!({ "quota_tokens_week": 200_000, "price_zen_week": 2 })).await.unwrap();
        assert_eq!(r.of(&alice).await.unwrap().0.price_zen_week, 2, "le défaut s'applique");
        let (own, set) = r.set_member(&alice, Some(&json!({ "mode": "own" }))).await.unwrap();
        assert_eq!((own.mode, own.quota_tokens_week, set), (Mode::Own, 200_000, true), "réglé à partir de ce qui s'appliquait");
        r.set_default(&json!({ "mode": "off" })).await.unwrap();
        assert_eq!(r.of(&alice).await.unwrap().0.mode, Mode::Own, "un réglage propre ne suit plus le défaut");
        assert_eq!(r.members().await.unwrap().len(), 1);
        assert_eq!(r.set_member(&alice, None).await.unwrap(), (Rights { mode: Mode::Off, quota_tokens_week: 200_000, price_zen_week: 2 }, false), "retour au défaut");
        for bad in [json!({ "mode": "tout" }), json!({ "quota_tokens_week": -1 }), json!({ "price_zen_week": 10_001 }), json!({ "price_zen_week": 1.5 })] {
            assert!(matches!(r.set_member(&alice, Some(&bad)).await, Err(RightsError::Invalid(_))), "{bad}");
        }
        assert!(matches!(r.set_member("pas-un-compte", Some(&json!({}))).await, Err(RightsError::Invalid(_))));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn the_captain_shares_and_a_referral_link_stays_on_the_provider_site() {
        let (r, dir) = rig("share");
        let cap = "c0".repeat(32);
        assert_eq!(r.shared_by().await.unwrap(), None);
        r.share(&cap, true).await.unwrap();
        assert_eq!(r.shared_by().await.unwrap().as_deref(), Some(cap.as_str()));
        r.share(&cap, false).await.unwrap();
        assert_eq!(r.shared_by().await.unwrap(), None);
        // Sans réglage : le lien de la coopérative.
        assert_eq!(r.referrals().await.unwrap()["mixroute"], DEFAULT_REFERRALS[0].1);
        assert!(valid_referral("mixroute", DEFAULT_REFERRALS[0].1).is_ok(), "le lien par défaut passe la même règle");
        r.set_referral("mixroute", Some("https://console.mixroute.ai/invite/CAPITAINE")).await.unwrap();
        assert_eq!(r.referrals().await.unwrap()["mixroute"], "https://console.mixroute.ai/invite/CAPITAINE", "celui du Capitaine le remplace");
        for bad in ["http://mixroute.ai/x", "https://mixroute.ai.evil.example/x", "https://evil.example/?mixroute.ai", "https://u:p@mixroute.ai/", "javascript:alert(1)"] {
            assert!(matches!(r.set_referral("mixroute", Some(bad)).await, Err(RightsError::Invalid(_))), "{bad}");
        }
        assert!(r.set_referral("inconnu", Some("https://x.example")).await.is_err());
        r.set_referral("mixroute", None).await.unwrap();
        assert_eq!(r.referrals().await.unwrap()["mixroute"], DEFAULT_REFERRALS[0].1, "retiré : retour au lien de la coopérative");
        assert_eq!(r.referrals().await.unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }
}
