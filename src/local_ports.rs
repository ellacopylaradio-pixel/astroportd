//! Ports TCP qui écoutent sur cette machine, pour que le Capitaine choisisse quoi partager sans connaître ses ports par cœur.
//!
//! Lecture seule de `/proc/net/tcp` et `/proc/net/tcp6` (sockets à l'état LISTEN), puis, quand c'est lisible, du programme
//! qui tient chaque socket (`/proc/<pid>/fd` → `socket:[inode]`, nom dans `/proc/<pid>/comm`). Rien n'est ouvert, rien
//! n'est sondé : c'est un inventaire. Hors Linux (pas de `/proc`), la liste est vide et l'écran garde la saisie à la main.
//!
//! Sont écartés : les ports tenus par la station elle-même (son interface, son maillage, et surtout les accès ouverts chez
//! un ami — les repartager ferait passer le service d'un ami à nos propres amis, tunnel → tunnel), et les adresses qui ne
//! sont ni cette machine ni le réseau local privé (la politique des tunnels les refuserait, [`crate::tunnel_policy::check_target`]).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::Path;

/// Au plus tant de ports rendus (une machine de développement peut en avoir des centaines).
pub const MAX_PORTS: usize = 64;
/// Au plus tant de processus et de descripteurs examinés pour nommer les programmes (borne du coût d'un affichage).
const MAX_PIDS: usize = 4096;
const MAX_FDS_PER_PID: usize = 4096;

/// Un port qui écoute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listener {
    /// Port TCP.
    pub port: u16,
    /// Adresse à donner à la politique des tunnels pour joindre ce port (`127.0.0.1:p`, `[::1]:p` ou une IP privée).
    pub target: String,
    /// Le port n'écoute que sur cette machine (pas sur le réseau local).
    pub loopback_only: bool,
    /// Programme qui tient la socket, quand on a le droit de le lire.
    pub program: Option<String>,
}

/// Lignes LISTEN d'un fichier `/proc/net/tcp{,6}` : (adresse d'écoute, port, inode de la socket).
pub fn parse_proc_net(text: &str, v6: bool) -> Vec<(IpAddr, u16, u64)> {
    text.lines()
        .skip(1)
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            if f.len() < 10 || f[3] != "0A" {
                return None;
            }
            let (ip, port) = f[1].split_once(':')?;
            let port = u16::from_str_radix(port, 16).ok()?;
            let ip = if v6 { IpAddr::V6(parse_v6(ip)?) } else { IpAddr::V4(parse_v4(ip)?) };
            let inode = f[9].parse().ok()?;
            Some((ip, port, inode))
        })
        .collect()
}

/// `/proc/net/tcp` écrit l'IPv4 comme un mot de 32 bits dans l'ordre de la machine (petit-boutiste sur x86 et ARM).
fn parse_v4(hex: &str) -> Option<Ipv4Addr> {
    (hex.len() == 8).then_some(())?;
    Some(Ipv4Addr::from(u32::from_str_radix(hex, 16).ok()?.to_le_bytes()))
}

/// L'IPv6 : quatre mots de 32 bits, chacun dans l'ordre de la machine.
fn parse_v6(hex: &str) -> Option<Ipv6Addr> {
    (hex.len() == 32).then_some(())?;
    let mut b = [0u8; 16];
    for w in 0..4 {
        let word = u32::from_str_radix(&hex[w * 8..w * 8 + 8], 16).ok()?;
        b[w * 4..w * 4 + 4].copy_from_slice(&word.to_le_bytes());
    }
    Some(Ipv6Addr::from(b))
}

