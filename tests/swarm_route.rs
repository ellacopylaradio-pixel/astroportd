//! Routage vers les services de la constellation, sans `12345.json` : l'annuaire des balises donne les stations et leur
//! Power-Score, chaque station dit ce qu'**elle nous ouvre**, et `tunnel connect` ouvre l'accès local choisi. Puis la prise
//! de contrôle (`control.sock`) qui porte les commandes `astroportd tunnel …`.
#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use astro_core::ActivityBus;
use astro_db::Db;
use astro_db::events::insert_event;
use astro_nostr_proto::{Event, UnsignedEvent, kind};
use astro_p2p::routing::Mode;
use astro_p2p::{Multiaddr, P2pConfig, P2pNode};
use astroportd::beacon::Stations;
use astroportd::forwards::Forwards;
use astroportd::swarm_route::SwarmRoute;
use astroportd::tunnel_policy::TunnelPolicy;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const UP: &str = "ConstellationDeRoutage";

async fn node(seed: u8) -> (P2pNode, Multiaddr) {
    let n = P2pNode::spawn(P2pConfig::local([seed; 32])).await.unwrap();
    let tcp = n.handle.listen_addrs().into_iter().find(|a| a.to_string().contains("/tcp/")).unwrap();
    let full: Multiaddr = format!("{tcp}/p2p/{}", n.handle.peer_id()).parse().unwrap();
    (n, full)
}

fn tmp(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("astroportd-route-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn store(db: &Db, ev: Event) {
    db.writer()
        .transaction(move |tx| {
            insert_event(tx, &ev)?;
            astro_db::apply_statements(tx, &astro_sync::Projector::plan(&ev).map_err(|e| astro_db::DbError::Corrupt(e.to_string()))?)
        })
        .await
        .unwrap();
}

/// Balise 30850 d'une station : PeerID, Power-Score, position.
async fn beacon(db: &Db, key: u8, peer: &str, name: &str, power: u32, pos: Option<(&str, &str)>) {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let mut tags = vec![
        vec!["d".into(), "economic-health".into()],
        vec!["station".into(), peer.to_owned()],
        vec!["station:name".into(), name.to_owned()],
        vec!["swarm_id".into(), astroportd::coop_wallets::uplanet_g1pub(UP).unwrap()],
        vec!["hw:power_score".into(), power.to_string()],
    ];
    if let Some((lat, lon)) = pos {
        tags.push(vec!["geo:lat".into(), lat.into()]);
        tags.push(vec!["geo:lon".into(), lon.into()]);
    }
    store(db, UnsignedEvent { created_at: now, kind: kind::ECONOMIC_HEALTH, tags, content: "{}".into() }.sign(&[key; 32]).unwrap()).await;
}

async fn echo() -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = l.accept().await {
            tokio::spawn(async move {
                let mut b = [0u8; 4];
                if s.read_exact(&mut b).await.is_ok() {
                    let _ = s.write_all(&b).await;
                }
            });
        }
    });
    addr
}

/// Une station qui offre `ollama` selon `mode`, et dont le service renvoie ce qu'on lui envoie.
async fn provider(node: &P2pNode, tag: &str, mode: &str) -> std::path::PathBuf {
    let dir = tmp(tag);
    let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
    let policy = TunnelPolicy::load(db, false, None, ActivityBus::new()).await;
    let fixed = vec!["ollama".to_owned()];
    policy.attach(&node.handle, &fixed);
    policy.set(&json!({ "service": "ollama", "mode": mode }), &fixed, 0).await.unwrap();
    let target = echo().await;
    // Le service local d'`ollama` : exposé derrière la politique, comme le fait `serve`.
    let opts = astro_p2p::tunnel::ExposeOpts { gate: Some(Arc::new(astroportd::tunnel_policy::PolicyGate::new(policy.clone()))), ..Default::default() };
    std::mem::forget(astro_p2p::tunnel::expose(&node.handle, "ollama", target, opts).unwrap());
    std::mem::forget(policy);
    dir
}

