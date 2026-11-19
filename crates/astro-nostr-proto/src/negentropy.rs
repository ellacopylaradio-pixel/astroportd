//! Negentropy, protocole V1 (NIP-77) : réconciliation d'ensembles par plages.
//!
//! Deux participants comparent leurs ensembles d'enregistrements `(created_at, id)` **sans se les envoyer** : ils échangent des
//! empreintes de plages, découpent celles qui diffèrent, et ne s'envoient des listes d'identifiants que pour les petites plages
//! en désaccord. Le coût dépend du nombre de **différences**, pas de la taille des ensembles. Implémentation fidèle à la
//! référence (hoytech/negentropy, utilisée par strfry) : messages interopérables, octet pour octet.

use sha2::{Digest, Sha256};

/// Octet de version du protocole V1.
pub const PROTOCOL_VERSION: u8 = 0x61;
const ID_SIZE: usize = 32;
const FINGERPRINT_SIZE: usize = 16;
const BUCKETS: usize = 16;
const MODE_SKIP: u64 = 0;
const MODE_FINGERPRINT: u64 = 1;
const MODE_ID_LIST: u64 = 2;
/// Horodatage « infini » (borne de fin).
const INFINITY: u64 = u64::MAX;

/// Erreurs de décodage d'un message.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum NegError {
    /// Message tronqué ou mal formé.
    #[error("message Negentropy mal formé : {0}")]
    Malformed(&'static str),
    /// Version de protocole non prise en charge.
    #[error("version de protocole Negentropy non prise en charge : {0:#x}")]
    Version(u8),
}

/// Un enregistrement : horodatage et identifiant de 32 octets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Item {
    /// `created_at` de l'événement.
    pub timestamp: u64,
    /// Identifiant de l'événement.
    pub id: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Bound {
    timestamp: u64,
    prefix: Vec<u8>,
}

impl Bound {
    fn new(timestamp: u64) -> Self {
        Self { timestamp, prefix: Vec::new() }
    }

    /// `item` est-il avant la borne ? (préfixe complété de zéros)
    fn after(&self, item: &Item) -> bool {
        match item.timestamp.cmp(&self.timestamp) {
            std::cmp::Ordering::Less => true,
            std::cmp::Ordering::Greater => false,
            std::cmp::Ordering::Equal => {
                let mut padded = [0u8; 32];
                padded[..self.prefix.len()].copy_from_slice(&self.prefix);
                item.id < padded
            }
        }
    }
}

/// Ensemble d'enregistrements triés (horodatage puis identifiant).
#[derive(Debug, Clone, Default)]
pub struct Storage {
    items: Vec<Item>,
}

impl Storage {
    /// Ensemble à partir d'enregistrements quelconques (triés et dédoublonnés ici). Un horodatage `u64::MAX` est réservé :
    /// il est ramené à `u64::MAX - 1`.
    pub fn new(mut items: Vec<Item>) -> Self {
        for i in &mut items {
            i.timestamp = i.timestamp.min(INFINITY - 1);
        }
        items.sort_unstable();
        items.dedup();
        Self { items }
    }

    /// Nombre d'enregistrements.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Ensemble vide ?
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    fn lower_bound(&self, from: usize, to: usize, bound: &Bound) -> usize {
        from + self.items[from..to].partition_point(|i| bound.after(i))
    }

    fn fingerprint(&self, from: usize, to: usize) -> [u8; FINGERPRINT_SIZE] {
        // Somme modulo 2^256 des identifiants lus en petit-boutiste, puis le nombre d'éléments (varint), puis SHA-256.
        let mut sum = [0u8; 32];
        for item in &self.items[from..to] {
            let mut carry = 0u16;
            for (s, b) in sum.iter_mut().zip(item.id.iter()) {
                let v = u16::from(*s) + u16::from(*b) + carry;
                *s = (v & 0xff) as u8;
                carry = v >> 8;
            }
        }
        let mut h = Sha256::new();
        h.update(sum);
        h.update(varint(u64::try_from(to - from).unwrap_or(u64::MAX)));
        let mut out = [0u8; FINGERPRINT_SIZE];
        out.copy_from_slice(&h.finalize()[..FINGERPRINT_SIZE]);
        out
    }
}

fn varint(mut n: u64) -> Vec<u8> {
    if n == 0 {
        return vec![0];
    }
    let mut digits = Vec::new();
    while n > 0 {
        digits.push((n & 0x7f) as u8);
        n >>= 7;
    }
    digits.reverse();
    let last = digits.len() - 1;
    for d in &mut digits[..last] {
        *d |= 0x80;
    }
    digits
}

struct Reader<'a> {
    buf: &'a [u8],
}

