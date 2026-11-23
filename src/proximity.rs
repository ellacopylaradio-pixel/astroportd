//! Niveaux de proximité : chaque membre range ses proches en cinq cercles, **en privé**.
//!
//! | Niveau | Nom | Par défaut |
//! |---|---|---|
//! | 5 | Famille | — |
//! | 4 | Proche | — |
//! | 3 | Ami | ami réciproque (double suivi) |
//! | 2 | Connaissance | — |
//! | 1 | Contact | suivi sans retour |
//!
//! Source de vérité : **un** événement kind 30078 (`d` = [`D_TAG`]) signé par le membre, contenu chiffré pour lui seul
//! (NIP-44 vers soi-même) : `{ "<clé>": niveau }`, seulement les exceptions au niveau par défaut. Jamais dans la liste de
//! contacts (kind 3) : personne, ni le relai ni la personne rangée, ne sait où elle l'est. L'événement voyage avec les autres
//! événements du membre (rejouable). On ne range que des personnes que l'on suit.
//!
//! Usages branchés : avertissement au moment de payer (`m-pay.js`) ; ordre des gardiens conseillés, après « sur un autre
//! Astroport » (`private_backup`). Les autres droits (photos, appels, confiance en île) s'y brancheront un par un.

use std::collections::BTreeMap;

use astro_crypto::KeySource;
use astro_db::Db;
use astro_nostr_proto::{Event, UnsignedEvent, is_hex64, kind};
use rusqlite::OptionalExtension;

/// Identifiant (`d`) de l'événement des niveaux.
pub const D_TAG: &str = "uplanet/proximity";
/// Personnes rangées au plus (exceptions au niveau par défaut).
pub const MAX_RANKED: usize = 2000;
/// Niveaux, du plus proche au plus lointain.
pub const LEVELS: [(u8, &str); 5] = [(5, "Famille"), (4, "Proche"), (3, "Ami"), (2, "Connaissance"), (1, "Contact")];

/// Niveaux rangés à la main : clé → niveau (1 à 5).
pub type Ranks = BTreeMap<String, u8>;

/// Niveau par défaut : Ami pour un ami réciproque, Contact sinon.
pub fn default_level(friends: bool) -> u8 {
    if friends { 3 } else { 1 }
}

/// Lit le contenu déchiffré ; ignore toute entrée invalide (clé, niveau hors 1..=5).
pub fn parse(plain: &str) -> Ranks {
    serde_json::from_str::<BTreeMap<String, serde_json::Value>>(plain)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(k, v)| {
            let l = u8::try_from(v.as_u64()?).ok()?;
            (is_hex64(&k) && (1..=5).contains(&l)).then(|| (k.to_ascii_lowercase(), l))
        })
        .take(MAX_RANKED)
        .collect()
}

/// Range `target` au niveau `level` (`None` : retour au niveau par défaut). Un niveau égal au défaut n'est pas gardé.
pub fn set(ranks: &mut Ranks, target: &str, level: Option<u8>, friends: bool) -> Result<(), &'static str> {
    match level {
        Some(l) if !(1..=5).contains(&l) => Err("niveau de 1 (Contact) à 5 (Famille)"),
        Some(l) if l != default_level(friends) => {
            if !ranks.contains_key(target) && ranks.len() >= MAX_RANKED {
                return Err("trop de personnes rangées");
            }
            ranks.insert(target.to_owned(), l);
            Ok(())
        }
        _ => {
            ranks.remove(target);
            Ok(())
        }
    }
}

