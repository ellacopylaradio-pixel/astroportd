//! Notifications Web Push du chat, **sans contenu** : quand un message arrive, la station réveille le navigateur du membre
//! (même onglet fermé) ; le service worker affiche « Nouveau message ». Rien du message ne passe par le service de push du
//! navigateur (Google, Mozilla, Apple) : ni texte, ni expéditeur — il ne voit qu'un réveil.
//!
//! - **VAPID** (RFC 8292) : la station signe chaque réveil avec sa clé P-256, créée une fois et gardée dans `kv`
//!   (`web_push`/`vapid`) ; le navigateur n'accepte que les réveils signés par la clé donnée à l'abonnement.
//! - Abonnements : `kv` `web_push_sub`, clé `<membre>:<empreinte de l'adresse>`, au plus [`MAX_SUBS`] par membre ; une adresse
//!   que le service dit disparue (404, 410) est oubliée.
//! - Un réveil au plus toutes les [`QUIET_SECS`] par membre : une rafale de messages ne fait sonner qu'une fois.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use astro_db::Db;
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use ring::rand::SystemRandom;
use ring::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair};
use rusqlite::OptionalExtension;
use sha2::Digest;

use crate::fetch::{FetchError, FetchLimits, request};

/// Navigateurs abonnés au plus par membre.
pub const MAX_SUBS: usize = 10;
/// Silence minimal entre deux réveils d'un même membre.
pub const QUIET_SECS: i64 = 20;
const SCOPE_KEY: &str = "web_push";
const SCOPE_SUB: &str = "web_push_sub";

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

/// Service de réveil des navigateurs.
pub struct WebPush {
    db: Arc<Db>,
    last: Mutex<HashMap<String, i64>>,
}

impl WebPush {
    /// Service sur la base de la station.
    pub fn new(db: Arc<Db>) -> Self {
        Self { db, last: Mutex::default() }
    }

