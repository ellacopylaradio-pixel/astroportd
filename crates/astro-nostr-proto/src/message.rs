//! Messages du protocole NIP-01 / NIP-42 entre client et relai.

use serde_json::{Value, json};

use crate::{Event, Filter, FilterError};

/// Longueur maximale d'un identifiant de souscription.
pub const MAX_SUB_ID_LEN: usize = 64;

/// Erreurs d'analyse d'un message.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MessageError {
    /// JSON invalide.
    #[error("JSON invalide")]
    Json,
    /// Type de message inconnu.
    #[error("type de message inconnu : {0}")]
    Unknown(String),
    /// Message mal formé.
    #[error("message mal formé : {0}")]
    Malformed(&'static str),
    /// Filtre invalide.
    #[error("filtre invalide : {0}")]
    Filter(#[from] FilterError),
}

/// Message client → relai.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientMessage {
    /// Publication d'un événement.
    Event(Event),
    /// Souscription.
    Req {
        /// Identifiant de souscription.
        sub_id: String,
        /// Filtres (OU logique).
        filters: Vec<Filter>,
    },
    /// Fin de souscription.
    Close(String),
    /// Réponse au défi NIP-42.
    Auth(Event),
    /// Décompte (NIP-45).
    Count {
        /// Identifiant de la requête.
        sub_id: String,
        /// Filtres (OU logique).
        filters: Vec<Filter>,
    },
    /// Ouverture d'une réconciliation Negentropy (NIP-77) : filtre et premier message (hexadécimal).
    NegOpen {
        /// Identifiant de la réconciliation.
        sub_id: String,
        /// Ensemble comparé.
        filter: Filter,
        /// Message Negentropy, en hexadécimal.
        message: String,
    },
    /// Message suivant d'une réconciliation (NIP-77).
    NegMsg {
        /// Identifiant de la réconciliation.
        sub_id: String,
        /// Message Negentropy, en hexadécimal.
        message: String,
    },
    /// Fin d'une réconciliation (NIP-77).
    NegClose(String),
}

impl ClientMessage {
    /// Analyse un message texte.
    pub fn parse(text: &str) -> Result<Self, MessageError> {
        let v: Value = serde_json::from_str(text).map_err(|_| MessageError::Json)?;
        let arr = v.as_array().ok_or(MessageError::Malformed("tableau attendu"))?;
        let kind = arr.first().and_then(Value::as_str).ok_or(MessageError::Malformed("type manquant"))?;
        match kind {
            "EVENT" => Ok(Self::Event(event_at(arr, 1)?)),
            "AUTH" => Ok(Self::Auth(event_at(arr, 1)?)),
            "CLOSE" => Ok(Self::Close(sub_id_at(arr, 1)?)),
            "REQ" => {
                let sub_id = sub_id_at(arr, 1)?;
                let filters = arr[2..].iter().map(Filter::from_value).collect::<Result<Vec<_>, _>>()?;
                Ok(Self::Req { sub_id, filters })
            }
            "COUNT" => {
                let sub_id = sub_id_at(arr, 1)?;
                let filters = arr[2..].iter().map(Filter::from_value).collect::<Result<Vec<_>, _>>()?;
                Ok(Self::Count { sub_id, filters })
            }
            "NEG-OPEN" => {
                let sub_id = sub_id_at(arr, 1)?;
                let filter = Filter::from_value(arr.get(2).ok_or(MessageError::Malformed("filtre manquant"))?)?;
                Ok(Self::NegOpen { sub_id, filter, message: hex_at(arr, 3)? })
            }
            "NEG-MSG" => Ok(Self::NegMsg { sub_id: sub_id_at(arr, 1)?, message: hex_at(arr, 2)? }),
            "NEG-CLOSE" => Ok(Self::NegClose(sub_id_at(arr, 1)?)),
            other => Err(MessageError::Unknown(other.chars().take(32).collect())),
        }
    }

    /// Sérialise le message.
    pub fn to_json(&self) -> String {
        match self {
            Self::Event(e) => json!(["EVENT", e]),
            Self::Auth(e) => json!(["AUTH", e]),
            Self::Close(id) => json!(["CLOSE", id]),
            Self::Req { sub_id, filters } => {
                let mut a = vec![json!("REQ"), json!(sub_id)];
                a.extend(filters.iter().map(Filter::to_value));
                Value::Array(a)
            }
            Self::Count { sub_id, filters } => {
                let mut a = vec![json!("COUNT"), json!(sub_id)];
                a.extend(filters.iter().map(Filter::to_value));
                Value::Array(a)
            }
            Self::NegOpen { sub_id, filter, message } => json!(["NEG-OPEN", sub_id, filter.to_value(), message]),
            Self::NegMsg { sub_id, message } => json!(["NEG-MSG", sub_id, message]),
            Self::NegClose(id) => json!(["NEG-CLOSE", id]),
        }
        .to_string()
    }
}

