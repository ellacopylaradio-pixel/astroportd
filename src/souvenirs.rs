//! Souvenirs qui reviennent : voyages repérés tout seuls et page d'une personne reconnue. Lecture seule, calculée à la demande
//! sur la base (`ucloud_entries`, `faces`) d'un seul compte : rien ne quitte la station, rien n'est comparé entre comptes.
//!
//! **Voyage** : des photos datées et situées, prises loin de « chez soi » (> [`AWAY_KM`]) plusieurs jours de suite (un jour sans
//! photo est toléré). « Chez soi » est la position déclarée par le MULTIPASS (`identities.lat/lon`) ; sans elle, la case de
//! 0,5° où le membre a photographié le plus de **jours** différents (pas le plus de photos : un mariage ne déplace pas la maison). Le nom vient du dossier où le membre a rangé la
//! plupart de ses photos (« Bretagne 2024 »), sauf s'il est générique (DCIM, Camera, une année…) ; sinon l'écran dit les dates.
//!
//! **Personne** : ses photos (visages nommés), la première et la dernière, combien par année, avec qui elle apparaît le plus,
//! les voyages où elle était.

use std::collections::{HashMap, HashSet};

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

/// Distance à partir de laquelle on est « en voyage » (km).
pub const AWAY_KM: f64 = 80.0;
/// Photos au moins pour parler de voyage.
const MIN_PHOTOS: usize = 6;
/// Photos lues au plus (les plus récentes) : borne la mémoire.
const MAX_ROWS: i64 = 200_000;
/// Vignettes par voyage.
const COVERS: usize = 6;

/// Une photo située.
#[derive(Clone)]
struct Spot {
    path: String,
    t: i64,
    lat: f64,
    lon: f64,
    thumb: Option<String>,
    mime: Option<String>,
}

fn km((a, b): (f64, f64), (c, d): (f64, f64)) -> f64 {
    let (p1, p2, dl, dg) = (a.to_radians(), c.to_radians(), (c - a).to_radians(), (d - b).to_radians());
    let h = (dl / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dg / 2.0).sin().powi(2);
    12_742.0 * h.sqrt().min(1.0).asin()
}

fn spots(c: &Connection, owner: &str) -> rusqlite::Result<Vec<Spot>> {
    let sql = format!(
        "SELECT e.path, e.taken_at, e.lat, e.lon, json_extract(e.data, '$.thumb'), e.mime FROM ucloud_entries e \
         WHERE {} AND e.taken_at IS NOT NULL AND e.lat IS NOT NULL AND e.lon IS NOT NULL AND (e.mime LIKE 'image/%' OR e.mime LIKE 'video/%') \
         ORDER BY e.taken_at DESC LIMIT ?2",
        crate::library::FILES
    );
    let mut s = c.prepare_cached(&sql)?;
    let mut out: Vec<Spot> = s
        .query_map(params![owner, MAX_ROWS], |r| Ok(Spot { path: r.get(0)?, t: r.get(1)?, lat: r.get(2)?, lon: r.get(3)?, thumb: r.get(4)?, mime: r.get(5)? }))?
        .collect::<Result<_, _>>()?;
    out.reverse();
    Ok(out)
}

