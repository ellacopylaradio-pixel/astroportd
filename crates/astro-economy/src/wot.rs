//! Toile de confiance N² : le graphe des **rencontres prouvées** (`meet`, kind 30855 `verified`) reçoit la mécanique de la
//! toile Ğ1 de Duniter v2s, pour savoir qui serait membre d'une économie Ğ1-N² autonome (DU N²) **sans** la chaîne.
//!
//! - **Certification** : une rencontre prouvée A → B (A a scanné le code de B, signé par B au moment de la rencontre). Une
//!   nomination (choisi dans une liste) ne certifie jamais.
//! - **Racines** (le « bloc de genèse ») : membres d'office, en pratique les Capitaines de la constellation.
//! - **Entrée** : [`Params::sig_qty`] certifications valides reçues de membres, et la **règle de distance** : au moins
//!   [`Params::referees_permille`] des **référents** atteignent la personne en [`Params::step_max`] pas au plus, en ne passant
//!   que par des membres. Un référent a émis **et** reçu au moins ⌈N^(1/step_max)⌉ certifications de membres (N = membres).
//! - **Validité** : une certification vaut [`Params::validity_secs`] ; un émetteur n'en garde que [`Params::stock`], les plus
//!   récentes. Une rencontre renouvelée (nouveau scan) repart de zéro.
//!
//! Calcul pur et déterministe (mêmes rencontres, mêmes racines, même instant → même toile) : la toile se recalcule depuis la
//! projection `meetings` sans rien stocker, et deux stations qui voient les mêmes événements trouvent les mêmes membres. On
//! part des racines et on ajoute, tour après tour, **tous** les candidats qui remplissent les règles face aux membres du tour
//! précédent : l'ordre d'arrivée des rencontres ne compte pas. Un groupe de faux comptes qui se certifient entre eux remplit
//! la première règle, jamais la seconde : aucun référent ne les atteint.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// Règles de la toile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Params {
    /// Certifications de membres nécessaires pour entrer.
    pub sig_qty: usize,
    /// Distance maximale (en pas) entre un référent et la personne.
    pub step_max: usize,
    /// Part des référents qui doivent l'atteindre, en ‰.
    pub referees_permille: u32,
    /// Durée de validité d'une certification, en secondes.
    pub validity_secs: u64,
    /// Certifications valides gardées au plus par émetteur (les plus récentes).
    pub stock: usize,
}

impl Params {
    /// Règles de la toile Ğ1 (Duniter v2s) : 5 certifications, 5 pas, 80 % des référents, 2 ans, 100 par émetteur.
    pub const G1: Params = Params { sig_qty: 5, step_max: 5, referees_permille: 800, validity_secs: 63_115_200, stock: 100 };
}

/// Une rencontre prouvée : `from` a rencontré `to` à l'instant `at` (secondes Unix).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cert {
    /// Celui qui atteste la rencontre.
    pub from: String,
    /// La personne rencontrée.
    pub to: String,
    /// Date de la rencontre.
    pub at: u64,
}

/// Certifications d'émission et de réception exigées d'un référent dans une toile de `members` membres : ⌈N^(1/step_max)⌉.
pub fn referee_threshold(members: usize, step_max: usize) -> usize {
    let k = u32::try_from(step_max.max(1)).unwrap_or(u32::MAX);
    let mut y = 1usize;
    while y.checked_pow(k).is_some_and(|p| p < members) {
        y += 1;
    }
    y
}

/// La toile calculée.
#[derive(Debug, Clone, Default)]
pub struct Wot {
    params: Option<Params>,
    roots: BTreeSet<String>,
    /// Membres (racines comprises).
    pub members: BTreeSet<String>,
    /// Référents parmi les membres.
    pub referees: BTreeSet<String>,
    received: BTreeMap<String, BTreeSet<String>>,
    issued: BTreeMap<String, BTreeSet<String>>,
}

/// Où en est une personne dans la toile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Standing {
    /// Membre de la toile.
    pub member: bool,
    /// Racine (membre d'office).
    pub root: bool,
    /// Référent.
    pub referee: bool,
    /// Certifications valides reçues de membres.
    pub certs: usize,
    /// Certifications nécessaires.
    pub needed: usize,
    /// Part des référents qui l'atteignent, en ‰ (1000 s'il n'y a pas encore de référent).
    pub reached_permille: u32,
    /// Membres rencontrés dans les deux sens (chacun a scanné l'autre).
    pub mutual: usize,
}

