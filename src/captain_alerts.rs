//! Boîte d'alertes du Capitaine : ce qui demande une intervention reste visible tant qu'il ne l'a pas traité.
//!
//! Les sous-systèmes publient leurs alertes sur le bus d'activité (`kind = "alert"`, `Audience::Captain`). Ce bus ne vit
//! qu'en mémoire (anneau de 200 événements) et n'est servi qu'aux interfaces ouvertes : un Capitaine absent, un
//! redémarrage ou un flot d'activité faisaient disparaître l'alerte. Ici, un seul abonné :
//!
//! - **enregistre** chaque alerte dans `captain_alerts` (une alerte identique encore ouverte est regroupée : compteur
//!   et date de la dernière occurrence, pas une ligne de plus) ;
//! - **prévient** le Capitaine par message privé Nostr à la première occurrence (au plus [`DM_PER_HOUR`] par heure : un
//!   emballement ne l'inonde pas, la liste reste complète) ;
//! - **liste** pour l'interface les alertes ouvertes et les opérations restées `sending` dans `kv` (virement dont l'issue
//!   est inconnue après un arrêt brutal) ; le Capitaine marque une alerte « traitée ».

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use astro_core::{Activity, ActivityBus, Audience};
use astro_db::Db;
use rusqlite::OptionalExtension;
use serde_json::{Value, json};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::watch;

use crate::upload_worker::Notify;

/// Messages privés au plus par heure (les alertes suivantes sont seulement enregistrées).
pub const DM_PER_HOUR: usize = 10;

/// Alertes ouvertes renvoyées au plus (les plus récentes).
const LIST_MAX: i64 = 500;

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

/// Boîte d'alertes persistante.
pub struct CaptainAlerts {
    db: Arc<Db>,
    notify: Option<Notify>,
    /// Dates des derniers messages privés envoyés (fenêtre glissante d'une heure).
    sent: std::sync::Mutex<std::collections::VecDeque<i64>>,
}

impl CaptainAlerts {
    /// `notify` : message privé à un compte (le bus BRO de la station) ; `None` = enregistrer seulement.
    pub fn new(db: Arc<Db>, notify: Option<Notify>) -> Self {
        Self { db, notify, sent: std::sync::Mutex::new(std::collections::VecDeque::new()) }
    }

    /// Écoute le bus jusqu'à l'arrêt. À lancer tôt : les alertes publiées avant l'abonnement ne sont reprises que si
    /// elles sont encore dans l'anneau.
    pub async fn run(self: Arc<Self>, activity: ActivityBus, mut stop: watch::Receiver<bool>) {
        let (backlog, mut rx) = activity.subscribe(0);
        let mut last = 0;
        for a in backlog {
            last = a.id;
            self.take(&a).await;
        }
        loop {
            tokio::select! {
                r = rx.recv() => match r {
                    Ok(a) => {
                        last = a.id;
                        self.take(&a).await;
                    }
                    // Trop lent : on relit dans l'anneau ce qui a été sauté plutôt que de perdre des alertes.
                    Err(RecvError::Lagged(_)) => {
                        let (missed, again) = activity.subscribe(last);
                        rx = again;
                        for a in missed {
                            last = a.id;
                            self.take(&a).await;
                        }
                    }
                    Err(RecvError::Closed) => return,
                },
                changed = stop.changed() => if changed.is_err() || *stop.borrow() { return },
            }
        }
    }

    async fn take(&self, a: &Activity) {
        if a.kind != "alert" || a.audience != Audience::Captain {
            return;
        }
        match self.record(&a.source, &a.message, a.data.as_ref(), i64::try_from(a.at).unwrap_or_else(|_| now())).await {
            Ok(true) => self.tell(&a.source, &a.message).await,
            Ok(false) => {}
            Err(e) => eprintln!("alertes : non enregistrée ({}) : {e}", a.source),
        }
    }