impl Reader<'_> {
    fn byte(&mut self) -> Result<u8, NegError> {
        let (&b, rest) = self.buf.split_first().ok_or(NegError::Malformed("fin de message inattendue"))?;
        self.buf = rest;
        Ok(b)
    }
    fn bytes(&mut self, n: usize) -> Result<&[u8], NegError> {
        if self.buf.len() < n {
            return Err(NegError::Malformed("fin de message inattendue"));
        }
        let (head, rest) = self.buf.split_at(n);
        self.buf = rest;
        Ok(head)
    }
    fn varint(&mut self) -> Result<u64, NegError> {
        let mut n: u64 = 0;
        for _ in 0..10 {
            let b = self.byte()?;
            n = n.checked_mul(128).ok_or(NegError::Malformed("varint trop grand"))? | u64::from(b & 0x7f);
            if b & 0x80 == 0 {
                return Ok(n);
            }
        }
        Err(NegError::Malformed("varint trop long"))
    }
}

/// Un participant à la réconciliation.
pub struct Negentropy {
    storage: Storage,
    frame_limit: usize,
    initiator: bool,
    last_in: u64,
    last_out: u64,
}

/// Ce qu'un tour de réconciliation apprend à l'initiateur.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Round {
    /// Message suivant à envoyer ; `None` : la réconciliation est terminée.
    pub next: Option<Vec<u8>>,
    /// Identifiants que j'ai et que l'autre n'a pas.
    pub have: Vec<[u8; 32]>,
    /// Identifiants que l'autre a et que je n'ai pas.
    pub need: Vec<[u8; 32]>,
}

impl Negentropy {
    /// `frame_limit` : taille maximale d'un message en octets (0 : aucune ; sinon au moins 4096).
    pub fn new(storage: Storage, frame_limit: usize) -> Self {
        let frame_limit = if frame_limit == 0 { 0 } else { frame_limit.max(4096) };
        Self { storage, frame_limit, initiator: false, last_in: 0, last_out: 0 }
    }

    /// Premier message de l'initiateur.
    pub fn initiate(&mut self) -> Vec<u8> {
        self.initiator = true;
        self.last_out = 0;
        let mut out = vec![PROTOCOL_VERSION];
        self.split_range(0, self.storage.len(), &Bound::new(INFINITY), &mut out);
        out
    }

    fn exceeded(&self, n: usize) -> bool {
        self.frame_limit != 0 && n > self.frame_limit - 200
    }

    fn encode_timestamp(&mut self, ts: u64) -> Vec<u8> {
        if ts == INFINITY {
            self.last_out = INFINITY;
            return varint(0);
        }
        let delta = ts.saturating_sub(self.last_out);
        self.last_out = ts;
        varint(delta + 1)
    }

    fn encode_bound(&mut self, b: &Bound) -> Vec<u8> {
        let mut out = self.encode_timestamp(b.timestamp);
        out.extend(varint(b.prefix.len() as u64));
        out.extend(&b.prefix);
        out
    }

    fn decode_bound(&mut self, r: &mut Reader<'_>) -> Result<Bound, NegError> {
        let raw = r.varint()?;
        let timestamp = if raw == 0 || self.last_in == INFINITY {
            INFINITY
        } else {
            let t = (raw - 1).checked_add(self.last_in).ok_or(NegError::Malformed("horodatage hors limites"))?;
            if t == INFINITY {
                return Err(NegError::Malformed("horodatage hors limites"));
            }
            t
        };
        self.last_in = timestamp;
        let len = usize::try_from(r.varint()?).map_err(|_| NegError::Malformed("préfixe trop long"))?;
        if len > ID_SIZE {
            return Err(NegError::Malformed("préfixe trop long"));
        }
        Ok(Bound { timestamp, prefix: r.bytes(len)?.to_vec() })
    }

    fn minimal_bound(prev: &Item, curr: &Item) -> Bound {
        if curr.timestamp != prev.timestamp {
            return Bound::new(curr.timestamp);
        }
        let shared = prev.id.iter().zip(curr.id.iter()).take_while(|(a, b)| a == b).count();
        Bound { timestamp: curr.timestamp, prefix: curr.id[..(shared + 1).min(ID_SIZE)].to_vec() }
    }

    fn split_range(&mut self, lower: usize, upper: usize, upper_bound: &Bound, out: &mut Vec<u8>) {
        let n = upper - lower;
        if n < BUCKETS * 2 {
            out.extend(self.encode_bound(upper_bound));
            out.extend(varint(MODE_ID_LIST));
            out.extend(varint(n as u64));
            for item in &self.storage.items[lower..upper] {
                out.extend(item.id);
            }
            return;
        }
        let (per, extra) = (n / BUCKETS, n % BUCKETS);
        let mut curr = lower;
        for i in 0..BUCKETS {
            let size = per + usize::from(i < extra);
            let fp = self.storage.fingerprint(curr, curr + size);
            curr += size;
            let next = if curr == upper { upper_bound.clone() } else { Self::minimal_bound(&self.storage.items[curr - 1], &self.storage.items[curr]) };
            out.extend(self.encode_bound(&next));
            out.extend(varint(MODE_FINGERPRINT));
            out.extend(fp);
        }
    }