/// Message relai → client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayMessage {
    /// Événement d'une souscription.
    Event {
        /// Souscription destinataire.
        sub_id: String,
        /// Événement.
        event: Box<Event>,
    },
    /// Accusé de réception d'un `EVENT` ou d'un `AUTH`.
    Ok {
        /// Identifiant de l'événement.
        event_id: String,
        /// Accepté ou non.
        accepted: bool,
        /// Message lisible préfixé (`duplicate:`, `invalid:`, `restricted:`, `rate-limited:`, `auth-required:`).
        message: String,
    },
    /// Fin de l'historique stocké.
    Eose(String),
    /// Souscription terminée par le relai.
    Closed {
        /// Souscription.
        sub_id: String,
        /// Raison préfixée.
        message: String,
    },
    /// Message d'information.
    Notice(String),
    /// Défi d'authentification NIP-42.
    Auth(String),
    /// Réponse à un `COUNT` (NIP-45).
    Count {
        /// Identifiant de la requête.
        sub_id: String,
        /// Nombre d'événements.
        count: u64,
        /// Le décompte s'est arrêté à la limite du relai : le vrai nombre est au moins celui-ci.
        approximate: bool,
    },
    /// Réponse d'une réconciliation Negentropy (NIP-77).
    NegMsg {
        /// Identifiant de la réconciliation.
        sub_id: String,
        /// Message Negentropy, en hexadécimal.
        message: String,
    },
    /// Réconciliation refusée ou close par le relai (NIP-77) : raison préfixée (`blocked:`, `closed:`).
    NegErr {
        /// Identifiant de la réconciliation.
        sub_id: String,
        /// Raison.
        reason: String,
    },
}

impl RelayMessage {
    /// Sérialise le message.
    pub fn to_json(&self) -> String {
        match self {
            Self::Event { sub_id, event } => json!(["EVENT", sub_id, event]),
            Self::Ok { event_id, accepted, message } => json!(["OK", event_id, accepted, message]),
            Self::Eose(id) => json!(["EOSE", id]),
            Self::Closed { sub_id, message } => json!(["CLOSED", sub_id, message]),
            Self::Notice(m) => json!(["NOTICE", m]),
            Self::Auth(c) => json!(["AUTH", c]),
            Self::Count { sub_id, count, approximate } => json!(["COUNT", sub_id, if *approximate { json!({ "count": count, "approximate": true }) } else { json!({ "count": count }) }]),
            Self::NegMsg { sub_id, message } => json!(["NEG-MSG", sub_id, message]),
            Self::NegErr { sub_id, reason } => json!(["NEG-ERR", sub_id, reason]),
        }
        .to_string()
    }

    /// Analyse un message du relai (côté client).
    pub fn parse(text: &str) -> Result<Self, MessageError> {
        let v: Value = serde_json::from_str(text).map_err(|_| MessageError::Json)?;
        let arr = v.as_array().ok_or(MessageError::Malformed("tableau attendu"))?;
        let kind = arr.first().and_then(Value::as_str).ok_or(MessageError::Malformed("type manquant"))?;
        let s = |i: usize| -> Result<String, MessageError> {
            arr.get(i).and_then(Value::as_str).map(str::to_owned).ok_or(MessageError::Malformed("chaîne attendue"))
        };
        match kind {
            "EVENT" => Ok(Self::Event { sub_id: s(1)?, event: Box::new(event_at(arr, 2)?) }),
            "OK" => Ok(Self::Ok {
                event_id: s(1)?,
                accepted: arr.get(2).and_then(Value::as_bool).ok_or(MessageError::Malformed("booléen attendu"))?,
                message: arr.get(3).and_then(Value::as_str).unwrap_or_default().to_owned(),
            }),
            "EOSE" => Ok(Self::Eose(s(1)?)),
            "CLOSED" => Ok(Self::Closed { sub_id: s(1)?, message: s(2).unwrap_or_default() }),
            "NOTICE" => Ok(Self::Notice(s(1)?)),
            "AUTH" => Ok(Self::Auth(s(1)?)),
            "NEG-MSG" => Ok(Self::NegMsg { sub_id: s(1)?, message: hex_at(arr, 2)? }),
            "NEG-ERR" => Ok(Self::NegErr { sub_id: s(1)?, reason: s(2).unwrap_or_default() }),
            "COUNT" => Ok(Self::Count { sub_id: s(1)?, count: arr.get(2).and_then(|c| c["count"].as_u64()).ok_or(MessageError::Malformed("décompte attendu"))?, approximate: arr.get(2).and_then(|c| c["approximate"].as_bool()).unwrap_or(false) }),
            other => Err(MessageError::Unknown(other.chars().take(32).collect())),
        }
    }
}

