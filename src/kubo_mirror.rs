//! **Miroir Kubo d'ORIGIN** : « le disque d'abord, Kubo en miroir ».
//!
//! Chaque fichier de la station s'écrit sur son disque ([`astro_dav::AutoBlobStore`]), qui reste sa vérité : vérifiable, et
//! lisible même quand Kubo est arrêté. Ce miroir, lui, **publie** ces blobs dans Kubo pour qu'ils soient trouvables sur le
//! **réseau IPFS des stations ORIGIN** (le Kubo d'ORIGIN porte la swarm.key à zéros : ce n'est pas l'IPFS mondial), et tient à
//! jour la table `kubo_mirror` (état local). Un passage :
//!
//! 1. **rapatrie** sur le disque les blobs que la base référence mais qui ne vivent que dans Kubo (rangés là avant ce choix) :
//!    un CID `raw` est vérifié par son empreinte, une racine `dag-pb` en la faisant recalculer par Kubo sans rien garder ;
//! 2. **publie** les blobs du disque pas encore publiés : par **référence** au fichier quand Kubo l'autorise (*filestore*,
//!    aucune place en plus), sinon par copie ; une référence que Kubo ne sait pas relire (droits) est retirée et remplacée
//!    par une copie ;
//! 3. **désépingle** ce que plus rien ne référence.
//!
//! Jamais en ẐEN : `serve` refuse `--kubo` avec une `swarm.key` (les CID d'une UPlanet privée ne sortent pas de son essaim).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use astro_db::Db;
use astro_dav::blob::{BlobError, BlobStore, BlobWriter, FsBlobStore, KuboBlobStore, is_cid};
use astro_dav::kubo::AddMode;
use tokio::io::AsyncWriteExt;

/// Blobs traités au plus par passage (rapatriés ou publiés) : le reste attend le passage suivant.
pub const PASS_BUDGET: usize = 500;

/// Ce qu'un passage a fait.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Pass {
    /// Kubo ne répondait pas : rien n'a été tenté.
    pub kubo_down: bool,
    /// Publiés par référence au fichier du disque.
    pub by_reference: usize,
    /// Publiés par copie.
    pub copied: usize,
    /// Rapatriés de Kubo sur le disque.
    pub brought_back: usize,
    /// Référencés par la base, absents du disque comme de Kubo.
    pub missing: usize,
    /// Désépinglés : plus rien ne les référence.
    pub unpinned: usize,
    /// Restent à traiter (budget du passage atteint).
    pub waiting: usize,
    /// Échecs, une phrase chacun.
    pub failures: Vec<String>,
}

impl Pass {
    /// Le passage a-t-il changé quelque chose ?
    pub fn did_something(&self) -> bool {
        self.by_reference + self.copied + self.brought_back + self.unpinned > 0
    }

    /// Résumé lisible par le Capitaine.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if self.by_reference + self.copied > 0 {
            parts.push(format!("{} fichier(s) publié(s) sur le réseau IPFS d'ORIGIN ({} par référence, {} par copie)", self.by_reference + self.copied, self.by_reference, self.copied));
        }
        if self.brought_back > 0 {
            parts.push(format!("{} rapatrié(s) de Kubo sur le disque", self.brought_back));
        }
        if self.unpinned > 0 {
            parts.push(format!("{} désépinglé(s)", self.unpinned));
        }
        if self.missing > 0 {
            parts.push(format!("{} introuvable(s) ni sur le disque ni dans Kubo", self.missing));
        }
        if self.waiting > 0 {
            parts.push(format!("{} en attente du passage suivant", self.waiting));
        }
        format!("miroir IPFS : {}", parts.join(", "))
    }
}

/// Le miroir : la base de la station (ce qu'elle référence), son disque, son Kubo.
pub struct Mirror {
    db: Arc<Db>,
    fs: FsBlobStore,
    kubo: KuboBlobStore,
    /// Les références au disque sont-elles relues par Kubo ? Mis à `false` au premier refus (droits) : copies ensuite.
    by_reference: AtomicBool,
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

impl Mirror {
    /// Miroir de `fs` dans `kubo`, pour ce que `db` référence.
    pub fn new(db: Arc<Db>, fs: FsBlobStore, kubo: KuboBlobStore) -> Self {
        Self { db, fs, kubo, by_reference: AtomicBool::new(true) }
    }