    /// Traite un message reçu. Côté **relai** (non initiateur), rend toujours la réponse (`next`) ; côté **initiateur**, rend
    /// ce qu'il a appris (`have`, `need`) et le message suivant, ou `None` quand tout est réconcilié.
    pub fn reconcile(&mut self, query: &[u8]) -> Result<Round, NegError> {
        let mut round = Round::default();
        let mut r = Reader { buf: query };
        self.last_in = 0;
        self.last_out = 0;
        let mut full = vec![PROTOCOL_VERSION];
        let version = r.byte()?;
        if !(0x60..=0x6f).contains(&version) {
            return Err(NegError::Malformed("octet de version invalide"));
        }
        if version != PROTOCOL_VERSION {
            if self.initiator {
                return Err(NegError::Version(version));
            }
            round.next = Some(full);
            return Ok(round);
        }
        let size = self.storage.len();
        let mut prev_bound = Bound::new(0);
        let mut prev_index = 0usize;
        let mut skip = false;
        while !r.buf.is_empty() {
            let mut o: Vec<u8> = Vec::new();
            let curr_bound = self.decode_bound(&mut r)?;
            let mode = r.varint()?;
            let lower = prev_index;
            let mut upper = self.storage.lower_bound(prev_index, size, &curr_bound);
            let flush_skip = |s: &mut Self, skip: &mut bool, o: &mut Vec<u8>| {
                if *skip {
                    *skip = false;
                    o.extend(s.encode_bound(&prev_bound));
                    o.extend(varint(MODE_SKIP));
                }
            };
            match mode {
                MODE_SKIP => skip = true,
                MODE_FINGERPRINT => {
                    let theirs = r.bytes(FINGERPRINT_SIZE)?.to_vec();
                    if theirs[..] != self.storage.fingerprint(lower, upper)[..] {
                        flush_skip(self, &mut skip, &mut o);
                        self.split_range(lower, upper, &curr_bound, &mut o);
                    } else {
                        skip = true;
                    }
                }
                MODE_ID_LIST => {
                    let n = usize::try_from(r.varint()?).map_err(|_| NegError::Malformed("liste trop longue"))?;
                    if n > r.buf.len() / ID_SIZE {
                        return Err(NegError::Malformed("liste d'identifiants tronquée"));
                    }
                    let mut theirs: std::collections::BTreeSet<[u8; 32]> = std::collections::BTreeSet::new();
                    for _ in 0..n {
                        let mut id = [0u8; 32];
                        id.copy_from_slice(r.bytes(ID_SIZE)?);
                        theirs.insert(id);
                    }
                    for item in &self.storage.items[lower..upper] {
                        if !theirs.remove(&item.id) && self.initiator {
                            round.have.push(item.id);
                        }
                    }
                    if self.initiator {
                        skip = true;
                        round.need.extend(theirs);
                    } else {
                        flush_skip(self, &mut skip, &mut o);
                        let mut ids: Vec<u8> = Vec::new();
                        let (mut count, mut end_bound) = (0u64, curr_bound.clone());
                        for (index, item) in self.storage.items[lower..upper].iter().enumerate() {
                            if self.exceeded(full.len() + ids.len()) {
                                end_bound = Bound { timestamp: item.timestamp, prefix: item.id.to_vec() };
                                upper = lower + index;
                                break;
                            }
                            ids.extend(item.id);
                            count += 1;
                        }
                        o.extend(self.encode_bound(&end_bound));
                        o.extend(varint(MODE_ID_LIST));
                        o.extend(varint(count));
                        o.extend(ids);
                        full.extend(std::mem::take(&mut o));
                    }
                }
                _ => return Err(NegError::Malformed("mode inconnu")),
            }
            if self.exceeded(full.len() + o.len()) {
                // Trame pleine : le reste de l'espace est résumé par une seule empreinte, l'autre redemandera.
                let fp = self.storage.fingerprint(upper, size);
                full.extend(self.encode_bound(&Bound::new(INFINITY)));
                full.extend(varint(MODE_FINGERPRINT));
                full.extend(fp);
                break;
            }
            full.extend(o);
            prev_index = upper;
            prev_bound = curr_bound;
        }
        round.next = if self.initiator && full.len() == 1 { None } else { Some(full) };
        Ok(round)
    }
}

