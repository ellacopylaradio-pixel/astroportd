//! Social : carnet de contacts, messagerie, présence et visio, agents, BRO, serveur MCP.
//!
//! La structure ne porte que les champs que ce domaine utilise ; elle est assemblée par `StationUi::into_router`.

use super::*;
use rusqlite::OptionalExtension;

/// État du domaine « social ».
pub struct SocialBackend<B: BlobStore> {
    /// Services partagés.
    pub(super) services: Arc<StationServices>,
    /// Authentification et WebDAV.
    pub(super) dav: Arc<DavState<B>>,
    /// Mémoire de BRO (avec `--ollama`).
    pub(super) memory: Option<Arc<BroMemory>>,
    pub(super) contacts: Option<Arc<crate::identity_links::IdentityLinks<B>>>,
    pub(super) agents: Option<Arc<crate::agents::Agents>>,
    pub(super) cosurf: Option<Arc<crate::cosurf::CoSurf>>,
    pub(super) ai: Option<Arc<crate::member_ai::MemberAi>>,
    pub(super) reading: Option<Arc<crate::delegation::Delegation>>,
    pub(super) book: Option<Arc<crate::address_book::AddressBook<B>>>,
    pub(super) chat: Option<Arc<crate::chat::Chat>>,
    /// Visio : port STUN de la station (UDP), secret du relais d'appel s'il est actif, serveurs supplémentaires.
    pub(super) ice: (Option<u16>, Option<[u8; 32]>, Vec<String>),
    pub(super) presence: Option<Arc<crate::presence::Presence>>,
    pub(super) people: Option<Arc<crate::person_keys::PersonKeys>>,
    /// « Se rencontrer » : ajout d'un proche (liste de contacts signée, `Invites::follow`).
    pub(super) invites: Option<Arc<crate::invites::Invites<B>>>,
    /// Moteur local de la recherche par l'image (l'outil « chercher » de BRO).
    pub(super) seer: Option<Arc<astro_index::LocalIndexer>>,
}

/// Outils de BRO pour un membre : la recherche de sa bibliothèque (mots, personnes, villes, dates, et ce que montrent les photos),
/// son solde, ses souvenirs ; une seule écriture, « noter », dans son propre journal, à sa demande.
struct BroTools {
    services: Arc<StationServices>,
    hex: String,
    memory: Option<Arc<BroMemory>>,
    seer: Option<Arc<astro_index::LocalIndexer>>,
}

impl crate::bro_chat::Tools for BroTools {
    async fn run(&self, name: &str, args: &Value) -> Result<Value, String> {
        match name {
            "chercher" => {
                let q: String = args.get("requete").and_then(Value::as_str).unwrap_or_default().chars().take(200).collect();
                if q.trim().is_empty() {
                    return Err("requête vide".into());
                }
                let owner = self.hex.clone();
                // Comme l'écran Fichiers : les mots « à voir » (« plage », « chat roux ») cherchent aussi dans ce que montrent les photos.
                let vector = super::ucloud::vision_query(&self.services, self.seer.as_ref(), &owner, &q).await.map_err(|e| e.to_string())?;
                let found = self
                    .services
                    .db
                    .readers()
                    .read(move |c| {
                        let seen = match &vector {
                            Some(v) => crate::library::seen(c, &owner, v)?,
                            None => crate::library::Seen::default(),
                        };
                        let s = crate::library::Search { q: Some(q), person: None, from: None, to: None, bbox: None, kind: None, after: None, limit: 8, seen: seen.strict, near: seen.near };
                        Ok(crate::library::search(c, &owner, &s)?)
                    })
                    .await
                    .map_err(|e| e.to_string())?;
                let pick = |v: &Value| json!({ "chemin": v["path"], "date": v["taken_at"].as_i64().or(v["mtime"].as_i64()).and_then(|t| u64::try_from(t).ok()).map(astro_core::week::date_string), "extrait": v["excerpt"] });
                let best = found["best"].as_array().filter(|b| !b.is_empty()).or(found["items"].as_array()).cloned().unwrap_or_default();
                Ok(json!({ "trouvés": best.iter().take(8).map(pick).collect::<Vec<_>>(), "compris": found["understood"] }))
            }
            "souvenirs" => {
                let memory = self.memory.as_ref().ok_or("mémoire indisponible sur cette station")?;
                let q: String = args.get("requete").and_then(Value::as_str).unwrap_or_default().chars().take(500).collect();
                let found = memory.recall(&self.hex, &q, 6, None).await.map_err(|e| e.to_string())?;
                Ok(json!({ "souvenirs": found.iter().map(|r| json!({ "texte": r.memory.content, "date": astro_core::week::date_string(r.memory.ts) })).collect::<Vec<_>>() }))
            }
            "noter" => {
                let memory = self.memory.as_ref().ok_or("mémoire indisponible sur cette station")?;
                let text: String = args.get("texte").and_then(Value::as_str).unwrap_or_default().chars().take(2000).collect();
                let meta = Meta { source: "bro".into(), ..Meta::default() };
                match memory.remember(&self.hex, Slot::Journal, &text, meta).await {
                    Ok(r) if r.mem_id.is_some() => Ok(json!({ "noté": true })),
                    Ok(_) => Ok(json!({ "noté": false, "raison": "déjà connu" })),
                    Err(e) => Err(format!("non noté : {e}")),
                }
            }
            "solde" => {
                let b = self.services.balance_of(&self.hex).await?;
                Ok(json!({ "zen": format!("{}.{:02}", b.zen_cents / 100, b.zen_cents % 100) }))
            }
            _ => Err("outil inconnu".into()),
        }
    }
    fn has(&self, name: &str) -> bool {
        !matches!(name, "souvenirs" | "noter") || self.memory.is_some()
    }
}

