//! Rencontres (« Se rencontrer ») : distinguer, pour calibrer Ğ1-N² (anti-Sybil), une **rencontre prouvée par un QR
//! scanné** d'une **liaison par nomination** (choisie dans une liste, un lien, un code de paiement imprimé).
//!
//! - **Preuve de présence** : le code montré à l'écran porte `<tranche>.<signature>` — la clé du membre montré signe la
//!   tranche de temps courante ([`SLOT_SECS`]), sous la forme d'un événement Nostr éphémère reconstructible
//!   ([`PROOF_KIND`], jamais publié). Le code se renouvelle ; une photo envoyée à distance expire en quelques minutes.
//! - **Attestation** (kind [`MEETING`](astro_nostr_proto::kind::MEETING), remplaçable par `d` = clé de l'autre), signée par
//!   celui qui ajoute : `how` = [`QR`] avec la preuve, ou [`NAMED`]. Toute station la revérifie seule
//!   ([`Meeting::from_event`]) : une rencontre n'est **vérifiée** que si la preuve est signée par la personne rencontrée et
//!   datée du moment de l'attestation (à [`PROOF_TTL_SECS`] près). Une rencontre vérifiée dans les deux sens est la plus forte.

use astro_nostr_proto::{Event, UnsignedEvent, is_hex64, kind};

/// Durée d'une tranche de temps de la preuve (secondes).
pub const SLOT_SECS: u64 = 120;
/// Âge maximal d'une preuve au moment de l'attestation : tranche courante et deux précédentes (temps de viser, horloges).
pub const PROOF_TTL_SECS: u64 = 3 * SLOT_SECS;
/// Kind de l'événement éphémère signé par la preuve (plage 20000-29999 : jamais rangé par un relai).
pub const PROOF_KIND: u32 = 27_855;
/// Lien né d'un QR « rencontre » scanné, preuve à l'appui.
pub const QR: &str = "qr";
/// Lien choisi sans preuve de présence (liste, recherche, lien, code de paiement).
pub const NAMED: &str = "named";

/// Événement (non publié) dont la signature fait la preuve : reconstruit à l'identique par le vérificateur.
fn proof_event(slot: u64) -> UnsignedEvent {
    UnsignedEvent { created_at: slot * SLOT_SECS, kind: PROOF_KIND, tags: vec![vec!["t".into(), "uplanet-meet".into()]], content: String::new() }
}

/// Preuve de présence du titulaire de `secret` à l'instant `now` : `<tranche>.<signature hex>`.
pub fn proof(secret: &[u8; 32], now: u64) -> Option<String> {
    let slot = now / SLOT_SECS;
    proof_event(slot).sign(secret).ok().map(|e| format!("{slot}.{}", e.sig))
}

/// La preuve est-elle signée par `shown` et assez fraîche à l'instant `at` ?
pub fn verify_proof(shown: &str, proof: &str, at: u64) -> bool {
    let Some((slot, sig)) = proof.split_once('.') else { return false };
    let Ok(slot) = slot.parse::<u64>() else { return false };
    let Some(start) = slot.checked_mul(SLOT_SECS) else { return false };
    // Pas plus de deux minutes dans le futur (horloges), pas plus vieille que la durée de vie.
    if start > at.saturating_add(SLOT_SECS) || at.saturating_sub(start) > PROOF_TTL_SECS || sig.len() != 128 || !is_hex64(shown) {
        return false;
    }
    let u = proof_event(slot);
    let mut e = Event { id: String::new(), pubkey: shown.to_owned(), created_at: u.created_at, kind: u.kind, tags: u.tags, content: u.content, sig: sig.to_owned() };
    match e.compute_id() {
        Ok(id) => e.id = id.iter().map(|b| format!("{b:02x}")).collect(),
        Err(_) => return false,
    }
    e.verify().is_ok()
}

/// Tags d'une attestation de rencontre de `target`.
pub fn tags(target: &str, how: &str, proof: Option<&str>) -> Vec<Vec<String>> {
    let mut t = vec![vec!["d".into(), target.to_owned()], vec!["p".into(), target.to_owned()], vec!["t".into(), "meet".into()], vec!["how".into(), how.to_owned()]];
    if let Some(p) = proof {
        t.push(vec!["proof".into(), p.to_owned()]);
    }
    t
}