/// Dernier événement des niveaux du membre `who`, s'il existe.
pub async fn latest(db: &Db, who: &str) -> Result<Option<(String, u64)>, String> {
    let w = who.to_owned();
    db.readers()
        .read(move |c| {
            Ok(c.query_row(
                "SELECT e.content, e.created_at FROM nostr_events e JOIN nostr_tags t ON t.event_id = e.seq WHERE e.kind = ?1 AND e.pubkey = ?2 AND t.name = 'd' AND t.value = ?3 ORDER BY e.created_at DESC LIMIT 1",
                rusqlite::params![kind::APP_DATA, w, D_TAG],
                |r| Ok((r.get::<_, String>(0)?, u64::try_from(r.get::<_, i64>(1)?).unwrap_or(0))),
            )
            .optional()?)
        })
        .await
        .map_err(|e| e.to_string())
}

/// Niveaux rangés par `who` (déchiffrés avec sa clé, détenue par la station). Vide s'il n'a rien rangé.
pub async fn load(db: &Db, keys: &dyn KeySource, who: &str) -> Result<(Ranks, u64), String> {
    let Some((content, at)) = latest(db, who).await? else { return Ok((Ranks::new(), 0)) };
    let secret = keys.secret(who).ok_or("clé du compte introuvable sur cette station")?;
    let plain = astro_crypto::nip44::decrypt_from_self(&secret, who, &content).map_err(|e| format!("niveaux illisibles : {e}"))?;
    Ok((parse(&plain), at))
}

/// Nouvel événement des niveaux de `who`, chiffré pour lui seul, daté après le précédent (`previous`).
pub fn event(keys: &dyn KeySource, who: &str, ranks: &Ranks, now: u64, previous: u64) -> Result<Event, String> {
    let secret = keys.secret(who).ok_or("clé du compte introuvable sur cette station")?;
    let plain = serde_json::to_string(ranks).map_err(|e| e.to_string())?;
    let content = astro_crypto::nip44::encrypt_to_self(&secret, who, &plain).map_err(|e| e.to_string())?;
    UnsignedEvent { created_at: now.max(previous + 1), kind: kind::APP_DATA, tags: vec![vec!["d".into(), D_TAG.into()]], content }.sign(&secret).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use astro_crypto::StaticKeySource;
    use astro_nostr_proto::pubkey_of;

    const ALICE: [u8; 32] = [0xA1; 32];

    #[test]
    fn levels_are_private_and_only_exceptions_are_kept() {
        let keys = StaticKeySource::new().with(ALICE);
        let alice = pubkey_of(&ALICE).unwrap();
        let (lea, paul) = ("ab".repeat(32), "cd".repeat(32));
        let mut r = Ranks::new();
        set(&mut r, &lea, Some(5), true).unwrap();
        set(&mut r, &paul, Some(3), true).unwrap(); // Ami = défaut d'un ami réciproque : rien à garder
        assert_eq!(r.len(), 1);
        let e = event(&keys, &alice, &r, 100, 0).unwrap();
        assert!(e.verify().is_ok() && e.kind == kind::APP_DATA);
        assert!(!e.content.contains(&lea) && !e.tags.iter().flatten().any(|t| t == &lea), "rien ne dit qui est rangé");
        let plain = astro_crypto::nip44::decrypt_from_self(&keys.secret(&alice).unwrap(), &alice, &e.content).unwrap();
        assert_eq!(parse(&plain), r);
        assert_eq!(event(&keys, &alice, &r, 50, 100).unwrap().created_at, 101, "toujours après le précédent");
        set(&mut r, &lea, None, true).unwrap();
        assert!(r.is_empty(), "retour au défaut");
    }

    #[test]
    fn bad_entries_and_levels_are_refused() {
        assert!(set(&mut Ranks::new(), &"ab".repeat(32), Some(6), true).is_err());
        assert!(set(&mut Ranks::new(), &"ab".repeat(32), Some(0), true).is_err());
        let r = parse(&format!(r#"{{"{}": 4, "abc": 5, "{}": 9, "{}": "5"}}"#, "ab".repeat(32), "cd".repeat(32), "ef".repeat(32)));
        assert_eq!(r.len(), 1);
        assert_eq!(parse("pas du json"), Ranks::new());
        assert_eq!((default_level(true), default_level(false)), (3, 1));
    }
}
