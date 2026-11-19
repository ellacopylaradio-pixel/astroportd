//! File **équitable** du service `ai-index` : quand plusieurs stations amies demandent du calcul en même temps, chacune a sa
//! petite file et les créneaux sont servis **à tour de rôle** entre stations (pas dans l'ordre d'arrivée global) : une station
//! très demandeuse ne peut pas affamer les autres.
//!
//! - au plus `slots` calculs à la fois ; [`PER_PEER`] demandes en attente par station, [`MAX_WAITING`] en tout ;
//! - au-delà, ou après [`MAX_WAIT`] d'attente : refus **avec un délai de retour** estimé (temps moyen d'un calcul × file) :
//!   le demandeur met la station de côté ce temps-là au lieu d'insister ;
//! - la charge est publique pour les amis ([`FairQueue::load`]) : un demandeur qui a plusieurs stations choisit la moins chargée.
//!
//! Les membres de la station qui prête ne passent pas par cette file : leur travail ne l'attend jamais.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use crate::IndexError;

/// Demandes en attente au plus par station.
pub const PER_PEER: usize = 2;
/// Demandes en attente au plus en tout.
pub const MAX_WAITING: usize = 16;
/// Attente maximale d'un créneau.
pub const MAX_WAIT: Duration = Duration::from_secs(120);

/// Charge annoncée aux amis.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Load {
    /// Calculs simultanés.
    pub slots: usize,
    /// En cours.
    pub running: usize,
    /// En attente.
    pub waiting: usize,
    /// Durée moyenne d'un calcul (ms).
    pub avg_ms: u64,
}

impl Load {
    /// Pression : (en cours + en attente) par créneau ; plus c'est bas, mieux c'est.
    pub fn pressure(&self) -> f64 {
        (self.running + self.waiting) as f64 / self.slots.max(1) as f64
    }
}

#[derive(Default)]
struct State {
    running: usize,
    queues: HashMap<String, VecDeque<(u64, oneshot::Sender<()>)>>,
    turn: VecDeque<String>,
    waiting: usize,
    next_id: u64,
}

/// La file.
pub struct FairQueue {
    slots: usize,
    state: Mutex<State>,
    avg_ms: AtomicU64,
}

/// Un créneau obtenu ; le rendre (le laisser tomber) sert la station suivante, à tour de rôle.
pub struct Ticket {
    queue: Arc<FairQueue>,
    started: Instant,
}

impl Drop for Ticket {
    fn drop(&mut self) {
        let ms = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let old = self.queue.avg_ms.load(Ordering::Relaxed);
        self.queue.avg_ms.store((old * 7 + ms) / 8, Ordering::Relaxed);
        self.queue.release();
    }
}