/// « Chez soi » : la position déclarée par le MULTIPASS, sinon celle déduite des photos ([`home_from_photos`]).
fn home_of(c: &Connection, owner: &str, spots: &[Spot]) -> rusqlite::Result<Option<((f64, f64), &'static str)>> {
    let declared: Option<(f64, f64)> = c
        .query_row("SELECT lat, lon FROM identities WHERE hex = ?1 AND hosted = 1 AND lat IS NOT NULL AND lon IS NOT NULL", [owner], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    Ok(declared.map(|h| (h, "multipass")).or_else(|| home_from_photos(spots).map(|h| (h, "photos"))))
}

/// Centre de la case de 0,5° photographiée le plus de jours différents.
fn home_from_photos(spots: &[Spot]) -> Option<(f64, f64)> {
    let mut days: HashMap<(i64, i64), HashSet<i64>> = HashMap::new();
    for s in spots {
        days.entry(((s.lat * 2.0).floor() as i64, (s.lon * 2.0).floor() as i64)).or_default().insert(s.t.div_euclid(86_400));
    }
    let (cell, _) = days.into_iter().max_by_key(|(k, d)| (d.len(), std::cmp::Reverse(*k)))?;
    Some(((cell.0 as f64 + 0.5) / 2.0, (cell.1 as f64 + 0.5) / 2.0))
}

/// Dossier qui nomme un voyage : le plus fréquent parmi ses photos, s'il dit quelque chose.
fn folder_name(paths: &[&str]) -> Option<String> {
    let mut n: HashMap<&str, usize> = HashMap::new();
    for p in paths {
        let dir = p.rsplit_once('/').map_or("", |(d, _)| d);
        *n.entry(dir.rsplit('/').next().unwrap_or("")).or_default() += 1;
    }
    let (name, count) = n.into_iter().max_by_key(|(k, v)| (*v, std::cmp::Reverse(*k)))?;
    let folded = crate::search_query::fold_str(name);
    let generic = ["", "dcim", "camera", "photos", "images", "pictures", "videos", "vidéos", "100andro", "100apple", "screenshots", "whatsapp images", "imports", "partages", "telephone", "google photos", "icloud"];
    let looks_auto = folded.chars().all(|c| c.is_ascii_digit() || "-_ ".contains(c)) || folded.starts_with("photos from") || generic.contains(&folded.as_str()) || folded.ends_with("apple");
    (count * 2 >= paths.len() && !looks_auto).then(|| name.to_owned())
}

fn item(s: &Spot) -> Value {
    let thumb = s.thumb.as_deref().and_then(|j| serde_json::from_str::<Value>(j).ok()).map_or(Value::Null, |v| json!({ "w": v["w"], "h": v["h"], "ow": v["ow"], "oh": v["oh"] }));
    json!({ "path": s.path, "mime": s.mime, "taken_at": s.t, "t": s.t, "thumb": thumb, "lat": s.lat, "lon": s.lon })
}

/// Personnes nommées sur un ensemble de photos, les plus présentes d'abord.
fn people_on(c: &Connection, owner: &str, paths: &[&str], except: Option<&str>, max: usize) -> rusqlite::Result<Vec<Value>> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let mut n: HashMap<String, HashSet<String>> = HashMap::new();
    let mut s = c.prepare_cached("SELECT name FROM faces WHERE owner_hex = ?1 AND ucloud_path = ?2 AND name IS NOT NULL AND name NOT LIKE 'Inconnu\\_%' ESCAPE '\\'")?;
    for p in paths {
        for name in s.query_map(params![owner, p], |r| r.get::<_, String>(0))? {
            let name = name?;
            if Some(name.as_str()) != except {
                n.entry(name).or_default().insert((*p).to_owned());
            }
        }
    }
    let mut v: Vec<(String, usize)> = n.into_iter().map(|(k, s)| (k, s.len())).collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    Ok(v.into_iter().take(max).map(|(name, photos)| json!({ "name": name, "photos": photos })).collect())
}

/// Un voyage repéré (indices dans la liste des photos).
struct Trip {
    from: usize,
    to: usize,
}

fn find_trips(spots: &[Spot], home: (f64, f64)) -> Vec<Trip> {
    let mut trips = Vec::new();
    let mut cur: Option<(usize, usize, i64, HashSet<i64>)> = None; // début, fin, dernier jour, jours
    let close = |cur: Option<(usize, usize, i64, HashSet<i64>)>, trips: &mut Vec<Trip>| {
        if let Some((a, b, _, days)) = cur
            && days.len() >= 2
            && b + 1 - a >= MIN_PHOTOS
        {
            trips.push(Trip { from: a, to: b });
        }
    };
    for (i, s) in spots.iter().enumerate() {
        if km((s.lat, s.lon), home) < AWAY_KM {
            // De retour : une photo à la maison ferme le voyage seulement si elle est d'un autre jour que le dernier.
            if cur.as_ref().is_some_and(|c| s.t.div_euclid(86_400) > c.2) {
                close(cur.take(), &mut trips);
            }
            continue;
        }
        let day = s.t.div_euclid(86_400);
        match &mut cur {
            Some((_, b, last, days)) if day - *last <= 2 => {
                *b = i;
                *last = day;
                days.insert(day);
            }
            _ => {
                close(cur.take(), &mut trips);
                cur = Some((i, i, day, HashSet::from([day])));
            }
        }
    }
    close(cur, &mut trips);
    trips
}

fn trip_view(c: &Connection, owner: &str, spots: &[Spot], tr: &Trip, home: (f64, f64)) -> rusqlite::Result<Value> {
    let all: Vec<&Spot> = spots[tr.from..=tr.to].iter().filter(|s| km((s.lat, s.lon), home) >= AWAY_KM).collect();
    let paths: Vec<&str> = all.iter().map(|s| s.path.as_str()).collect();
    let (mut lats, mut lons): (Vec<f64>, Vec<f64>) = all.iter().map(|s| (s.lat, s.lon)).unzip();
    lats.sort_by(f64::total_cmp);
    lons.sort_by(f64::total_cmp);
    let mid = (lats[lats.len() / 2], lons[lons.len() / 2]);
    let step = (all.len() / COVERS).max(1);
    let covers: Vec<Value> = all.iter().step_by(step).take(COVERS).map(|s| item(s)).collect();
    let days: HashSet<i64> = all.iter().map(|s| s.t.div_euclid(86_400)).collect();
    Ok(json!({
        "from": all[0].t, "to": all[all.len() - 1].t, "days": days.len(), "photos": all.len(), "name": folder_name(&paths),
        "lat": mid.0, "lon": mid.1, "km": km(mid, home).round() as i64, "covers": covers, "people": people_on(c, owner, &paths, None, 4)?,
        "bbox": [lats[0], lons[0], lats[lats.len() - 1], lons[lons.len() - 1]],
    }))
}