impl Wot {
    /// Calcule la toile à l'instant `now`.
    pub fn compute(certs: &[Cert], roots: &BTreeSet<String>, now: u64, p: Params) -> Wot {
        // Certifications valides : pas vers soi, pas trop vieilles (ni trop dans le futur), les `stock` plus récentes par émetteur.
        let mut by_issuer: BTreeMap<&str, Vec<&Cert>> = BTreeMap::new();
        for c in certs {
            if c.from != c.to && c.at <= now.saturating_add(120) && now.saturating_sub(c.at) < p.validity_secs {
                by_issuer.entry(c.from.as_str()).or_default().push(c);
            }
        }
        let (mut received, mut issued): (BTreeMap<String, BTreeSet<String>>, BTreeMap<String, BTreeSet<String>>) = Default::default();
        for (from, mut list) in by_issuer {
            list.sort_by(|a, b| b.at.cmp(&a.at).then_with(|| a.to.cmp(&b.to)));
            list.dedup_by(|a, b| a.to == b.to);
            for c in list.into_iter().take(p.stock) {
                received.entry(c.to.clone()).or_default().insert(from.to_owned());
                issued.entry(from.to_owned()).or_default().insert(c.to.clone());
            }
        }
        let mut w = Wot { params: Some(p), roots: roots.clone(), members: roots.clone(), referees: BTreeSet::new(), received, issued };
        loop {
            w.referees = w.compute_referees();
            let added: Vec<String> = w
                .received
                .keys()
                .filter(|c| !w.members.contains(*c) && w.certs_from_members(c) >= p.sig_qty && w.reached_permille(c) >= p.referees_permille)
                .cloned()
                .collect();
            if added.is_empty() {
                break;
            }
            w.members.extend(added);
        }
        w
    }

    fn params(&self) -> Params {
        self.params.unwrap_or(Params::G1)
    }

    fn compute_referees(&self) -> BTreeSet<String> {
        let y = referee_threshold(self.members.len(), self.params().step_max);
        let count = |m: &BTreeMap<String, BTreeSet<String>>, who: &str| m.get(who).map_or(0, |s| s.iter().filter(|x| self.members.contains(*x)).count());
        self.members.iter().filter(|m| count(&self.received, m) >= y && count(&self.issued, m) >= y).cloned().collect()
    }

    fn certs_from_members(&self, who: &str) -> usize {
        self.received.get(who).map_or(0, |s| s.iter().filter(|x| self.members.contains(*x)).count())
    }

    /// Part des référents qui atteignent `who` en `step_max` pas au plus, par des membres seulement (en ‰).
    fn reached_permille(&self, who: &str) -> u32 {
        let referees: BTreeSet<&String> = self.referees.iter().filter(|r| r.as_str() != who).collect();
        if referees.is_empty() {
            return 1000;
        }
        let mut seen: BTreeSet<&str> = BTreeSet::from([who]);
        let mut queue = VecDeque::from([(who, 0usize)]);
        let mut reached = 0usize;
        while let Some((at, depth)) = queue.pop_front() {
            if depth == self.params().step_max {
                continue;
            }
            for from in self.received.get(at).into_iter().flatten() {
                if self.members.contains(from) && seen.insert(from.as_str()) {
                    if referees.contains(from) {
                        reached += 1;
                    }
                    queue.push_back((from.as_str(), depth + 1));
                }
            }
        }
        u32::try_from(reached * 1000 / referees.len()).unwrap_or(1000)
    }

    /// Où en est `who`.
    pub fn standing(&self, who: &str) -> Standing {
        let mutual = self.issued.get(who).map_or(0, |s| s.iter().filter(|x| self.members.contains(*x) && self.received.get(who).is_some_and(|r| r.contains(*x))).count());
        Standing {
            member: self.members.contains(who),
            root: self.roots.contains(who),
            referee: self.referees.contains(who),
            certs: self.certs_from_members(who),
            needed: self.params().sig_qty,
            reached_permille: self.reached_permille(who),
            mutual,
        }
    }

