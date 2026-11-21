//! Répartition des données d'un membre, pour « Mon espace » : où sont ses octets (Coffre, partagé, Public, privé, tiroirs de la
//! station : index de recherche, visages, messages, mémoire, carnet et agenda), quelle part est à l'abri chez ses amis
//! (copies entières `backup_items` et paquets k-sur-n `backup_blobs`, encore vivants), et ce qu'il garde, lui, pour ses amis
//! (`backup_items` et fragments `backup_frags` dont il est l'hébergeur, face au volume accordé dans `backup_pacts`).
//! Lecture seule, en SQL, sur la base d'un seul compte.

use rusqlite::{Connection, params};
use serde_json::{Value, json};

/// Zones, dans l'ordre d'affichage : (identifiant, condition SQL sur `e`).
const ZONES: &[(&str, &str)] = &[
    ("coffre", "e.path >= '/_/Coffre/' AND e.path < '/_/Coffre0'"),
    ("souvenirs", "(e.path >= '/_/Visages/' AND e.path < '/_/Visages0') OR (e.path >= '/_/Messages/' AND e.path < '/_/Messages0') OR (e.path >= '/_/Memoire/' AND e.path < '/_/Memoire0')"),
    ("recherche", "e.path >= '/_/Recherche/' AND e.path < '/_/Recherche0'"),
    ("carnet", "(e.path >= '/_/Contacts/' AND e.path < '/_/Contacts0') OR (e.path >= '/_/Calendrier/' AND e.path < '/_/Calendrier0')"),
    ("public", "e.path = '/Public' OR (e.path >= '/Public/' AND e.path < '/Public0')"),
    ("partage", "e.cid IN (SELECT s.cid FROM ucloud_sharing s WHERE s.owner_hex = e.owner_hex)"),
];

