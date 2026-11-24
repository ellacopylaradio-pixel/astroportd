//! **BRO discute** (lot 3 de `docs/design/IA.md`) : une réponse de l'IA attribuée au membre ([`crate::member_ai::Route`]),
//! avec le **routage hybride** — une conversation qui transporte une donnée privée reste en uCloud — le quota du Capitaine,
//! un limiteur de débit (une clé partagée a un plafond par minute chez le fournisseur) et des outils en lecture seule.
//!
//! Classement : une demande est **privée** dès qu'un outil a rendu une donnée du membre dans la conversation (le client
//! renvoie ce drapeau, collant jusqu'à la fin) ; les outils ne sont offerts qu'à une IA uCloud, ou au cloud avec l'accord
//! explicite du membre pour cette fois (`allow_cloud`). Une demande ouverte et lourde (long texte, code) va d'abord au cloud.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::sync::Mutex;

use serde_json::{Value, json};

use crate::ai_client::Family;
use crate::member_ai::{AiError, MemberAi, Route};

/// Tours de conversation au plus pour une question (appels d'outils compris).
const MAX_ROUNDS: usize = 4;
/// Caractères gardés d'un résultat d'outil.
const MAX_TOOL_CHARS: usize = 4000;
/// Au-delà, une demande ouverte est « lourde » : le cloud d'abord.
const HEAVY_CHARS: usize = 12_000;
/// Messages d'historique gardés.
const MAX_HISTORY: usize = 20;
/// Jetons de réponse au plus (un modèle qui « réfléchit » en dépense une partie avant de répondre).
const MAX_TOKENS: u32 = 2000;
/// Requêtes par minute d'un membre sur l'IA prêtée par le Capitaine (part équitable d'une clé partagée).
const MEMBER_PER_MINUTE: usize = 12;


const SYSTEM: &str = "Tu es BRO, l'assistant de la station Astroport du membre. Réponds en français, simplement et brièvement. \
Pour ses fichiers, photos, documents ou son solde, utilise les outils quand ils sont proposés ; n'invente jamais un fichier, une date ni un montant. \
Ne demande jamais de mot de passe ni de clé.";

/// Outils en lecture seule offerts au modèle (format OpenAI). Tous rendent une donnée privée.
fn tool_specs() -> Vec<Value> {
    vec![
        json!({ "type": "function", "function": { "name": "chercher", "description": "Cherche dans les fichiers, photos et documents du membre : mots, personnes reconnues, villes, mois, années (ex. « Léa Toulouse 2025 », « facture électricité »). Rend chemins, dates et extraits.", "parameters": { "type": "object", "properties": { "requete": { "type": "string" } }, "required": ["requete"] } } }),
        json!({ "type": "function", "function": { "name": "solde", "description": "Solde du membre en Ẑen.", "parameters": { "type": "object", "properties": {} } } }),
    ]
}

/// Exécute les outils pour un membre (lecture seule).
pub trait Tools: Send + Sync {
    /// Résultat de l'outil `name` avec `args`, ou l'erreur à dire au modèle.
    fn run(&self, name: &str, args: &Value) -> impl Future<Output = Result<Value, String>> + Send;
}

/// Une question à BRO.
#[derive(Debug, Clone, Default)]
pub struct Ask {
    /// Le message du membre.
    pub text: String,
    /// Messages précédents `(rôle, texte)`, `user` ou `assistant`.
    pub history: Vec<(String, String)>,
    /// La conversation a déjà transporté une donnée privée.
    pub private: bool,
    /// Le membre accepte, pour cette fois, qu'une conversation privée parte vers une IA cloud.
    pub allow_cloud: bool,
}

/// Où envoyer la réponse au fil de l'eau : `{ type: "turn" }` au début de chaque tour (le texte d'avant est remplacé),
/// `{ type: "delta", text }`, `{ type: "tool", name }`. `None` : réponse d'un bloc.
pub type Sink<'a> = Option<&'a (dyn Fn(Value) + Send + Sync)>;