#[tokio::test(flavor = "multi_thread")]
async fn route_picks_the_strongest_station_that_opens_the_service_to_us_and_connect_opens_a_local_port() {
    let ((na, _), (nb, b_addr), (nc, c_addr), (nd, d_addr)) = (node(91).await, node(92).await, node(93).await, node(94).await);
    for addr in [b_addr, c_addr, d_addr] {
        na.handle.dial(addr).await.unwrap();
    }
    let (pa, pb, pc, pd) = (na.handle.peer_id().to_string(), nb.handle.peer_id().to_string(), nc.handle.peer_id().to_string(), nd.handle.peer_id().to_string());
    // B (20) et C (80) offrent ollama à tout le réseau ; D (99) l'a fermé : plus puissante, mais inutilisable.
    let dirs = [provider(&nb, "b", "swarm").await, provider(&nc, "c", "swarm").await, provider(&nd, "d", "off").await];
    let da = tmp("a");
    let dba = Arc::new(Db::open(&da.join("u.db"), 2).unwrap());
    beacon(&dba, 1, &pa, "moi", 10, Some(("45.76", "4.83"))).await; // Lyon
    beacon(&dba, 2, &pb, "b-lyon", 20, Some(("45.75", "4.85"))).await;
    beacon(&dba, 3, &pc, "c-marseille", 80, Some(("43.30", "5.37"))).await;
    beacon(&dba, 4, &pd, "d-ferme", 99, None).await;

    let stations = Arc::new(Stations::new(dba.clone(), UP.into(), Some(pa.clone())));
    let forwards = Arc::new(Forwards::new(Some(na.handle.clone())));
    let route = Arc::new(SwarmRoute::new(stations, Some(na.handle.clone()), forwards, None));

    // Puissance : C. D, plus fort, ne vous ouvre pas ollama : il n'est pas même candidat.
    let (peer, report) = route.route("ollama", Mode::Power).await.unwrap();
    assert_eq!((peer.as_str(), report["candidates"].as_u64()), (pc.as_str(), Some(2)), "{report}");
    // Proximité : B, à Lyon comme nous.
    let (peer, report) = route.route("ollama", Mode::Closest).await.unwrap();
    assert_eq!(peer, pb, "{report}");
    assert!(route.route("comfyui", Mode::Power).await.unwrap_err().contains("aucune station ne vous ouvre"), "service que personne n'offre");

    // La vue dit, station par station, ce qui nous est ouvert.
    let v = route.act(&json!({})).await.unwrap();
    let by_name = |n: &str| v["stations"].as_array().unwrap().iter().find(|s| s["name"] == n).unwrap().clone();
    assert_eq!(by_name("c-marseille")["services"][0]["name"], "ollama");
    assert_eq!(by_name("d-ferme")["services"].as_array().unwrap().len(), 0, "fermé à nous : rien n'est annoncé");

    // Connexion automatique sur un port libre : le flux traverse le maillage jusqu'au service de C.
    let r = route.act(&json!({ "connect": { "service": "ollama", "mode": "power", "hours": 1 } })).await.unwrap();
    assert_eq!(r["peer"], pc.as_str());
    let mut s = tokio::net::TcpStream::connect(r["addr"].as_str().unwrap()).await.unwrap();
    s.write_all(b"ping").await.unwrap();
    let mut back = [0u8; 4];
    tokio::time::timeout(Duration::from_secs(8), s.read_exact(&mut back)).await.unwrap().unwrap();
    assert_eq!(&back, b"ping");
    let v = route.act(&json!({})).await.unwrap();
    assert!(by_open(&v, "c-marseille").is_some_and(|a| a == r["addr"].as_str().unwrap()), "l'accès ouvert est visible dans la vue");

    // Mauvaises demandes : refusées sans rien ouvrir.
    for bad in [json!({ "connect": { "service": "ollama", "hours": 0 } }), json!({ "connect": { "service": "ollama", "hours": 9999 } }), json!({ "connect": { "service": "ollama", "port": 70000 } }), json!({ "connect": { "service": "" } }), json!({ "connect": { "service": "ollama", "peer": "pas-un-peer" } }), json!({ "format": "disque" })] {
        assert!(route.act(&bad).await.is_err(), "{bad}");
    }
    // Fermeture : par service, puis plus rien à fermer.
    assert_eq!(route.act(&json!({ "disconnect": { "service": "ollama" } })).await.unwrap()["closed"], 1);
    assert!(route.act(&json!({ "disconnect": { "service": "ollama" } })).await.is_err());

    // La section d'administration (écran Réseau) passe par la même porte : vue, ouverture chez une station précise, fermeture.
    let admin = route.clone().admin();
    let v = admin("route".into(), None).await.unwrap();
    assert!(by_open(&v, "b-lyon").is_none() && v["mesh"] == true, "{v}");
    let r = admin("route".into(), Some(json!({ "connect": { "service": "ollama", "peer": pb } }))).await.unwrap();
    assert_eq!(r["peer"], pb.as_str());
    let v = admin("route".into(), None).await.unwrap();
    assert_eq!(by_open(&v, "b-lyon").as_deref(), r["addr"].as_str(), "l'adresse locale apparaît dans la vue de l'écran");
    assert_eq!(admin("route".into(), Some(json!({ "disconnect": { "service": "ollama", "peer": pb } }))).await.unwrap()["closed"], 1);
    assert!(by_open(&admin("route".into(), None).await.unwrap(), "b-lyon").is_none());
    assert!(admin("route".into(), Some(json!({ "connect": { "service": "../ssh", "peer": pb } }))).await.unwrap_err().contains("non pris en charge"), "nom de service hostile refusé");

    for d in dirs.into_iter().chain([da]) {
        let _ = std::fs::remove_dir_all(d);
    }
}