/// Vue complète (voir l'en-tête du module).
pub fn spread(c: &Connection, owner: &str) -> rusqlite::Result<Value> {
    // Une zone par fichier, la première qui convient ; le reste est « privé ».
    let case = ZONES.iter().map(|(id, cond)| format!("WHEN {cond} THEN '{id}'")).collect::<Vec<_>>().join(" ");
    let sql = format!("SELECT CASE {case} ELSE 'prive' END z, COALESCE(SUM(e.size), 0), COUNT(*) FROM ucloud_entries e WHERE e.owner_hex = ?1 AND e.type = 'file' GROUP BY z");
    let mut st = c.prepare_cached(&sql)?;
    let mut zones = serde_json::Map::new();
    let mut total = 0i64;
    for row in st.query_map([owner], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?)))? {
        let (z, bytes, files) = row?;
        total += bytes;
        zones.insert(z, json!({ "bytes": bytes, "files": files }));
    }
    let published: i64 = c.query_row("SELECT COALESCE(SUM(size), 0) FROM public_blobs WHERE owner = ?1", [owner], |r| r.get(0))?;
    if published > 0 {
        let z = zones.entry("public").or_insert_with(|| json!({ "bytes": 0, "files": 0 }));
        z["bytes"] = json!(z["bytes"].as_i64().unwrap_or(0) + published);
        total += published;
    }
    // À l'abri : fichiers vivants dont une copie entière est acquittée chez un ami, ou rangés dans un paquet k-sur-n.
    let safe: i64 = c.query_row(
        "SELECT COALESCE(SUM(e.size), 0) FROM ucloud_entries e WHERE e.owner_hex = ?1 AND e.type = 'file' AND e.cid IS NOT NULL AND ( \
           EXISTS (SELECT 1 FROM backup_items i WHERE i.owner_hex = ?1 AND i.cid = e.cid AND i.missing_since IS NULL AND i.host_hex <> ?1) \
           OR EXISTS (SELECT 1 FROM backup_blobs b WHERE b.owner_hex = ?1 AND b.cid = e.cid AND b.gone_at IS NULL))",
        [owner],
        |r| r.get(0),
    )?;
    // Ce que mes gardiens gardent pour moi (copies entières + fragments acquittés), par gardien.
    let mut g = c.prepare_cached(
        "SELECT host_hex, SUM(n) FROM (SELECT host_hex, size n FROM backup_items WHERE owner_hex = ?1 AND host_hex <> ?1 \
         UNION ALL SELECT host_hex, size FROM backup_frags WHERE owner_hex = ?1 AND acked_at IS NOT NULL AND host_hex <> ?1) GROUP BY host_hex ORDER BY 2 DESC",
    )?;
    let guardians: Vec<Value> = g
        .query_map([owner], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|(hex, bytes)| Ok(json!({ "hex": hex, "name": crate::find_more::name_of(c, owner, &hex)?, "bytes": bytes })))
        .collect::<rusqlite::Result<_>>()?;
    // Ce que je garde pour mes amis, face au volume que je leur ai accordé.
    let mut h = c.prepare_cached(
        "SELECT o, SUM(n), (SELECT p.quota_bytes FROM backup_pacts p WHERE p.owner_hex = o AND p.host_hex = ?1 AND p.status = 'accepted') FROM ( \
           SELECT owner_hex o, size n FROM backup_items WHERE host_hex = ?1 AND owner_hex <> ?1 \
           UNION ALL SELECT owner_hex, size FROM backup_frags WHERE host_hex = ?1 AND owner_hex <> ?1) GROUP BY o ORDER BY 2 DESC",
    )?;
    let hosted: Vec<Value> = h
        .query_map(params![owner], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, Option<i64>>(2)?)))?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|(hex, bytes, quota)| Ok(json!({ "hex": hex, "name": crate::find_more::name_of(c, owner, &hex)?, "bytes": bytes, "quota": quota })))
        .collect::<rusqlite::Result<_>>()?;
    let hosted_total: i64 = hosted.iter().filter_map(|v| v["bytes"].as_i64()).sum();
    Ok(json!({ "total": total, "zones": zones, "safe": safe, "guardians": guardians, "hosted": hosted, "hosted_total": hosted_total }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_byte_has_one_zone_and_hosting_is_measured_against_the_granted_volume() {
        let c = astro_db::open_in_memory().unwrap();
        let (me, lea, paul) = ("aa".repeat(32), "bb".repeat(32), "cc".repeat(32));
        let put = |owner: &str, path: &str, cid: &str, size: i64| {
            c.execute("INSERT INTO ucloud_entries(owner_hex, path, parent, type, cid, size, mtime, data) VALUES (?1, ?2, '/', 'file', ?3, ?4, 0, '{}')", params![owner, path, cid, size]).unwrap();
        };
        put(&me, "/_/Coffre/passeports.pdf", "c1", 100);
        put(&me, "/_/Messages/chat.astroidx", "c2", 20);
        put(&me, "/_/Recherche/index.astroidx", "c3", 30);
        put(&me, "/Public/cv.pdf", "c4", 40);
        put(&me, "/Photos/plage.jpg", "c5", 500);
        put(&me, "/Photos/partagee.jpg", "c6", 300);
        put(&lea, "/Photos/autre.jpg", "c7", 9999);
        c.execute("INSERT INTO ucloud_sharing(owner_hex, cid, kind, at) VALUES (?1, 'c6', 'link', 0)", [&me]).unwrap();
        // c5 à l'abri chez Léa (copie entière), c1 dans un paquet k-sur-n ; c6 copiée puis disparue chez moi : pas comptée.
        c.execute("INSERT INTO backup_items(owner_hex, host_hex, cid, size, at, missing_since) VALUES (?1, ?2, 'c5', 500, 0, NULL), (?1, ?2, 'c6', 300, 0, 5)", params![me, lea]).unwrap();
        c.execute("INSERT INTO backup_blobs(owner_hex, cid, seq, off, len, at) VALUES (?1, 'c1', 0, 0, 100, 0)", [&me]).unwrap();
        // Je garde pour Paul 700 octets sur 1000 accordés.
        c.execute("INSERT INTO backup_items(owner_hex, host_hex, cid, size, at) VALUES (?1, ?2, 'p1', 700, 0)", params![paul, me]).unwrap();
        c.execute("INSERT INTO backup_pacts(owner_hex, host_hex, status, quota_bytes, enabled, asked_at) VALUES (?1, ?2, 'accepted', 1000, 1, 0)", params![paul, me]).unwrap();

        let v = spread(&c, &me).unwrap();
        let z = |k: &str| v["zones"][k]["bytes"].as_i64().unwrap_or(0);
        assert_eq!((z("coffre"), z("souvenirs"), z("recherche"), z("public"), z("partage"), z("prive")), (100, 20, 30, 40, 300, 500), "{v}");
        assert_eq!(v["total"], 990, "le compte de Léa n'entre jamais");
        assert_eq!(v["safe"], 600);
        assert_eq!((v["guardians"][0]["bytes"].as_i64(), v["guardians"].as_array().unwrap().len()), (Some(800), 1));
        assert_eq!((v["hosted"][0]["bytes"].as_i64(), v["hosted"][0]["quota"].as_i64(), v["hosted_total"].as_i64()), (Some(700), Some(1000), Some(700)));
    }
}
