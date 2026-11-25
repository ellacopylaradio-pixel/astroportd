//! **Activité des membres** (écran Capitaine « Membres ») : depuis quand un compte n'a rien fait, et le retrait d'un
//! MULTIPASS resté inactif plus d'un mois.
//!
//! La « dernière activité » est la plus récente de ces traces, toutes lues en SQL :
//! - dernière connexion authentifiée (`kv member_seen`, notée par [`crate::member_primo::Proofs`]) et première (`member_proof`) ;
//! - dernier événement Nostr **écrit par le membre** (notes, réactions, messages, articles, médias : pas les événements
//!   que la station publie seule) ;
//! - dernier fichier déposé dans son uCloud (hors photos reçues automatiquement d'un ami, `facecloud-share`) ;
//! - dernier usage d'un jeton d'appareil (`dav_tokens.last_used`) ;
//! - à défaut, la création du compte (`member_born`, `identities.created_at`) : un compte neuf n'est jamais « inactif ».
//!
//! Retirer un compte est **réversible pendant 30 jours** : ses clés vont dans la corbeille de la station
//! (`trash/removed/<email>.<date>`, [`crate::trash`]), il cesse d'être hébergé ici (`hosted = 0`, comme un déménagement),
//! ses fichiers restent dans le cache et sur le disque. Pour le rendre : remettre le dossier dans `nostr/` puis
//! `UPDATE identities SET hosted = 1`. Après 30 jours, la nuit de 20h12 efface les clés : le retrait devient définitif.

/// Inactivité minimale avant qu'un compte puisse être retiré (30 jours).
pub const REMOVE_AFTER_SECS: i64 = 30 * 86_400;

/// Raison de corbeille des comptes retirés.
pub const TRASH_REASON: &str = "removed";

/// Kinds Nostr que seul le membre écrit (jamais la station à sa place).
const HUMAN_KINDS: &str = "1, 4, 6, 7, 14, 16, 20, 21, 22, 1063, 1111, 1222, 9802, 30023, 30024, 30311";

/// Dernière activité du compte `?1` (horodatage Unix), `?2` = maintenant (borne les dates venues du futur).
pub fn last_activity_sql() -> String {
    format!(
        "SELECT MAX(t) FROM ( \
           SELECT CAST(value AS INTEGER) AS t FROM kv WHERE key = ?1 AND scope IN ('{seen}', '{proof}', '{born}') \
           UNION ALL SELECT MAX(created_at) FROM nostr_events WHERE pubkey = ?1 AND kind IN ({HUMAN_KINDS}) AND created_at <= ?2 \
           UNION ALL SELECT MAX(mtime) FROM ucloud_entries WHERE owner_hex = ?1 AND type = 'file' AND COALESCE(origin, '') <> 'facecloud-share' AND mtime <= ?2 \
           UNION ALL SELECT MAX(last_used) FROM dav_tokens WHERE owner_hex = ?1 \
           UNION ALL SELECT created_at FROM identities WHERE hex = ?1)",
        seen = crate::member_primo::SEEN_SCOPE,
        proof = crate::member_primo::PROOF_SCOPE,
        born = crate::member_primo::BORN_SCOPE,
    )
}

/// Ancienneté du compte : sa création ici, sinon celle de son identité, sinon sa plus ancienne trace.
pub fn since_sql() -> String {
    format!(
        "SELECT COALESCE( \
           (SELECT CAST(value AS INTEGER) FROM kv WHERE scope = '{born}' AND key = ?1), \
           (SELECT created_at FROM identities WHERE hex = ?1), \
           (SELECT MIN(created_at) FROM nostr_events WHERE pubkey = ?1), \
           (SELECT MIN(mtime) FROM ucloud_entries WHERE owner_hex = ?1 AND mtime > 0))",
        born = crate::member_primo::BORN_SCOPE,
    )
}

/// Activité d'un compte : `(dernière activité, ancienneté)`.
pub fn read(c: &rusqlite::Connection, hex: &str, now: i64) -> rusqlite::Result<(Option<i64>, Option<i64>)> {
    let last = c.query_row(&last_activity_sql(), rusqlite::params![hex, now], |r| r.get::<_, Option<i64>>(0))?;
    let since = c.query_row(&since_sql(), [hex], |r| r.get::<_, Option<i64>>(0))?;
    Ok((last, since))
}

/// Le compte peut-il être retiré ? Un MULTIPASS hébergé ici, sans activité depuis plus de [`REMOVE_AFTER_SECS`].
pub fn removable(role: &str, last: Option<i64>, now: i64) -> Result<(), String> {
    if role != "multipass" {
        return Err("seul un MULTIPASS peut être retiré (le Capitaine et la ZenCard tiennent la station)".into());
    }
    match last {
        Some(t) if now - t < REMOVE_AFTER_SECS => Err(format!("compte actif il y a {} jour(s) : il ne se retire qu'après 30 jours sans activité", (now - t) / 86_400)),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_multipass_inactive_for_a_month_can_be_removed() {
        let now = 100 * 86_400;
        assert!(removable("multipass", Some(now - 29 * 86_400), now).is_err(), "actif il y a 29 jours");
        assert!(removable("multipass", Some(now - REMOVE_AFTER_SECS), now).is_ok());
        assert!(removable("multipass", None, now).is_ok(), "aucune trace");
        assert!(removable("captain", Some(0), now).is_err());
        assert!(removable("zencard", None, now).is_err());
    }
}
