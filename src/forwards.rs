//! Accès locaux aux services d'un ami, ouverts à chaud depuis l'écran d'assistance : l'ami aidant clique « Se connecter »
//! au lieu de redémarrer avec `--p2p-forward`.
//!
//! L'écoute est **toujours** sur `127.0.0.1` et se ferme seule à l'échéance de l'aide. Elle ne donne aucun droit : c'est la
//! politique de tunnels de la station aidée qui accepte ou refuse chaque flux.
//!
//! **Port stable** ([`Forwards::open_stable`]) : le port habituel du client (Ollama 11434, ComfyUI 8188) s'il est libre, sinon
//! un port déterministe pour le couple (service, station amie) dans la plage [`STABLE_PORTS`] — l'esprit de l'`ALT_PORT` de
//! DRAGON (21220 + décalage du NODE) —, avec quelques essais déterministes s'il est occupé. Le dernier port obtenu est redonné
//! à la réouverture : le client local (ssh, Open WebUI…) n'est pas à reconfigurer.
//!
//! **Anti-cascade** : chaque écoute ouverte ici est inscrite dans [`LocalBinds`], que la politique des tunnels consulte pour ne
//! jamais republier aux amis un service dont la cible est en réalité un tunnel vers un autre ami (tunnel → tunnel).

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::ops::RangeInclusive;
use std::sync::{Arc, Mutex, PoisonError};

use sha2::{Digest, Sha256};

use astro_p2p::tunnel::{Forward, forward};
use astro_p2p::{P2pHandle, PeerId};

/// Plage des ports stables des accès locaux (1000 ports, sous la plage éphémère de Linux 32768–60999 : le système ne les
/// donne jamais de lui-même à une connexion sortante).
pub const STABLE_PORTS: RangeInclusive<u16> = 24000..=24999;
/// Essais déterministes dans [`STABLE_PORTS`] quand le port du couple est occupé.
const STABLE_TRIES: u32 = 8;

/// Port stable n° `k` du couple (`service`, `peer`) dans [`STABLE_PORTS`].
pub fn stable_port(service: &str, peer: &str, k: u32) -> u16 {
    let d = Sha256::new().chain_update(service.as_bytes()).chain_update([0]).chain_update(peer.as_bytes()).chain_update(k.to_be_bytes()).finalize();
    let span = u32::from(STABLE_PORTS.end() - STABLE_PORTS.start()) + 1;
    let off = u32::from_be_bytes([d[0], d[1], d[2], d[3]]) % span;
    STABLE_PORTS.start().saturating_add(u16::try_from(off).unwrap_or(0))
}

/// Écoutes locales de cette station qui mènent chez un ami (accès à chaud, `--p2p-forward`). La politique des tunnels les
/// consulte : un service dont la cible est l'une d'elles n'est ni annoncé ni servi (pas de tunnel → tunnel).
#[derive(Default)]
pub struct LocalBinds {
    held: Mutex<BTreeMap<SocketAddr, usize>>,
}

/// Inscription d'une écoute dans [`LocalBinds`], retirée quand la garde est abandonnée (l'écoute fermée).
pub struct BindGuard {
    binds: Arc<LocalBinds>,
    addr: SocketAddr,
}

impl Drop for BindGuard {
    fn drop(&mut self) {
        let mut held = self.binds.held.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(n) = held.get_mut(&self.addr) {
            *n -= 1;
            if *n == 0 {
                held.remove(&self.addr);
            }
        }
    }
}

impl LocalBinds {
    /// Inscrit l'écoute `addr` tant que la garde rendue vit.
    pub fn hold(self: &Arc<Self>, addr: SocketAddr) -> BindGuard {
        *self.held.lock().unwrap_or_else(PoisonError::into_inner).entry(addr).or_insert(0) += 1;
        BindGuard { binds: self.clone(), addr }
    }

    /// `target` (adresse d'un service de la station) tombe-t-il sur une écoute qui mène chez un ami ? Même port, et même
    /// adresse, ou écoute sur toutes les interfaces, ou deux adresses de boucle locale.
    pub fn holds(&self, target: SocketAddr) -> bool {
        self.held.lock().unwrap_or_else(PoisonError::into_inner).keys().any(|h| {
            h.port() == target.port() && (h.ip() == target.ip() || h.ip().is_unspecified() || target.ip().is_unspecified() || (h.ip().is_loopback() && target.ip().is_loopback()))
        })
    }
}

/// Début du message d'une écoute impossible (port déjà pris).
const BIND_ERR: &str = "écoute impossible sur";

struct Open {
    fwd: Forward,
    until: i64,
    _bind: BindGuard,
}

/// Accès locaux ouverts, par identifiant de demande.
pub struct Forwards {
    handle: Option<P2pHandle>,
    open: Arc<Mutex<HashMap<String, Open>>>,
    binds: Arc<LocalBinds>,
    /// Dernier port obtenu par identifiant de demande, redonné à la réouverture.
    last: Mutex<HashMap<String, u16>>,
}

impl Forwards {
    /// `handle` : le maillage P2P (`None` : rien ne peut être ouvert).
    pub fn new(handle: Option<P2pHandle>) -> Self {
        Self { handle, open: Arc::new(Mutex::new(HashMap::new())), binds: Arc::default(), last: Mutex::default() }
    }

    /// Partage le registre des écoutes avec la politique des tunnels (anti-cascade).
    #[must_use]
    pub fn with_binds(mut self, binds: Arc<LocalBinds>) -> Self {
        self.binds = binds;
        self
    }

    /// Le registre des écoutes ouvertes ici.
    pub fn binds(&self) -> Arc<LocalBinds> {
        self.binds.clone()
    }