    /// Un passage (voir le module), `budget` blobs au plus.
    pub async fn pass(&self, budget: usize) -> Result<Pass, String> {
        let mut p = Pass::default();
        if !self.kubo.ping().await {
            p.kubo_down = true;
            return Ok(p);
        }
        let (referenced, mirrored): (HashSet<String>, HashMap<String, String>) = self
            .db
            .readers()
            .read(|c| {
                let all = astro_dav::pool::referenced_set_of(c).map_err(|_| astro_db::DbError::Aborted)?;
                let mut stmt = c.prepare("SELECT blob, root FROM kubo_mirror")?;
                let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?.collect::<Result<HashMap<_, _>, _>>()?;
                Ok((all.into_iter().filter(|c| is_cid(c)).collect(), rows))
            })
            .await
            .map_err(|e| format!("lecture de la base : {e}"))?;
        let nocopy = self.by_reference.load(Ordering::Relaxed) && self.kubo.filestore_enabled().await;
        let mut todo: Vec<&String> = referenced.iter().filter(|c| !mirrored.contains_key(*c)).collect();
        todo.sort();
        p.waiting = todo.len().saturating_sub(budget);
        for cid in todo.into_iter().take(budget) {
            if let Err(why) = self.one(cid, nocopy, &mut p).await {
                p.failures.push(format!("{cid} : {why}"));
            }
        }
        for (blob, root) in mirrored.iter().filter(|(b, _)| !referenced.contains(*b)) {
            match self.kubo.remove(root).await {
                Ok(()) | Err(BlobError::NotFound) => {
                    let _ = self.db.writer().execute("DELETE FROM kubo_mirror WHERE blob = ?1", vec![blob.clone().into()]).await;
                    p.unpinned += 1;
                }
                Err(e) => p.failures.push(format!("{root} : désépinglage impossible : {e}")),
            }
        }
        Ok(p)
    }

    /// Rapatrie `cid` si besoin, puis le publie.
    async fn one(&self, cid: &str, nocopy: bool, p: &mut Pass) -> Result<(), String> {
        let path = self.fs.blob_path(cid).map_err(|e| e.to_string())?;
        if !tokio::fs::try_exists(&path).await.unwrap_or(false) {
            if !self.bring_back(cid).await? {
                p.missing += 1;
                return Ok(());
            }
            p.brought_back += 1;
            // Rangé dans Kubo par copie avant ce choix, et toujours épinglé : déjà publié.
            if self.kubo.is_pinned(cid).await {
                return self.record(cid, cid, false).await;
            }
        }
        if nocopy && self.by_reference.load(Ordering::Relaxed) {
            match self.kubo.add_file(&path, AddMode::NoCopy).await {
                Ok(root) if self.kubo.readable(&root).await => {
                    p.by_reference += 1;
                    return self.record(cid, &root, true).await;
                }
                Ok(root) => {
                    // Kubo a pris la référence mais ne peut pas relire le fichier (il tourne sous un autre utilisateur ?) :
                    // la référence est retirée, et la suite se fait par copie.
                    let _ = self.kubo.remove(&root).await;
                    self.by_reference.store(false, Ordering::Relaxed);
                    p.failures.push(format!("Kubo ne peut pas relire les fichiers du disque ({}) : publication par copie (vérifiez que le service ipfs tourne sous le même utilisateur qu'astroportd)", path.display()));
                }
                Err(e) => p.failures.push(format!("{cid} : référence refusée par Kubo ({e}) : copie")),
            }
        }
        let root = self.kubo.add_file(&path, AddMode::Copy).await.map_err(|e| format!("publication impossible : {e}"))?;
        p.copied += 1;
        self.record(cid, &root, false).await
    }

