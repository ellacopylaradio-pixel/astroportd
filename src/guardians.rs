//! Sécurité estimée d'un compte : disponibilité de chaque gardien et chance de pouvoir tout retrouver.
//!
//! Arithmétique entière en **pour mille** (‰) : une disponibilité de 970 ‰ veut dire « vu 29 jours sur 30 ». Le calcul est
//! pur et généralisé à « au moins k sur n » : aujourd'hui une seule part de clé (k = 1) et une copie complète des fichiers chez
//! chaque gardien suffisent ; le codage à effacement des sauvegardes demandera k > 1.

/// Fenêtre d'observation glissante de la présence d'un gardien (jours).
pub const WINDOW_DAYS: i64 = 30;
/// Recul minimal avant d'oser un chiffre (jours d'observation).
pub const MIN_DAYS: i64 = 7;

/// Échelle interne : 10¹⁸ = certitude (tient dans un `u128` multiplié par 1 000).
const ONE: u128 = 1_000_000_000_000_000_000;

/// Probabilité (‰) qu'au moins `k` des gardiens soient joignables, chacun indépendamment avec la disponibilité `dispos[i]`
/// (‰, plafonnée à 1 000). Arrondi par défaut : on ne surestime jamais la sécurité. `k = 0` rend 1 000 ; `k > n`, 0.
pub fn proba_au_moins_k(dispos: &[u32], k: usize) -> u32 {
    if k == 0 {
        return 1000;
    }
    if k > dispos.len() {
        return 0;
    }
    // dp[j] = probabilité d'avoir exactement j gardiens joignables parmi ceux déjà vus.
    let mut dp = vec![0u128; dispos.len() + 1];
    dp[0] = ONE;
    for (i, &p) in dispos.iter().enumerate() {
        let p = u128::from(p.min(1000));
        for j in (0..=i + 1).rev() {
            let stay = dp[j] * (1000 - p) / 1000;
            let up = if j > 0 { dp[j - 1] * p / 1000 } else { 0 };
            dp[j] = stay + up;
        }
    }
    let at_least: u128 = dp[k..].iter().sum();
    u32::try_from(at_least / (ONE / 1000)).unwrap_or(1000).min(1000)
}

/// Disponibilité (‰) d'un gardien vu `seen_days` jours sur une fenêtre de `window_days` jours ; `None` sans recul suffisant.
pub fn availability(seen_days: i64, window_days: i64) -> Option<u32> {
    if window_days < MIN_DAYS {
        return None;
    }
    let w = window_days.min(WINDOW_DAYS);
    u32::try_from(seen_days.clamp(0, w) * 1000 / w).ok()
}

/// Estimation pour l'écran : chance de retrouver le compte (‰), et ce qu'elle deviendrait avec un gardien de plus aussi présent
/// que le meilleur actuel (ou que 900 ‰ sans référence) — seulement quand cela fait gagner au moins un point.
pub fn estimate(dispos: &[Option<u32>]) -> (Option<u32>, Option<u32>) {
    if dispos.is_empty() || dispos.iter().any(Option::is_none) {
        return (None, None);
    }
    let known: Vec<u32> = dispos.iter().flatten().copied().collect();
    let now = proba_au_moins_k(&known, 1);
    let mut more = known.clone();
    more.push(known.iter().copied().max().filter(|&p| p > 0).unwrap_or(900));
    let next = proba_au_moins_k(&more, 1);
    (Some(now), (now < 990 && next >= now + 10).then_some(next))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn au_moins_un_gardien() {
        assert_eq!(proba_au_moins_k(&[], 1), 0, "aucun gardien : rien à espérer");
        assert_eq!(proba_au_moins_k(&[900], 1), 900);
        assert_eq!(proba_au_moins_k(&[900, 900], 1), 990);
        assert_eq!(proba_au_moins_k(&[900, 900, 900], 1), 999);
        assert_eq!(proba_au_moins_k(&[0, 0], 1), 0);
        assert_eq!(proba_au_moins_k(&[1000, 0], 1), 1000);
        assert_eq!(proba_au_moins_k(&[5000], 1), 1000, "une disponibilité au-delà de 1 000 ‰ est plafonnée");
    }

    #[test]
    fn k_sur_n() {
        assert_eq!(proba_au_moins_k(&[500, 500, 500], 0), 1000);
        assert_eq!(proba_au_moins_k(&[500, 500, 500], 2), 500);
        assert_eq!(proba_au_moins_k(&[500, 500, 500], 3), 125);
        assert_eq!(proba_au_moins_k(&[500, 500], 3), 0, "k > n");
        assert_eq!(proba_au_moins_k(&[900, 800], 2), 720);
        // Arrondi par défaut, jamais au-dessus du vrai 0,999 999 9.
        assert_eq!(proba_au_moins_k(&[990; 4], 1), 999);
        // Beaucoup de gardiens : pas de débordement, résultat borné.
        assert_eq!(proba_au_moins_k(&[999; 64], 1), 999, "presque sûr n'est jamais arrondi à la certitude");
        assert_eq!(proba_au_moins_k(&[999; 64], 64), 937);
    }

    #[test]
    fn disponibilite_et_recul() {
        assert_eq!(availability(29, 30), Some(966));
        assert_eq!(availability(40, 90), Some(1000), "la fenêtre est plafonnée à 30 jours");
        assert_eq!(availability(3, 6), None, "moins de 7 jours de recul : pas de chiffre");
        assert_eq!(availability(7, 7), Some(1000));
    }

    #[test]
    fn estimation_et_suggestion() {
        assert_eq!(estimate(&[]), (None, None));
        assert_eq!(estimate(&[Some(900), None]), (None, None), "un gardien sans recul : pas de chiffre inventé");
        assert_eq!(estimate(&[Some(900)]), (Some(900), Some(990)));
        assert_eq!(estimate(&[Some(990), Some(990)]), (Some(999), None), "déjà sûr : pas de suggestion");
        assert_eq!(estimate(&[Some(0)]), (Some(0), Some(900)));
    }
}