/// « Se rencontrer » : une demande reçue reste montrée 30 minutes (le temps d'une rencontre), 10 au plus.
const MEET_WINDOW_SECS: i64 = 1800;
/// Personnes rendues au plus par une recherche dans la constellation.
const MEET_FOUND: usize = 20;

impl_auth!(SocialBackend);

impl<B: BlobStore> SocialBackend<B> {
    /// Ajoute à une fiche `keys` : les clés de la personne dans chaque UPlanet (emails de la fiche, clé déclarée, liens prouvés).
    async fn add_keys(&self, card: &mut Value) {
        let Some(people) = &self.people else { return };
        let emails: Vec<String> = card["fields"]["emails"].as_array().map(|a| a.iter().filter_map(|e| e["value"].as_str().map(str::to_owned)).collect()).unwrap_or_default();
        let declared = card["pubkey"].as_str().map(str::to_owned);
        card["keys"] = json!(people.keys_for(&emails, declared.as_deref()).await);
        card["plane"] = json!(people.plane());
    }

    fn cosurf_service(&self) -> Result<&Arc<crate::cosurf::CoSurf>, ApiError> {
        self.cosurf.as_ref().ok_or_else(|| ApiError::unavailable("navigation partagée non disponible sur cette station"))
    }

    /// La session est-elle tenue par une autre station ? (`station` donnée et différente de celle-ci.)
    fn elsewhere<'a>(&self, station: Option<&'a str>) -> Option<&'a str> {
        let here = self.cosurf.as_ref().and_then(|c| c.station());
        station.filter(|s| !s.is_empty() && Some(*s) != here.as_deref())
    }

    fn agents_service(&self) -> Result<&Arc<crate::agents::Agents>, ApiError> {
        self.agents.as_ref().ok_or_else(|| ApiError::unavailable("agents non disponibles sur cette station"))
    }

    fn contacts_service(&self) -> Result<&Arc<crate::identity_links::IdentityLinks<B>>, ApiError> {
        self.contacts.as_ref().ok_or_else(|| ApiError::unavailable("l'annuaire n'est pas disponible sur cette station"))
    }
}