impl FairQueue {
    /// File de `slots` calculs simultanés.
    pub fn new(slots: usize) -> Arc<Self> {
        Arc::new(Self { slots: slots.max(1), state: Mutex::default(), avg_ms: AtomicU64::new(2000) })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Charge du moment.
    pub fn load(&self) -> Load {
        let s = self.lock();
        Load { slots: self.slots, running: s.running, waiting: s.waiting, avg_ms: self.avg_ms.load(Ordering::Relaxed) }
    }

    /// Délai de retour conseillé (secondes, 5 à 600) : le temps de vider la file.
    pub fn retry_after(&self) -> u32 {
        let l = self.load();
        let ms = l.avg_ms.saturating_mul(u64::try_from(l.running + l.waiting + 1).unwrap_or(1)) / u64::try_from(l.slots).unwrap_or(1);
        u32::try_from(ms.div_ceil(1000)).unwrap_or(600).clamp(5, 600)
    }

    /// Un créneau pour `peer`, tout de suite ou à son tour ; refus [`IndexError::Busy`] (avec le délai de retour) si sa file ou la
    /// file totale est pleine, ou si l'attente dépasse [`MAX_WAIT`].
    pub async fn enter(self: &Arc<Self>, peer: &str) -> Result<Ticket, IndexError> {
        let (id, rx) = {
            let mut s = self.lock();
            if s.running < self.slots && s.waiting == 0 {
                s.running += 1;
                return Ok(Ticket { queue: self.clone(), started: Instant::now() });
            }
            let mine = s.queues.get(peer).map_or(0, VecDeque::len);
            if mine >= PER_PEER || s.waiting >= MAX_WAITING {
                drop(s);
                return Err(IndexError::Busy(self.retry_after()));
            }
            let (tx, rx) = oneshot::channel();
            s.next_id += 1;
            let id = s.next_id;
            if mine == 0 {
                s.turn.push_back(peer.to_owned());
            }
            s.queues.entry(peer.to_owned()).or_default().push_back((id, tx));
            s.waiting += 1;
            (id, rx)
        };
        match tokio::time::timeout(MAX_WAIT, rx).await {
            Ok(Ok(())) => Ok(Ticket { queue: self.clone(), started: Instant::now() }),
            _ => {
                // Parti avant son tour : on se retire de la file (si on y est encore) pour ne pas compter pour rien.
                let mut s = self.lock();
                if let Some(q) = s.queues.get_mut(peer) {
                    if let Some(at) = q.iter().position(|(i, _)| *i == id) {
                        q.remove(at);
                        s.waiting -= 1;
                        if s.queues.get(peer).is_some_and(VecDeque::is_empty) {
                            s.queues.remove(peer);
                            s.turn.retain(|p| p != peer);
                        }
                    }
                }
                drop(s);
                Err(IndexError::Busy(self.retry_after()))
            }
        }
    }

    /// Rend un créneau : à la station suivante dans le tour, sinon il se libère.
    fn release(&self) {
        let mut s = self.lock();
        while let Some(peer) = s.turn.pop_front() {
            let Some(q) = s.queues.get_mut(&peer) else { continue };
            let Some((_, tx)) = q.pop_front() else { continue };
            let more = !q.is_empty();
            if more {
                s.turn.push_back(peer);
            } else {
                s.queues.remove(&peer);
            }
            s.waiting -= 1;
            if tx.send(()).is_ok() {
                return; // le créneau passe au suivant, `running` ne bouge pas
            }
        }
        s.running = s.running.saturating_sub(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn slots_are_served_in_turn_between_stations_not_first_come() {
        let q = FairQueue::new(1);
        let first = q.enter("A").await.unwrap();
        let order = Arc::new(Mutex::new(Vec::new()));
        let mut tasks = Vec::new();
        // A demande deux fois, puis B une fois : B passe entre les deux demandes de A.
        for (peer, delay) in [("A", 0), ("A", 10), ("B", 20)] {
            let (q, order) = (q.clone(), order.clone());
            tasks.push(tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(delay)).await;
                let t = q.enter(peer).await.unwrap();
                order.lock().unwrap().push(peer);
                tokio::time::sleep(Duration::from_millis(5)).await;
                drop(t);
            }));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(q.load().waiting, 3);
        drop(first);
        for t in tasks {
            t.await.unwrap();
        }
        assert_eq!(*order.lock().unwrap(), ["A", "B", "A"], "à tour de rôle");
        assert_eq!(q.load(), Load { slots: 1, running: 0, waiting: 0, avg_ms: q.load().avg_ms });
    }

    #[tokio::test]
    async fn a_greedy_station_is_told_when_to_come_back_and_others_still_get_in() {
        let q = FairQueue::new(1);
        let _busy = q.enter("A").await.unwrap();
        let waiters: Vec<_> = (0..PER_PEER).map(|_| { let q = q.clone(); tokio::spawn(async move { q.enter("A").await.map(drop) }) }).collect();
        tokio::time::sleep(Duration::from_millis(20)).await;
        let refused = q.enter("A").await;
        assert!(matches!(refused, Err(IndexError::Busy(s)) if (5..=600).contains(&s)), "refus avec un délai de retour");
        let b = { let q = q.clone(); tokio::spawn(async move { q.enter("B").await.map(drop) }) };
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(q.load().waiting, PER_PEER + 1, "B attend son tour malgré la file pleine de A");
        drop(_busy);
        for w in waiters {
            w.await.unwrap().unwrap();
        }
        b.await.unwrap().unwrap();
        assert_eq!(q.load().running, 0);
    }
}
