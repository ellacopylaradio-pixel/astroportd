//! Les **écluses** de la station (écran Capitaine « Écluses ») : par où les données entrent et sortent, pour **cette**
//! station, et le mode d'ouverture qui les règle d'un geste ([`astro_db::station_mode`]).
//!
//! Six sas, du plus intime au plus ouvert : le coffre (le membre seul), le uCloud chiffré, les amis réciproques (N¹
//! mutuel : photos seul-à-seul, sauvegarde, morceau de clé de secours, tunnels « amis »), le cercle (N¹ ∪ N² : écrire
//! sur le relai, être crawlé), la constellation (`swarm.key` : réseau privé libp2p) et le public (profils, `/Public`,
//! Kubo pour ORIGIN seulement). La vue dit, chiffres à l'appui, qui passe chaque sas ; le mode change les portes.

use std::sync::Arc;

use astro_core::ActivityBus;
use astro_db::Db;
use astro_db::station_mode::{FORTRESS_SQL, Mode};
use astro_relay::DbCircle;
use serde_json::{Value, json};

use crate::station_admin::KindsControl;

/// Ce que `serve` sait de la station et qui ne change pas à chaud.
#[derive(Debug, Clone, Default)]
pub struct Facts {
    /// ORIGIN (pas de `swarm.key`, ou celle à zéros) ?
    pub origin: bool,
    /// Nom du plan de cette UPlanet (`planes.json`, « origin » à défaut).
    pub name: String,
    /// `UPLANETNAME` : seule son empreinte courte sort de la vue, jamais le secret.
    pub uplanet: String,
    /// Kubo branché (ORIGIN seulement) ?
    pub kubo: bool,
    /// Autres plans qui partagent le bassin de blobs.
    pub pool_peers: usize,
    /// Maillage P2P démarré ?
    pub p2p: bool,
    /// Crawl des amis d'amis actif (`--no-crawl` le coupe) ?
    pub crawl: bool,
    /// Inscriptions libres permises par `serve` (`--no-register` les ferme) ?
    pub register: bool,
}

/// Pilote des écluses.
pub struct Locks {
    db: Arc<Db>,
    kinds: Arc<KindsControl>,
    circle: Arc<DbCircle>,
    facts: Facts,
    activity: ActivityBus,
}

impl Locks {
    /// Branche le pilote.
    pub fn new(db: Arc<Db>, kinds: Arc<KindsControl>, circle: Arc<DbCircle>, facts: Facts, activity: ActivityBus) -> Self {
        Self { db, kinds, circle, facts, activity }
    }

    /// Vue de l'écran : mode, plan, portes, et qui passe chaque sas.
    pub async fn view(&self) -> Result<Value, String> {
        let sql = format!(
            "SELECT \
               (SELECT count(*) FROM identities WHERE hosted = 1), \
               (SELECT count(DISTINCT pubkey) FROM social_graph WHERE level = 1 AND owner_hex IN (SELECT hex FROM identities WHERE hosted = 1)), \
               (SELECT count(DISTINCT g.pubkey) FROM social_graph g WHERE g.level = 1 AND g.owner_hex IN (SELECT hex FROM identities WHERE hosted = 1) \
                  AND EXISTS (SELECT 1 FROM nostr_events e JOIN nostr_tags t ON t.event_id = e.seq WHERE e.kind = 3 AND e.pubkey = g.pubkey AND t.name = 'p' AND t.value = g.owner_hex)), \
               (SELECT count(DISTINCT pubkey) FROM social_graph WHERE level = 2 AND pubkey NOT IN (SELECT pubkey FROM social_graph WHERE level = 1)), \
               (SELECT count(*) FROM kv WHERE scope = '{auto}' AND value = '0'), \
               (SELECT count(*) FROM ucloud_entries WHERE type = 'file' AND path >= '/Public/' AND path < '/Public0'), \
               {FORTRESS_SQL}",
            auto = astro_facecloud::review::AUTO_SHARE_SCOPE
        );
        let (hosted, n1, mutual, n2, auto_off, public, fortress) = self
            .db
            .readers()
            .read(move |c| Ok(c.query_row(&sql, [], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?, r.get::<_, i64>(4)?, r.get::<_, i64>(5)?, r.get::<_, bool>(6)?)))?))
            .await
            .map_err(|e| e.to_string())?;
        let privacy = crate::privacy_watch::sweep(&self.db).await.map_err(|e| e.to_string())?;
        let (unserved, sensitive) = privacy.iter().fold((0, 0), |(u, s), (_, f)| (u + f.unserved.len(), s + f.sensitive.len() + f.papers.len()));
        let mode = crate::station_admin::station_mode(&self.db).await;
        let f = &self.facts;
        Ok(json!({
            "mode": mode.as_str(),
            "plane": { "origin": f.origin, "name": f.name, "tag": crate::bridge::plane_tag(&f.uplanet), "kubo": f.kubo, "pool_peers": f.pool_peers, "p2p": f.p2p },
            "gates": {
                "register": f.register && !fortress,
                "register_cli": f.register,
                "crawl": f.crawl,
                "n2": !fortress,
                "mutual_only": fortress,
                "photos_auto": !fortress,
                "open_writes": self.kinds.open_writes(),
            },
            "people": { "hosted": hosted, "n1": n1, "n1_mutual": mutual, "n2": if fortress { 0 } else { n2 }, "n2_known": n2 },
            "faces": { "auto_off_members": auto_off },
            "public": { "files": public, "unserved": unserved, "sensitive": sensitive, "members": privacy.len() },
        }))
    }

    /// `{ "mode": "forteresse" | "village" | "place" }` : enregistre, applique au relai et au cercle sur-le-champ, dit le
    /// changement sur le bus d'activité ; le crawl et FaceCloud le lisent à leur prochain passage.
    pub async fn set(&self, body: &Value) -> Result<Value, String> {
        let mode = body["mode"].as_str().and_then(Mode::parse).ok_or("mode attendu : forteresse, village ou place")?;
        let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
        self.db
            .writer()
            .execute(
                "INSERT INTO kv(scope, key, value, updated_at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                vec![astro_db::station_mode::SCOPE.to_owned().into(), astro_db::station_mode::KEY.to_owned().into(), mode.as_str().to_owned().into(), at.into()],
            )
            .await
            .map_err(|e| format!("mode non enregistré : {e}"))?;
        self.kinds.apply_mode().await?;
        self.circle.refresh(&self.db).await.map_err(|e| format!("cercle du relai : {e}"))?;
        let said = match mode {
            Mode::Forteresse => "station en Forteresse : seuls les amis réciproques des membres entrent, chaque photo attend un accord",
            Mode::Village => "station en Village : amis et amis d'amis, photos envoyées seules quand l'ami est seul avec vous",
            Mode::Place => "station en Place publique : tout le monde peut publier son profil et des notes publiques sur le relai",
        };
        self.activity.publish("system", "écluses", said, None);
        self.view().await
    }
}
