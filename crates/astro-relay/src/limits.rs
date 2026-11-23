//! Limitation de débit par source (seau à jetons).
//!
//! Une source est une adresse IPv4, ou un préfixe IPv6 /64 : un fournisseur d'accès donne un /64 entier à chaque abonné,
//! compter chaque adresse IPv6 à part donnerait à un seul attaquant des milliards de seaux pleins.
//! La table est **bornée** : quand elle est pleine, les seaux revenus pleins (équivalents à une source jamais vue) sont
//! oubliés, au plus une fois par seconde ; si elle reste pleine, les nouvelles sources partagent un seau commun de
//! débordement. Une inondation d'adresses ne fait donc ni grossir la mémoire ni parcourir la table à chaque requête.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv6Addr};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Nombre maximal de sources suivies chacune avec son propre seau.
const MAX_TRACKED: usize = 10_000;
/// Intervalle minimal entre deux purges de la table pleine.
const SWEEP_EVERY: Duration = Duration::from_secs(1);

/// Seau : jetons restants et dernier passage.
type Bucket = (f64, Instant);

struct State {
    buckets: HashMap<IpAddr, Bucket>,
    /// Seau commun des sources nouvelles quand la table est pleine.
    overflow: Bucket,
    swept: Option<Instant>,
}

/// Seau à jetons par source : `burst` jetons au repos, `per_minute` jetons rendus par minute.
pub struct RateLimiter {
    burst: f64,
    per_sec: f64,
    state: Mutex<State>,
}

/// La source d'une adresse : l'IPv4 elle-même (y compris sous forme `::ffff:a.b.c.d`), ou le /64 d'une IPv6.
fn source(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(_) => ip,
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => {
                let s = v6.segments();
                IpAddr::V6(Ipv6Addr::new(s[0], s[1], s[2], s[3], 0, 0, 0, 0))
            }
        },
    }
}

impl RateLimiter {
    /// Crée un limiteur.
    pub fn new(burst: u32, per_minute: u32) -> Self {
        let burst = f64::from(burst);
        let state = State { buckets: HashMap::new(), overflow: (burst, Instant::now()), swept: None };
        Self { burst, per_sec: f64::from(per_minute) / 60.0, state: Mutex::new(state) }
    }

    /// Consomme un jeton pour `ip` ; `false` si la limite est atteinte.
    pub fn check(&self, ip: IpAddr) -> bool {
        self.check_at(ip, Instant::now())
    }

    /// Variante à horloge injectée.
    pub fn check_at(&self, ip: IpAddr, now: Instant) -> bool {
        let key = source(ip);
        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let st = &mut *guard;
        if st.buckets.len() >= MAX_TRACKED && !st.buckets.contains_key(&key) && st.swept.is_none_or(|t| now.saturating_duration_since(t) >= SWEEP_EVERY) {
            st.swept = Some(now);
            let (burst, per_sec) = (self.burst, self.per_sec);
            // Un seau revenu plein ne se distingue pas d'une source jamais vue : l'oublier ne change aucune décision.
            st.buckets.retain(|_, (tokens, last)| *tokens + now.saturating_duration_since(*last).as_secs_f64() * per_sec < burst);
        }
        let bucket = if st.buckets.len() < MAX_TRACKED || st.buckets.contains_key(&key) {
            st.buckets.entry(key).or_insert((self.burst, now))
        } else {
            &mut st.overflow
        };
        let elapsed = now.saturating_duration_since(bucket.1).as_secs_f64();
        bucket.0 = (bucket.0 + elapsed * self.per_sec).min(self.burst);
        bucket.1 = now;
        if bucket.0 >= 1.0 {
            bucket.0 -= 1.0;
            true
        } else {
            false
        }
    }

    #[cfg(test)]
    fn tracked(&self) -> usize {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).buckets.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(n: u8) -> IpAddr {
        IpAddr::from([10, 0, 0, n])
    }

    #[test]
    fn burst_then_refill() {
        let l = RateLimiter::new(3, 60); // 1 jeton par seconde
        let t0 = Instant::now();
        assert!(l.check_at(ip(1), t0));
        assert!(l.check_at(ip(1), t0));
        assert!(l.check_at(ip(1), t0));
        assert!(!l.check_at(ip(1), t0), "burst épuisé");
        assert!(!l.check_at(ip(1), t0 + Duration::from_millis(500)));
        assert!(l.check_at(ip(1), t0 + Duration::from_millis(1500)), "un jeton rendu après 1 s");
        assert!(!l.check_at(ip(1), t0 + Duration::from_millis(1500)));
    }

    #[test]
    fn addresses_are_independent_and_refill_is_capped() {
        let l = RateLimiter::new(2, 60);
        let t0 = Instant::now();
        assert!(l.check_at(ip(1), t0) && l.check_at(ip(1), t0));
        assert!(!l.check_at(ip(1), t0));
        assert!(l.check_at(ip(2), t0), "une autre IP a son propre seau");
        let later = t0 + Duration::from_secs(3600);
        assert!(l.check_at(ip(1), later) && l.check_at(ip(1), later));
        assert!(!l.check_at(ip(1), later), "le seau ne dépasse jamais le burst");
    }

    #[test]
    fn hostile_rotation_inside_one_ipv6_64_shares_one_bucket() {
        let l = RateLimiter::new(2, 1);
        let t0 = Instant::now();
        let addr = |n: u16| IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 1, 2, n, n, n, n));
        assert!(l.check_at(addr(1), t0) && l.check_at(addr(2), t0));
        for n in 3..20_000 {
            assert!(!l.check_at(addr(n), t0), "même /64 : même seau, même avec une adresse neuve à chaque requête");
        }
        assert_eq!(l.tracked(), 1);
        assert!(l.check_at(IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 1, 3, 0, 0, 0, 1)), t0), "un autre /64 est une autre source");
        // Une IPv4 vue en IPv6 (double pile) reste la même source.
        let v4 = IpAddr::from([192, 0, 2, 7]);
        assert!(l.check_at(v4, t0) && l.check_at(v4, t0));
        assert!(!l.check_at(IpAddr::V6(Ipv6Addr::new(0, 0, 0, 0, 0, 0xffff, 0xc000, 0x0207)), t0));
    }

    #[test]
    fn hostile_flood_of_sources_keeps_the_table_bounded() {
        let l = RateLimiter::new(1, 60);
        let t0 = Instant::now();
        let v4 = |n: u32| IpAddr::from(n.to_be_bytes());
        for n in 0..(MAX_TRACKED as u32 + 5_000) {
            l.check_at(v4(0x0b00_0000 + n), t0);
        }
        assert_eq!(l.tracked(), MAX_TRACKED, "la table ne grossit plus au-delà de sa borne");
        // Les sources en trop partagent le seau de débordement : la première passe, les suivantes attendent.
        assert!(!l.check_at(v4(0x0c00_0000), t0));
        // Une source déjà suivie garde son propre seau.
        assert!(l.check_at(v4(0x0b00_0000), t0 + Duration::from_secs(1)));
        // Une minute plus tard, les seaux sont revenus pleins : la purge fait de la place aux nouvelles sources.
        let later = t0 + Duration::from_secs(61);
        assert!(l.check_at(v4(0x0d00_0000), later));
        assert!(l.tracked() < MAX_TRACKED);
    }
}