fn event_at(arr: &[Value], i: usize) -> Result<Event, MessageError> {
    let v = arr.get(i).ok_or(MessageError::Malformed("événement manquant"))?;
    serde_json::from_value(v.clone()).map_err(|_| MessageError::Malformed("événement invalide"))
}

/// Message Negentropy hexadécimal : longueur paire, chiffres hexadécimaux, 4 Mio décodés au plus.
fn hex_at(arr: &[Value], i: usize) -> Result<String, MessageError> {
    let h = arr.get(i).and_then(Value::as_str).ok_or(MessageError::Malformed("message Negentropy manquant"))?;
    if h.len() % 2 != 0 || h.len() > 8 * 1024 * 1024 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(MessageError::Malformed("message Negentropy invalide"));
    }
    Ok(h.to_ascii_lowercase())
}

fn sub_id_at(arr: &[Value], i: usize) -> Result<String, MessageError> {
    let id = arr.get(i).and_then(Value::as_str).ok_or(MessageError::Malformed("identifiant de souscription manquant"))?;
    if id.is_empty() || id.len() > MAX_SUB_ID_LEN {
        return Err(MessageError::Malformed("identifiant de souscription hors limites"));
    }
    Ok(id.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::UnsignedEvent;

    fn event() -> Event {
        UnsignedEvent { created_at: 1_800_000_000, kind: 1, tags: vec![], content: "x".into() }.sign(&[5u8; 32]).unwrap()
    }

    #[test]
    fn client_messages_round_trip() {
        let e = event();
        for m in [
            ClientMessage::Event(e.clone()),
            ClientMessage::Auth(e.clone()),
            ClientMessage::Close("s1".into()),
            ClientMessage::Req { sub_id: "s".into(), filters: vec![Filter::default(), Filter { kinds: Some(vec![1]), ..Filter::default() }] },
            ClientMessage::NegOpen { sub_id: "n".into(), filter: Filter { kinds: Some(vec![21]), ..Filter::default() }, message: "61".into() },
            ClientMessage::NegMsg { sub_id: "n".into(), message: "6100".into() },
            ClientMessage::NegClose("n".into()),
        ] {
            assert_eq!(ClientMessage::parse(&m.to_json()).unwrap(), m);
        }
    }

    #[test]
    fn relay_messages_round_trip() {
        let e = event();
        for m in [
            RelayMessage::Event { sub_id: "s".into(), event: Box::new(e) },
            RelayMessage::Ok { event_id: "i".into(), accepted: false, message: "invalid: x".into() },
            RelayMessage::Eose("s".into()),
            RelayMessage::Closed { sub_id: "s".into(), message: "auth-required: x".into() },
            RelayMessage::Notice("n".into()),
            RelayMessage::Auth("c".into()),
            RelayMessage::NegMsg { sub_id: "n".into(), message: "61".into() },
            RelayMessage::NegErr { sub_id: "n".into(), reason: "blocked: trop gros".into() },
        ] {
            assert_eq!(RelayMessage::parse(&m.to_json()).unwrap(), m);
        }
    }

    #[test]
    fn malformed_client_messages_are_rejected() {
        assert_eq!(ClientMessage::parse("nope"), Err(MessageError::Json));
        assert!(matches!(ClientMessage::parse("{}"), Err(MessageError::Malformed(_))));
        assert!(matches!(ClientMessage::parse("[]"), Err(MessageError::Malformed(_))));
        assert!(matches!(ClientMessage::parse("[\"FOO\"]"), Err(MessageError::Unknown(_))));
        assert!(matches!(ClientMessage::parse("[\"EVENT\"]"), Err(MessageError::Malformed(_))));
        assert!(matches!(ClientMessage::parse("[\"EVENT\",{\"id\":1}]"), Err(MessageError::Malformed(_))));
        assert!(matches!(ClientMessage::parse("[\"REQ\"]"), Err(MessageError::Malformed(_))));
        assert!(matches!(ClientMessage::parse("[\"REQ\",\"\"]"), Err(MessageError::Malformed(_))));
        let long = format!("[\"CLOSE\",\"{}\"]", "x".repeat(65));
        assert!(matches!(ClientMessage::parse(&long), Err(MessageError::Malformed(_))));
        assert!(matches!(ClientMessage::parse("[\"REQ\",\"s\",1]"), Err(MessageError::Filter(_))));
    }
}