/// Attestation de rencontre lue et revérifiée.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Meeting {
    /// Celui qui atteste (auteur).
    pub owner: String,
    /// La personne rencontrée.
    pub target: String,
    /// [`QR`] ou [`NAMED`] (une autre valeur est lue comme [`NAMED`]).
    pub how: &'static str,
    /// Preuve de présence valide : signée par `target`, datée du moment de l'attestation.
    pub verified: bool,
    /// Instant de l'attestation.
    pub at: u64,
}

impl Meeting {
    /// Lit une attestation (kind `MEETING`) ; `None` si elle est mal formée.
    pub fn from_event(e: &Event) -> Option<Self> {
        if e.kind != kind::MEETING {
            return None;
        }
        let target = e.tag_value("p")?;
        if !is_hex64(target) || e.tag_value("d") != Some(target) || target == e.pubkey {
            return None;
        }
        let how = if e.tag_value("how") == Some(QR) { QR } else { NAMED };
        let verified = how == QR && e.tag_value("proof").is_some_and(|p| verify_proof(target, p, e.created_at));
        Some(Self { owner: e.pubkey.clone(), target: target.to_owned(), how, verified, at: e.created_at })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use astro_nostr_proto::pubkey_of;

    const BOB: [u8; 32] = [0xB2; 32];
    const ALICE: [u8; 32] = [0xA1; 32];
    const EVE: [u8; 32] = [0xE5; 32];

    fn attest(secret: &[u8; 32], target: &str, how: &str, proof: Option<&str>, at: u64) -> Event {
        UnsignedEvent { created_at: at, kind: kind::MEETING, tags: tags(target, how, proof), content: String::new() }.sign(secret).unwrap()
    }

    #[test]
    fn a_fresh_code_scanned_face_to_face_is_a_verified_meeting() {
        let (bob, now) = (pubkey_of(&BOB).unwrap(), 1_790_000_000);
        let p = proof(&BOB, now).unwrap();
        assert!(verify_proof(&bob, &p, now + 30));
        let m = Meeting::from_event(&attest(&ALICE, &bob, QR, Some(&p), now + 30)).unwrap();
        assert_eq!((m.how, m.verified, m.target.as_str()), (QR, true, bob.as_str()));
    }

    #[test]
    fn a_photo_of_the_code_sent_later_no_longer_proves_anything() {
        let (bob, now) = (pubkey_of(&BOB).unwrap(), 1_790_000_000);
        let p = proof(&BOB, now).unwrap();
        assert!(!verify_proof(&bob, &p, now + PROOF_TTL_SECS + SLOT_SECS), "code expiré");
        assert!(!verify_proof(&bob, &p, now - 3 * SLOT_SECS), "attestation antidatée avant le code");
        let m = Meeting::from_event(&attest(&ALICE, &bob, QR, Some(&p), now + 3600)).unwrap();
        assert_eq!((m.how, m.verified), (QR, false), "se dire « qr » ne suffit pas");
    }

    #[test]
    fn a_proof_signed_by_someone_else_or_tampered_is_refused() {
        let (bob, now) = (pubkey_of(&BOB).unwrap(), 1_790_000_000);
        let forged = proof(&EVE, now).unwrap();
        assert!(!verify_proof(&bob, &forged, now), "Eve ne prouve pas la présence de Bob");
        let p = proof(&BOB, now).unwrap();
        let (slot, sig) = p.split_once('.').unwrap();
        assert!(!verify_proof(&bob, &format!("{}.{sig}", slot.parse::<u64>().unwrap() + 1), now), "tranche changée");
        for bad in ["", "abc", "12.zz", &format!("{slot}.{}", &sig[..64]), &format!("{}.{sig}", u64::MAX)] {
            assert!(!verify_proof(&bob, bad, now), "{bad}");
        }
    }

    #[test]
    fn a_named_link_is_never_verified_and_malformed_attestations_are_ignored() {
        let (bob, alice, now) = (pubkey_of(&BOB).unwrap(), pubkey_of(&ALICE).unwrap(), 1_790_000_000);
        let m = Meeting::from_event(&attest(&ALICE, &bob, NAMED, None, now)).unwrap();
        assert_eq!((m.how, m.verified), (NAMED, false));
        let p = proof(&BOB, now).unwrap();
        assert_eq!(Meeting::from_event(&attest(&ALICE, &bob, "sur l'honneur", Some(&p), now)).unwrap().how, NAMED);
        assert!(Meeting::from_event(&attest(&ALICE, &alice, QR, None, now)).is_none(), "soi-même");
        let mut odd = attest(&ALICE, &bob, QR, Some(&p), now);
        odd.tags.retain(|t| t[0] != "d");
        assert!(Meeting::from_event(&odd).is_none(), "sans d");
    }
}