    /// Copie sur le disque un blob qui ne vit que dans Kubo, après vérification. `false` : Kubo ne l'a pas.
    async fn bring_back(&self, cid: &str) -> Result<bool, String> {
        let mut reader = match self.kubo.open(cid).await {
            Ok((r, _)) => r,
            Err(BlobError::NotFound) => return Ok(false),
            Err(e) => return Err(format!("lecture dans Kubo : {e}")),
        };
        let mut w = self.fs.begin_named(cid).await.map_err(|e| e.to_string())?;
        tokio::io::copy(&mut reader, &mut w).await.map_err(|e| format!("rapatriement interrompu : {e}"))?;
        w.flush().await.map_err(|e| e.to_string())?;
        // Une racine `dag-pb` ne se recalcule pas ici : Kubo la recalcule sur ce qui vient d'être reçu, sans rien garder.
        if cid.starts_with("bafybei") {
            let again = self.kubo.add_file(w.tmp_path(), AddMode::OnlyHash).await.map_err(|e| format!("vérification impossible : {e}"))?;
            if again != cid {
                return Err(format!("le contenu relu de Kubo donne {again} : rapatriement refusé (découpage différent, ou contenu altéré)"));
            }
        }
        w.commit().await.map_err(|e| e.to_string())?;
        Ok(true)
    }

    async fn record(&self, blob: &str, root: &str, nocopy: bool) -> Result<(), String> {
        self.db
            .writer()
            .execute(
                "INSERT INTO kubo_mirror(blob, root, nocopy, at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(blob) DO UPDATE SET root = excluded.root, nocopy = excluded.nocopy, at = excluded.at",
                vec![blob.to_owned().into(), root.to_owned().into(), i64::from(nocopy).into(), now().into()],
            )
            .await
            .map(|_| ())
            .map_err(|e| format!("écriture de la base : {e}"))
    }

    /// Passages en boucle : le premier après `first`, puis toutes les `every`, jusqu'à l'arrêt. Ce qui change est dit au
    /// Capitaine sur le bus d'activité ; un échec aussi (jamais avalé). Kubo arrêté : la publication attend, sans bruit
    /// (dit une fois, puis à son retour).
    pub async fn run_forever(self: Arc<Self>, first: Duration, every: Duration, bus: astro_core::ActivityBus, mut stop: tokio::sync::watch::Receiver<bool>) {
        let mut wait = first;
        let mut was_down = false;
        loop {
            tokio::select! {
                () = tokio::time::sleep(wait) => {}
                changed = stop.changed() => {
                    if changed.is_err() || *stop.borrow() {
                        return;
                    }
                    continue;
                }
            }
            wait = every;
            match self.pass(PASS_BUDGET).await {
                Ok(p) if p.kubo_down => {
                    if !was_down {
                        bus.publish_to(astro_core::Audience::Captain, "alert", "kubo", "miroir IPFS en pause : Kubo ne répond pas (les fichiers restent lisibles sur le disque ; la publication reprendra à son retour)", None);
                    }
                    was_down = true;
                }
                Ok(p) => {
                    if was_down {
                        bus.publish_to(astro_core::Audience::Captain, "info", "kubo", "miroir IPFS : Kubo est de retour, la publication reprend", None);
                    }
                    was_down = false;
                    if p.did_something() || p.missing > 0 {
                        bus.publish_to(astro_core::Audience::Captain, "info", "kubo", p.summary(), None);
                    }
                    if let Some(first) = p.failures.first() {
                        bus.publish_to(astro_core::Audience::Captain, "alert", "kubo", format!("miroir IPFS : {} échec(s), par exemple {first}", p.failures.len()), None);
                    }
                    // Du travail en attente : le passage suivant vient vite.
                    if p.waiting > 0 {
                        wait = Duration::from_secs(5);
                    }
                }
                Err(e) => {
                    bus.publish_to(astro_core::Audience::Captain, "alert", "kubo", format!("miroir IPFS interrompu : {e}"), None);
                }
            }
        }
    }
}