/// Adresse joignable pour une écoute donnée ; `None` si elle n'est ni locale ni privée.
fn target_of(ip: IpAddr, port: u16) -> Option<(String, bool)> {
    let v4 = match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
        _ => ip,
    };
    match v4 {
        // Écoute sur toutes les interfaces : on passe par la boucle locale, toujours présente.
        IpAddr::V4(a) if a.is_unspecified() => Some((format!("127.0.0.1:{port}"), false)),
        IpAddr::V6(a) if a.is_unspecified() => Some((format!("127.0.0.1:{port}"), false)),
        IpAddr::V4(a) if a.is_loopback() => Some((format!("127.0.0.1:{port}"), true)),
        IpAddr::V6(a) if a.is_loopback() => Some((format!("[::1]:{port}"), true)),
        IpAddr::V4(a) if a.is_private() => Some((SocketAddr::new(v4, port).to_string(), false)),
        IpAddr::V6(a) if (a.segments()[0] & 0xfe00) == 0xfc00 => Some((SocketAddr::new(v4, port).to_string(), false)),
        _ => None,
    }
}

/// inode de socket → (pid, programme), pour les processus lisibles (la station comprise, pour être écartée ensuite).
fn owners(proc_root: &Path, wanted: &HashSet<u64>) -> HashMap<u64, (u32, String)> {
    let mut found = HashMap::new();
    let Ok(dir) = std::fs::read_dir(proc_root) else { return found };
    for entry in dir.flatten().take(MAX_PIDS) {
        let Some(pid) = entry.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else { continue };
        let Ok(fds) = std::fs::read_dir(entry.path().join("fd")) else { continue };
        let mut comm: Option<String> = None;
        for fd in fds.flatten().take(MAX_FDS_PER_PID) {
            let Ok(link) = std::fs::read_link(fd.path()) else { continue };
            let Some(inode) = link.to_str().and_then(|s| s.strip_prefix("socket:[")).and_then(|s| s.strip_suffix(']')).and_then(|s| s.parse::<u64>().ok()) else { continue };
            if !wanted.contains(&inode) {
                continue;
            }
            let name = comm.get_or_insert_with(|| std::fs::read_to_string(entry.path().join("comm")).map(|s| s.trim().to_owned()).unwrap_or_default()).clone();
            found.insert(inode, (pid, name));
        }
        if found.len() == wanted.len() {
            break;
        }
    }
    found
}