    /// Enregistre une alerte ; `true` si elle est nouvelle (pas d'alerte identique encore ouverte).
    pub async fn record(&self, source: &str, message: &str, data: Option<&Value>, at: i64) -> Result<bool, String> {
        let (source, message, data) = (source.to_owned(), message.to_owned(), data.map(Value::to_string));
        self.db
            .writer()
            .transaction(move |tx| {
                let open: Option<i64> = tx.query_row("SELECT id FROM captain_alerts WHERE source = ?1 AND message = ?2 AND done_at IS NULL", rusqlite::params![source, message], |r| r.get(0)).optional()?;
                if let Some(id) = open {
                    tx.execute("UPDATE captain_alerts SET count = count + 1, last_at = MAX(last_at, ?2), data = COALESCE(?3, data) WHERE id = ?1", rusqlite::params![id, at, data])?;
                    return Ok(false);
                }
                tx.execute("INSERT INTO captain_alerts(source, message, data, first_at, last_at, count) VALUES (?1, ?2, ?3, ?4, ?4, 1)", rusqlite::params![source, message, data, at])?;
                Ok(true)
            })
            .await
            .map_err(|e| e.to_string())
    }

    /// Message privé au(x) Capitaine(s) de la station, dans la limite horaire.
    async fn tell(&self, source: &str, message: &str) {
        let Some(notify) = &self.notify else { return };
        {
            let mut sent = self.sent.lock().unwrap_or_else(|e| e.into_inner());
            let t = now();
            while sent.front().is_some_and(|&s| s <= t - 3600) {
                sent.pop_front();
            }
            if sent.len() >= DM_PER_HOUR {
                return;
            }
            sent.push_back(t);
        }
        let captains: Vec<String> = self
            .db
            .readers()
            .read(|c| {
                let mut s = c.prepare("SELECT hex FROM identities WHERE role = 'captain' AND hosted = 1")?;
                let rows = s.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .await
            .unwrap_or_default();
        let text = format!("⚠️ Alerte station ({source}) : {message}\nÀ traiter dans l'interface : Station → Alertes.");
        for hex in captains {
            notify(hex, text.clone()).await;
        }
    }

    /// Alertes ouvertes (les plus récentes d'abord) et opérations restées `sending`.
    pub async fn open(&self) -> Result<Value, String> {
        self.db
            .readers()
            .read(|c| {
                let mut s = c.prepare("SELECT id, source, message, data, first_at, last_at, count FROM captain_alerts WHERE done_at IS NULL ORDER BY last_at DESC, id DESC LIMIT ?1")?;
                let alerts = s
                    .query_map([LIST_MAX], |r| {
                        let data: Option<String> = r.get(3)?;
                        Ok(json!({
                            "id": r.get::<_, i64>(0)?, "source": r.get::<_, String>(1)?, "message": r.get::<_, String>(2)?,
                            "data": data.and_then(|d| serde_json::from_str::<Value>(&d).ok()),
                            "first_at": r.get::<_, i64>(4)?, "last_at": r.get::<_, i64>(5)?, "count": r.get::<_, i64>(6)?,
                        }))
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                let mut s = c.prepare("SELECT scope, key, updated_at FROM kv WHERE value = 'sending' ORDER BY updated_at")?;
                let pending = s
                    .query_map([], |r| Ok(json!({ "scope": r.get::<_, String>(0)?, "key": r.get::<_, String>(1)?, "since": r.get::<_, i64>(2)? })))?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(json!({ "alerts": alerts, "pending": pending }))
            })
            .await
            .map_err(|e| e.to_string())
    }

    /// Marque une alerte traitée ; `false` si elle n'existe pas ou l'était déjà.
    pub async fn done(&self, id: i64, by: &str) -> Result<bool, String> {
        let out = self
            .db
            .writer()
            .execute("UPDATE captain_alerts SET done_at = ?2, done_by = ?3 WHERE id = ?1 AND done_at IS NULL", vec![id.into(), now().into(), by.to_owned().into()])
            .await
            .map_err(|e| e.to_string())?;
        Ok(out.changes == 1)
    }
}

impl CaptainAlerts {
    /// Efface une alerte (ouverte ou traitée) ; `false` si elle n'existe pas.
    pub async fn delete(&self, id: i64) -> Result<bool, String> {
        let out = self.db.writer().execute("DELETE FROM captain_alerts WHERE id = ?1", vec![id.into()]).await.map_err(|e| e.to_string())?;
        Ok(out.changes == 1)
    }

    /// Efface toutes les alertes ; renvoie le nombre effacé. Les virements en suspens (`kv`) ne sont pas touchés : ils
    /// restent listés tant que leur issue n'est pas connue.
    pub async fn clear(&self) -> Result<usize, String> {
        let out = self.db.writer().execute("DELETE FROM captain_alerts", vec![]).await.map_err(|e| e.to_string())?;
        Ok(out.changes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn db(tag: &str) -> Arc<Db> {
        let dir = std::env::temp_dir().join(format!("astroportd-alerts-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Arc::new(Db::open(&dir.join("a.db"), 2).unwrap())
    }

    fn counter() -> (Arc<AtomicUsize>, Notify) {
        let n = Arc::new(AtomicUsize::new(0));
        let c = n.clone();
        (n, Arc::new(move |_, _| {
            c.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {})
        }))
    }

    #[tokio::test]
    async fn an_alert_survives_and_repeats_are_grouped_until_handled() {
        let boxed = CaptainAlerts::new(db("group"), None);
        assert!(boxed.record("hosting", "issue inconnue", None, 10).await.unwrap());
        assert!(!boxed.record("hosting", "issue inconnue", None, 20).await.unwrap(), "même alerte ouverte : regroupée");
        let v = boxed.open().await.unwrap();
        let a = &v["alerts"][0];
        assert_eq!((a["count"].as_i64(), a["first_at"].as_i64(), a["last_at"].as_i64()), (Some(2), Some(10), Some(20)));
        let id = a["id"].as_i64().unwrap();
        assert!(boxed.done(id, "cap").await.unwrap());
        assert!(!boxed.done(id, "cap").await.unwrap(), "déjà traitée");
        assert!(boxed.open().await.unwrap()["alerts"].as_array().unwrap().is_empty());
        assert!(boxed.record("hosting", "issue inconnue", None, 30).await.unwrap(), "elle revient après traitement : nouvelle");
        let id = boxed.open().await.unwrap()["alerts"][0]["id"].as_i64().unwrap();
        assert!(boxed.delete(id).await.unwrap());
        assert!(!boxed.delete(id).await.unwrap(), "déjà effacée");
        boxed.record("a", "1", None, 1).await.unwrap();
        boxed.record("b", "2", None, 2).await.unwrap();
        assert_eq!(boxed.clear().await.unwrap(), 3, "les deux ouvertes et celle déjà traitée");
    }

    #[tokio::test]
    async fn pending_sending_markers_are_listed() {
        let db = db("pending");
        db.writer().execute("INSERT INTO kv(scope, key, value, updated_at) VALUES ('hosting_fee', 'abc:1', 'sending', 5), ('hosting_fee', 'abc:2', 'paid', 6)", vec![]).await.unwrap();
        let v = CaptainAlerts::new(db, None).open().await.unwrap();
        assert_eq!(v["pending"], json!([{ "scope": "hosting_fee", "key": "abc:1", "since": 5 }]));
    }

    #[tokio::test]
    async fn only_captain_alerts_are_kept_and_the_captain_is_messaged_once() {
        let db = db("bus");
        db.writer().execute("INSERT INTO identities(hex, role, hosted) VALUES (?1, 'captain', 1)", vec!["c".repeat(64).into()]).await.unwrap();
        let (sent, notify) = counter();
        let boxed = Arc::new(CaptainAlerts::new(db, Some(notify)));
        let bus = ActivityBus::new();
        let (stop_tx, stop) = watch::channel(false);
        let task = tokio::spawn(boxed.clone().run(bus.clone(), stop));
        tokio::task::yield_now().await;
        bus.publish_to(Audience::Captain, "alert", "hosting", "virement inconnu", None);
        bus.publish_to(Audience::Captain, "alert", "hosting", "virement inconnu", None);
        bus.publish("alert", "public", "pas pour le Capitaine", None);
        bus.publish_to(Audience::Captain, "system", "ui", "pas une alerte", None);
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        stop_tx.send(true).unwrap();
        task.await.unwrap();
        let v = boxed.open().await.unwrap();
        assert_eq!(v["alerts"].as_array().unwrap().len(), 1);
        assert_eq!(v["alerts"][0]["count"], 2);
        assert_eq!(sent.load(Ordering::SeqCst), 1, "un seul message pour une alerte répétée");
    }
}
