//! Corps GF(2⁸) modulo x⁸ + x⁴ + x³ + x + 1 (`0x11B`, le corps d'AES), partagé par le partage de Shamir
//! (`astro-lifecycle::share`) et le code d'effacement de Cauchy ([`crate::encode`]).
//!
//! Tables log/exp de générateur `0x03`, calculées à la compilation (`const fn`) : `EXP` a 512 entrées pour que
//! `EXP[log a + log b]` n'ait jamais besoin de réduction modulo 255.

/// Polynôme réducteur (bit 8 compris).
pub const POLY: u16 = 0x11B;
/// Générateur du groupe multiplicatif utilisé pour les tables.
pub const GENERATOR: u8 = 0x03;

/// `a · 0x03` par décalage et réduction (sert seulement à construire les tables).
const fn times3(a: u8) -> u8 {
    let mut x2 = (a as u16) << 1;
    if x2 & 0x100 != 0 {
        x2 ^= POLY;
    }
    (x2 as u8) ^ a
}

const fn build_exp() -> [u8; 512] {
    let mut t = [0u8; 512];
    let mut x = 1u8;
    let mut i = 0;
    while i < 255 {
        t[i] = x;
        t[i + 255] = x;
        x = times3(x);
        i += 1;
    }
    // 510 et 511 : jamais atteints par log a + log b (≤ 508), recopiés pour la symétrie.
    t[510] = t[0];
    t[511] = t[1];
    t
}

const fn build_log(exp: &[u8; 512]) -> [u8; 256] {
    let mut t = [0u8; 256];
    let mut i = 0;
    while i < 255 {
        t[exp[i] as usize] = i as u8;
        i += 1;
    }
    t // log[0] n'a pas de sens : laissé à 0, jamais lu (mul et inv traitent 0 à part)
}

/// `EXP[i] = 0x03^i` (i ∈ 0..512, période 255).
pub static EXP: [u8; 512] = build_exp();
/// `LOG[a]` tel que `0x03^LOG[a] = a`, pour a ≠ 0.
pub static LOG: [u8; 256] = build_log(&build_exp());

/// Produit dans GF(2⁸).
#[inline]
pub fn mul(a: u8, b: u8) -> u8 {
    if a == 0 || b == 0 { 0 } else { EXP[LOG[a as usize] as usize + LOG[b as usize] as usize] }
}

/// Inverse de `a` ; par convention `inv(0) = 0` (comme a²⁵⁴, l'ancienne définition de `share.rs`).
#[inline]
pub fn inv(a: u8) -> u8 {
    if a == 0 { 0 } else { EXP[255 - LOG[a as usize] as usize] }
}

/// Table de multiplication par la constante `c` : `t[x] = c · x`.
pub fn mul_table(c: u8) -> [u8; 256] {
    let mut t = [0u8; 256];
    for (x, v) in t.iter_mut().enumerate() {
        *v = mul(c, x as u8);
    }
    t
}

/// `dst[i] ^= c · src[i]` (sur la longueur commune).
pub fn mul_add(c: u8, src: &[u8], dst: &mut [u8]) {
    match c {
        0 => {}
        1 => dst.iter_mut().zip(src).for_each(|(d, s)| *d ^= s),
        _ => {
            let t = mul_table(c);
            dst.iter_mut().zip(src).for_each(|(d, s)| *d ^= t[*s as usize]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Multiplication « à la main » (décalages), indépendante des tables : l'ancienne définition de `share.rs`.
    fn slow_mul(mut a: u8, mut b: u8) -> u8 {
        let mut p = 0;
        while b != 0 {
            if b & 1 == 1 {
                p ^= a;
            }
            let carry = a & 0x80;
            a <<= 1;
            if carry != 0 {
                a ^= 0x1b;
            }
            b >>= 1;
        }
        p
    }

    #[test]
    fn tables_agree_with_shift_multiplication_everywhere() {
        for a in 0..=255u8 {
            for b in 0..=255u8 {
                assert_eq!(mul(a, b), slow_mul(a, b), "{a}·{b}");
            }
        }
    }

    #[test]
    fn generator_has_order_255() {
        let mut seen = [false; 256];
        for i in 0..255 {
            assert!(!seen[EXP[i] as usize], "0x03 n'est pas générateur");
            seen[EXP[i] as usize] = true;
        }
        assert!(!seen[0]);
    }

    #[test]
    fn inverse_is_checked_by_product() {
        assert_eq!(inv(0), 0);
        for a in 1..=255u8 {
            assert_eq!(slow_mul(a, inv(a)), 1, "{a}");
        }
    }
}
