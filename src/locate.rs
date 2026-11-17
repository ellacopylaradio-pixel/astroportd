//! « Retrouver ma station » : où vit le MULTIPASS d'un email ou d'une clé Nostr ?
//!
//! Un membre de la station B qui ouvre la station A ne doit ni tomber sur un refus muet, ni confier son code PASS à une
//! station qui n'est pas la sienne. La connexion demande donc d'abord l'identifiant, puis :
//!
//! - **ici** : le compte est hébergé sur cette station, on demande le PASS ;
//! - **ailleurs** : la station connaît sa station d'accueil (`identities.home_station`, tirée du DID kind 30800 public)
//!   et l'adresse de celle-ci (sa balise kind 30850) : on y envoie le membre, sans jamais lui demander son PASS ;
//! - **inconnu** : réponse neutre.
//!
//! Ce n'est pas une fuite nouvelle : l'email (`alsoKnownAs: mailto:…`) et la station d'accueil sont déjà publics dans le
//! DID du membre. La route publique qui s'en sert est tout de même bornée par adresse (pas de moissonnage).

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Mutex, OnceLock};

use astro_db::{Db, DbError};
use serde_json::Value;

/// Recherches au plus par adresse et par minute.
pub const PER_MINUTE: u32 = 20;
/// Adresses suivies au plus (au-delà, la table repart de zéro : la mémoire reste bornée).
const MAX_TRACKED: usize = 10_000;

/// Limite de débit de la recherche, par adresse IP et fenêtre d'une minute.
#[derive(Default)]
pub struct Limiter {
    seen: HashMap<IpAddr, (u64, u32)>,
}

impl Limiter {
    /// `true` si `ip` peut encore chercher à l'instant `now` (secondes). Sans adresse connue (ligne de commande, tests
    /// internes), toujours permis.
    pub fn allow(&mut self, ip: Option<IpAddr>, now: u64) -> bool {
        let Some(ip) = ip else { return true };
        if self.seen.len() >= MAX_TRACKED {
            self.seen.retain(|_, (t, _)| now.saturating_sub(*t) < 60);
            if self.seen.len() >= MAX_TRACKED {
                self.seen.clear();
            }
        }
        let e = self.seen.entry(ip).or_insert((now, 0));
        if now.saturating_sub(e.0) >= 60 {
            *e = (now, 0);
        }
        e.1 += 1;
        e.1 <= PER_MINUTE
    }
}

/// Le limiteur de la station (partagé par toutes les requêtes).
pub fn limiter() -> &'static Mutex<Limiter> {
    static L: OnceLock<Mutex<Limiter>> = OnceLock::new();
    L.get_or_init(Mutex::default)
}

/// Où vit un compte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    /// Hébergé sur cette station.
    Here,
    /// Hébergé sur la station à cette adresse publique (`https://…`, sans chemin).
    Elsewhere(String),
    /// Ni ici, ni ailleurs à notre connaissance.
    Unknown,
}

/// Adresse publique (`url` de sa balise kind 30850) de la station d'accueil `home` (`…:<NODE hex>`, format de
/// `identities.home_station`). `None` si la balise est absente ou si l'adresse n'est pas une origine `http(s)://hôte`.
pub fn home_url(c: &rusqlite::Connection, home: &str) -> rusqlite::Result<Option<String>> {
    use rusqlite::OptionalExtension;
    let Some(node) = home.split(':').nth(1) else { return Ok(None) };
    let summary: Option<String> = c.query_row("SELECT value FROM kv WHERE scope = 'economic_health' AND key = ?1", [node], |r| r.get(0)).optional()?;
    let url = summary.and_then(|s| serde_json::from_str::<Value>(&s).ok()).and_then(|v| v["tags"]["url"].as_str().map(|u| u.trim_end_matches('/').to_owned()));
    Ok(url.filter(|u| (u.starts_with("https://") || u.starts_with("http://")) && u.find("//").is_some_and(|i| !u[i + 2..].is_empty() && !u[i + 2..].contains(['/', ' ', '?', '#']))))
}

/// Où vit le compte de `q` (email, `npub1…` ou clé hexadécimale de 64 caractères).
pub async fn locate(db: &Db, q: &str) -> Result<Found, DbError> {
    let q = q.trim();
    let (email, hex) = if q.contains('@') { (Some(q.to_lowercase()), None) } else { (None, astro_dav::vcard::nostr_key(q)) };
    if email.is_none() && hex.is_none() {
        return Ok(Found::Unknown);
    }
    db.readers()
        .read(move |c| {
            use rusqlite::OptionalExtension;
            let row: Option<(bool, Option<String>)> = c
                .query_row(
                    "SELECT hosted = 1, home_station FROM identities WHERE (lower(email) = ?1 OR hex = ?2) AND role IN ('multipass', 'captain') \
                     ORDER BY hosted DESC, (home_station IS NOT NULL) DESC LIMIT 1",
                    rusqlite::params![email, hex],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            Ok(match row {
                Some((true, _)) => Found::Here,
                Some((false, Some(home))) => home_url(c, &home)?.map_or(Found::Unknown, Found::Elsewhere),
                _ => Found::Unknown,
            })
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_harvester_is_stopped_after_twenty_lookups_a_minute() {
        let mut l = Limiter::default();
        let (a, b): (IpAddr, IpAddr) = ("192.0.2.1".parse().unwrap(), "192.0.2.2".parse().unwrap());
        assert!((0..PER_MINUTE).all(|_| l.allow(Some(a), 1_000)));
        assert!(!l.allow(Some(a), 1_030), "21e recherche de la minute : refusée");
        assert!(l.allow(Some(b), 1_030), "une autre adresse n'est pas touchée");
        assert!(l.allow(Some(a), 1_061), "la minute suivante repart");
        assert!((0..100).all(|_| l.allow(None, 1_000)), "sans adresse (interne) : jamais bornée");
        for i in 0..(MAX_TRACKED as u32 + 5) {
            l.allow(Some(IpAddr::from(i.to_be_bytes())), 5_000);
        }
        assert!(l.seen.len() <= MAX_TRACKED, "mémoire bornée face à une nuée d'adresses");
    }
}
