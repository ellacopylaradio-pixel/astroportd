//! ISLAND-1 « Ğ1 en différé » : quand la station perd l'accès à Duniter alors que la chaîne, elle, continue.
//!
//! **Aucune création de monnaie.** Chaque MULTIPASS hébergé peut dépenser, en promesses de Ğ1, au plus :
//! son dernier solde dépensable connu (instantané pris quand la chaîne répondait) **plus** les DU que la chaîne lui verse
//! pendant la coupure (membre de la toile de confiance seulement), calculables d'avance : périodes écoulées × DU en vigueur.
//! Le registre Ğ1-N² (kind 30852, `t=island`) sert de carnet ; au retour de la chaîne, chaque promesse acceptée est
//! **réglée en vraie Ğ1** par la station du payeur (elle détient sa clé), puis marquée réglée.
//!
//! Deux événements :
//! - **ouverture de crédit** (kind 30854, signée par le NODE, une par membre et par île) : `island`, `p` (membre), `g1`
//!   (son portefeuille), `balance` (dépensable à l'instantané), `ud`, `ud_period`, `ud_until`, `at` ;
//! - **promesse** (kind 30852, signée par le membre) : la forme du registre N² (`d`, `p`, `amount`, `prev`, `t=g1-n2`) plus
//!   `t=island`, `island` et `g1` (portefeuille Ğ1 du destinataire, où le règlement arrivera).
//!
//! [`validate`] ne dépend que de l'**ensemble** des événements, jamais de leur ordre d'arrivée : le temps réel et le
//! replay convergent. Une fourche (deux promesses sur le même `prev`, ou un `d` réutilisé) gèle le payeur : rien au-delà
//! de la fourche n'est accepté, et aucune des deux branches.
//!
//! Les promesses **reçues** ne sont pas dépensables avant leur règlement : une promesse non tenue ne se propage pas.

use std::collections::{BTreeMap, HashMap, HashSet};

use astro_core::Amount;
use astro_nostr_proto::{Event, is_hex64, kind};

use crate::ledger::{LedgerError, Tx};

/// Valeur du tag `t` et nom du tag qui porte l'identifiant de l'île.
pub const TAG: &str = "island";
/// Avance maximale tolérée d'un horodatage sur l'horloge locale.
pub const MAX_FUTURE_SECS: u64 = 600;
/// Durée maximale pendant laquelle des DU sont comptés : au-delà, seule une vraie lecture de la chaîne fait foi.
pub const MAX_UD_SECS: u64 = 30 * 86_400;

/// Pourquoi une promesse est refusée.
#[derive(Debug, thiserror::Error, PartialEq, Eq, Clone)]
pub enum IslandError {
    /// Forme du registre N² invalide.
    #[error("forme : {0}")]
    Form(LedgerError),
    /// Identifiant ou signature invalide.
    #[error("signature ou identifiant invalide")]
    BadSignature,
    /// Ce n'est pas une promesse de cette île ou de ce payeur.
    #[error("hors de cette île")]
    Foreign,
    /// Une île ne crée pas de monnaie.
    #[error("création monétaire interdite en île")]
    Mint,
    /// Portefeuille Ğ1 du destinataire absent ou invalide.
    #[error("portefeuille Ğ1 du destinataire absent")]
    NoDestination,
    /// Antérieure à l'ouverture du crédit.
    #[error("antérieure à l'ouverture de l'île")]
    BeforeOpening,
    /// Horodatage trop loin dans le futur.
    #[error("horodatage dans le futur")]
    FromFuture,
    /// Fourche : le payeur est gelé.
    #[error("double dépense : payeur gelé")]
    Fork,
    /// `prev` inconnu ou chaîne interrompue par un refus.
    #[error("chaînage interrompu")]
    Broken,
    /// Au-delà du crédit disponible à cet instant.
    #[error("au-delà du crédit : disponible {have}, demandé {need}")]
    Overdraft {
        /// Disponible.
        have: Amount,
        /// Demandé.
        need: Amount,
    },
}

/// Crédit d'un membre dans une île.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Credit {
    /// Dépensable au dernier instantané (réserve déjà déduite).
    pub opening: Amount,
    /// DU par période (0 : pas membre de la toile de confiance).
    pub ud: Amount,
    /// Période de création du DU, en secondes.
    pub ud_period: u64,
    /// Instant de l'instantané.
    pub from: u64,
    /// Au-delà, plus aucun DU n'est compté (fin d'adhésion connue ou [`MAX_UD_SECS`]).
    pub ud_until: u64,
}