    /// Comme [`Forwards::open`], sur un **port stable** : `habitual` (port habituel du client) s'il est libre, sinon le
    /// dernier port donné à cette demande, sinon les ports déterministes du couple (`service`, `peer`) dans
    /// [`STABLE_PORTS`]. En dernier recours, un port choisi par le système.
    pub async fn open_stable(&self, id: &str, peer: &str, service: &str, until: i64, now: i64, habitual: Option<u16>) -> Result<SocketAddr, String> {
        let last = self.last.lock().unwrap_or_else(PoisonError::into_inner).get(id).copied();
        let mut ports: Vec<u16> = habitual.into_iter().chain(last).chain((0..STABLE_TRIES).map(|k| stable_port(service, peer, k))).collect();
        let mut seen = std::collections::BTreeSet::new();
        ports.retain(|p| *p != 0 && seen.insert(*p));
        let mut err = String::new();
        for p in ports.into_iter().chain([0]) {
            match self.open_on(id, peer, service, until, now, p).await {
                Ok(a) => return Ok(a),
                // Seule une écoute impossible (port pris) fait essayer le suivant ; le reste est une vraie erreur.
                Err(e) if e.starts_with(BIND_ERR) => err = e,
                Err(e) => return Err(e),
            }
        }
        Err(err)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Open>> {
        self.open.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Ouvre (ou retrouve) l'accès local à `service` de la station `peer` pour la demande `id`, jusqu'à `until` (secondes Unix).
    pub async fn open(&self, id: &str, peer: &str, service: &str, until: i64, now: i64) -> Result<SocketAddr, String> {
        self.open_on(id, peer, service, until, now, 0).await
    }

    /// Comme [`Forwards::open`], sur le port `port` de `127.0.0.1` (0 : choisi par le système). Un port fixe donne à un
    /// client local (Ollama sur 11434, ComfyUI sur 8188) l'adresse qu'il attend ; l'écoute reste sur la boucle locale.
    pub async fn open_on(&self, id: &str, peer: &str, service: &str, until: i64, now: i64, port: u16) -> Result<SocketAddr, String> {
        self.sweep(now);
        // Tout nom de service bien formé : c'est la politique de la station aidée qui décide (SSH, ComfyUI, Ollama, ou un
        // service que son Capitaine partage à chaud, comme un bureau à distance).
        if service.is_empty() || service.len() > 32 || !service.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-') {
            return Err(format!("service « {service} » non pris en charge"));
        }
        let peer: PeerId = peer.parse().map_err(|_| "identifiant de station invalide".to_owned())?;
        if let Some(o) = self.lock().get(id) {
            return Ok(o.fwd.local_addr);
        }
        let handle = self.handle.as_ref().ok_or("maillage P2P inactif sur cette station")?;
        let bind = SocketAddr::from(([127, 0, 0, 1], port));
        // Écouter d'abord : un port pris est dit tel quel (et `open_stable` essaie le suivant).
        if port != 0 {
            drop(std::net::TcpListener::bind(bind).map_err(|e| format!("{BIND_ERR} {bind} : {e}"))?);
        }
        let fwd = forward(handle, bind, peer, service).await.map_err(|e| e.to_string())?;
        let addr = fwd.local_addr;
        let guard = self.binds.hold(addr);
        self.last.lock().unwrap_or_else(PoisonError::into_inner).insert(id.to_owned(), addr.port());
        self.lock().insert(id.to_owned(), Open { fwd, until, _bind: guard });
        // L'accès se referme à l'échéance, même si personne n'ouvre plus l'écran d'assistance : sans minuteur, le port local
        // resterait actif jusqu'au prochain `open` ou `list`. Si la demande a été rouverte entre-temps (autre échéance), on n'y touche pas.
        let (map, key) = (self.open.clone(), id.to_owned());
        let wait = std::time::Duration::from_secs(u64::try_from(until.saturating_sub(now)).unwrap_or(0));
        tokio::spawn(async move {
            tokio::time::sleep(wait).await;
            let mut g = map.lock().unwrap_or_else(|e| e.into_inner());
            if g.get(&key).is_some_and(|o| o.until == until) {
                g.remove(&key);
            }
        });
        Ok(addr)
    }

    /// Ferme l'accès de la demande `id` (sans effet s'il n'est pas ouvert).
    pub fn close(&self, id: &str) {
        self.lock().remove(id);
    }

    /// Accès ouverts : `(id, adresse locale, échéance)`.
    pub fn list(&self, now: i64) -> Vec<(String, SocketAddr, i64)> {
        self.sweep(now);
        self.lock().iter().map(|(id, o)| (id.clone(), o.fwd.local_addr, o.until)).collect()
    }

    /// Referme ce qui est échu.
    fn sweep(&self, now: i64) {
        self.lock().retain(|_, o| o.until > now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn refuses_malformed_services_bad_peers_and_missing_mesh() {
        let f = Forwards::new(None);
        for bad in ["../ssh", "Bureau", "", "a b", &"x".repeat(33)] {
            assert!(f.open("a", "12D3KooWx", bad, 100, 0).await.unwrap_err().contains("non pris en charge"), "{bad}");
        }
        // Un service partagé à chaud par l'ami (ex. son bureau) est un nom bien formé : c'est sa politique qui tranchera.
        assert!(f.open("a", "12D3KooWx", "bureau-vnc", 100, 0).await.unwrap_err().contains("invalide"), "seul le PeerID bloque ici");
        assert!(f.open("a", "pas-un-peer", "ssh", 100, 0).await.unwrap_err().contains("invalide"));
        let peer = astro_p2p::keypair_from_seed(&[1; 32]).unwrap().public().to_peer_id().to_string();
        assert!(f.open("a", &peer, "ssh", 100, 0).await.unwrap_err().contains("P2P inactif"));
        assert!(f.list(0).is_empty());
    }
}
