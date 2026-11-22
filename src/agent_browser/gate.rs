//! Porte CDP : l'unique entrée vers le navigateur prêté.
//!
//! WebSocket en boucle locale sur un port choisi par le système, chemin `/cdp/<64 hex>` comparé en temps constant, **un seul
//! client à la fois**, trames bornées. Chaque message passe par [`super::cdp::Policy`]. Abandonner la [`Gate`] coupe
//! l'agent sur-le-champ : la tâche d'écoute et toutes les connexions qu'elle porte sont arrêtées.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};

use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::task::{JoinHandle, JoinSet};
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::StatusCode;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message;

use super::mux::{AgentSend, Mux};
use super::cdp::{Policy, Stats, Verdict};

/// Taille maximale d'un message (une capture d'écran pleine page tient dedans).
pub const MAX_MESSAGE: usize = 16 << 20;

/// Porte ouverte ; se ferme à la destruction.
pub struct Gate {
    addr: SocketAddr,
    token: String,
    task: JoinHandle<()>,
}

impl Drop for Gate {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Gate {
    /// Adresse remise à l'agent : `ws://127.0.0.1:<port>/cdp/<jeton>`.
    #[must_use]
    pub fn endpoint(&self) -> String {
        format!("ws://{}/cdp/{}", self.addr, self.token)
    }

    /// Adresse d'écoute (boucle locale).
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }
}

/// Ouvre la porte vers `link`, avec les règles `policy` ; les comptes vont dans `stats`.
///
/// # Errors
/// Aléa du système ou écoute indisponibles.
pub async fn open(mux: Arc<Mux>, policy: Arc<Policy>, stats: Arc<Mutex<Stats>>) -> std::io::Result<Gate> {
    let mut raw = [0u8; 32];
    getrandom::fill(&mut raw).map_err(|e| std::io::Error::other(format!("aléa du système indisponible : {e}")))?;
    let token = hex::encode(raw);
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))).await?;
    let addr = listener.local_addr()?;
    let path = format!("/cdp/{token}");
    let task = tokio::spawn(async move {
        // Les connexions vivent dans ce JoinSet : arrêter la tâche d'écoute les arrête toutes.
        let mut conns = JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let Ok((tcp, _)) = accepted else { continue };
                    let (mux, policy, stats, path) = (mux.clone(), policy.clone(), stats.clone(), path.clone());
                    conns.spawn(async move { serve(tcp, mux, policy, stats, path).await });
                }
                Some(_) = conns.join_next(), if !conns.is_empty() => {}
            }
        }
    });
    Ok(Gate { addr, token, task })
}

async fn serve(
    tcp: tokio::net::TcpStream,
    mux: Arc<Mux>,
    policy: Arc<Policy>,
    stats: Arc<Mutex<Stats>>,
    path: String,
) {
    // La signature du rappel est imposée par tungstenite (réponse d'erreur HTTP complète).
    #[allow(clippy::result_large_err)]
    let check = move |req: &Request, resp: Response| -> Result<Response, ErrorResponse> {
        if astro_dav::auth::ct_eq(req.uri().path().as_bytes(), path.as_bytes()) {
            Ok(resp)
        } else {
            let mut no = ErrorResponse::new(None);
            *no.status_mut() = StatusCode::NOT_FOUND;
            Err(no)
        }
    };
    let config = WebSocketConfig::default().max_message_size(Some(MAX_MESSAGE)).max_frame_size(Some(MAX_MESSAGE));
    let Ok(ws) = tokio::time::timeout(std::time::Duration::from_secs(10), tokio_tungstenite::accept_hdr_async_with_config(tcp, check, Some(config))).await else { return };
    let Ok(mut ws) = ws else { return };
    // Un seul client, pris **après** le jeton (sinon un inconnu tiendrait la place) : le récepteur des messages du navigateur
    // reste à lui toute la connexion.
    let Some((mut from, _slot)) = mux.connect_agent() else {
        let _ = ws.close(None).await;
        return;
    };
    loop {
        tokio::select! {
            incoming = ws.next() => {
                let text = match incoming {
                    Some(Ok(Message::Text(t))) => t.to_string(),
                    Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_))) => continue,
                    _ => break, // fermeture, binaire, erreur
                };
                let verdict = policy.inbound(&text, &mut stats.lock().unwrap_or_else(PoisonError::into_inner));
                match verdict {
                    Verdict::Forward(m) => match mux.check_agent(&m) {
                        AgentSend::Send => if mux.send_agent(m).await.is_err() { break },
                        AgentSend::Reply(r) => if ws.send(Message::text(r)).await.is_err() { break },
                    },
                    Verdict::Deny(reply) => if ws.send(Message::text(reply)).await.is_err() { break },
                    Verdict::Drop => {}
                }
            }
            out = from.recv() => {
                let Some(m) = out else { break }; // navigateur arrêté
                if ws.send(Message::text(Policy::outbound(m))).await.is_err() { break }
            }
        }
    }
    let _ = ws.close(None).await;
}