impl Credit {
    /// Crédit total à l'instant `t` : solde de départ plus les DU **entièrement** écoulés depuis l'instantané.
    pub fn at(&self, t: u64) -> Amount {
        if self.ud_period == 0 || self.ud == Amount::ZERO {
            return self.opening;
        }
        let until = self.ud_until.min(self.from.saturating_add(MAX_UD_SECS));
        let periods = t.min(until).saturating_sub(self.from) / self.ud_period;
        Amount(self.opening.0.saturating_add(self.ud.0.saturating_mul(periods)))
    }
}

/// Ouverture de crédit lue d'un kind 30854.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opening {
    /// Identifiant de l'île (`<16 premiers hex du NODE>-<instant d'ouverture>`).
    pub island: String,
    /// NODE qui ouvre (auteur de l'événement).
    pub node: String,
    /// Membre crédité.
    pub member: String,
    /// Portefeuille Ğ1 du membre (SS58), d'où partira le règlement.
    pub g1: String,
    /// Crédit.
    pub credit: Credit,
}

fn tag<'a>(e: &'a Event, name: &str) -> Option<&'a str> {
    e.tags.iter().find(|t| t.first().map(String::as_str) == Some(name)).and_then(|t| t.get(1)).map(String::as_str)
}

fn has_t(e: &Event, v: &str) -> bool {
    e.tags.iter().any(|t| t.first().map(String::as_str) == Some("t") && t.get(1).map(String::as_str) == Some(v))
}

/// Une adresse Ğ1 plausible (la vérification complète, somme SS58 comprise, est faite par `astro-duniter` au règlement).
fn plausible_g1(s: &str) -> bool {
    (40..=60).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Identifiant d'île ouverte par `node` à l'instant `at`.
pub fn island_id(node: &str, at: u64) -> String {
    format!("{}-{at}", node.get(..16).unwrap_or(node))
}

/// L'île d'un événement (tag `island`), s'il en porte une.
pub fn island_of(e: &Event) -> Option<&str> {
    tag(e, TAG)
}

/// Une promesse d'île (kind 30852 `t=island`).
pub fn is_promise(e: &Event) -> bool {
    e.kind == kind::LEDGER_N2 && has_t(e, TAG)
}

impl Opening {
    /// Lit et vérifie une ouverture : forme, signature, et île bien ouverte par son auteur.
    pub fn from_event(e: &Event) -> Option<Self> {
        if e.kind != kind::ISLAND_CREDIT || e.verify().is_err() {
            return None;
        }
        let island = tag(e, TAG)?;
        let (prefix, _) = island.split_once('-')?;
        if !e.pubkey.starts_with(prefix) || prefix.len() != 16 {
            return None;
        }
        let member = tag(e, "p").filter(|p| is_hex64(p))?;
        let g1 = tag(e, "g1").filter(|g| plausible_g1(g))?;
        let num = |n: &str| tag(e, n).and_then(|v| v.parse::<u64>().ok());
        let money = |n: &str| tag(e, n).and_then(|v| v.parse::<Amount>().ok());
        let credit = Credit { opening: money("balance")?, ud: money("ud")?, ud_period: num("ud_period")?, from: num("at")?, ud_until: num("ud_until")? };
        Some(Self { island: island.to_owned(), node: e.pubkey.clone(), member: member.to_owned(), g1: g1.to_owned(), credit })
    }

    /// Tags du kind 30854 (le `d` rend l'ouverture remplaçable : un instantané plus récent du même membre la remplace).
    pub fn tags(&self) -> Vec<Vec<String>> {
        let c = &self.credit;
        [
            ("d", format!("{}:{}", self.island, self.member)),
            ("t", TAG.to_owned()),
            (TAG, self.island.clone()),
            ("p", self.member.clone()),
            ("g1", self.g1.clone()),
            ("balance", c.opening.to_string()),
            ("ud", c.ud.to_string()),
            ("ud_period", c.ud_period.to_string()),
            ("ud_until", c.ud_until.to_string()),
            ("at", c.from.to_string()),
        ]
        .into_iter()
        .map(|(k, v)| vec![k.to_owned(), v])
        .collect()
    }
}

/// Tags d'une promesse (kind 30852) : forme du registre N² + île + portefeuille Ğ1 du destinataire.
pub fn promise_tags(island: &str, d: &str, dest: &str, dest_g1: &str, amount: Amount, prev: &str) -> Vec<Vec<String>> {
    [("d", d), ("p", dest), ("amount", &amount.to_string()), ("prev", prev), ("t", "g1-n2"), ("t", TAG), (TAG, island), ("g1", dest_g1)]
        .into_iter()
        .map(|(k, v)| vec![k.to_owned(), v.to_owned()])
        .collect()
}

/// Promesse acceptée.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Accepted {
    /// Virement.
    pub tx: Tx,
    /// Portefeuille Ğ1 du destinataire.
    pub dest_g1: String,
    /// Total promis par le payeur, celle-ci comprise.
    pub spent_after: Amount,
}

