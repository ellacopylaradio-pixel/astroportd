//! Mode d'ouverture de la station, choisi par le Capitaine (écran **Écluses**). Une seule valeur, en `kv`
//! (`scope = 'station'`, `key = 'mode'`), lue **en SQL** là où chaque écluse se décide : le cercle du relai
//! (`astro-relay`), le crawl N² (`astro-sync`), le partage automatique des photos (`astro-facecloud`), les inscriptions et
//! la matrice des kinds (binaire). État local : un réglage de la station, pas une projection.
//!
//! | | Forteresse | Village (défaut) | Place publique |
//! |---|---|---|---|
//! | Cercle du relai | hébergés + amis **réciproques** | hébergés + N¹ + N² | idem Village |
//! | Crawl | hébergés + N¹ | + N² | + N² |
//! | Inscriptions libres, restaurations | fermées (invitations d'un membre seulement) | selon `serve` | selon `serve` |
//! | Photos (FaceCloud) | toutes attendent un accord | envoi auto, seul avec vous | idem Village |
//! | Relai : profil et notes publiques | cercle | cercle | **tout le monde** peut publier |

use std::fmt;

/// Portée `kv` du mode.
pub const SCOPE: &str = "station";
/// Clé `kv` du mode.
pub const KEY: &str = "mode";

/// Expression SQL vraie si la station est en **Forteresse** (à placer dans une clause `WHERE`).
pub const FORTRESS_SQL: &str = "(COALESCE((SELECT value FROM kv WHERE scope = 'station' AND key = 'mode'), 'village') = 'forteresse')";

/// Mode d'ouverture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Fermée : seuls les amis réciproques des membres entrent.
    Forteresse,
    /// Entre amis et amis d'amis (comportement historique).
    #[default]
    Village,
    /// Ouverte : tout le monde peut publier une note publique sur le relai.
    Place,
}

impl Mode {
    /// Nom stocké et échangé avec l'interface.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Forteresse => "forteresse",
            Self::Village => "village",
            Self::Place => "place",
        }
    }

    /// Lit un nom ; `None` s'il est inconnu.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "forteresse" => Some(Self::Forteresse),
            "village" => Some(Self::Village),
            "place" => Some(Self::Place),
            _ => None,
        }
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Mode courant (Village si rien n'est réglé ou si la valeur est inconnue).
pub fn read(c: &rusqlite::Connection) -> rusqlite::Result<Mode> {
    use rusqlite::OptionalExtension;
    let v: Option<String> = c.query_row("SELECT value FROM kv WHERE scope = ?1 AND key = ?2", [SCOPE, KEY], |r| r.get(0)).optional()?;
    Ok(v.as_deref().and_then(Mode::parse).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mode_round_trips_and_the_sql_flag_follows_it() {
        let c = rusqlite::Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE kv (scope TEXT, key TEXT, value TEXT, updated_at INTEGER, PRIMARY KEY (scope, key))").unwrap();
        let fortress = |c: &rusqlite::Connection| c.query_row(&format!("SELECT {FORTRESS_SQL}"), [], |r| r.get::<_, bool>(0)).unwrap();
        assert_eq!((read(&c).unwrap(), fortress(&c)), (Mode::Village, false), "rien de réglé : Village");
        for m in [Mode::Forteresse, Mode::Village, Mode::Place] {
            c.execute("INSERT OR REPLACE INTO kv VALUES ('station', 'mode', ?1, 0)", [m.as_str()]).unwrap();
            assert_eq!((read(&c).unwrap(), fortress(&c)), (m, m == Mode::Forteresse));
            assert_eq!(Mode::parse(&m.to_string()), Some(m));
        }
        c.execute("INSERT OR REPLACE INTO kv VALUES ('station', 'mode', 'inconnu', 0)", []).unwrap();
        assert_eq!((read(&c).unwrap(), fortress(&c)), (Mode::Village, false), "valeur inconnue : jamais plus fermé ni plus ouvert que le défaut");
    }
}