    /// Membres dont la rencontre prouvée certifie `who` (valides, dans l'ordre des clés).
    pub fn certifiers(&self, who: &str) -> Vec<String> {
        self.received.get(who).map_or_else(Vec::new, |s| s.iter().filter(|x| self.members.contains(*x)).cloned().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 86_400;
    const NOW: u64 = 20_000 * DAY;
    fn k(i: u32) -> String {
        format!("{i:064x}")
    }
    fn c(from: u32, to: u32) -> Cert {
        Cert { from: k(from), to: k(to), at: NOW - DAY }
    }
    fn roots(ids: impl IntoIterator<Item = u32>) -> BTreeSet<String> {
        ids.into_iter().map(k).collect()
    }
    /// Toute la toile se certifie deux à deux (village où chacun a rencontré chacun).
    fn clique(ids: &[u32]) -> Vec<Cert> {
        ids.iter().flat_map(|&a| ids.iter().filter(move |&&b| b != a).map(move |&b| c(a, b))).collect()
    }

    #[test]
    fn the_referee_threshold_is_the_step_max_root_of_the_size() {
        assert_eq!([1, 2, 32, 33, 243, 244, 10_000].map(|n| referee_threshold(n, 5)), [1, 2, 2, 3, 3, 4, 7]);
    }

    #[test]
    fn five_proven_meetings_with_members_make_a_member_and_named_ones_do_not_exist_here() {
        let r = roots(1..=6);
        let mut certs = clique(&[1, 2, 3, 4, 5, 6]);
        certs.extend((1..=4).map(|i| c(i, 100))); // 4 rencontres : pas assez
        let w = Wot::compute(&certs, &r, NOW, Params::G1);
        let s = w.standing(&k(100));
        assert!(!s.member && s.certs == 4 && s.needed == 5);
        certs.push(c(5, 100));
        let w = Wot::compute(&certs, &r, NOW, Params::G1);
        assert!(w.standing(&k(100)).member, "la cinquième rencontre prouvée fait entrer");
        assert!(w.standing(&k(1)).root && w.standing(&k(1)).referee);
        // Elle certifie à son tour : membre au tour suivant, sans dépendre de l'ordre des rencontres.
        let mut chain = certs.clone();
        chain.extend([1, 2, 3, 4].map(|i| c(i, 200)));
        chain.push(c(100, 200));
        chain.reverse();
        assert!(Wot::compute(&chain, &r, NOW, Params::G1).members.contains(&k(200)));
    }

    #[test]
    fn a_ring_of_fake_accounts_certifying_each_other_never_gets_in() {
        let r = roots(1..=6);
        let mut certs = clique(&[1, 2, 3, 4, 5, 6]);
        certs.extend(clique(&[50, 51, 52, 53, 54, 55, 56])); // 7 faux comptes, chacun 6 certifications
        certs.push(c(50, 1)); // ils rencontrent même un membre : ça ne les fait pas entrer
        let w = Wot::compute(&certs, &r, NOW, Params::G1);
        assert_eq!(w.members, r);
        assert_eq!(w.standing(&k(50)).certs, 0, "les certifications de non-membres ne comptent pas");
        // Un seul vrai membre les rencontre tous : toujours une certification de membre chacun, pas cinq.
        certs.extend((50..=56).map(|i| c(1, i)));
        assert_eq!(Wot::compute(&certs, &r, NOW, Params::G1).members.len(), 6);
    }

    #[test]
    fn the_distance_rule_keeps_out_who_only_a_corner_of_the_web_knows() {
        // 40 membres en cercle, chacun certifie ses 8 suivants : en 2 pas, la plupart des référents n'atteignent pas « 300 ».
        let r = roots(1..=40);
        let mut certs: Vec<Cert> = Vec::new();
        for i in 1..=40u32 {
            for d in 1..=8u32 {
                certs.push(c(i, (i + d - 1) % 40 + 1));
            }
        }
        // Cinq membres voisins certifient 300 : il a ses 5 certifications de membres.
        certs.extend((1..=5).map(|i| c(i, 300)));
        let strict = Params { step_max: 2, ..Params::G1 };
        let w = Wot::compute(&certs, &r, NOW, strict);
        let s = w.standing(&k(300));
        assert!(s.certs == 5 && s.reached_permille < 800 && !s.member, "trop loin des référents : {s:?}");
        assert!(Wot::compute(&certs, &r, NOW, Params::G1).members.contains(&k(300)), "en 5 pas, tout le cercle l'atteint");
    }

    #[test]
    fn old_meetings_expire_and_an_issuer_keeps_only_his_stock() {
        let r = roots(1..=6);
        let mut certs = clique(&[1, 2, 3, 4, 5, 6]);
        certs.extend((1..=5).map(|i| Cert { from: k(i), to: k(100), at: NOW - 3 * 365 * DAY }));
        assert!(!Wot::compute(&certs, &r, NOW, Params::G1).members.contains(&k(100)), "rencontres de plus de 2 ans");
        let tiny = Params { stock: 5, ..Params::G1 }; // chaque racine a déjà 5 certifications plus récentes
        certs.extend((1..=5).map(|i| Cert { from: k(i), to: k(100), at: NOW - 2 * DAY }));
        assert_eq!(Wot::compute(&certs, &r, NOW, tiny).standing(&k(100)).certs, 0);
        assert_eq!(Wot::compute(&certs, &r, NOW, Params::G1).standing(&k(100)).certs, 5, "renouvelée : valide à nouveau");
        assert_eq!(Wot::compute(&certs, &r, NOW, Params::G1).standing(&k(1)).mutual, 5);
    }
}