/// Bilan d'un payeur dans une île.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Verdict {
    /// Promesses acceptées, dans l'ordre de la chaîne.
    pub accepted: Vec<Accepted>,
    /// Promesses refusées (id, cause).
    pub rejected: Vec<(String, IslandError)>,
    /// Fourche constatée : plus rien n'est accepté de ce payeur.
    pub frozen: bool,
    /// Total promis accepté.
    pub spent: Amount,
}

impl Verdict {
    /// Ce que le payeur peut encore promettre à l'instant `now`.
    pub fn available(&self, credit: &Credit, now: u64) -> Amount {
        if self.frozen { Amount::ZERO } else { credit.at(now).checked_sub(self.spent).unwrap_or(Amount::ZERO) }
    }

    /// `prev` de la prochaine promesse.
    pub fn next_prev(&self) -> String {
        self.accepted.last().map_or_else(|| "genesis".to_owned(), |a| a.tx.id.clone())
    }
}

/// Valide toutes les promesses de `author` dans l'île `island`, à partir de l'**ensemble** `events` (ordre indifférent).
pub fn validate(author: &str, island: &str, credit: &Credit, events: &[Event], now: u64) -> Verdict {
    let mut v = Verdict::default();
    // 1. Forme de chaque promesse ; les candidates sont rangées par `prev` (ordre déterministe : id).
    let mut children: HashMap<Option<String>, BTreeMap<String, (Tx, String)>> = HashMap::new();
    let mut seen = HashSet::new();
    let mut sorted: Vec<&Event> = events.iter().collect();
    sorted.sort_by(|a, b| a.id.cmp(&b.id));
    for e in sorted {
        if !seen.insert(e.id.clone()) {
            continue;
        }
        let check = || -> Result<(Tx, String), IslandError> {
            if e.pubkey != author || !is_promise(e) || island_of(e) != Some(island) {
                return Err(IslandError::Foreign);
            }
            let tx = Tx::from_event(e).map_err(IslandError::Form)?;
            e.verify().map_err(|_| IslandError::BadSignature)?;
            if tx.is_mint {
                return Err(IslandError::Mint);
            }
            let g1 = tag(e, "g1").filter(|g| plausible_g1(g)).ok_or(IslandError::NoDestination)?;
            if tx.created_at < credit.from {
                return Err(IslandError::BeforeOpening);
            }
            if tx.created_at > now.saturating_add(MAX_FUTURE_SECS) {
                return Err(IslandError::FromFuture);
            }
            Ok((tx, g1.to_owned()))
        };
        match check() {
            Ok((tx, g1)) => {
                children.entry(tx.prev.clone()).or_default().insert(tx.id.clone(), (tx, g1));
            }
            Err(err) => v.rejected.push((e.id.clone(), err)),
        }
    }
    // 2. Parcours de la chaîne depuis `genesis`. Une fourche arrête tout et gèle le payeur.
    let mut cur: Option<String> = None;
    let mut reached = HashSet::new();
    let mut used_d = HashSet::new();
    while let Some(next) = children.get(&cur) {
        if next.len() > 1 || next.values().any(|(tx, _)| used_d.contains(&tx.d)) {
            v.frozen = true;
            break;
        }
        let Some((tx, g1)) = next.values().next() else { break };
        let need = v.spent.0.saturating_add(tx.amount.0);
        let have = credit.at(tx.created_at);
        if need > have.0 {
            v.rejected.push((tx.id.clone(), IslandError::Overdraft { have: have.checked_sub(v.spent).unwrap_or(Amount::ZERO), need: tx.amount }));
            reached.insert(tx.id.clone());
            break;
        }
        v.spent = Amount(need);
        used_d.insert(tx.d.clone());
        reached.insert(tx.id.clone());
        v.accepted.push(Accepted { tx: tx.clone(), dest_g1: g1.clone(), spent_after: v.spent });
        cur = Some(tx.id.clone());
    }
    // 3. Tout ce qui n'a pas été atteint est refusé : fourche, ou chaîne interrompue.
    let why = if v.frozen { IslandError::Fork } else { IslandError::Broken };
    let mut rest: Vec<String> = children.values().flat_map(|m| m.keys().cloned()).filter(|id| !reached.contains(id)).collect();
    rest.sort();
    v.rejected.extend(rest.into_iter().map(|id| (id, why.clone())));
    v
}