fn by_open(v: &serde_json::Value, name: &str) -> Option<String> {
    v["stations"].as_array()?.iter().find(|s| s["name"] == name)?["services"][0]["local"]["addr"].as_str().map(str::to_owned)
}

#[tokio::test(flavor = "multi_thread")]
async fn the_control_socket_serves_only_declared_sections_to_its_owner_and_refuses_hostile_requests() {
    use astroportd::control;
    let dir = tmp("ctl");
    let path = dir.join("control.sock");
    let mut sections: BTreeMap<&'static str, astroportd::ui_backend::AdminFn> = BTreeMap::new();
    sections.insert("echo", Arc::new(|_s, body| Box::pin(async move { Ok(json!({ "got": body })) })));
    sections.insert("boom", Arc::new(|_s, _b| Box::pin(async { Err("refusé par la section".to_owned()) })));
    let _ctl = control::serve(path.clone(), sections.clone()).unwrap();

    assert_eq!(control::call(&path, "echo", Some(json!({ "x": 1 }))).await.unwrap(), json!({ "got": { "x": 1 } }));
    assert_eq!(control::call(&path, "boom", None).await.unwrap_err(), "refusé par la section");
    let err = control::call(&path, "console", None).await.unwrap_err();
    assert!(err.contains("inconnue") && err.contains("echo"), "section non déclarée : {err}");

    // Droits : socket réservé à son propriétaire.
    let mode = std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(&path).unwrap().permissions()) & 0o777;
    assert_eq!(mode, 0o600);

    // Demande hostile : trop longue, sans fin de ligne, JSON cassé.
    // (Trop longue : le démon ne lit que 64 Kio puis referme ; selon le noyau le client lit l'erreur ou une coupure.)
    let mut big = tokio::net::UnixStream::connect(&path).await.unwrap();
    let _ = big.write_all(&vec![b'x'; 70 * 1024]).await;
    let mut out = String::new();
    let _ = tokio::time::timeout(Duration::from_secs(8), big.read_to_string(&mut out)).await.unwrap();
    assert!(!out.contains("\"ok\""), "jamais acceptée : {out:?}");
    for raw in [b"{\"section\":\"echo\"".to_vec(), b"pas du json\n".to_vec()] {
        let mut s = tokio::net::UnixStream::connect(&path).await.unwrap();
        let _ = s.write_all(&raw).await;
        let _ = s.shutdown().await;
        let mut out = String::new();
        let _ = tokio::time::timeout(Duration::from_secs(8), s.read_to_string(&mut out)).await.unwrap();
        assert!(out.contains("\"error\""), "refusée : {out:?}");
    }
    // Le démon tient toujours après ces demandes.
    assert!(control::call(&path, "echo", None).await.is_ok());

    // Un fichier ordinaire au même endroit n'est jamais écrasé.
    drop(_ctl);
    tokio::time::sleep(Duration::from_millis(200)).await;
    std::fs::write(&path, b"precieux").unwrap();
    assert!(control::serve(path.clone(), sections).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"precieux");
    let _ = std::fs::remove_dir_all(dir);
}

/// Port libre de la boucle locale (pris puis rendu).
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// Envoie `ping` à `addr` : l'écho revient-il ?
async fn echoes(addr: &str) -> bool {
    let Ok(mut s) = tokio::net::TcpStream::connect(addr).await else { return false };
    if s.write_all(b"ping").await.is_err() {
        return false;
    }
    let mut back = [0u8; 4];
    matches!(tokio::time::timeout(Duration::from_secs(5), s.read_exact(&mut back)).await, Ok(Ok(_))) && &back == b"ping"
}

