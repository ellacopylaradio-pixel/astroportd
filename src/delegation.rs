//! « Lecture de vos documents » (Mon compte) : le membre choisit **lui-même** si une station amie lit ses documents quand la
//! sienne n'a pas de moteur, et laquelle. C'est la seule porte d'écriture de `index_delegation`.
//!
//! - **Vue** (rapide, sans réseau) : qui lit (`local`, `friend` ou personne), où en est la lecture, ce qui peut être confié.
//! - **Stations proposées** (lente : chaque station est interrogée par le maillage) : celles qui ouvrent **à cette station** leur
//!   service `ai-index` ([`SwarmRoute`] ne rend que ce que leur politique nous autorise).
//! - **Confier** : seulement à une station qui le propose **au moment de l'accord**, et seulement le texte des documents (la
//!   recherche par l'image se fait sur la station qui a le modèle, jamais à distance) ; **arrêter** : la ligne disparaît, plus rien ne part, ce qui a déjà été lu reste chez le membre.
//!
//! L'accord est un **état local** de la station : une base perdue l'oublie, et alors rien ne part (défaut sûr).
//!
//! **Le Capitaine décide au-dessus** (Ma station › Membres, section d'administration `index`) : lecture coupée pour toute la
//! station, réglage par défaut des MULTIPASS, réglage de chacun (`index_members`), et prêt de la puissance de la station à ses
//! Capitaines amis (`ai-index`). Un compte que le Capitaine a désactivé n'est jamais lu, ni ici ni chez un ami.

use std::sync::Arc;

use astro_db::Db;
use astro_index::Task;
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};

use crate::swarm_route::SwarmRoute;
use crate::tunnel_policy::{AI_INDEX, Mode, TunnelPolicy};

/// Réglages de la station (`kv`).
const KV: (&str, &str) = ("options", "search_index");

/// Tâches qu'un membre peut confier aujourd'hui (les suivantes arriveront avec leur moteur). Les photos ne se confient pas :
/// chercher par l'image demande le modèle sur la station même (voir `search_index`).
pub const READY: [&str; 1] = ["ocr"];
/// Tâches annoncées, pas encore confiables.
pub const SOON: [&str; 0] = [];
/// Stations amies au plus par membre.
pub const MAX_LENDERS: usize = 5;
/// Pause de l'analyse décidée par le membre (`kv` scope `search_pause`, clé = son compte ; lue par `allowed_sql!`).
const PAUSE_SQL: &str = "INSERT INTO kv(scope, key, value, updated_at) VALUES ('search_pause', ?1, '1', ?2) ON CONFLICT(scope, key) DO UPDATE SET updated_at = excluded.updated_at";
/// Fichiers comptés au plus dans « en attente ».
const MAX_WAITING: i64 = 10_000;

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

/// Le service.
pub struct Delegation {
    db: Arc<Db>,
    route: std::sync::OnceLock<Arc<SwarmRoute>>,
    local: Vec<Task>,
    /// Politique des tunnels et services offerts : pour prêter la station aux amis depuis la même carte.
    lend: Option<(Arc<TunnelPolicy>, Vec<String>)>,
}

impl Delegation {
    /// `local` : ce que la station lit elle-même ; `lend` : la politique des tunnels et les services offerts (sans P2P : `None`).
    pub fn new(db: Arc<Db>, local: Vec<Task>, lend: Option<(Arc<TunnelPolicy>, Vec<String>)>) -> Self {
        Self { db, route: std::sync::OnceLock::new(), local, lend }
    }

    /// Branche le routage des services du maillage (créé plus tard au démarrage) : sans lui, aucune station amie n'est proposée.
    pub fn set_route(&self, route: Arc<SwarmRoute>) {
        let _ = self.route.set(route);
    }

    fn reads_locally(&self) -> bool {
        self.local.contains(&Task::Ocr)
    }