/// Différence trouvée : `(ce que A a seul, ce que B a seul, octets échangés)`.
pub type Difference = (Vec<[u8; 32]>, Vec<[u8; 32]>, usize);

/// Réconcilie deux ensembles en mémoire (les deux rôles ici) : `(ce que A a seul, ce que B a seul, octets échangés)`. Sert
/// aux tests et à mesurer le gain avant de passer par le réseau.
pub fn reconcile_local(a: &Storage, b: &Storage, frame_limit: usize) -> Result<Difference, NegError> {
    let (mut client, mut relay) = (Negentropy::new(a.clone(), frame_limit), Negentropy::new(b.clone(), frame_limit));
    let mut msg = client.initiate();
    let (mut have, mut need, mut bytes) = (Vec::new(), Vec::new(), msg.len());
    for _ in 0..1000 {
        let answer = relay.reconcile(&msg)?.next.unwrap_or_default();
        bytes += answer.len();
        let round = client.reconcile(&answer)?;
        have.extend(round.have);
        need.extend(round.need);
        match round.next {
            Some(m) => {
                bytes += m.len();
                msg = m;
            }
            None => break,
        }
    }
    have.sort_unstable();
    need.sort_unstable();
    Ok((have, need, bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(ts: u64, n: u32) -> Item {
        let mut id = [0u8; 32];
        id[..4].copy_from_slice(&n.to_be_bytes());
        id[31] = (n % 251) as u8;
        Item { timestamp: ts, id }
    }

    #[test]
    fn varints_follow_the_spec() {
        assert_eq!(varint(0), [0]);
        assert_eq!(varint(127), [0x7f]);
        assert_eq!(varint(128), [0x81, 0x00]);
        assert_eq!(Reader { buf: &varint(1_000_000) }.varint(), Ok(1_000_000));
    }

    #[test]
    fn identical_sets_agree_in_one_round_trip() {
        let s = Storage::new((0..10_000).map(|n| item(1_700_000_000 + u64::from(n / 3), n)).collect());
        let (have, need, bytes) = reconcile_local(&s, &s, 0).unwrap();
        assert!(have.is_empty() && need.is_empty());
        assert!(bytes < 1200, "10 000 événements identiques : {bytes} octets échangés");
    }

    #[test]
    fn a_few_differences_are_found_for_a_few_kilobytes() {
        let common: Vec<Item> = (0..20_000).map(|n| item(1_700_000_000 + u64::from(n), n)).collect();
        let mut a = common.clone();
        a.extend([item(5, 900_001), item(1_700_005_000, 900_002)]);
        let mut b = common;
        b.extend((0..30).map(|n| item(1_700_010_000 + u64::from(n), 800_000 + n)));
        b.retain(|i| i.timestamp != 1_700_012_345);
        let (have, need, bytes) = reconcile_local(&Storage::new(a.clone()), &Storage::new(b.clone()), 0).unwrap();
        let mut want_have: Vec<[u8; 32]> = a.iter().filter(|x| !b.contains(x)).map(|x| x.id).collect();
        let mut want_need: Vec<[u8; 32]> = b.iter().filter(|x| !a.contains(x)).map(|x| x.id).collect();
        want_have.sort_unstable();
        want_need.sort_unstable();
        assert_eq!((have, need), (want_have, want_need));
        // 20 000 identifiants de 32 octets pèseraient 640 Ko : la réconciliation n'en échange qu'une fraction.
        assert!(bytes < 40_000, "{bytes} octets pour 33 différences sur 20 000");
    }

    #[test]
    fn a_frame_limit_splits_the_work_without_losing_anything() {
        let a = Storage::new((0..3000).map(|n| item(u64::from(n), n)).collect());
        let b = Storage::new((1500..6000).map(|n| item(u64::from(n), n)).collect());
        let (have, need, _) = reconcile_local(&a, &b, 4096).unwrap();
        assert_eq!((have.len(), need.len()), (1500, 3000));
    }

    #[test]
    fn empty_sets_and_bad_messages() {
        let (have, need, _) = reconcile_local(&Storage::default(), &Storage::new(vec![item(1, 1)]), 0).unwrap();
        assert_eq!((have.len(), need.len()), (0, 1));
        let mut relay = Negentropy::new(Storage::default(), 0);
        assert!(relay.reconcile(&[]).is_err());
        assert!(relay.reconcile(&[0x61, 0x01, 0x00, 0x02, 0x05]).is_err(), "liste annoncée plus longue que le message");
        assert_eq!(relay.reconcile(&[0x62]).unwrap().next, Some(vec![0x61]), "version inconnue : on répond avec la nôtre");
    }
}