#[cfg(test)]
mod tests {
    use astro_nostr_proto::{UnsignedEvent, pubkey_of};

    use super::*;

    const T0: u64 = 1_800_000_000;
    const DAY: u64 = 86_400;
    const G1: &str = "g1LsRGbH5st3WZfdF2SpknxM12W1THsmU5iFzwgKWLHDQqZDq";

    fn hex(n: u8) -> String {
        pubkey_of(&[n; 32]).unwrap()
    }

    fn island() -> String {
        island_id(&hex(1), T0)
    }

    fn promise(from: u8, to: u8, amount: &str, prev: &str, d: &str, at: u64) -> Event {
        let tags = promise_tags(&island(), d, &hex(to), G1, amount.parse().unwrap(), prev);
        UnsignedEvent { created_at: at, kind: kind::LEDGER_N2, tags, content: String::new() }.sign(&[from; 32]).unwrap()
    }

    /// 10 Ğ1 dépensables, DU de 11,00 Ğ1 par jour.
    fn credit() -> Credit {
        Credit { opening: Amount(1_000), ud: Amount(1_100), ud_period: DAY, from: T0, ud_until: u64::MAX }
    }

    #[test]
    fn the_credit_grows_by_whole_ud_periods_and_never_past_the_cap() {
        let c = credit();
        assert_eq!(c.at(T0), Amount(1_000));
        assert_eq!(c.at(T0 + DAY - 1), Amount(1_000), "un DU n'est compté qu'une fois sa période écoulée");
        assert_eq!(c.at(T0 + 3 * DAY), Amount(1_000 + 3 * 1_100));
        assert_eq!(c.at(T0 + 365 * DAY), Amount(1_000 + 30 * 1_100), "au-delà de 30 jours, plus de DU présumé");
        let not_member = Credit { ud: Amount::ZERO, ..c };
        assert_eq!(not_member.at(T0 + 10 * DAY), Amount(1_000), "pas membre : pas de DU");
        let leaving = Credit { ud_until: T0 + 2 * DAY, ..c };
        assert_eq!(leaving.at(T0 + 10 * DAY), Amount(1_000 + 2 * 1_100), "fin d'adhésion connue : les DU s'arrêtent");
    }

    #[test]
    fn promises_chain_up_to_the_credit_and_the_ud_unlocks_more() {
        let a = promise(2, 3, "6.00", "genesis", "1", T0 + 60);
        let b = promise(2, 4, "4.00", &a.id, "2", T0 + 120);
        let over = promise(2, 4, "0.01", &b.id, "3", T0 + 180);
        let v = validate(&hex(2), &island(), &credit(), &[a.clone(), b.clone(), over.clone()], T0 + 200);
        assert_eq!(v.accepted.iter().map(|x| x.tx.id.clone()).collect::<Vec<_>>(), [a.id.clone(), b.id.clone()]);
        assert_eq!(v.spent, Amount(1_000));
        assert!(matches!(v.rejected.as_slice(), [(id, IslandError::Overdraft { .. })] if *id == over.id));
        assert_eq!(v.available(&credit(), T0 + 200), Amount::ZERO);
        // Le lendemain, le DU du jour rend 11 Ğ1 disponibles.
        let next_day = promise(2, 4, "11.00", &b.id, "3b", T0 + DAY + 5);
        let v = validate(&hex(2), &island(), &credit(), &[a, b, next_day], T0 + DAY + 10);
        assert_eq!((v.accepted.len(), v.spent), (3, Amount(2_100)));
        assert_eq!(v.next_prev(), v.accepted[2].tx.id);
    }