    /// Clé de signature de la station (créée à la première demande).
    async fn key(&self) -> Result<EcdsaKeyPair, String> {
        let rng = SystemRandom::new();
        let stored: Option<String> = self.db.readers().read(|c| Ok(c.query_row("SELECT value FROM kv WHERE scope = ?1 AND key = 'vapid'", [SCOPE_KEY], |r| r.get(0)).optional()?)).await.map_err(|e| e.to_string())?;
        let pkcs8 = match stored.and_then(|s| STANDARD.decode(s).ok()) {
            Some(k) => k,
            None => {
                let k = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).map_err(|_| "clé VAPID : génération impossible".to_owned())?;
                let v = STANDARD.encode(k.as_ref());
                // Deux demandes simultanées : la première écrite gagne, la seconde relit la même.
                self.db.writer().execute("INSERT OR IGNORE INTO kv(scope, key, value, updated_at) VALUES (?1, 'vapid', ?2, ?3)", vec![SCOPE_KEY.to_owned().into(), v.into(), now().into()]).await.map_err(|e| e.to_string())?;
                let again: String = self.db.readers().read(|c| Ok(c.query_row("SELECT value FROM kv WHERE scope = ?1 AND key = 'vapid'", [SCOPE_KEY], |r| r.get(0))?)).await.map_err(|e| e.to_string())?;
                STANDARD.decode(again).map_err(|e| e.to_string())?
            }
        };
        EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &pkcs8, &rng).map_err(|_| "clé VAPID illisible".to_owned())
    }

    /// Clé publique à donner au navigateur (`applicationServerKey`), en base64url.
    pub async fn public_key(&self) -> Result<String, String> {
        Ok(URL_SAFE_NO_PAD.encode(self.key().await?.public_key().as_ref()))
    }

    fn sub_key(owner: &str, endpoint: &str) -> String {
        let h = sha2::Sha256::digest(endpoint.as_bytes());
        format!("{owner}:{}", h.iter().take(12).map(|b| format!("{b:02x}")).collect::<String>())
    }

    /// Abonne (`on`) ou désabonne un navigateur du membre.
    pub async fn subscribe(&self, owner: &str, endpoint: &str, on: bool) -> Result<(), String> {
        let u = url::Url::parse(endpoint).map_err(|_| "adresse d'abonnement invalide".to_owned())?;
        if u.scheme() != "https" || endpoint.len() > 1000 || u.host_str().is_none() {
            return Err("adresse d'abonnement invalide (https attendu)".into());
        }
        let key = Self::sub_key(owner, endpoint);
        if !on {
            self.db.writer().execute("DELETE FROM kv WHERE scope = ?1 AND key = ?2", vec![SCOPE_SUB.to_owned().into(), key.into()]).await.map_err(|e| e.to_string())?;
            return Ok(());
        }
        let n = self.endpoints(owner).await.len();
        if n >= MAX_SUBS && !self.endpoints(owner).await.iter().any(|e| e == endpoint) {
            return Err(format!("{MAX_SUBS} navigateurs abonnés au plus : désabonnez-en un"));
        }
        self.db.writer().execute("INSERT OR REPLACE INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4)", vec![SCOPE_SUB.to_owned().into(), key.into(), endpoint.to_owned().into(), now().into()]).await.map_err(|e| e.to_string())?;
        Ok(())
    }

    async fn endpoints(&self, owner: &str) -> Vec<String> {
        let prefix = format!("{owner}:%");
        self.db
            .readers()
            .read(move |c| {
                let mut s = c.prepare("SELECT value FROM kv WHERE scope = ?1 AND key LIKE ?2")?;
                Ok(s.query_map([SCOPE_SUB, prefix.as_str()], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .unwrap_or_default()
    }

    /// Jeton VAPID pour le service de push de `endpoint` (valable 12 h).
    fn token(key: &EcdsaKeyPair, endpoint: &url::Url) -> Result<String, String> {
        let aud = format!("{}://{}", endpoint.scheme(), endpoint.host_str().unwrap_or_default());
        let head = URL_SAFE_NO_PAD.encode(br#"{"typ":"JWT","alg":"ES256"}"#);
        let claims = URL_SAFE_NO_PAD.encode(serde_json::json!({ "aud": aud, "exp": now() + 12 * 3600, "sub": "https://astroport.one" }).to_string());
        let msg = format!("{head}.{claims}");
        let sig = key.sign(&SystemRandom::new(), msg.as_bytes()).map_err(|_| "signature VAPID impossible".to_owned())?;
        Ok(format!("{msg}.{}", URL_SAFE_NO_PAD.encode(sig.as_ref())))
    }

    /// Réveille les navigateurs du membre (au plus une fois par [`QUIET_SECS`]). Rend le nombre de réveils acceptés.
    pub async fn notify(&self, owner: &str) -> usize {
        {
            let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
            let t = now();
            if last.get(owner).is_some_and(|&l| t - l < QUIET_SECS) {
                return 0;
            }
            last.insert(owner.to_owned(), t);
        }
        let subs = self.endpoints(owner).await;
        if subs.is_empty() {
            return 0;
        }
        let Ok(key) = self.key().await else { return 0 };
        let pk = URL_SAFE_NO_PAD.encode(key.public_key().as_ref());
        let mut limits = FetchLimits::new(64 * 1024);
        limits.timeout = Duration::from_secs(10);
        limits.max_redirects = 0;
        let mut ok = 0;
        for endpoint in subs {
            let Ok(u) = url::Url::parse(&endpoint) else { continue };
            let Ok(jwt) = Self::token(&key, &u) else { continue };
            let headers = [("TTL", "86400".to_owned()), ("Urgency", "high".to_owned()), ("Authorization", format!("vapid t={jwt}, k={pk}"))];
            match request("POST", &endpoint, &headers, Some(bytes::Bytes::new()), &limits).await {
                Ok(_) => ok += 1,
                Err(FetchError::Status(404 | 410)) => {
                    let _ = self.subscribe(owner, &endpoint, false).await;
                }
                Err(e) => eprintln!("web push : réveil non remis ({}) : {e}", u.host_str().unwrap_or_default()),
            }
        }
        ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db(tag: &str) -> Arc<Db> {
        let dir = std::env::temp_dir().join(format!("astroportd-push-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Arc::new(Db::open(&dir.join("p.db"), 2).unwrap())
    }

    #[tokio::test]
    async fn the_station_key_is_stable_and_signs_a_valid_vapid_token() {
        let p = WebPush::new(db("key"));
        let k1 = p.public_key().await.unwrap();
        assert_eq!(k1, p.public_key().await.unwrap(), "la clé est gardée");
        assert_eq!(URL_SAFE_NO_PAD.decode(&k1).unwrap().len(), 65, "point P-256 non compressé");
        let key = p.key().await.unwrap();
        let jwt = WebPush::token(&key, &url::Url::parse("https://push.example.org/abc").unwrap()).unwrap();
        let parts: Vec<&str> = jwt.split('.').collect();
        let claims: serde_json::Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
        assert_eq!(claims["aud"], "https://push.example.org");
        let pk = ring::signature::UnparsedPublicKey::new(&ring::signature::ECDSA_P256_SHA256_FIXED, URL_SAFE_NO_PAD.decode(&k1).unwrap());
        assert!(pk.verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &URL_SAFE_NO_PAD.decode(parts[2]).unwrap()).is_ok());
    }

    #[tokio::test]
    async fn subscriptions_are_https_bounded_and_removable() {
        let p = WebPush::new(db("subs"));
        assert!(p.subscribe("a", "http://push.example.org/x", true).await.is_err());
        for i in 0..MAX_SUBS {
            p.subscribe("a", &format!("https://push.example.org/{i}"), true).await.unwrap();
        }
        assert!(p.subscribe("a", "https://push.example.org/trop", true).await.is_err());
        p.subscribe("a", "https://push.example.org/0", true).await.unwrap();
        p.subscribe("a", "https://push.example.org/0", false).await.unwrap();
        assert_eq!(p.endpoints("a").await.len(), MAX_SUBS - 1);
        assert!(p.endpoints("b").await.is_empty());
        // Une rafale : un seul essai de réveil (le second est retenu sans réseau).
        p.last.lock().unwrap().insert("b".into(), now());
        assert_eq!(p.notify("b").await, 0);
    }
}