/// Anti-cascade (l'`_is_native_process` de DRAGON) : A n'a pas d'Ollama, son Capitaine ouvre celui de B sur le port même que
/// A expose. C ne doit ni voir d'`ollama` chez A, ni pouvoir y passer : A republierait l'Ollama de B (tunnel → tunnel).
#[tokio::test(flavor = "multi_thread")]
async fn a_station_never_republishes_a_friends_service_it_opened_locally() {
    use astroportd::forwards::{Forwards, LocalBinds};
    use astroportd::tunnel_policy::{PolicyGate, ask_services};
    let ((na, _), (nb, b_addr), (nc, _)) = (node(81).await, node(82).await, node(83).await);
    na.handle.dial(b_addr).await.unwrap();
    let a_addr: Multiaddr = format!("{}/p2p/{}", na.handle.listen_addrs().into_iter().find(|a| a.to_string().contains("/tcp/")).unwrap(), na.handle.peer_id()).parse().unwrap();
    nc.handle.dial(a_addr).await.unwrap();
    let (pa, pb) = (na.handle.peer_id().to_string(), nb.handle.peer_id().to_string());
    let db_b = provider(&nb, "casc-b", "swarm").await;

    // A : ollama attendu sur 127.0.0.1:P (rien n'y tourne), comfyui sur un port mort ; les deux ouverts à tout le réseau.
    let da = tmp("casc-a");
    let dba = Arc::new(Db::open(&da.join("u.db"), 2).unwrap());
    let policy = TunnelPolicy::load(dba, false, None, ActivityBus::new()).await;
    let binds = Arc::new(LocalBinds::default());
    policy.watch_binds(binds.clone());
    let (p, dead) = (free_port(), free_port());
    let fixed = vec!["ollama".to_owned(), "comfyui".to_owned()];
    for (name, port) in [("ollama", p), ("comfyui", dead)] {
        let target = format!("127.0.0.1:{port}");
        policy.note_target(name, &target);
        let opts = astro_p2p::tunnel::ExposeOpts { gate: Some(Arc::new(PolicyGate::new(policy.clone()))), ..Default::default() };
        std::mem::forget(astro_p2p::tunnel::expose(&na.handle, name, target, opts).unwrap());
    }
    policy.attach(&na.handle, &fixed);
    for name in ["ollama", "comfyui"] {
        policy.set(&json!({ "service": name, "mode": "swarm" }), &fixed, 0).await.unwrap();
    }
    assert!(policy.own_ports().contains(&p), "ports des services de la station connus");

    // Le Capitaine de A ouvre l'Ollama de B sur P : l'accès fonctionne pour lui.
    let forwards = Forwards::new(Some(na.handle.clone())).with_binds(binds.clone());
    let addr = forwards.open_on("cap:b/ollama", &pb, "ollama", i64::MAX, 0, p).await.unwrap();
    assert_eq!(addr.port(), p);
    assert!(echoes(&addr.to_string()).await, "l'accès local de A mène bien à l'Ollama de B");
    assert!(policy.relayed("ollama") && !policy.relayed("comfyui"));

    // C : A ne lui annonce ni ollama (cascade) ni comfyui (cible morte)…
    let seen = ask_services(&nc.handle, &pa).await.unwrap();
    assert!(seen.is_empty(), "rien d'utilisable chez A : {seen:?}");
    // … et un flux direct vers A/ollama est refusé (pas d'écho de B).
    let c_fwd = astro_p2p::tunnel::forward(&nc.handle, "127.0.0.1:0".parse().unwrap(), na.handle.peer_id(), "ollama").await.unwrap();
    assert!(!echoes(&c_fwd.local_addr.to_string()).await, "A ne sert pas l'Ollama de B à C");

    // A referme l'accès et lance son propre Ollama sur P : celui-là est natif, il est annoncé et servi.
    forwards.close("cap:b/ollama");
    assert!(!policy.relayed("ollama"));
    let mut native = None;
    for _ in 0..40 {
        if let Ok(l) = tokio::net::TcpListener::bind(("127.0.0.1", p)).await {
            native = Some(l);
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let l = native.unwrap();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = l.accept().await {
            tokio::spawn(async move {
                let mut b = [0u8; 4];
                if s.read_exact(&mut b).await.is_ok() {
                    let _ = s.write_all(&b).await;
                }
            });
        }
    });
    assert_eq!(ask_services(&nc.handle, &pa).await.unwrap(), vec!["ollama".to_owned()], "service natif vivant : annoncé ; comfyui toujours mort");
    assert!(echoes(&c_fwd.local_addr.to_string()).await, "service natif servi à C");

    for d in [db_b, da] {
        let _ = std::fs::remove_dir_all(d);
    }
}