/// Fenêtres glissantes d'une minute (`clé → instants des requêtes`).
#[derive(Default)]
pub struct Limiter {
    seen: Mutex<HashMap<String, VecDeque<u64>>>,
}

impl Limiter {
    /// Réserve une requête sous `key` (au plus `max` par minute) ; sinon, secondes à attendre.
    fn take(&self, key: &str, max: usize, now: u64) -> Result<(), u64> {
        let mut map = self.seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let q = map.entry(key.to_owned()).or_default();
        while q.front().is_some_and(|t| now.saturating_sub(*t) >= 60) {
            q.pop_front();
        }
        if q.len() >= max {
            return Err(q.front().map_or(1, |t| 60u64.saturating_sub(now.saturating_sub(*t)).max(1)));
        }
        q.push_back(now);
        Ok(())
    }
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Ordre d'essai des routes : privée → uCloud seulement (le cloud avec accord) ; ouverte → uCloud d'abord, sauf lourde.
fn order(routes: Vec<Route>, private: bool, allow_cloud: bool, heavy: bool) -> Vec<Route> {
    let (mut local, cloud): (Vec<Route>, Vec<Route>) = routes.into_iter().partition(|r| r.family == Family::UCloud);
    // La sienne avant celle de la station : elle ne compte pas dans le quota du Capitaine.
    local.sort_by_key(|r| r.source != "own");
    let mut cloud = cloud;
    cloud.sort_by_key(|r| r.source != "own");
    match (private, allow_cloud, heavy) {
        (true, false, _) => local,
        (true, true, _) | (false, _, false) => local.into_iter().chain(cloud).collect(),
        (false, _, true) => cloud.into_iter().chain(local).collect(),
    }
}

/// Une réponse de BRO. `Ok` porte toujours une réponse à montrer : un refus (privée sans uCloud, quota, débit) en est une.
///
/// # Errors
/// Base ou clés indisponibles.
pub async fn ask<T: Tools>(ai: &MemberAi, owner: &str, captain: bool, q: Ask, tools: &T, sink: Sink<'_>) -> Result<Value, AiError> {
    let (rights, limiter) = (ai.rights().map(std::sync::Arc::as_ref), ai.limiter());
    let text: String = q.text.chars().filter(|c| !c.is_control() || *c == '\n').take(8000).collect();
    let history: Vec<(String, String)> = q.history.into_iter().filter(|(r, _)| r == "user" || r == "assistant").rev().take(MAX_HISTORY).collect::<Vec<_>>().into_iter().rev().map(|(r, c)| (r, c.chars().take(6000).collect())).collect();
    let size = text.len() + history.iter().map(|(_, c)| c.len()).sum::<usize>();
    let heavy = size > HEAVY_CHARS || text.contains("```");
    let routes = ai.routes(owner, captain).await?;
    if routes.is_empty() {
        return Ok(json!({ "refused": "none", "reply": "Aucune IA ne m'est attribuée pour vous : voyez Moi › Appareils, ou demandez à votre Capitaine." }));
    }
    let cloud_label = routes.iter().find(|r| r.family == Family::Cloud).map(|r| r.label.clone());
    let candidates = order(routes, q.private, q.allow_cloud, heavy);
    if candidates.is_empty() {
        return Ok(json!({ "refused": "private", "cloud": cloud_label,
            "reply": "Cette conversation contient vos données privées, et votre station n'a pas d'IA qui tourne chez vous : je ne les envoie pas sur Internet sans votre accord." }));
    }

    // Quota du Capitaine (sauf avec sa propre clé cloud, qui ne coûte qu'au membre).
    let week = astro_core::week::iso_week(now());
    let (rights_now, used) = match rights {
        Some(r) => (r.of(owner).await.map(|x| x.0).ok(), r.used(owner, &week).await.map(|x| x.1).unwrap_or(0)),
        None => (None, 0),
    };
    let quota = rights_now.filter(|_| !captain).map_or(0, |r| r.quota_tokens_week);

    let mut last_error = String::new();
    for route in candidates {
        let counted = !(route.source == "own" && route.family == Family::Cloud);
        if counted && quota > 0 && used >= quota {
            last_error = format!("quota de la semaine atteint ({used} jetons sur {quota}) : il repart lundi");
            continue;
        }
        // Une fenêtre par clé : celle que prête le Capitaine est commune à la station, chaque clé de membre a la sienne.
        let t = now();
        let key = if route.source == "station" { format!("station:{}", route.endpoint.provider()) } else { format!("own:{owner}:{}", route.endpoint.provider()) };
        if let Err(wait) = limiter.take(&key, usize::try_from(route.rpm).unwrap_or(usize::MAX), t) {
            last_error = format!("{} est très demandé : réessayez dans {wait} s", route.label);
            continue;
        }
        if route.source == "station" && !captain {
            if let Err(wait) = limiter.take(&format!("m:{owner}"), MEMBER_PER_MINUTE, t) {
                return Ok(json!({ "refused": "rate", "reply": format!("Doucement : réessayez dans {wait} s.") }));
            }
        }
        let model = match route.endpoint.pick_model(route.model.as_deref()).await {
            Ok(m) => m,
            Err(e) => {
                last_error = format!("{} : {e}", route.label);
                continue;
            }
        };
        // Les outils rendent des données privées : seulement vers l'uCloud, ou le cloud avec l'accord du membre.
        let offer_tools = route.family == Family::UCloud || q.allow_cloud;
        let mut messages = vec![json!({ "role": "system", "content": SYSTEM })];
        messages.extend(history.iter().map(|(r, c)| json!({ "role": r, "content": c })));
        messages.push(json!({ "role": "user", "content": text }));
        if let Some(out) = sink {
            out(json!({ "type": "route", "via": route.label, "where": if route.family == Family::Cloud { "cloud" } else { "ucloud" }, "model": model }));
        }
        match converse(&route, &model, messages, offer_tools, tools, sink).await {
            Ok(done) => {
                if let Some(r) = rights.filter(|_| counted) {
                    let _ = r.record(owner, &week, done.tokens_in, done.tokens_out).await;
                }
                return Ok(json!({
                    "reply": done.text, "where": if route.family == Family::Cloud { "cloud" } else { "ucloud" }, "via": route.label, "source": route.source,
                    "model": model, "private": q.private || done.private, "tools": done.used, "tools_offered": offer_tools,
                    "tokens": done.tokens_in + done.tokens_out,
                }));
            }
            Err(e) => last_error = format!("{} : {e}", route.label),
        }
    }
    Ok(json!({ "refused": "failed", "reply": format!("Je n'ai pas pu répondre : {last_error}.") }))
}

struct Done {
    text: String,
    private: bool,
    used: Vec<String>,
    tokens_in: u64,
    tokens_out: u64,
}

/// La conversation avec une IA : jusqu'à [`MAX_ROUNDS`] tours, les outils demandés exécutés entre deux. Un modèle qui refuse
/// les outils est réessayé sans.
async fn converse<T: Tools>(route: &Route, model: &str, mut messages: Vec<Value>, offer_tools: bool, tools: &T, sink: Sink<'_>) -> Result<Done, String> {
    let specs = if offer_tools { tool_specs() } else { Vec::new() };
    let mut done = Done { text: String::new(), private: false, used: Vec::new(), tokens_in: 0, tokens_out: 0 };
    let mut with_tools = !specs.is_empty();
    for round in 0..MAX_ROUNDS {
        let last = round + 1 == MAX_ROUNDS;
        let offered: &[Value] = if with_tools && !last { &specs } else { &[] };
        let asked = match sink {
            Some(out) => {
                out(json!({ "type": "turn" }));
                let piece = |p: &str| out(json!({ "type": "delta", "text": p }));
                route.endpoint.turn_stream(model, &messages, offered, MAX_TOKENS, &piece).await
            }
            None => route.endpoint.turn(model, &messages, offered, MAX_TOKENS).await,
        };
        let turn = match asked {
            Ok(t) => t,
            Err(_) if !offered.is_empty() && round == 0 => {
                with_tools = false;
                continue;
            }
            Err(e) => return Err(e.to_string()),
        };
        done.tokens_in += turn.tokens_in;
        done.tokens_out += turn.tokens_out;
        if turn.calls.is_empty() {
            done.text = turn.text.unwrap_or_default();
            return Ok(done);
        }
        let calls: Vec<Value> = turn.calls.iter().map(|c| json!({ "id": c.id, "type": "function", "function": { "name": c.name, "arguments": c.args.to_string() } })).collect();
        messages.push(json!({ "role": "assistant", "content": turn.text, "tool_calls": calls }));
        for c in turn.calls.iter().take(3) {
            if let Some(out) = sink {
                out(json!({ "type": "tool", "name": c.name }));
            }
            let out = match tools.run(&c.name, &c.args).await {
                Ok(v) => v.to_string(),
                Err(e) => json!({ "erreur": e }).to_string(),
            };
            done.private = true;
            done.used.push(c.name.clone());
            messages.push(json!({ "role": "tool", "tool_call_id": c.id, "content": out.chars().take(MAX_TOOL_CHARS).collect::<String>() }));
        }
    }
    Err("trop d'allers-retours avec les outils".into())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use astro_crypto::StaticKeySource;
    use astro_db::Db;
    use astro_nostr_proto::pubkey_of;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use crate::ai_rights::AiRights;
    use crate::register::PublishFn;

    struct Fake;
    impl Tools for Fake {
        async fn run(&self, name: &str, args: &Value) -> Result<Value, String> {
            match name {
                "chercher" => Ok(json!({ "trouvés": [{ "chemin": "/Photos/Toulouse/lea.jpg", "date": "2025-07-14", "requete": args["requete"] }] })),
                _ => Err("outil inconnu".into()),
            }
        }
    }

    /// Faux Ollama (`/v1`) : la première réponse demande l'outil « chercher », la suivante répond en texte.
    async fn ollama() -> (String, Arc<Mutex<Vec<String>>>) {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (log, n) = (seen.clone(), Arc::new(AtomicUsize::new(0)));
        tokio::spawn(async move {
            while let Ok((mut s, _)) = l.accept().await {
                let (log, n) = (log.clone(), n.clone());
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 1 << 16];
                    let k = s.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..k]).to_string();
                    let body = if req.starts_with("GET /v1/models") {
                        json!({ "data": [{ "id": "llama3.2:3b" }] })
                    } else if n.fetch_add(1, Ordering::SeqCst) % 2 == 0 {
                        json!({ "choices": [{ "message": { "content": null, "tool_calls": [{ "id": "c1", "type": "function", "function": { "name": "chercher", "arguments": "{\"requete\":\"Léa Toulouse\"}" } }] } }], "usage": { "prompt_tokens": 100, "completion_tokens": 10 } })
                    } else {
                        json!({ "choices": [{ "message": { "content": "J'ai trouvé une photo de Léa à Toulouse, le 14 juillet 2025." } }], "usage": { "prompt_tokens": 150, "completion_tokens": 20 } })
                    }
                    .to_string();
                    log.lock().unwrap().push(req);
                    let _ = s.write_all(format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).as_bytes()).await;
                });
            }
        });
        (format!("http://{addr}"), seen)
    }

    #[tokio::test]
    async fn bro_answers_locally_with_a_tool_counts_tokens_and_stops_at_the_quota() {
        const ME: [u8; 32] = [0xA1; 32];
        let dir = std::env::temp_dir().join(format!("astroportd-brochat-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(Db::open(&dir.join("u.db"), 2).unwrap());
        let (url, seen) = ollama().await;
        let rights = Arc::new(AiRights::new(db.clone()));
        let publish: PublishFn = Arc::new(|_| Box::pin(async { Ok(()) }));
        let ai = MemberAi::new(db, Arc::new(StaticKeySource::new().with(ME)), publish).with_offers(Some(url.clone()), None).with_rights(rights.clone());
        let me = pubkey_of(&ME).unwrap();
        // Droit « station » par défaut et rien de prêté : aucune IA.
        let none = ask(&ai, &me, false, Ask { text: "Bonjour".into(), ..Ask::default() }, &Fake, None).await.unwrap();
        assert_eq!(none["refused"], "none");
        // Sa propre IA, l'Ollama de la station, avec un quota de 300 jetons.
        rights.set_member(&me, Some(&json!({ "mode": "own", "quota_tokens_week": 300 }))).await.unwrap();
        ai.set(&me, &json!({ "ollama_url": url })).await.unwrap();
        let r = ask(&ai, &me, false, Ask { text: "Retrouve les photos de Léa à Toulouse".into(), ..Ask::default() }, &Fake, None).await.unwrap();
        assert_eq!((r["where"].as_str(), r["via"].as_str(), r["private"].as_bool(), r["tools"][0].as_str()), (Some("ucloud"), Some("Ollama de cette station"), Some(true), Some("chercher")), "{r}");
        assert!(r["reply"].as_str().unwrap().contains("Toulouse"));
        assert_eq!(r["tokens"], 280);
        let reqs = seen.lock().unwrap().join("\n");
        assert!(reqs.contains("\"tools\"") && reqs.contains("lea.jpg"), "l'outil est offert, et son résultat renvoyé au modèle local");
        assert_eq!(rights.used(&me, &astro_core::week::iso_week(now())).await.unwrap(), (1, 280));
        // 280 < 300 : encore une ; ensuite le quota est atteint.
        ask(&ai, &me, false, Ask { text: "Et à Paris ?".into(), ..Ask::default() }, &Fake, None).await.unwrap();
        let stop = ask(&ai, &me, false, Ask { text: "Encore ?".into(), ..Ask::default() }, &Fake, None).await.unwrap();
        assert!(stop["reply"].as_str().unwrap().contains("quota de la semaine atteint"), "{stop}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_private_conversation_never_reaches_the_cloud_without_consent() {
        let route = |family, source: &'static str, label: &str| Route { family, source, label: label.into(), endpoint: crate::ai_client::Endpoint::local("http://127.0.0.1:1").unwrap(), model: None, rpm: 50 };
        let both = || vec![route(Family::Cloud, "station", "MixRoute"), route(Family::UCloud, "station", "Ollama")];
        let names = |v: Vec<Route>| v.into_iter().map(|r| r.label).collect::<Vec<_>>();
        assert_eq!(names(order(both(), true, false, false)), ["Ollama"], "privée : uCloud seulement");
        assert!(order(vec![route(Family::Cloud, "station", "MixRoute")], true, false, false).is_empty(), "privée sans uCloud : rien, BRO le dit");
        assert_eq!(names(order(both(), true, true, false)), ["Ollama", "MixRoute"], "avec accord : le cloud en secours");
        assert_eq!(names(order(both(), false, false, false)), ["Ollama", "MixRoute"], "ouverte : chez soi d'abord");
        assert_eq!(names(order(both(), false, false, true)), ["MixRoute", "Ollama"], "ouverte et lourde : le cloud d'abord");
        assert_eq!(names(order(vec![route(Family::UCloud, "station", "S"), route(Family::UCloud, "own", "O")], false, false, false)), ["O", "S"], "la sienne avant celle de la station");
    }

    #[test]
    fn a_minute_window_refuses_beyond_the_ceiling_then_frees() {
        let l = Limiter::default();
        for _ in 0..3 {
            assert!(l.take("k", 3, 1000).is_ok());
        }
        assert_eq!(l.take("k", 3, 1010), Err(50), "attendre que la plus ancienne sorte de la fenêtre");
        assert!(l.take("autre", 3, 1010).is_ok(), "une autre clé a sa fenêtre");
        assert!(l.take("k", 3, 1060).is_ok(), "une minute après : libre");
    }
}