    #[test]
    fn the_verdict_does_not_depend_on_arrival_order() {
        let a = promise(2, 3, "1.00", "genesis", "1", T0 + 1);
        let b = promise(2, 3, "2.00", &a.id, "2", T0 + 2);
        let c = promise(2, 3, "3.00", &b.id, "3", T0 + 3);
        let forward = validate(&hex(2), &island(), &credit(), &[a.clone(), b.clone(), c.clone()], T0 + 10);
        let backward = validate(&hex(2), &island(), &credit(), &[c, b, a.clone(), a], T0 + 10);
        assert_eq!(forward, backward);
        assert_eq!(forward.spent, Amount(600));
    }

    #[test]
    fn hostile_fork_freezes_the_payer_and_accepts_neither_branch() {
        let a = promise(2, 3, "1.00", "genesis", "1", T0 + 1);
        let left = promise(2, 3, "5.00", &a.id, "2", T0 + 2);
        let right = promise(2, 4, "5.00", &a.id, "2b", T0 + 2);
        let v = validate(&hex(2), &island(), &credit(), &[a.clone(), left.clone(), right.clone()], T0 + 10);
        assert!(v.frozen);
        assert_eq!(v.accepted.len(), 1, "ce qui précède la fourche reste promis");
        assert!(v.rejected.iter().all(|(_, e)| *e == IslandError::Fork) && v.rejected.len() == 2);
        assert_eq!(v.available(&credit(), T0 + 10 * DAY), Amount::ZERO, "gelé : plus rien à promettre");
        // Le même `d` sur deux branches est aussi une double dépense.
        let again = promise(2, 4, "1.00", &a.id, "1", T0 + 3);
        assert!(validate(&hex(2), &island(), &credit(), &[a, again], T0 + 10).frozen);
    }

    #[test]
    fn hostile_promises_outside_the_rules_are_refused() {
        let other_island = {
            let tags = promise_tags("aaaaaaaaaaaaaaaa-1", "1", &hex(3), G1, Amount(100), "genesis");
            UnsignedEvent { created_at: T0 + 1, kind: kind::LEDGER_N2, tags, content: String::new() }.sign(&[2; 32]).unwrap()
        };
        let mint = {
            let mut tags = promise_tags(&island(), "1", &hex(3), G1, Amount(100), "genesis");
            tags.push(vec!["t".into(), "mint".into()]);
            UnsignedEvent { created_at: T0 + 1, kind: kind::LEDGER_N2, tags, content: String::new() }.sign(&[2; 32]).unwrap()
        };
        let mut forged = promise(2, 3, "1.00", "genesis", "1", T0 + 1);
        forged.tags[2][1] = "9.00".into();
        let early = promise(2, 3, "1.00", "genesis", "1", T0 - 1);
        let future = promise(2, 3, "1.00", "genesis", "1", T0 + 10 + MAX_FUTURE_SECS + 1);
        let someone_else = promise(5, 3, "1.00", "genesis", "1", T0 + 1);
        let orphan = promise(2, 3, "1.00", &"ab".repeat(32), "1", T0 + 1);
        let v = validate(&hex(2), &island(), &credit(), &[other_island, mint, forged, early, future, someone_else, orphan], T0 + 10);
        assert!(v.accepted.is_empty() && !v.frozen);
        let mut why: Vec<String> = v.rejected.iter().map(|(_, e)| format!("{e:?}")).collect();
        why.sort();
        assert_eq!(why, ["BadSignature", "BeforeOpening", "Broken", "Foreign", "Foreign", "FromFuture", "Mint"]);
    }

    #[test]
    fn an_opening_is_bound_to_its_node_and_reads_back() {
        let node = hex(1);
        let o = Opening { island: island_id(&node, T0), node: node.clone(), member: hex(2), g1: G1.into(), credit: credit() };
        let e = UnsignedEvent { created_at: T0, kind: kind::ISLAND_CREDIT, tags: o.tags(), content: String::new() }.sign(&[1; 32]).unwrap();
        assert_eq!(Opening::from_event(&e), Some(o.clone()));
        // Un autre NODE ne peut pas ouvrir de crédit dans cette île.
        let stolen = UnsignedEvent { created_at: T0, kind: kind::ISLAND_CREDIT, tags: o.tags(), content: String::new() }.sign(&[7; 32]).unwrap();
        assert_eq!(Opening::from_event(&stolen), None);
    }
}