impl<B: BlobStore> SocialApi for SocialBackend<B> {
    async fn agents_list(&self, who: &Principal) -> Result<Value, ApiError> {
        self.agents_service()?.list(&who.hex).await.map_err(agent_err)
    }
    async fn agents_enroll(&self, who: &Principal, name: String, group: String, max_secs: u64, totp: bool, station: Option<String>) -> Result<Value, ApiError> {
        self.agents_service()?.enroll_on(&who.hex, &name, &group, max_secs, totp, station.as_deref()).await.map_err(agent_err)
    }
    async fn agents_revoke(&self, who: &Principal, id: String) -> Result<Value, ApiError> {
        self.agents_service()?.revoke(&who.hex, &id).await.map_err(agent_err)
    }
    async fn agents_requests(&self, who: &Principal) -> Result<Value, ApiError> {
        Ok(self.agents_service()?.requests(&who.hex))
    }
    async fn cosurf_list(&self, who: &Principal) -> Result<Value, ApiError> {
        Ok(self.cosurf_service()?.list(&who.hex))
    }
    async fn cosurf_create(&self, who: &Principal, url: String, guests: Vec<(String, String)>, level: String) -> Result<Value, ApiError> {
        let label = who.email.as_deref().and_then(|e| e.split('@').next()).filter(|s| !s.is_empty()).map_or_else(|| format!("…{}", &who.hex[56..]), str::to_owned);
        self.cosurf_service()?.create(&who.hex, &label, &url, &guests, &level).await.map_err(super::cosurf_err)
    }
    async fn cosurf_view(&self, who: &Principal, id: String, station: Option<String>) -> Result<Value, ApiError> {
        let c = self.cosurf_service()?;
        match self.elsewhere(station.as_deref()) {
            Some(s) => c.remote(s, &who.hex, &id, "view", &Value::Null).await,
            None => c.view(&who.hex, &id),
        }
        .map_err(super::cosurf_err)
    }
    async fn cosurf_level(&self, who: &Principal, id: String, guest: String, level: String) -> Result<Value, ApiError> {
        self.cosurf_service()?.set_level(&who.hex, &id, &guest, &level).await.map_err(super::cosurf_err)
    }
    async fn cosurf_stop(&self, who: &Principal, id: String, station: Option<String>) -> Result<Value, ApiError> {
        let c = self.cosurf_service()?;
        match self.elsewhere(station.as_deref()) {
            Some(s) => c.remote(s, &who.hex, &id, "stop", &Value::Null).await,
            None => c.stop(&who.hex, &id).await,
        }
        .map_err(super::cosurf_err)
    }
    async fn cosurf_screen(&self, who: &Principal, id: String, station: Option<String>) -> Result<tokio::sync::mpsc::Receiver<(String, Arc<str>)>, ApiError> {
        let c = self.cosurf_service()?;
        match self.elsewhere(station.as_deref()) {
            Some(s) => c.remote_watch(s, &who.hex, &id).await,
            None => c.watch(&who.hex, &id).await,
        }
        .map_err(super::cosurf_err)
    }
    async fn cosurf_input(&self, who: &Principal, id: String, station: Option<String>, gesture: Value) -> Result<Value, ApiError> {
        let c = self.cosurf_service()?;
        match self.elsewhere(station.as_deref()) {
            Some(s) => c.remote(s, &who.hex, &id, "input", &gesture).await,
            None => c.input(&who.hex, &id, &gesture).await,
        }
        .map_err(super::cosurf_err)
    }
    async fn agents_ready(&self, who: &Principal, id: String) -> Result<Value, ApiError> {
        self.agents_service()?.ready(&who.hex, &id).await.map_err(agent_err)
    }
    async fn agents_stop(&self, who: &Principal, id: String) -> Result<Value, ApiError> {
        self.agents_service()?.stop(&who.hex, &id).await.map_err(agent_err)
    }
    async fn agents_screen(&self, who: &Principal, id: String) -> Result<tokio::sync::watch::Receiver<Option<Arc<str>>>, ApiError> {
        self.agents_service()?.screen_watch(&who.hex, &id).await.map_err(agent_err)
    }
    async fn agents_input(&self, who: &Principal, id: String, gesture: Value) -> Result<Value, ApiError> {
        self.agents_service()?.screen_input(&who.hex, &id, &gesture).await.map_err(agent_err)
    }
    async fn agents_decide(&self, who: &Principal, id: String, approve: bool, secs: Option<u64>, hosts: Vec<String>) -> Result<Value, ApiError> {
        self.agents_service()?.decide_hosts(&who.hex, &id, approve, secs, &hosts).await.map_err(agent_err)
    }
    async fn mcp(&self, bearer: Option<String>, station: Option<String>, body: Value) -> Result<Option<Value>, ApiError> {
        self.agents_service()?.mcp_via(bearer.as_deref(), station.as_deref(), &body).await.map_err(agent_err)
    }
    async fn book(&self, who: &Principal, op: BookOp) -> Result<Value, ApiError> {
        let book = self.book.as_ref().ok_or_else(|| ApiError::unavailable("le carnet d'adresses n'est pas disponible sur cette station"))?;
        let o = who.hex.as_str();
        let fields = |v: Value| serde_json::from_value::<astro_dav::vcard::Fields>(v).map_err(|e| ApiError::bad_request(format!("champs de la fiche invalides : {e}")));
        let out = match op {
            BookOp::List(q, group) => book.list(o, &q, group.as_deref()).await,
            BookOp::Get(path) => match book.get(o, &path).await {
                Ok(mut card) => {
                    self.add_keys(&mut card).await;
                    Ok(card)
                }
                e => e,
            },
            BookOp::Save(path, f) => book.save(o, path.as_deref(), &fields(f)?).await,
            BookOp::Delete(path) => book.delete(o, &path).await,
            BookOp::DeleteMany(paths) => book.delete_many(o, &paths).await,
            BookOp::Photo(path) => book.photo(o, &path).await,
            BookOp::PhotoChoices(path) => book.photo_choices(o, &path).await,
            BookOp::SetPhoto(path, uri) => book.set_photo(o, &path, uri.as_deref()).await,
            BookOp::Take(path, keys) => book.take(o, &path, &keys).await,
            BookOp::Group(from, to) => book.set_group(o, &from, to.as_deref()).await,
            BookOp::Import(vcf) => book.import(o, &vcf).await,
            BookOp::Duplicates => book.duplicates(o).await,
            BookOp::Merge(keep, drop) => book.merge(o, &keep, &drop).await,
            BookOp::MergeExact(apply, limit) => book.merge_exact(o, apply, limit).await,
            BookOp::Me => book.me(o).await,
            BookOp::SaveMe(f) => book.save_me(o, &fields(f)?).await,
        };
        out.map_err(book_error)
    }
    async fn my_links(&self, who: &Principal) -> Result<Value, ApiError> {
        let people = self.people.as_ref().ok_or_else(|| ApiError::unavailable("annuaire des UPlanet non disponible sur cette station"))?;
        let keys = people.class_of(&who.hex).await;
        Ok(json!({ "plane": people.plane(), "siblings": people.siblings().iter().map(|s| &s.name).collect::<Vec<_>>(), "keys": keys }))
    }
    async fn announce_links(&self, who: &Principal) -> Result<Value, ApiError> {
        let people = self.people.as_ref().ok_or_else(|| ApiError::unavailable("annuaire des UPlanet non disponible sur cette station"))?;
        let email = who.email.as_deref().ok_or_else(|| ApiError::bad_request("ce compte n'a pas d'email : rien à relier"))?;
        people.announce(&who.hex, email).await.map_err(ApiError::bad_request)
    }
    async fn meet(&self, who: &Principal, op: MeetOp) -> Result<Value, ApiError> {
        let invites = self.invites.as_ref().ok_or_else(|| ApiError::unavailable("« Se rencontrer » n'est pas disponible sur cette station"))?;
        let db = &self.services.db;
        let fail = |e: astro_db::DbError| {
            eprintln!("interface : se rencontrer : {e}");
            ApiError::internal("proches illisibles")
        };
        // Où en est l'amitié : je le suis (mon graphe N¹), il me suit (sa liste de contacts).
        let status = |me: String, to: String| async move {
            db.readers()
                .read(move |c| {
                    let mine = c.query_row("SELECT 1 FROM social_graph WHERE owner_hex = ?1 AND pubkey = ?2 AND level = 1", [&me, &to], |r| r.get::<_, i64>(0)).optional()?.is_some();
                    let back = c.query_row("SELECT 1 FROM nostr_events e JOIN nostr_tags t ON t.event_id = e.seq WHERE e.kind = 3 AND e.pubkey = ?1 AND t.name = 'p' AND t.value = ?2 LIMIT 1", [&to, &me], |r| r.get::<_, i64>(0)).optional()?.is_some();
                    Ok((mine, back))
                })
                .await
        };
        // Rencontre attestée par `me` avec `to` : (how, verified).
        let met = |me: String, to: String| async move {
            db.readers()
                .read(move |c| Ok(c.query_row("SELECT how, verified FROM meetings WHERE owner_hex = ?1 AND target_hex = ?2", [&me, &to], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? == 1))).optional()?))
                .await
        };
        let q = match op {
            MeetOp::Code => {
                let now = u64::try_from(now_secs()).unwrap_or(0);
                let proof = invites.meet_proof(&who.hex, now).ok_or_else(|| ApiError::unavailable("clé du compte introuvable sur cette station"))?;
                let left = astro_economy::meet::SLOT_SECS - now % astro_economy::meet::SLOT_SECS;
                return Ok(json!({ "proof": proof, "expires_in": left }));
            }
            MeetOp::Add { to, proof } => {
                let now = u64::try_from(now_secs()).unwrap_or(0);
                // Un code « rencontre » scanné doit être frais et signé par la personne : sinon on le dit, sans rien ajouter.
                if let Some(p) = &proof {
                    if !astro_economy::meet::verify_proof(&to, p, now) {
                        return Err(ApiError::bad_request("ce code a expiré : demandez à la personne de le remontrer"));
                    }
                }
                invites.follow(&who.hex, &to).await.map_err(|e| {
                    eprintln!("interface : se rencontrer : {e}");
                    ApiError::internal("ajout impossible pour l'instant")
                })?;
                // Attestation : jamais une nomination par-dessus une rencontre déjà prouvée (la plus récente l'emporte).
                let how = if proof.is_some() { astro_economy::meet::QR } else { astro_economy::meet::NAMED };
                let before = met(who.hex.clone(), to.clone()).await.map_err(fail)?;
                let upgrade = match &before { None => true, Some((_, true)) => false, Some((h, false)) => how == astro_economy::meet::QR || h != how };
                if upgrade {
                    invites.attest_meeting(&who.hex, &to, how, proof.as_deref()).await.map_err(|e| {
                        eprintln!("interface : se rencontrer : {e}");
                        ApiError::internal("rencontre non enregistrée")
                    })?;
                }
                Ok(to)
            }
            MeetOp::Status(to) => Ok(to),
            MeetOp::List(q) => Err(q),
        };
        let q = match q {
            Ok(to) => {
                let (mine, back) = status(who.hex.clone(), to.clone()).await.map_err(fail)?;
                let m = met(who.hex.clone(), to.clone()).await.map_err(fail)?;
                let theirs = met(to.clone(), who.hex.clone()).await.map_err(fail)?;
                let side = |m: Option<(String, bool)>| m.map(|(how, verified)| json!({ "how": how, "verified": verified }));
                // Niveau de proximité (privé) : seulement pour quelqu'un que je suis.
                let level = if mine {
                    let (ranks, _) = crate::proximity::load(db, invites.keys().as_ref(), &who.hex).await.unwrap_or_default();
                    Some(ranks.get(&to).copied().unwrap_or_else(|| crate::proximity::default_level(mine && back)))
                } else {
                    None
                };
                return Ok(json!({ "hex": to, "label": invites.display_name(&to).await, "i_follow": mine, "follows_me": back, "friends": mine && back,
                    "met": side(m), "met_back": side(theirs), "level": level }));
            }
            Err(q) => q,
        };
        // Demandes : qui m'a ajouté récemment, sans que je l'aie ajouté en retour.
        let (me, since) = (who.hex.clone(), now_secs() - MEET_WINDOW_SECS);
        let asked: Vec<(String, i64)> = db
            .readers()
            .read(move |c| {
                let mut st = c.prepare("SELECT e.pubkey, MAX(e.created_at) FROM nostr_events e JOIN nostr_tags t ON t.event_id = e.seq WHERE e.kind = 3 AND t.name = 'p' AND t.value = ?1 AND e.pubkey <> ?1 AND e.created_at >= ?2 AND NOT EXISTS (SELECT 1 FROM social_graph g WHERE g.owner_hex = ?1 AND g.pubkey = e.pubkey AND g.level = 1) GROUP BY e.pubkey ORDER BY 2 DESC LIMIT 10")?;
                let rows = st.query_map(rusqlite::params![me, since], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .await
            .map_err(fail)?;
        let mut requests = Vec::new();
        for (hex, at) in asked {
            requests.push(json!({ "hex": hex, "label": invites.display_name(&hex).await, "at": at }));
        }
        let mut out = json!({ "requests": requests });
        if let Some(q) = q {
            // MULTIPASS de la constellation connus ici : nom du profil, sinon début de l'email ; jamais l'email entier.
            let me = who.hex.clone();
            let all: Vec<(String, Option<String>, Option<String>, i64)> = db
                .readers()
                .read(move |c| {
                    let mut st = c.prepare("SELECT i.hex, (SELECT json_extract(n.content, '$.name') FROM nostr_events n WHERE n.pubkey = i.hex AND n.kind = 0 ORDER BY n.created_at DESC LIMIT 1), i.email, i.hosted FROM identities i WHERE i.role IN ('multipass', 'captain') AND i.hex <> ?1")?;
                    let rows = st.query_map([&me], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<Result<Vec<_>, _>>()?;
                    Ok(rows)
                })
                .await
                .map_err(fail)?;
            let mut found: Vec<(String, String, bool)> = all
                .into_iter()
                .filter_map(|(hex, name, email, hosted)| {
                    let local = email.as_deref().and_then(|e| e.split('@').next()).unwrap_or_default().to_owned();
                    let label = name.filter(|n| !n.trim().is_empty()).unwrap_or_else(|| local.clone());
                    (!label.is_empty() && (label.to_lowercase().contains(&q) || local.to_lowercase().contains(&q))).then_some((hex, label, hosted == 1))
                })
                .collect();
            found.sort_by_key(|f| f.1.to_lowercase());
            found.truncate(MEET_FOUND);
            let mut people = Vec::new();
            for (hex, label, here) in found {
                let (mine, back) = status(who.hex.clone(), hex.clone()).await.map_err(fail)?;
                people.push(json!({ "hex": hex, "label": label, "here": here, "i_follow": mine, "follows_me": back, "friends": mine && back }));
            }
            out["people"] = json!(people);
        }
        Ok(out)
    }
    async fn circles(&self, who: &Principal, set: Option<(String, Option<u8>)>) -> Result<Value, ApiError> {
        use crate::proximity as px;
        let invites = self.invites.as_ref().ok_or_else(|| ApiError::unavailable("les cercles ne sont pas disponibles sur cette station"))?;
        let db = &self.services.db;
        let fail = |e: String| {
            eprintln!("interface : cercles : {e}");
            ApiError::internal("cercles illisibles")
        };
        // Ceux que je suis : ami réciproque ou non, rencontre vérifiée ou non.
        let me = who.hex.clone();
        let rows: Vec<(String, bool, bool)> = db
            .readers()
            .read(move |c| {
                let mut st = c.prepare(
                    "SELECT g.pubkey, EXISTS (SELECT 1 FROM nostr_events e JOIN nostr_tags t ON t.event_id = e.seq WHERE e.kind = 3 AND e.pubkey = g.pubkey AND t.name = 'p' AND t.value = ?1), \
                     COALESCE((SELECT verified FROM meetings m WHERE m.owner_hex = ?1 AND m.target_hex = g.pubkey), 0) \
                     FROM social_graph g WHERE g.owner_hex = ?1 AND g.level = 1 AND g.pubkey <> ?1 LIMIT 2000",
                )?;
                let r = st.query_map([&me], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? == 1, r.get::<_, i64>(2)? == 1)))?.collect::<Result<Vec<_>, _>>()?;
                Ok(r)
            })
            .await
            .map_err(|e| fail(e.to_string()))?;
        let (mut ranks, at) = px::load(db, invites.keys().as_ref(), &who.hex).await.map_err(fail)?;
        if let Some((to, level)) = set {
            let Some((_, friends, _)) = rows.iter().find(|r| r.0 == to) else {
                return Err(ApiError::bad_request("ajoutez d'abord cette personne à vos proches"));
            };
            px::set(&mut ranks, &to, level, *friends).map_err(ApiError::bad_request)?;
            let now = u64::try_from(now_secs()).unwrap_or(0);
            let e = px::event(invites.keys().as_ref(), &who.hex, &ranks, now, at).map_err(fail)?;
            invites.store_and_publish(e, "cercles").await.map_err(fail)?;
        }
        let mut people = Vec::with_capacity(rows.len());
        for (hex, friends, met) in rows {
            let ranked = ranks.get(&hex).copied();
            let level = ranked.unwrap_or_else(|| px::default_level(friends));
            people.push(json!({ "hex": hex, "label": invites.display_name(&hex).await, "level": level, "ranked": ranked.is_some(), "friends": friends, "met_verified": met }));
        }
        people.sort_by(|a, b| b["level"].as_u64().cmp(&a["level"].as_u64()).then_with(|| a["label"].as_str().map(str::to_lowercase).cmp(&b["label"].as_str().map(str::to_lowercase))));
        Ok(json!({ "levels": px::LEVELS.iter().map(|(l, n)| json!({ "level": l, "name": n })).collect::<Vec<_>>(), "people": people }))
    }
    async fn wot(&self, who: &Principal) -> Result<Value, ApiError> {
        use astro_economy::wot::{Cert, Params, Wot};
        let invites = self.invites.as_ref().ok_or_else(|| ApiError::unavailable("la toile de confiance n'est pas disponible sur cette station"))?;
        let fail = |e: String| {
            eprintln!("interface : toile de confiance : {e}");
            ApiError::internal("toile de confiance illisible")
        };
        // Rencontres prouvées seulement (revérifiées par cette station) ; racines : les Capitaines de la constellation.
        let (certs, roots): (Vec<Cert>, std::collections::BTreeSet<String>) = self
            .services
            .db
            .readers()
            .read(|c| {
                let mut st = c.prepare("SELECT owner_hex, target_hex, at FROM meetings WHERE verified = 1 LIMIT 200000")?;
                let certs = st.query_map([], |r| Ok(Cert { from: r.get(0)?, to: r.get(1)?, at: u64::try_from(r.get::<_, i64>(2)?).unwrap_or(0) }))?.collect::<Result<Vec<_>, _>>()?;
                let mut st = c.prepare("SELECT hex FROM identities WHERE role = 'captain'")?;
                let roots = st.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<_, _>>()?;
                Ok((certs, roots))
            })
            .await
            .map_err(|e| fail(e.to_string()))?;
        let now = u64::try_from(now_secs()).unwrap_or(0);
        let me = who.hex.clone();
        let (s, by, web) = tokio::task::spawn_blocking(move || {
            let w = Wot::compute(&certs, &roots, now, Params::G1);
            (w.standing(&me), w.certifiers(&me), (w.members.len(), w.referees.len(), roots.len()))
        })
        .await
        .map_err(|e| fail(e.to_string()))?;
        let mut names = Vec::new();
        for hex in by.iter().take(20) {
            names.push(invites.display_name(hex).await);
        }
        Ok(json!({ "member": s.member, "root": s.root, "certs": s.certs, "needed": s.needed, "reached_pct": s.reached_permille / 10, "mutual": s.mutual,
                   "certifiers": names, "web": { "members": web.0, "referees": web.1, "roots": web.2 } }))
    }
    async fn presence(&self, who: &Principal, body: Option<Value>) -> Result<Value, ApiError> {
        let p = self.presence.as_ref().ok_or_else(|| ApiError::unavailable("la présence visio n'est pas disponible sur cette station"))?;
        if let Some(b) = body {
            if let Some(on) = b["reachable"].as_bool() {
                p.set_reachable(&who.hex, on).await.map_err(ApiError::unavailable)?;
            }
            if let Some(on) = b["in_call"].as_bool() {
                p.set_in_call(&who.hex, on).await;
            }
        }
        Ok(p.view(&who.hex).await)
    }
    async fn call_ice(&self, who: &Principal) -> Result<Value, ApiError> {
        // Les identifiants du relais sont fabriqués pour ce membre seul (l'API est authentifiée) ; ses amis N¹ reçoivent les leurs
        // dans l'offre d'appel.
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let turn = self.ice.1.as_ref().map(|s| crate::turn::credentials(s, &who.hex[..16.min(who.hex.len())], now, crate::turn::CREDENTIAL_TTL)).map(|(u, c)| json!({ "username": u, "credential": c }));
        Ok(json!({ "stun_port": self.ice.0, "turn": turn, "servers": self.ice.2 }))
    }
    async fn chat(&self, who: &Principal, op: ChatOp) -> Result<Value, ApiError> {
        let chat = self.chat.as_ref().ok_or_else(|| ApiError::unavailable("le chat privé n'est pas disponible sur cette station"))?;
        let o = who.hex.as_str();
        let out = match op {
            ChatOp::List => chat.conversations(o).await,
            ChatOp::Messages(peer, before) => chat.messages(o, &peer, before).await,
            ChatOp::Send { peer, text, reply, reply_at, files } => chat.send_full(o, &peer, &text, reply.as_deref(), reply_at, &files).await,
            ChatOp::Read(peer) => chat.mark_read(o, &peer).await,
            ChatOp::Clear(peer) => chat.clear(o, &peer).await,
            ChatOp::Delete(id) => chat.delete(o, &id).await,
            ChatOp::Search(q) => chat.search(o, &q).await,
            ChatOp::Group(b) => {
                let members: Vec<String> = b["members"].as_array().map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).take(40).collect()).unwrap_or_default();
                chat.create_group(o, b["name"].as_str().unwrap_or_default(), &members).await
            }
            ChatOp::Preview(url) => chat.preview(&url).await,
            ChatOp::PushKey => chat.push_key().await,
            ChatOp::React(b) => chat.react(o, b["id"].as_str().unwrap_or_default(), b["emoji"].as_str().unwrap_or_default()).await,
            ChatOp::Typing(peer) => chat.typing(o, &peer).await,
            ChatOp::Edit(b) => chat.edit(o, b["id"].as_str().unwrap_or_default(), b["text"].as_str().unwrap_or_default()).await,
            ChatOp::GroupUpdate(b) => chat.group_update(o, b["id"].as_str().unwrap_or_default(), &b).await,
            ChatOp::Prefs(b) => chat.prefs(o, b["peer"].as_str().unwrap_or_default(), &b).await,
            ChatOp::Settings(b) => chat.settings(o, b.as_ref()).await,
            ChatOp::Push(b) => chat.push_subscribe(o, b["endpoint"].as_str().unwrap_or_default(), b["on"].as_bool().unwrap_or(true)).await,
            ChatOp::Signal(sig) => chat.signal(o, sig["peer"].as_str().unwrap_or_default(), &sig).await,
        };
        out.map_err(|e| match e {
            crate::chat::ChatError::Invalid(m) => ApiError::bad_request(m),
            crate::chat::ChatError::NoKey => ApiError::unavailable("clé du compte indisponible sur cette station : envoi impossible"),
            crate::chat::ChatError::Unreachable(m) => ApiError::unavailable(m),
            crate::chat::ChatError::Db(m) => {
                eprintln!("interface : chat : {m}");
                ApiError::internal("opération impossible : voir les journaux de la station")
            }
        })
    }
    async fn ai_params(&self, who: &Principal) -> Result<Value, ApiError> {
        let ai = self.ai.as_ref().ok_or_else(|| ApiError::unavailable("paramètres IA non disponibles sur cette station"))?;
        ai.view_for(&who.hex, who.captain).await.map_err(ai_error)
    }
    async fn station_ai(&self, who: &Principal) -> Result<Value, ApiError> {
        let ai = self.ai.as_ref().ok_or_else(|| ApiError::unavailable("paramètres IA non disponibles sur cette station"))?;
        ai.station_view(&who.hex).await.map_err(ai_error)
    }
    async fn set_station_ai(&self, who: &Principal, body: Value) -> Result<Value, ApiError> {
        let ai = self.ai.as_ref().ok_or_else(|| ApiError::unavailable("paramètres IA non disponibles sur cette station"))?;
        ai.set_station(&who.hex, &body).await.map_err(ai_error)
    }
    async fn test_ai(&self, who: &Principal, family: String) -> Result<Value, ApiError> {
        let ai = self.ai.as_ref().ok_or_else(|| ApiError::unavailable("paramètres IA non disponibles sur cette station"))?;
        ai.test(&who.hex, who.captain, &family).await.map_err(ai_error)
    }
    async fn set_ai_params(&self, who: &Principal, patch: Value) -> Result<Value, ApiError> {
        let ai = self.ai.as_ref().ok_or_else(|| ApiError::unavailable("paramètres IA non disponibles sur cette station"))?;
        ai.set(&who.hex, &patch).await.map(|p| p.view()).map_err(ai_error)
    }
    async fn reading(&self, who: &Principal, op: ReadingOp) -> Result<Value, ApiError> {
        let r = self.reading.as_ref().ok_or_else(|| ApiError::unavailable("lecture des documents non disponible sur cette station"))?;
        match op {
            ReadingOp::View => r.view(&who.hex).await.map_err(db_err),
            ReadingOp::Offers => r.offers().await.map_err(ApiError::unavailable),
            ReadingOp::Accept { peers, tasks } => r.accept(&who.hex, &peers, &tasks).await.map_err(ApiError::bad_request),
            ReadingOp::Revoke(peer) => r.revoke(&who.hex, peer.as_deref()).await.map_err(ApiError::bad_request),
            ReadingOp::Pause(on) => r.pause(&who.hex, on).await.map_err(ApiError::internal),
            ReadingOp::Erase => {
                // 1. les calculs (et la pause) ; 2. le paquet du uCloud ; 3. la station l'oublie. Une panne entre deux étapes
                // laisse au pire un paquet que la station ne relit pas : rien de ce qui a été effacé ne revient.
                r.erase(&who.hex).await.map_err(ApiError::internal)?;
                // Toutes les parts d'un paquet découpé (`index.astroidx`, puis `index-N.astroidx`).
                for i in 0..crate::search_pack::MAX_PARTS {
                    let (o, p) = (who.hex.clone(), format!("{}/{}", crate::search_pack::PACK_DIR, crate::search_pack::part_name(i)));
                    match astro_dav::vfs::vfs_tx(&self.dav.db, move |tx| astro_dav::vfs::delete_tree_tx(tx, &o, &p)).await {
                        Ok(cids) => astro_dav::server::release_blobs(&self.dav, cids).await,
                        Err(astro_dav::vfs::VfsError::NotFound) => {}
                        Err(e) => return Err(ApiError::internal(format!("paquet d'index : {e}"))),
                    }
                }
                r.forget_pack(&who.hex).await.map_err(ApiError::internal)?;
                r.view(&who.hex).await.map_err(db_err)
            }
        }
    }
    async fn contacts(&self, who: &Principal) -> Result<Value, ApiError> {
        let links = self.contacts_service()?;
        let mut view = links.list(&who.hex).await.map_err(link_error)?;
        let login = crate::account_login(&self.services.db, &who.hex).await;
        let base = self.dav.config.public_base_url.clone().unwrap_or_default();
        view["carddav"] = json!({ "url": format!("{}{}/addressbooks/{login}/contacts/", base.trim_end_matches('/'), self.dav.config.mount.trim_end_matches('/')), "server": format!("{}{}/", base.trim_end_matches('/'), self.dav.config.mount.trim_end_matches('/')), "login": login });
        Ok(view)
    }
    async fn contacts_sync(&self, who: &Principal) -> Result<Value, ApiError> {
        let r = self.contacts_service()?.sync_owner(&who.hex).await.map_err(link_error)?;
        Ok(json!({ "indexed": r.indexed, "removed": r.removed, "linked": r.linked.iter().map(|(n, _)| n).collect::<Vec<_>>() }))
    }
    async fn contacts_create(&self, who: &Principal, name: String, email: Option<String>) -> Result<Value, ApiError> {
        self.contacts_service()?.create_from_person(&who.hex, &name, email.as_deref(), false).await.map_err(link_error)
    }
    async fn contacts_link(&self, who: &Principal, path: String, target: Option<String>) -> Result<Value, ApiError> {
        self.contacts_service()?.link(&who.hex, &path, target.as_deref()).await.map_err(link_error)
    }
    /// BRO discute : une commande `#…` comme avant ; sinon, si une IA est attribuée au membre, sa réponse (routage hybride,
    /// quota, outils en lecture seule : `crate::bro_chat`) ; sinon la console de mémoire.
    async fn bro_ask(&self, who: &Principal, ask: astro_ui::api::BroAsk) -> Result<Value, ApiError> {
        let Some(ai) = self.ai.as_ref().filter(|_| bus::parse(&ask.text).is_none()) else { return self.bro_say(who, ask.text).await };
        if !ai.available(&who.hex, who.captain).await {
            return self.bro_say(who, ask.text).await;
        }
        let tools = BroTools { services: self.services.clone(), hex: who.hex.clone(), memory: self.memory.clone(), seer: self.seer.clone() };
        let q = crate::bro_chat::Ask { text: ask.text, history: ask.history, private: ask.private, allow_cloud: ask.allow_cloud };
        crate::bro_chat::ask(ai, &who.hex, who.captain, q, &tools, None).await.map_err(ai_error)
    }
    /// BRO en flux : la même réponse que [`Self::bro_ask`], envoyée au fil de l'eau par une tâche (les morceaux, puis `done`).
    async fn bro_stream(&self, who: &Principal, ask: astro_ui::api::BroAsk) -> Result<tokio::sync::mpsc::UnboundedReceiver<Value>, ApiError> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let ai = match self.ai.clone() {
            Some(ai) if bus::parse(&ask.text).is_none() && ai.available(&who.hex, who.captain).await => ai,
            _ => {
                let mut done = self.bro_ask(who, ask).await?;
                done["type"] = Value::from("done");
                let _ = tx.send(done);
                return Ok(rx);
            }
        };
        let (hex, captain, services, memory, seer) = (who.hex.clone(), who.captain, self.services.clone(), self.memory.clone(), self.seer.clone());
        tokio::spawn(async move {
            let tools = BroTools { services, hex: hex.clone(), memory, seer };
            let q = crate::bro_chat::Ask { text: ask.text, history: ask.history, private: ask.private, allow_cloud: ask.allow_cloud };
            let sink = |v: Value| {
                let _ = tx.send(v);
            };
            let end = match crate::bro_chat::ask(&ai, &hex, captain, q, &tools, Some(&sink)).await {
                Ok(mut v) => {
                    v["type"] = Value::from("done");
                    v
                }
                Err(e) => json!({ "type": "error", "error": match e { crate::member_ai::AiError::Invalid(m) => m, crate::member_ai::AiError::Internal(_) => "BRO est indisponible : voir les journaux de la station".to_owned() } }),
            };
            let _ = tx.send(end);
        });
        Ok(rx)
    }
    /// Console de BRO : `#commande` exécute la commande (comme par message privé) ; un autre texte est noté dans le
    /// journal (emplacement 15) après avoir cherché les souvenirs proches. Pas de génération de réponse par un LLM.
    async fn bro_say(&self, who: &Principal, text: String) -> Result<Value, ApiError> {
        if let Some(command) = bus::parse(&text) {
            let out = bus::execute(self.services.as_ref(), &command, &who.hex).await;
            return Ok(json!({ "reply": out.unwrap_or_else(|e| format!("⚠️ {e}")) }));
        }
        let Some(memory) = &self.memory else { return Err(ApiError::unavailable("La mémoire de BRO n'est pas disponible : cette station n'a pas de moteur d'IA (Ollama).")) };
        let related = memory.recall(&who.hex, &text, 3, None).await.map_err(|e| ApiError::unavailable(format!("mémoire indisponible : {e}")))?;
        let meta = Meta { source: "console".into(), ..Meta::default() };
        let mut reply = match memory.remember(&who.hex, Slot::Journal, &text, meta).await {
            Ok(r) if r.mem_id.is_some() => "📝 Noté dans votre journal.".to_owned(),
            Ok(_) => "📝 Je le savais déjà.".to_owned(),
            Err(e @ astro_bro::BroError::Unstorable(_)) => format!("🔒 {e} : je ne le garde pas."),
            Err(e) => return Err(ApiError::unavailable(format!("mémoire indisponible : {e}"))),
        };
        if !related.is_empty() {
            reply.push_str("\nJe me souviens que :");
            for r in related {
                reply.push_str(&format!("\n• {}", r.memory.content));
            }
        }
        Ok(json!({ "reply": reply }))
    }
}