/// Les ports qui écoutent sous `proc_root` (`/proc` en vrai), sans ceux du processus `self_pid`, triés par port.
pub fn list(proc_root: &Path, self_pid: u32) -> Vec<Listener> {
    let mut socks = Vec::new();
    for (file, v6) in [("net/tcp", false), ("net/tcp6", true)] {
        if let Ok(text) = std::fs::read_to_string(proc_root.join(file)) {
            socks.extend(parse_proc_net(&text, v6));
        }
    }
    let wanted: HashSet<u64> = socks.iter().map(|s| s.2).filter(|i| *i != 0).collect();
    let who = owners(proc_root, &wanted);
    // Un port peut écouter en IPv4 et en IPv6 : on garde l'écoute la plus large (toutes interfaces avant boucle locale).
    let mut by_port: BTreeMap<u16, Listener> = BTreeMap::new();
    // Un port dont une socket (IPv4 ou IPv6) est tenue par la station est retiré en entier.
    let mine: HashSet<u16> = socks.iter().filter(|s| who.get(&s.2).is_some_and(|(pid, _)| *pid == self_pid)).map(|s| s.1).collect();
    for (ip, port, inode) in socks {
        if mine.contains(&port) {
            continue;
        }
        let owner = who.get(&inode);
        let Some((target, loopback_only)) = target_of(ip, port) else { continue };
        let program = owner.map(|(_, n)| n.clone()).filter(|n| !n.is_empty());
        let l = Listener { port, target, loopback_only, program };
        match by_port.get_mut(&port) {
            Some(cur) if cur.loopback_only && !l.loopback_only => *cur = Listener { program: l.program.or(cur.program.take()), ..l },
            Some(cur) => {
                if cur.program.is_none() {
                    cur.program = l.program;
                }
            }
            None => {
                by_port.insert(port, l);
            }
        }
    }
    by_port.into_values().take(MAX_PORTS).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TCP: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
   0: 0100007F:2CAA 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 111 1 0 100 0 0 10 0\n\
   1: 00000000:170C 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 222 1 0 100 0 0 10 0\n\
   2: 0100007F:1005 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 333 1 0 100 0 0 10 0\n\
   3: 0100007F:B5C2 0100007F:2CAA 01 00000000:00000000 00:00000000 00000000  1000        0 444 1 0 100 0 0 10 0\n\
   4: 0101A8C0:01BB 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 555 1 0 100 0 0 10 0\n\
   5: 08080808:0035 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 666 1 0 100 0 0 10 0\n";
    const TCP6: &str = "  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
   0: 00000000000000000000000001000000:0277 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 777 1 0 100 0 0 10 0\n\
   1: 00000000000000000000000000000000:170C 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 888 1 0 100 0 0 10 0\n";

    #[test]
    fn reads_only_listening_sockets_and_decodes_addresses() {
        let v4 = parse_proc_net(TCP, false);
        assert_eq!(v4.len(), 5, "la connexion établie (01) est ignorée");
        assert_eq!(v4[0], (IpAddr::V4(Ipv4Addr::LOCALHOST), 11434, 111));
        assert_eq!(v4[1], (IpAddr::V4(Ipv4Addr::UNSPECIFIED), 5900, 222));
        assert_eq!(v4[3].0, IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)));
        let v6 = parse_proc_net(TCP6, true);
        assert_eq!(v6[0], (IpAddr::V6(Ipv6Addr::LOCALHOST), 631, 777));
        assert!(parse_proc_net("entête\n 0: zz:zz 0 0A", false).is_empty(), "une ligne abîmée ne fait pas tomber la liste");
    }

    #[test]
    fn lists_shareable_ports_without_the_station_own_ones() {
        let root = std::env::temp_dir().join(format!("astroportd-local-ports-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let p = root.as_path();
        std::fs::create_dir_all(p.join("net")).unwrap();
        std::fs::write(p.join("net/tcp"), TCP).unwrap();
        std::fs::write(p.join("net/tcp6"), TCP6).unwrap();
        // Processus 42 : Ollama ; processus 7 : la station (elle tient le 4101 = 0x1005).
        for (pid, comm, inodes) in [(42u32, "ollama", vec![111u64]), (7, "astroportd", vec![333])] {
            let d = p.join(pid.to_string());
            std::fs::create_dir_all(d.join("fd")).unwrap();
            std::fs::write(d.join("comm"), format!("{comm}\n")).unwrap();
            for (i, ino) in inodes.iter().enumerate() {
                std::os::unix::fs::symlink(format!("socket:[{ino}]"), d.join("fd").join(i.to_string())).unwrap();
            }
        }
        let l = list(p, 7);
        let ports: Vec<u16> = l.iter().map(|x| x.port).collect();
        assert_eq!(ports, vec![443, 631, 5900, 11434], "ni le port de la station (4101), ni l'adresse publique (8.8.8.8:53)");
        let ollama = l.iter().find(|x| x.port == 11434).unwrap();
        assert_eq!((ollama.target.as_str(), ollama.loopback_only, ollama.program.as_deref()), ("127.0.0.1:11434", true, Some("ollama")));
        let vnc = l.iter().find(|x| x.port == 5900).unwrap();
        assert_eq!((vnc.target.as_str(), vnc.loopback_only, vnc.program.as_deref()), ("127.0.0.1:5900", false, None), "IPv4 et IPv6 réunis");
        assert_eq!(l.iter().find(|x| x.port == 631).unwrap().target, "[::1]:631");
        assert_eq!(l.iter().find(|x| x.port == 443).unwrap().target, "192.168.1.1:443");
        for x in &l {
            assert!(crate::tunnel_policy::check_target(&x.target).is_ok(), "{} doit être accepté par la politique", x.target);
        }
        assert!(list(&p.join("absent"), 7).is_empty(), "sans /proc : liste vide");
        let _ = std::fs::remove_dir_all(&root);
    }
}