    /// Vue du membre : `{ state, stations, progress, tasks }` (`stations` : celles qui lisent pour lui, plusieurs possibles). `state` : `local` (sa station lit), `friend` (un ami lit),
    /// `off` (personne : il peut confier), `unavailable` (ni moteur ni maillage).
    pub async fn view(&self, owner: &str) -> Result<Value, astro_db::DbError> {
        let o = owner.to_owned();
        let local = self.reads_locally();
        let sees = self.local.contains(&Task::Vision) && self.local.contains(&Task::VisionQuery);
        let (stations, done, by_friend, failed, waiting, photos, station_on, member_on, (paused, pack)) = self
            .db
            .readers()
            .read(move |c| {
                let stations: Vec<(String, String, String, i64, i64)> = c
                    .prepare("SELECT d.peer, d.label, d.tasks, d.accepted_at, (SELECT count(*) FROM search_index s WHERE s.owner_hex = d.owner_hex AND s.via = d.peer AND s.state = 'done') FROM index_delegation d WHERE d.owner_hex = ?1 ORDER BY d.accepted_at")?
                    .query_map([&o], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
                    .collect::<Result<_, _>>()?;
                let (done, by_friend, failed): (i64, i64, i64) = c.query_row(
                    "SELECT count(*) FILTER (WHERE state = 'done'), count(*) FILTER (WHERE state = 'done' AND via IS NOT NULL), count(*) FILTER (WHERE state = 'failed') FROM search_index WHERE owner_hex = ?1 AND task = 'ocr'",
                    [&o],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )?;
                // En attente : ce qui serait traité si quelqu'un le faisait (on compte comme si la station avait les moteurs).
                let count = |job: Option<crate::search_index::Job>| -> rusqlite::Result<usize> {
                    job.map_or(Ok(0), |j| Ok(crate::search_index::candidates(c, &j, true, now(), MAX_WAITING, Some(&o))?.len()))
                };
                let waiting = count(crate::search_index::Job::ocr())?;
                let seen: i64 = c.query_row("SELECT count(*) FROM search_index WHERE owner_hex = ?1 AND task = 'vision' AND state = 'done'", [&o], |r| r.get(0))?;
                let photos = (seen, if sees { count(crate::search_index::Job::photos())? } else { 0 });
                let (station_on, member_on): (bool, bool) = c.query_row(
                    "SELECT COALESCE((SELECT json_extract(value, '$.enabled') FROM kv WHERE scope = 'options' AND key = 'search_index'), 1) = 1, \
                     COALESCE((SELECT enabled FROM index_members WHERE owner_hex = ?1), (SELECT json_extract(value, '$.default') FROM kv WHERE scope = 'options' AND key = 'search_index'), 1) = 1",
                    [&o],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                let paused_pack: (bool, Option<i64>) = c.query_row(
                    "SELECT EXISTS (SELECT 1 FROM kv WHERE scope = 'search_pause' AND key = ?1), (SELECT SUM(size) FROM ucloud_entries WHERE owner_hex = ?1 AND type = 'file' AND path LIKE ?2)",
                    params![o, format!("{}/index%.astroidx", crate::search_pack::PACK_DIR)],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                Ok((stations, done, by_friend, failed, waiting, photos, station_on, member_on, paused_pack))
            })
            .await?;
        let state = match (stations.is_empty(), local) {
            _ if !station_on || !member_on => "blocked",
            (_, true) => "local",
            (false, false) => "friend",
            (true, false) if self.route.get().is_some() => "off",
            (true, false) => "unavailable",
        };
        let done_here: Vec<&str> = [(local, "ocr"), (sees, "vision")].iter().filter(|(on, _)| *on).map(|(_, n)| *n).collect();
        Ok(json!({
            "state": state,
            // Ce que la station fait elle-même ; la lecture du texte peut être confiée, pas la recherche par l'image.
            "local": done_here,
            "blocked": if !station_on { Some("station") } else if !member_on { Some("member") } else { None },
            // Le membre a mis l'analyse de ses fichiers en pause ; place prise par son paquet d'index (octets).
            "paused": paused,
            "pack": pack,
            "stations": stations.into_iter().map(|(peer, label, tasks, since, read)| json!({ "peer": peer, "label": label, "tasks": tasks.split(',').collect::<Vec<_>>(), "since": since, "read": read })).collect::<Vec<_>>(),
            "progress": { "read": done, "by_friend": by_friend, "failed": failed, "waiting": waiting, "more": waiting as i64 >= MAX_WAITING },
            // Photos (et vidéos si ffmpeg est là) analysées : seulement sur une station qui a le modèle.
            "photos": sees.then(|| json!({ "seen": photos.0, "waiting": photos.1, "more": photos.1 as i64 >= MAX_WAITING, "videos": crate::search_index::Job::photos().is_some_and(|j| j.video) })),
            "tasks": READY.iter().map(|t| json!({ "id": t, "ready": true })).chain(SOON.iter().map(|t| json!({ "id": t, "ready": false }))).collect::<Vec<_>>(),
        }))
    }

    /// Stations amies qui proposent la lecture à cette station : `{ stations: [{ peer, name, distance_km, power }], silent }`.
    /// `silent` : stations qui n'ont pas répondu (hors ligne).
    pub async fn offers(&self) -> Result<Value, String> {
        let route = self.route.get().ok_or("le maillage entre stations est coupé sur cette station")?;
        let view = route.view().await?;
        let (mut stations, mut silent) = (Vec::new(), 0);
        for s in view["stations"].as_array().into_iter().flatten() {
            match s["services"].as_array() {
                Some(list) if list.iter().any(|x| x["name"] == AI_INDEX) => {
                    stations.push(json!({ "peer": s["peer"], "name": s["name"], "distance_km": s["distance_km"], "power": s["power_score"] }));
                }
                Some(_) => {}
                None => silent += 1,
            }
        }
        Ok(json!({ "stations": stations, "silent": silent }))
    }

    /// Confie à `peers` (une ou plusieurs stations, qui se partagent alors la charge) les tâches `tasks` (parmi [`READY`]) ; chaque
    /// station doit le proposer maintenant. Les stations déjà choisies restent. Rend la vue.
    pub async fn accept(&self, owner: &str, peers: &[String], tasks: &[String]) -> Result<Value, String> {
        if self.reads_locally() {
            return Err("votre station lit déjà vos documents elle-même : rien à confier".into());
        }
        if self.view(owner).await.map_err(|e| e.to_string())?["state"] == "blocked" {
            return Err("votre Capitaine a désactivé la lecture des documents pour votre compte".into());
        }
        if tasks.is_empty() || tasks.iter().any(|t| !READY.contains(&t.as_str())) {
            return Err("seule la lecture du texte de vos documents peut être confiée".into());
        }
        if peers.is_empty() || peers.len() > MAX_LENDERS {
            return Err(format!("choisissez de 1 à {MAX_LENDERS} stations"));
        }
        let offers = self.offers().await?;
        let mut rows = Vec::new();
        for peer in peers {
            let station = offers["stations"].as_array().into_iter().flatten().find(|s| s["peer"] == peer.as_str()).ok_or("une station choisie ne propose plus la lecture : rechargez la liste")?;
            let label: String = station["name"].as_str().unwrap_or("station amie").chars().take(80).collect();
            rows.push(astro_db::Statement::new(
                "INSERT OR REPLACE INTO index_delegation(owner_hex, peer, label, tasks, accepted_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                vec![owner.to_owned().into(), peer.clone().into(), label.into(), tasks.join(",").into(), now().into()],
            ));
        }
        self.db.writer().batch(rows).await.map_err(|e| e.to_string())?;
        self.view(owner).await.map_err(|e| e.to_string())
    }

    /// Met en pause (`true`) ou reprend (`false`) l'analyse des fichiers du membre, ici comme chez les amis ; ce qui est
    /// déjà calculé reste cherchable. Rend la vue.
    pub async fn pause(&self, owner: &str, on: bool) -> Result<Value, String> {
        let o = owner.to_owned();
        self.db
            .writer()
            .transaction(move |tx| {
                if on {
                    tx.execute(PAUSE_SQL, params![o, now()])?;
                } else {
                    tx.execute("DELETE FROM kv WHERE scope = 'search_pause' AND key = ?1", [&o])?;
                }
                Ok(())
            })
            .await
            .map_err(|e| e.to_string())?;
        self.view(owner).await.map_err(|e| e.to_string())
    }

    /// Efface ce qui a été calculé pour le membre (texte lu, photos et images clés regardées, états) et met l'analyse en
    /// pause, sans quoi tout serait aussitôt recalculé. L'appelant retire ensuite le paquet de son uCloud, puis
    /// [`Self::forget_pack`]. La Médiathèque (fiches) n'est pas touchée.
    pub async fn erase(&self, owner: &str) -> Result<(), String> {
        let o = owner.to_owned();
        self.db
            .writer()
            .transaction(move |tx| {
                tx.execute(PAUSE_SQL, params![o, now()])?;
                tx.execute("DELETE FROM search_text WHERE rowid IN (SELECT rowid FROM search_text WHERE owner_hex = ?1 AND src IN ('ocr', 'vision', 'speech'))", [&o])?;
                tx.execute("DELETE FROM search_vision_vectors WHERE vis_id IN (SELECT vis_id FROM search_frames WHERE owner_hex = ?1)", [&o])?;
                tx.execute("DELETE FROM search_frames WHERE owner_hex = ?1", [&o])?;
                tx.execute("DELETE FROM search_index WHERE owner_hex = ?1", [&o])?;
                Ok(())
            })
            .await
            .map_err(|e| e.to_string())
    }

    /// Oublie le paquet d'index du membre, une fois retiré de son uCloud. Pas avant : tant que la station s'en souvient, un
    /// paquet encore là n'est pas relu (il ferait revenir ce qui vient d'être effacé).
    pub async fn forget_pack(&self, owner: &str) -> Result<(), String> {
        self.db.writer().execute("DELETE FROM kv WHERE scope = 'search_pack' AND key = ?1", vec![owner.to_owned().into()]).await.map(|_| ()).map_err(|e| e.to_string())
    }

    /// Retire une station (`peer`), ou toutes (`None`) : plus rien ne part vers elle(s). Ce qui a déjà été lu reste. Rend la vue.
    pub async fn revoke(&self, owner: &str, peer: Option<&str>) -> Result<Value, String> {
        let (o, p) = (owner.to_owned(), peer.map(str::to_owned));
        self.db.writer().transaction(move |tx| Ok(tx.execute("DELETE FROM index_delegation WHERE owner_hex = ?1 AND (?2 IS NULL OR peer = ?2)", params![o, p])?)).await.map_err(|e| e.to_string())?;
        self.view(owner).await.map_err(|e| e.to_string())
    }
}

fn mode_name(m: Mode) -> &'static str {
    match m {
        Mode::Off => "off",
        Mode::Listed => "listed",
        Mode::Friends => "friends",
        Mode::Swarm => "swarm",
    }
}

impl Delegation {
    async fn settings(&self) -> Result<(bool, bool), String> {
        let raw: Option<String> = self
            .db
            .readers()
            .read(|c| Ok(c.query_row("SELECT value FROM kv WHERE scope = ?1 AND key = ?2", [KV.0, KV.1], |r| r.get(0)).optional()?))
            .await
            .map_err(|e| e.to_string())?;
        let v: Value = raw.and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        Ok((v["enabled"].as_bool().unwrap_or(true), v["default"].as_bool().unwrap_or(true)))
    }

    async fn store(&self, enabled: bool, default: bool) -> Result<(), String> {
        let value = json!({ "enabled": enabled, "default": default }).to_string();
        self.db
            .writer()
            .execute("INSERT INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at", vec![KV.0.to_owned().into(), KV.1.to_owned().into(), value.into(), now().into()])
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Vue du Capitaine : `{ enabled, default, engine, lend, members: [{ hex, email, enabled, own, read, friend }] }`. `own` : le
    /// Capitaine a réglé ce compte (sinon le défaut s'applique) ; `friend` : la station amie à qui le membre confie la lecture.
    pub async fn admin_view(&self) -> Result<Value, String> {
        let (enabled, default) = self.settings().await?;
        let members: Vec<Value> = self
            .db
            .readers()
            .read(|c| {
                let mut stmt = c.prepare(
                    "SELECT i.hex, COALESCE(i.email, ''), m.enabled, (SELECT count(*) FROM search_index s WHERE s.owner_hex = i.hex AND s.task = 'ocr' AND s.state = 'done'), \
                     (SELECT group_concat(label, ', ') FROM index_delegation d WHERE d.owner_hex = i.hex) FROM identities i LEFT JOIN index_members m ON m.owner_hex = i.hex \
                     WHERE i.hosted = 1 AND i.role IN ('multipass', 'captain') ORDER BY lower(COALESCE(i.email, i.hex))",
                )?;
                let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<i64>>(2)?, r.get::<_, i64>(3)?, r.get::<_, Option<String>>(4)?)))?;
                Ok(rows.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|(hex, email, own, read, friend)| json!({ "hex": hex, "email": email, "enabled": own.map_or(default, |v| v == 1), "own": own.is_some(), "read": read, "friend": friend }))
            .collect();
        let lend = self.lend.as_ref().filter(|(_, exposed)| exposed.iter().any(|s| s == AI_INDEX)).map(|(p, _)| mode_name(p.rule(AI_INDEX).mode));
        Ok(json!({ "enabled": enabled, "default": default, "engine": self.local.iter().map(|t| t.name()).collect::<Vec<_>>(), "lend": lend, "members": members }))
    }

    /// Réglages du Capitaine : `{ enabled }`, `{ default }`, `{ member: { hex, enabled } }` (`enabled: null` = revenir au défaut)
    /// ou `{ lend: "off" | "friends" }`. Rend la vue.
    pub async fn admin_set(&self, b: &Value) -> Result<Value, String> {
        let (enabled, default) = self.settings().await?;
        if let Some(on) = b["enabled"].as_bool() {
            self.store(on, default).await?;
        } else if let Some(on) = b["default"].as_bool() {
            self.store(enabled, on).await?;
        } else if let Some(m) = b.get("member").filter(|m| m.is_object()) {
            let hex = m["hex"].as_str().filter(|h| astro_nostr_proto::is_hex64(h)).ok_or("compte attendu (hex)")?.to_owned();
            let w = self.db.writer();
            let sql = match m["enabled"].as_bool() {
                Some(on) => w.execute("INSERT OR REPLACE INTO index_members(owner_hex, enabled, updated_at) SELECT ?1, ?2, ?3 WHERE EXISTS (SELECT 1 FROM identities WHERE hex = ?1 AND hosted = 1)", vec![hex.into(), i64::from(on).into(), now().into()]).await,
                None => w.execute("DELETE FROM index_members WHERE owner_hex = ?1", vec![hex.into()]).await,
            };
            sql.map_err(|e| e.to_string())?;
        } else if let Some(mode) = b["lend"].as_str() {
            if !matches!(mode, "off" | "friends") {
                return Err("prêt : off ou friends".into());
            }
            let (policy, exposed) = self.lend.as_ref().filter(|(_, e)| e.iter().any(|s| s == AI_INDEX)).ok_or("cette station n'a pas de moteur à prêter (astroportd models add ppocrv6-small)")?;
            policy.set(&json!({ "service": AI_INDEX, "mode": mode }), exposed, now()).await?;
        } else {
            return Err("attendu : enabled, default, member ou lend".into());
        }
        self.admin_view().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn db(tag: &str) -> (std::path::PathBuf, Arc<Db>) {
        let dir = std::env::temp_dir().join(format!("astro-deleg-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
        db.writer().execute("INSERT INTO identities(hex, role, hosted) VALUES ('alice', 'multipass', 1)", vec![]).await.unwrap();
        db.writer().execute("INSERT INTO ucloud_entries(owner_hex, path, parent, type, cid, mime, size, mtime, sha_plain, data) VALUES ('alice', '/s.jpg', '/', 'file', 'c', 'image/jpeg', 1, 1, 's1', '{}')", vec![]).await.unwrap();
        (dir, db)
    }

    #[tokio::test]
    async fn the_view_says_who_reads_and_what_waits() {
        let (dir, db) = db("view").await;
        let off = Delegation::new(db.clone(), vec![], None);
        let v = off.view("alice").await.unwrap();
        assert_eq!((v["state"].as_str(), v["progress"]["waiting"].as_u64(), v["stations"].as_array().map(Vec::len)), (Some("unavailable"), Some(1), Some(0)));
        assert_eq!(v["tasks"], json!([{ "id": "ocr", "ready": true }]), "seul le texte se confie");
        assert!(v["photos"].is_null(), "sans le modèle, pas de recherche par l'image (rien n'attend)");
        let reads = Delegation::new(db.clone(), vec![Task::Ocr], None);
        let v = reads.view("alice").await.unwrap();
        assert_eq!((v["state"].as_str(), v["local"].clone(), v["photos"].is_null()), (Some("local"), json!(["ocr"]), true), "lit les documents ; les photos ne se confient pas");
        let local = Delegation::new(db.clone(), vec![Task::Ocr, Task::Vision, Task::VisionQuery], None);
        let v = local.view("alice").await.unwrap();
        assert_eq!((v["state"].as_str(), v["local"].clone(), v["photos"]["waiting"].as_u64()), (Some("local"), json!(["ocr", "vision"]), Some(1)), "une photo à regarder ici");
        assert!(off.accept("alice", &["p".to_owned()], &["vision".into()]).await.unwrap_err().contains("seule"), "les photos ne se confient pas");
        let p = |s: &str| vec![s.to_owned()];
        assert!(local.accept("alice", &p("p"), &["ocr".into()]).await.unwrap_err().contains("elle-même"), "rien à confier");
        db.writer().execute("INSERT INTO index_delegation(owner_hex, peer, label, tasks, accepted_at) VALUES ('alice', 'p', 'Station de Marie', 'ocr', 5), ('alice', 'q', 'Atelier', 'ocr', 6)", vec![]).await.unwrap();
        let v = off.view("alice").await.unwrap();
        assert_eq!((v["state"].as_str(), v["stations"][0]["label"].as_str(), v["stations"][0]["since"].as_i64(), v["stations"][1]["label"].as_str()), (Some("friend"), Some("Station de Marie"), Some(5), Some("Atelier")));
        assert!(v["stations"][0]["tasks"] == json!(["ocr"]));
        let v = off.revoke("alice", Some("p")).await.unwrap();
        assert_eq!(v["stations"].as_array().map(Vec::len), Some(1), "une station retirée, l'autre reste");
        let v = off.revoke("alice", None).await.unwrap();
        assert!(v["stations"].as_array().is_some_and(Vec::is_empty), "tout arrêté");
        assert!(off.accept("alice", &p("p"), &["musique".into()]).await.unwrap_err().contains("seule"), "tâche inconnue");
        assert!(off.accept("alice", &[], &["ocr".into()]).await.unwrap_err().contains("1 à"), "au moins une station");
        assert!(off.accept("alice", &p("p"), &["ocr".into()]).await.unwrap_err().contains("maillage"), "sans maillage : personne à qui confier");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn the_captain_turns_reading_off_for_a_member_and_she_is_told_why() {
        let (dir, db) = db("admin").await;
        let alice = "a".repeat(64);
        db.writer().execute("UPDATE identities SET hex = ?1, email = 'alice@example.org'", vec![alice.clone().into()]).await.unwrap();
        db.writer().execute("UPDATE ucloud_entries SET owner_hex = ?1", vec![alice.clone().into()]).await.unwrap();
        let d = Delegation::new(db.clone(), vec![Task::Ocr, Task::Vision, Task::VisionQuery], None);
        let v = d.admin_view().await.unwrap();
        assert_eq!((v["enabled"].as_bool(), v["default"].as_bool(), v["lend"].is_null()), (Some(true), Some(true), true));
        assert_eq!(v["members"][0], json!({ "hex": alice, "email": "alice@example.org", "enabled": true, "own": false, "read": 0, "friend": null }));
        let v = d.admin_set(&json!({ "member": { "hex": alice, "enabled": false } })).await.unwrap();
        assert_eq!((v["members"][0]["enabled"].as_bool(), v["members"][0]["own"].as_bool()), (Some(false), Some(true)));
        let me = d.view(&alice).await.unwrap();
        assert_eq!((me["state"].as_str(), me["blocked"].as_str(), me["progress"]["waiting"].as_u64()), (Some("blocked"), Some("member"), Some(0)));
        d.admin_set(&json!({ "member": { "hex": alice, "enabled": null } })).await.unwrap();
        d.admin_set(&json!({ "enabled": false })).await.unwrap();
        assert_eq!(d.view(&alice).await.unwrap()["blocked"], "station");
        let v = d.admin_set(&json!({ "enabled": true })).await.unwrap();
        assert!(v["members"][0]["enabled"] == true && v["members"][0]["own"] == false, "retour au défaut");
        assert!(d.admin_set(&json!({ "lend": "friends" })).await.unwrap_err().contains("moteur"), "rien à prêter sans tunnels");
        assert!(d.admin_set(&json!({ "member": { "hex": "x", "enabled": true } })).await.is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn the_member_pauses_her_analysis_and_erases_only_what_was_computed_for_her() {
        let (dir, db) = db("pause").await;
        let d = Delegation::new(db.clone(), vec![Task::Ocr, Task::Vision, Task::VisionQuery], None);
        db.writer()
            .execute(
                "INSERT INTO search_text(body, owner_hex, sha, src, t_ms) VALUES ('facture', 'alice', 's1', 'ocr', 0), ('fiche du film', 'alice', 'm1', 'fiche', 0), ('facture', 'bob', 'b1', 'ocr', 0)",
                vec![],
            )
            .await
            .unwrap();
        db.writer().execute("INSERT INTO search_index(owner_hex, sha, task, state, updated_at) VALUES ('alice', 's1', 'ocr', 'done', 1), ('bob', 'b1', 'ocr', 'done', 1)", vec![]).await.unwrap();
        db.writer().execute("INSERT INTO kv(scope, key, value, updated_at) VALUES ('search_pack', 'alice', '{}', 1)", vec![]).await.unwrap();
        let waiting = |v: &Value| v["photos"]["waiting"].as_u64();
        assert_eq!((d.view("alice").await.unwrap()["paused"].as_bool(), waiting(&d.view("alice").await.unwrap())), (Some(false), Some(1)));
        let v = d.pause("alice", true).await.unwrap();
        assert_eq!((v["paused"].as_bool(), waiting(&v), v["progress"]["waiting"].as_u64()), (Some(true), Some(0), Some(0)), "en pause : plus rien n'attend, ni photo ni document");
        assert_eq!(v["progress"]["read"].as_u64(), Some(1), "ce qui est lu reste cherchable");
        assert_eq!(waiting(&d.pause("alice", false).await.unwrap()), Some(1), "reprise");
        d.erase("alice").await.unwrap();
        let count = |sql: &'static str| {
            let db = db.clone();
            async move { db.readers().read(move |c| Ok(c.query_row(sql, [], |r| r.get::<_, i64>(0))?)).await.unwrap() }
        };
        assert_eq!(count("SELECT count(*) FROM search_text WHERE owner_hex = 'alice'").await, 1, "la fiche de la Médiathèque reste");
        assert_eq!(count("SELECT count(*) FROM search_index WHERE owner_hex = 'alice'").await, 0);
        assert_eq!(count("SELECT count(*) FROM search_text WHERE owner_hex = 'bob'").await + count("SELECT count(*) FROM search_index WHERE owner_hex = 'bob'").await, 2, "rien de Bob n'est touché");
        assert_eq!(count("SELECT count(*) FROM kv WHERE scope = 'search_pack'").await, 1, "le paquet n'est oublié qu'une fois retiré du uCloud");
        assert_eq!(d.view("alice").await.unwrap()["paused"].as_bool(), Some(true), "effacer met en pause : rien n'est aussitôt recalculé");
        d.forget_pack("alice").await.unwrap();
        assert_eq!(count("SELECT count(*) FROM kv WHERE scope = 'search_pack'").await, 0);
        let _ = std::fs::remove_dir_all(dir);
    }
}