/// Port de repli stable : même port à la réouverture, un autre pour un autre ami, repli déterministe si occupé ; le port
/// habituel d'abord, sauf si c'est celui d'un service de la station.
#[tokio::test(flavor = "multi_thread")]
async fn local_access_ports_are_stable_per_service_and_friend() {
    use astroportd::forwards::{STABLE_PORTS, stable_port};
    let (na, _) = node(71).await;
    let peer = |s: u8| astro_p2p::keypair_from_seed(&[s; 32]).unwrap().public().to_peer_id().to_string();
    let (b, c) = (peer(72), peer(73));
    let f = Forwards::new(Some(na.handle.clone()));

    // SSH (pas de port habituel) : port de la plage, redonné à la réouverture.
    let first = f.open_stable("cap:b/ssh", &b, "ssh", i64::MAX, 0, None).await.unwrap().port();
    assert!(STABLE_PORTS.contains(&first), "{first}");
    f.close("cap:b/ssh");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(f.open_stable("cap:b/ssh", &b, "ssh", i64::MAX, 0, None).await.unwrap().port(), first, "même port à la réouverture");
    // Autre ami : autre port (déterministe).
    let other = f.open_stable("cap:c/ssh", &c, "ssh", i64::MAX, 0, None).await.unwrap().port();
    assert!(STABLE_PORTS.contains(&other) && other != first, "{other} / {first}");
    assert_eq!(stable_port("ssh", &b, 0), first, "le port est celui du couple (service, ami), le même d'un démarrage à l'autre");

    // Occupé : le port suivant de la suite déterministe. Puis la mémoire redonne ce port de repli.
    let d = peer(74);
    let busy = std::net::TcpListener::bind(("127.0.0.1", stable_port("ssh", &d, 0))).unwrap();
    let got = f.open_stable("cap:d/ssh", &d, "ssh", i64::MAX, 0, None).await.unwrap().port();
    let expected = (1..8).map(|k| stable_port("ssh", &d, k)).find(|p| *p != stable_port("ssh", &d, 0)).unwrap();
    assert_eq!(got, expected, "repli déterministe");
    drop(busy);
    f.close("cap:d/ssh");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(f.open_stable("cap:d/ssh", &d, "ssh", i64::MAX, 0, None).await.unwrap().port(), got, "le dernier port obtenu est redonné");

    // Port habituel libre : pris ; occupé : repli dans la plage.
    let habitual = free_port();
    assert_eq!(f.open_stable("cap:b/ollama", &b, "ollama", i64::MAX, 0, Some(habitual)).await.unwrap().port(), habitual);
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let hp = taken.local_addr().unwrap().port();
    assert!(STABLE_PORTS.contains(&f.open_stable("cap:c/ollama", &c, "ollama", i64::MAX, 0, Some(hp)).await.unwrap().port()));

    // Par la section `route` : le port habituel d'Ollama est évité quand la station attend elle-même Ollama sur 11434.
    let da = tmp("stable");
    let db = Arc::new(Db::open(&da.join("u.db"), 2).unwrap());
    let policy = TunnelPolicy::load(db.clone(), false, None, ActivityBus::new()).await;
    policy.note_target("ollama", "localhost:11434");
    let stations = Arc::new(Stations::new(db, UP.into(), Some(na.handle.peer_id().to_string())));
    let route = SwarmRoute::new(stations, Some(na.handle.clone()), Arc::new(f), None).with_policy(policy);
    let e = peer(75);
    let r = route.act(&json!({ "connect": { "service": "ollama", "peer": e, "hours": 1 } })).await.unwrap();
    let port: u16 = r["addr"].as_str().unwrap().rsplit(':').next().unwrap().parse().unwrap();
    assert!(port != 11434 && STABLE_PORTS.contains(&port), "{r}");
    assert_eq!(port, stable_port("ollama", &e, 0));
    let _ = std::fs::remove_dir_all(da);
}