/// Les voyages du membre, du plus récent au plus ancien (`max` au plus).
pub fn trips(c: &Connection, owner: &str, max: usize) -> rusqlite::Result<Value> {
    let spots = spots(c, owner)?;
    let Some((home, source)) = home_of(c, owner, &spots)? else { return Ok(json!({ "trips": [], "home": null })) };
    let mut out = Vec::new();
    for tr in find_trips(&spots, home).iter().rev().take(max) {
        out.push(trip_view(c, owner, &spots, tr, home)?);
    }
    Ok(json!({ "trips": out, "home": [home.0, home.1], "home_source": source }))
}

/// Page d'une personne reconnue (nom donné par le membre).
/// `uid` : fiche du carnet de la personne ; ses visages y sont rattachés même si leur nom diffère de `name`.
pub fn person(c: &Connection, owner: &str, name: &str, uid: Option<&str>) -> rusqlite::Result<Value> {
    let sql = format!(
        "SELECT DISTINCT e.path, COALESCE(e.taken_at, e.mtime) AS t, e.lat, e.lon, json_extract(e.data, '$.thumb'), e.mime FROM ucloud_entries e \
         JOIN faces f ON f.owner_hex = e.owner_hex AND f.ucloud_path = e.path WHERE (f.name = ?2 OR (?3 IS NOT NULL AND f.contact_uid = ?3)) AND {}",
        crate::library::FILES
    );
    let mut s = c.prepare_cached(&format!("{sql} ORDER BY t DESC LIMIT ?4"))?;
    let rows: Vec<Spot> = s
        .query_map(params![owner, name, uid, MAX_ROWS], |r| {
            Ok(Spot { path: r.get(0)?, t: r.get::<_, Option<i64>>(1)?.unwrap_or(0), lat: r.get::<_, Option<f64>>(2)?.unwrap_or(f64::NAN), lon: r.get::<_, Option<f64>>(3)?.unwrap_or(f64::NAN), thumb: r.get(4)?, mime: r.get(5)? })
        })?
        .collect::<Result<_, _>>()?;
    if rows.is_empty() {
        return Ok(json!({ "name": name, "photos": 0, "items": [], "years": [], "with": [], "trips": [] }));
    }
    let mut ys = c.prepare_cached(&format!("SELECT CAST(strftime('%Y', t, 'unixepoch') AS INTEGER) y, count(*) FROM ({sql}) GROUP BY y ORDER BY y"))?;
    let years: Vec<Value> = ys.query_map(params![owner, name, uid], |r| Ok(json!({ "year": r.get::<_, i64>(0)?, "photos": r.get::<_, i64>(1)? })))?.collect::<Result<_, _>>()?;
    let paths: Vec<&str> = rows.iter().map(|r| r.path.as_str()).collect();
    let mine: HashSet<&str> = paths.iter().copied().collect();
    // Voyages où la personne apparaît.
    let all = spots(c, owner)?;
    let mut trips = Vec::new();
    if let Some((home, _)) = home_of(c, owner, &all)? {
        for tr in find_trips(&all, home).iter().rev() {
            if all[tr.from..=tr.to].iter().any(|s| mine.contains(s.path.as_str())) {
                trips.push(trip_view(c, owner, &all, tr, home)?);
            }
            if trips.len() >= 6 {
                break;
            }
        }
    }
    Ok(json!({
        "name": name, "photos": rows.len(), "first": rows[rows.len() - 1].t, "last": rows[0].t,
        "items": rows.iter().take(36).map(item).collect::<Vec<_>>(), "years": years,
        "with": people_on(c, owner, &paths[..paths.len().min(2000)], Some(name), 6)?, "trips": trips,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: &str = "aa";

    fn put(c: &Connection, path: &str, t: i64, lat: f64, lon: f64) {
        c.execute(
            "INSERT INTO ucloud_entries(owner_hex, path, parent, type, cid, mime, size, mtime, taken_at, lat, lon, data) VALUES (?1, ?2, '/', 'file', ?3, 'image/jpeg', 1, ?4, ?4, ?5, ?6, '{}')",
            params![ME, path, format!("cid-{path}"), t, lat, lon],
        )
        .unwrap();
    }
    fn face(c: &Connection, path: &str, name: &str) {
        c.execute("INSERT INTO faces(owner_hex, name, embed_hash, ts, ucloud_path) VALUES (?1, ?2, ?3, 0, ?4)", params![ME, name, format!("{name}{path}"), path]).unwrap();
    }

    const DAY: i64 = 86_400;
    const T0: i64 = 1_720_000_000; // juillet 2024

    #[test]
    fn a_trip_is_several_days_far_from_home_and_takes_its_folder_name() {
        let c = astro_db::open_in_memory().unwrap();
        // Chez soi : Toulouse, 30 jours de photos.
        for d in 0..30 {
            put(&c, &format!("/Photos/2024/t{d}.jpg"), T0 + d * DAY, 43.6, 1.44);
        }
        // Un mariage à Toulouse : beaucoup de photos un seul jour, ne déplace pas la maison.
        for i in 0..50 {
            put(&c, &format!("/Photos/Mariage/m{i}.jpg"), T0 + 40 * DAY + i, 43.61, 1.45);
        }
        // Bretagne : 4 jours (un jour sans photo), 8 photos, rangées dans « Bretagne 2024 ».
        for (i, d) in [50, 51, 53, 53, 54, 54, 54, 50].iter().enumerate() {
            put(&c, &format!("/Photos/Bretagne 2024/b{i}.jpg"), T0 + d * DAY + i as i64, 48.39, -4.49);
        }
        face(&c, "/Photos/Bretagne 2024/b1.jpg", "Léa");
        face(&c, "/Photos/Bretagne 2024/b2.jpg", "Léa");
        face(&c, "/Photos/Bretagne 2024/b2.jpg", "Paul");
        // Une journée à Paris : un seul jour, pas un voyage.
        for i in 0..10 {
            put(&c, &format!("/DCIM/p{i}.jpg"), T0 + 60 * DAY + i, 48.85, 2.35);
        }
        let r = trips(&c, ME, 10).unwrap();
        let t = r["trips"].as_array().unwrap();
        assert_eq!(t.len(), 1, "{r}");
        assert_eq!((t[0]["name"].as_str(), t[0]["photos"].as_i64(), t[0]["days"].as_i64()), (Some("Bretagne 2024"), Some(8), Some(4)));
        assert!(t[0]["km"].as_i64().unwrap() > 600, "{}", t[0]["km"]);
        assert_eq!(t[0]["people"][0]["name"], "Léa");
        assert_eq!(t[0]["covers"].as_array().unwrap().len(), COVERS);

        let p = person(&c, ME, "Léa", None).unwrap();
        assert_eq!((p["photos"].as_i64(), p["trips"].as_array().unwrap().len()), (Some(2), 1));
        assert_eq!(p["with"][0]["name"], "Paul");
        assert_eq!(p["years"][0]["year"], 2024);
        assert_eq!(person(&c, "autre", "Léa", None).unwrap()["photos"], 0, "les visages d'un autre compte restent les siens");
        // Un visage nommé autrement mais rattaché à la fiche de Léa compte pour elle.
        c.execute("INSERT INTO faces(owner_hex, name, embed_hash, ts, ucloud_path, contact_uid) VALUES (?1, 'Léa M.', 'x1', 0, '/Photos/Bretagne 2024/b3.jpg', 'uid-lea')", [ME]).unwrap();
        assert_eq!(person(&c, ME, "Léa", Some("uid-lea")).unwrap()["photos"], 3);
        assert_eq!(person(&c, ME, "Léa", None).unwrap()["photos"], 2);
    }

    #[test]
    fn home_is_where_the_multipass_says_it_is() {
        let c = astro_db::open_in_memory().unwrap();
        // Beaucoup de jours à Paris, mais le MULTIPASS déclare Toulouse : Paris devient un voyage.
        for d in 0..10 {
            put(&c, &format!("/DCIM/p{d}.jpg"), T0 + d * DAY, 48.85, 2.35);
        }
        assert_eq!(trips(&c, ME, 10).unwrap()["trips"].as_array().unwrap().len(), 0, "sans position déclarée : Paris est chez soi");
        c.execute("INSERT INTO identities(hex, role, hosted, lat, lon) VALUES (?1, 'multipass', 1, 43.6, 1.44)", [ME]).unwrap();
        let r = trips(&c, ME, 10).unwrap();
        assert_eq!((r["home_source"].as_str(), r["trips"].as_array().unwrap().len()), (Some("multipass"), 1), "{r}");
    }

    #[test]
    fn generic_folders_do_not_name_a_trip() {
        assert_eq!(folder_name(&["/DCIM/Camera/a.jpg", "/DCIM/Camera/b.jpg"]), None);
        assert_eq!(folder_name(&["/Photos/2024-08/a.jpg"]), None);
        assert_eq!(folder_name(&["/Voyages/Rome/a.jpg", "/Voyages/Rome/b.jpg", "/DCIM/c.jpg"]).as_deref(), Some("Rome"));
    }
}
