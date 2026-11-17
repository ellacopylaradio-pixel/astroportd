//! FaceCloud de bout en bout : dépôt WebDAV → file → appariement 512D → partage local et enveloppe NIP-59.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use astro_crypto::nip59;
use astro_crypto::{UencKey, uenc};
use astro_dav::BlobStore;
use astro_facecloud::{Decision, ShareVia, embedding_hash, unknown_name};
use common::*;

async fn faces_of(rig: &Rig, owner: &[u8; 32]) -> Vec<(String, Option<String>, Option<String>, Option<String>)> {
    let o = hex(owner);
    rig.db
        .readers()
        .read(move |c| {
            let mut s = c.prepare("SELECT name, pubkey, ref, suggest_name FROM faces WHERE owner_hex = ?1 ORDER BY face_id")?;
            Ok(s.query_map([o], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<Result<Vec<_>, _>>()?)
        })
        .await
        .unwrap()
}

async fn count(rig: &Rig, sql: &'static str) -> i64 {
    rig.db.readers().read(move |c| Ok(c.query_row(sql, [], |r| r.get(0))?)).await.unwrap()
}

async fn count_with(rig: &Rig, sql: &'static str, a: String, b: &'static str) -> i64 {
    rig.db.readers().read(move |c| Ok(c.query_row(sql, rusqlite::params![a, b], |r| r.get(0))?)).await.unwrap()
}

#[tokio::test]
async fn drop_a_photo_recognize_friends_share_locally_and_wrap_for_the_remote_friend() {
    let r = rig("e2e", |_| {}).await;
    enroll_friends(&r).await;
    let group = "FACES:bob+p1,carol+p1,dave+p1,stranger";
    put_photo(&r, "groupe.jpg", group).await;
    let stats = drain(&r).await;
    assert_eq!((stats.done, stats.retried, stats.failed), (1, 0, 0));

    // Catalogue : 3 références enrôlées + 4 visages de la photo (Bob, Carol, Dave reconnus ; un inconnu nommé par son empreinte).
    let faces = faces_of(&r, &ALICE).await;
    assert_eq!(faces.len(), 7);
    let on_photo = &faces[3..];
    let names: Vec<&str> = on_photo.iter().map(|f| f.0.as_str()).collect();
    assert_eq!(&names[..3], ["Bob", "Carol", "Dave"]);
    assert!(names[3].starts_with("Inconnu_") && names[3].len() == 8 + 8, "{names:?}");
    assert_eq!(on_photo[0].1.as_deref(), Some(hex(&BOB).as_str()));
    assert!(on_photo[3].1.is_none());
    let (status, info): (String, i64) = r
        .db
        .readers()
        .read(|c| Ok(c.query_row("SELECT faceid_status, json_extract(data, '$.faceid.count') FROM ucloud_entries WHERE path = '/groupe.jpg'", [], |r| Ok((r.get(0)?, r.get(1)?)))?))
        .await
        .unwrap();
    assert_eq!((status.as_str(), info), ("faces", 4));

    // Photo de groupe : rien ne part tout seul. Bob et Carol (amis réciproques) attendent l'accord d'Alice, car d'autres
    // personnes paraissent avec eux ; Dave (pas réciproque) n'est même pas proposé.
    assert!(r.outbox.sent.lock().unwrap().is_empty());
    assert_eq!(count(&r, "SELECT count(*) FROM face_shares").await, 0);
    assert_eq!(count(&r, "SELECT count(*) FROM face_share_pending WHERE path = '/groupe.jpg'").await, 2);

    // Bob seul sur la photo : copie silencieuse. Carol seule : une enveloppe.
    let photo = "FACES:bob+p2";
    put_photo(&r, "vacances.jpg", photo).await;
    let carol_photo = "FACES:carol+p3";
    put_photo(&r, "plage.jpg", carol_photo).await;
    assert_eq!(drain(&r).await.done, 2);

    // Bob (ami réciproque, hébergé ici) : copie silencieuse, même blob, même clé.
    let (alice_cid,): (String,) = r.db.readers().read(|c| Ok((c.query_row("SELECT cid FROM ucloud_entries WHERE path = '/vacances.jpg'", [], |r| r.get(0))?,))).await.unwrap();
    let (cid, origin, status): (String, String, Option<String>) = r
        .db
        .readers()
        .read(|c| Ok(c.query_row("SELECT cid, origin, faceid_status FROM ucloud_entries WHERE owner_hex = ?1 AND path = '/Partages/De_alice/vacances.jpg'", [hex(&BOB)], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?))
        .await
        .unwrap();
    assert_eq!((cid.as_str(), origin.as_str(), status), (alice_cid.as_str(), "facecloud-share", None));
    let (code, body) = dav(&r.app, "GET", "/dav/Partages/De_alice/vacances.jpg", "bob@example.org", &BOB, vec![]).await;
    assert_eq!((code, body.as_slice()), (200, photo.as_bytes()), "Bob relit la photo avec sa propre clé de trousseau");
    assert_eq!(count(&r, "SELECT count(*) FROM ucloud_keyring WHERE owner_hex = (SELECT hex FROM identities WHERE email = 'bob@example.org')").await, 1);

    // Carol (amie réciproque, hébergée ailleurs) : une seule enveloppe NIP-59, adressée à elle.
    let sent = r.outbox.sent.lock().unwrap().clone();
    assert_eq!(sent.len(), 1, "ni Dave (pas réciproque) ni l'inconnu");
    let (recipient, wrap) = &sent[0];
    assert_eq!(recipient, &hex(&CAROL));
    assert_eq!(wrap.kind, 1059);
    wrap.verify().unwrap();
    assert_eq!(wrap.tags, vec![vec!["p".to_owned(), hex(&CAROL)]]);
    assert_ne!(wrap.pubkey, hex(&ALICE), "l'expéditeur n'apparaît pas sur l'enveloppe");
    let cid: String = r.db.readers().read(|c| Ok(c.query_row("SELECT cid FROM ucloud_entries WHERE path = '/plage.jpg'", [], |r| r.get(0))?)).await.unwrap();
    assert!(!wrap.content.contains(&cid));
    let opened = nip59::unwrap(&CAROL, wrap).unwrap();
    assert_eq!(opened.sender, hex(&ALICE), "expéditeur authentifié par le scellé");
    assert_eq!(opened.rumor.kind, 15);
    assert_eq!(opened.rumor.tag_value("cid"), Some(cid.as_str()));
    assert_eq!(opened.rumor.tag_value("file-type"), Some("image/jpeg"));
    assert_eq!(opened.rumor.tag_value("encryption-algorithm"), Some("uenc-v2"));
    assert_eq!(opened.rumor.tag_value("name"), Some("plage.jpg"));
    assert_eq!(opened.rumor.content, format!("ipfs://{cid}"));
    // Avec le CID et la clé du message, Carol déchiffre le blob IPFS.
    let key = UencKey::from_hex(opened.rumor.tag_value("decryption-key").unwrap()).unwrap();
    let (mut blob, _) = r.blobs.open(opened.rumor.tag_value("cid").unwrap()).await.unwrap();
    let mut plain = Vec::new();
    uenc::decrypt_stream(&key, &mut blob, &mut plain).await.unwrap();
    assert_eq!(plain, carol_photo.as_bytes());
    // Seule Carol peut ouvrir l'enveloppe.
    assert!(nip59::unwrap(&BOB, wrap).is_err() && nip59::unwrap(&DAVE, wrap).is_err());

    let shares: Vec<(String, String)> = r
        .db
        .readers()
        .read(|c| Ok(c.prepare("SELECT recipient_hex, via FROM face_shares ORDER BY via")?.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<Vec<_>, _>>()?))
        .await
        .unwrap();
    assert_eq!(shares, [(hex(&BOB), "local".to_owned()), (hex(&CAROL), "nip17".to_owned())]);

    // Le partage local est ancré dans le manifeste de Bob (rejouable).
    assert!(r.publisher.flush().await.is_empty());
    assert_eq!(count(&r, "SELECT count(*) FROM nostr_events WHERE kind = 30078 AND d_tag = 'ucloud:manifest'").await, 2, "Alice et Bob");
}

#[tokio::test]
async fn thresholds_recognized_suggestion_unknown_and_deterministic_names() {
    let r = rig("thresholds", |_| {}).await;
    enroll_friends(&r).await;
    put_photo(&r, "p.jpg", "FACES:bob+x,bob~carol,mystere").await;
    // Les références de Bob et Carol sont rangées dans leurs fiches : un visage reconnu (ou suggéré) suit la fiche, pas le nom.
    r.db.writer().execute("UPDATE faces SET contact_uid = 'fiche-' || lower(name)", vec![]).await.unwrap();
    let rep = r.fc.process_photo(&hex(&ALICE), "/p.jpg").await.unwrap();
    assert_eq!(rep.status, "faces");
    let contact_of = |id: i64| { let db = r.db.clone(); async move { db.readers().read(move |c| Ok(c.query_row("SELECT contact_uid, suggest_contact_uid FROM faces WHERE face_id = ?1", [id], |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<String>>(1)?)))?)).await.unwrap() } };
    assert_eq!(contact_of(rep.faces[0].face_id).await, (Some("fiche-bob".to_owned()), None), "reconnu : rejoint la fiche de Bob");
    let (none, suggested) = contact_of(rep.faces[1].face_id).await;
    assert!(none.is_none() && suggested.is_some_and(|c| c == "fiche-bob" || c == "fiche-carol"), "suggéré : la fiche attend la validation");
    // 1. Même personne, autre photo : reconnu (score ≥ 0,82).
    assert!(matches!(rep.faces[0].decision, Decision::Recognized { score } if score > 0.95), "{:?}", rep.faces[0]);
    assert_eq!(rep.faces[0].name, "Bob");
    // 2. Mélange de Bob et Carol : zone grise → suggestion, jamais d'identité attribuée ni de partage.
    match &rep.faces[1].decision {
        Decision::Suggestion { score, candidate } => {
            assert!((0.55..0.82).contains(score), "{score}");
            assert!(candidate.as_deref().is_some_and(|c| c == "Bob" || c == "Carol"));
        }
        other => panic!("suggestion attendue : {other:?}"),
    }
    assert!(rep.faces[1].pubkey.is_none() && rep.faces[1].name.starts_with("Inconnu_"));
    // 3. Inconnu : nom déterministe issu de l'empreinte de l'embedding.
    assert_eq!(rep.faces[2].decision, Decision::Unknown);
    let mut v = r.engine_embedding("mystere");
    unit_check(&mut v);
    assert_eq!(rep.faces[2].name, unknown_name(&embedding_hash(&v)));
    // Partages : Bob (visage 0) est reconnu avec certitude, mais un suggéré et un inconnu paraissent avec lui : la photo
    // attend l'accord d'Alice au lieu de partir.
    assert!(rep.shares.is_empty(), "{:?}", rep.shares);
    assert_eq!(rep.skipped.iter().map(|s| (s.recipient.as_str(), s.reason.as_str())).collect::<Vec<_>>(), [(hex(&BOB).as_str(), "en attente de confirmation")]);

    // Validation humaine de la suggestion : le visage prend l'identité suggérée.
    let id = rep.faces[1].face_id;
    assert!(r.fc.accept_suggestion(&hex(&ALICE), id).await.unwrap());
    assert!(!r.fc.accept_suggestion(&hex(&ALICE), id).await.unwrap(), "plus de suggestion en attente");
    let row = faces_of(&r, &ALICE).await.into_iter().find(|f| f.0 == "Bob" || f.0 == "Carol").unwrap();
    assert!(row.1.is_some());
    assert!(contact_of(id).await.0.is_some_and(|c| c.starts_with("fiche-")), "validée : le visage rejoint la fiche suggérée");
    // Un inconnu nommé à la main devient un contact.
    let unknown_id = rep.faces[2].face_id;
    r.fc.name_face(&hex(&ALICE), unknown_id, Some(&hex(&DAVE)), "Dave bis").await.unwrap();
    assert!(r.fc.name_face(&hex(&ALICE), 9999, None, "x").await.is_err());
    // Une nouvelle photo du même inconnu (autre cliché) est désormais reconnue sous ce nom.
    put_photo(&r, "q.jpg", "FACES:mystere+z").await;
    let rep2 = r.fc.process_photo(&hex(&ALICE), "/q.jpg").await.unwrap();
    assert!(matches!(rep2.faces[0].decision, Decision::Recognized { .. }));
    assert_eq!(rep2.faces[0].name, "Dave bis");
}

#[tokio::test]
async fn photos_without_faces_and_weak_detections() {
    let r = rig("noface", |_| {}).await;
    put_photo(&r, "paysage.jpg", "NOFACE").await;
    put_photo(&r, "flou.jpg", "FACES:bob@0.5").await;
    for p in ["/paysage.jpg", "/flou.jpg"] {
        let rep = r.fc.process_photo(&hex(&ALICE), p).await.unwrap();
        assert_eq!((rep.status.as_str(), rep.faces.len()), ("no_face", 0), "{p}");
    }
    assert_eq!(count(&r, "SELECT count(*) FROM ucloud_entries WHERE faceid_status = 'no_face'").await, 2);
    assert_eq!(count(&r, "SELECT count(*) FROM faces").await, 0, "détection sous 0,60 écartée");
    // Pas une image : refusée sans analyse.
    assert_eq!(dav(&r.app, "PUT", "/dav/notes.txt", "alice@example.org", &ALICE, b"FACES:bob".to_vec()).await.0, 201);
    assert!(r.fc.process_photo(&hex(&ALICE), "/notes.txt").await.is_err());
    assert!(r.fc.process_photo(&hex(&ALICE), "/absent.jpg").await.is_err());
}

#[tokio::test]
async fn reprocessing_is_idempotent_and_received_copies_are_never_analyzed() {
    let r = rig("idem", |_| {}).await;
    enroll_friends(&r).await;
    put_photo(&r, "a.jpg", "FACES:bob+1").await;
    put_photo(&r, "c.jpg", "FACES:carol+1").await;
    for p in ["/a.jpg", "/c.jpg"] {
        assert_eq!(r.fc.process_photo(&hex(&ALICE), p).await.unwrap().shares.len(), 1, "{p}");
    }
    let faces_before = count(&r, "SELECT count(*) FROM faces").await;
    for p in ["/a.jpg", "/c.jpg"] {
        let again = r.fc.process_photo(&hex(&ALICE), p).await.unwrap();
        assert!(again.faces.iter().all(|f| f.decision == Decision::AlreadyKnown), "{again:?}");
        assert!(again.shares.is_empty() && again.skipped.iter().all(|s| s.reason == "déjà partagé"), "{again:?}");
    }
    assert_eq!(count(&r, "SELECT count(*) FROM faces").await, faces_before);
    assert_eq!(r.outbox.sent.lock().unwrap().len(), 1, "une seule enveloppe, jamais de doublon");
    // La copie reçue par Bob n'est jamais analysée (pas de cascade de partages).
    let calls = r.engine.calls.load(std::sync::atomic::Ordering::SeqCst);
    let rep = r.fc.process_photo(&hex(&BOB), "/Partages/De_alice/a.jpg").await.unwrap();
    assert_eq!(rep.status, "skipped");
    assert_eq!(r.engine.calls.load(std::sync::atomic::Ordering::SeqCst), calls);
    // Une autre photo avec le même ami local ne réécrase pas la première : noms uniques.
    put_photo(&r, "b.jpg", "FACES:bob+2").await;
    r.fc.process_photo(&hex(&ALICE), "/b.jpg").await.unwrap();
    assert_eq!(count(&r, "SELECT count(*) FROM ucloud_entries WHERE origin = 'facecloud-share'").await, 2);
}

#[tokio::test]
async fn a_photo_leaves_on_its_own_only_when_it_shows_nobody_but_the_owner_and_the_friend() {
    let r = rig("crowd", |_| {}).await;
    enroll_friends(&r).await;
    let (alice, bob) = (hex(&ALICE), hex(&BOB));
    let local_shares = |p: &'static str| count_with(&r, "SELECT count(*) FROM face_shares s JOIN ucloud_entries e ON e.owner_hex = s.owner_hex AND e.cid = s.cid WHERE s.recipient_hex = ?1 AND e.path = ?2", bob.clone(), p);

    // Un inconnu net à côté de Bob : la photo attend l'accord d'Alice, puis part sur un clic.
    put_photo(&r, "fete.jpg", "FACES:bob+1,inconnu").await;
    let rep = r.fc.process_photo(&alice, "/fete.jpg").await.unwrap();
    assert!(rep.shares.is_empty() && r.fc.crowded(&alice, "/fete.jpg", &bob).await.unwrap());
    assert_eq!(local_shares("/fete.jpg").await, 0);
    assert_eq!(r.fc.resolve_pending(&alice, &bob, true).await.unwrap().0, 1, "accord : envoyée");
    assert_eq!(local_shares("/fete.jpg").await, 1);

    // Alice elle-même (visage nommé) avec Bob : rien d'étranger, la photo part seule.
    assert_eq!(r.fc.enroll(&alice, &alice, "Alice", b"FACES:alice").await.unwrap(), 1);
    put_photo(&r, "nous.jpg", "FACES:alice+1,bob+2").await;
    let shared = r.fc.process_photo(&alice, "/nous.jpg").await.unwrap().shares;
    assert_eq!(shared.iter().map(|x| (x.recipient.as_str(), x.via)).collect::<Vec<_>>(), [(bob.as_str(), ShareVia::Local)]);
    // Un passant flou à l'arrière-plan (détection sous `share_det_min`) ne compte pas.
    put_photo(&r, "rue.jpg", "FACES:bob+3,passant@0.65").await;
    assert_eq!(r.fc.process_photo(&alice, "/rue.jpg").await.unwrap().shares.len(), 1);

    // Partage automatique coupé par Alice : même Bob seul attend un clic.
    assert!(r.fc.auto_share(&alice).await.unwrap());
    r.fc.set_auto_share(&alice, false).await.unwrap();
    assert!(!r.fc.auto_share(&alice).await.unwrap());
    put_photo(&r, "seul.jpg", "FACES:bob+4").await;
    let rep = r.fc.process_photo(&alice, "/seul.jpg").await.unwrap();
    assert!(rep.shares.is_empty() && rep.skipped.iter().any(|s| s.reason == "en attente de confirmation"), "{rep:?}");
    r.fc.set_auto_share(&alice, true).await.unwrap();

    // Relier une personne à sa clé rattrape ses photos, sous la même règle : la photo de groupe refusée reste en attente.
    put_photo(&r, "groupe.jpg", "FACES:bob+5,carol+5").await;
    r.fc.process_photo(&alice, "/groupe.jpg").await.unwrap();
    r.fc.resolve_pending(&alice, &bob, false).await.unwrap();
    let (sent, held) = r.fc.share_backlog(&alice, &bob).await.unwrap();
    assert_eq!(local_shares("/groupe.jpg").await, 0, "rattrapage : la photo de groupe ne part pas seule (envoyées {sent}, en attente {held})");
    assert_eq!(count_with(&r, "SELECT count(*) FROM face_share_pending WHERE recipient_hex = ?1 AND path = ?2", bob.clone(), "/groupe.jpg").await, 1);
}

#[tokio::test]
async fn an_image_in_the_system_drawer_is_never_analyzed_nor_shared() {
    let r = rig("drawer", |_| {}).await;
    enroll_friends(&r).await;
    for d in ["/dav/_", "/dav/_/Contacts"] {
        assert_eq!(dav(&r.app, "MKCOL", d, "alice@example.org", &ALICE, vec![]).await.0, 201);
    }
    assert_eq!(dav(&r.app, "PUT", "/dav/_/Contacts/bob.jpg", "alice@example.org", &ALICE, b"FACES:bob+1".to_vec()).await.0, 201);
    drain(&r).await;
    let calls = r.engine.calls.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(r.fc.process_photo(&hex(&ALICE), "/_/Contacts/bob.jpg").await.unwrap().status, "skipped");
    assert_eq!(r.engine.calls.load(std::sync::atomic::Ordering::SeqCst), calls, "aucune inférence");
    assert_eq!(count(&r, "SELECT count(*) FROM faces WHERE ucloud_path LIKE '/!_/%' ESCAPE '!'").await, 0);
    assert_eq!(count(&r, "SELECT count(*) FROM face_shares").await + count(&r, "SELECT count(*) FROM tasks").await, 0, "ni file, ni partage");
}

#[tokio::test]
async fn owners_are_isolated_in_the_vector_index() {
    let r = rig("owners", |_| {}).await;
    enroll_friends(&r).await; // Alice connaît Carol
    // Bob dépose une photo de Carol : pour Bob, c'est un inconnu.
    let (code, _) = dav(&r.app, "PUT", "/dav/c.jpg", "bob@example.org", &BOB, b"FACES:carol+9".to_vec()).await;
    assert_eq!(code, 201);
    let rep = r.fc.process_photo(&hex(&BOB), "/c.jpg").await.unwrap();
    assert_eq!(rep.faces[0].decision, Decision::Unknown);
    assert!(rep.faces[0].pubkey.is_none() && rep.shares.is_empty());
}

#[tokio::test]
async fn sharing_failures_and_non_reciprocal_friends_are_reported_not_fatal() {
    let r = rig("failures", |_| {}).await;
    enroll_friends(&r).await;
    *r.outbox.fail.lock().unwrap() = true;
    put_photo(&r, "a.jpg", "FACES:carol+1").await;
    put_photo(&r, "d.jpg", "FACES:dave+1").await;
    let rep = r.fc.process_photo(&hex(&ALICE), "/a.jpg").await.unwrap();
    assert!(rep.shares.is_empty());
    let mut reasons: Vec<String> = rep.skipped.iter().map(|s| s.reason.clone()).collect();
    reasons.extend(r.fc.process_photo(&hex(&ALICE), "/d.jpg").await.unwrap().skipped.into_iter().map(|s| s.reason));
    assert!(reasons.iter().any(|r| r.starts_with("envoi impossible")) && reasons.iter().any(|r| r == "pas un ami réciproque (N1)"), "{reasons:?}");
    assert_eq!(count(&r, "SELECT count(*) FROM face_shares").await, 0, "échec d'envoi : rien n'est marqué partagé, un retraitement réessaiera");
    // Le relai revient : le retraitement partage enfin.
    *r.outbox.fail.lock().unwrap() = false;
    let rep = r.fc.process_photo(&hex(&ALICE), "/a.jpg").await.unwrap();
    assert_eq!(rep.shares.len(), 1);
    // Partage désactivé : catalogage seul.
    let off = rig("off", |c| c.share_enabled = false).await;
    enroll_friends(&off).await;
    put_photo(&off, "a.jpg", "FACES:bob+1").await;
    let rep = off.fc.process_photo(&hex(&ALICE), "/a.jpg").await.unwrap();
    assert!(rep.shares.is_empty() && rep.skipped.is_empty());
}

#[tokio::test]
async fn only_one_inference_runs_at_a_time() {
    let r = rig("gpu", |_| {}).await;
    r.engine.delay_ms.store(30, std::sync::atomic::Ordering::SeqCst);
    for i in 0..4 {
        put_photo(&r, &format!("p{i}.jpg"), "NOFACE").await;
    }
    let owner = hex(&ALICE);
    let tasks: Vec<_> = (0..4)
        .map(|i| {
            let (fc, owner) = (r.fc.clone(), owner.clone());
            tokio::spawn(async move { fc.process_photo(&owner, &format!("/p{i}.jpg")).await.unwrap() })
        })
        .collect();
    for t in tasks {
        t.await.unwrap();
    }
    assert_eq!(r.engine.calls.load(std::sync::atomic::Ordering::SeqCst), 4);
    assert_eq!(r.engine.max_running.load(std::sync::atomic::Ordering::SeqCst), 1, "jamais deux inférences simultanées");
}

#[tokio::test]
async fn queue_dedupes_retries_with_backoff_and_gives_up() {
    let r = rig("queue", |_| {}).await;
    put_photo(&r, "a.jpg", "NOFACE").await;
    for _ in 0..3 {
        r.fc.enqueue(&hex(&ALICE), "/a.jpg").await.unwrap();
    }
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(count(&r, "SELECT count(*) FROM tasks WHERE status = 'pending'").await, 1, "un seul exemplaire en attente (hook + enqueue manuels)");
    // Moteur en panne : replanifié avec délai, donc pas rejouable immédiatement.
    *r.engine.fail.lock().unwrap() = true;
    let s = r.fc.run_pending(10).await.unwrap();
    assert_eq!((s.done, s.retried, s.failed), (0, 1, 0));
    assert_eq!(r.fc.run_pending(10).await.unwrap(), Default::default(), "le délai de reprise n'est pas écoulé");
    // Échéance forcée : après 5 échecs, abandon.
    for expected_failed in [0, 0, 0, 1] {
        r.db.writer().execute("UPDATE tasks SET not_before = 0 WHERE status = 'pending'", vec![]).await.unwrap();
        let s = r.fc.run_pending(10).await.unwrap();
        assert_eq!(s.failed, expected_failed);
    }
    assert_eq!(count(&r, "SELECT count(*) FROM tasks WHERE status = 'failed'").await, 1);
    // Reprise sur incident : une tâche restée « running » après un arrêt brutal repart.
    r.fc.enqueue(&hex(&ALICE), "/a.jpg").await.unwrap();
    r.db.writer().execute("UPDATE tasks SET status = 'running' WHERE status = 'pending'", vec![]).await.unwrap();
    assert_eq!(r.fc.recover().await.unwrap(), 1);
    *r.engine.fail.lock().unwrap() = false;
    assert_eq!(r.fc.run_pending(10).await.unwrap().done, 1);
    // Photo disparue : échec définitif sans réessai.
    r.fc.enqueue(&hex(&ALICE), "/supprimee.jpg").await.unwrap();
    let s = r.fc.run_pending(10).await.unwrap();
    assert_eq!((s.failed, s.retried), (1, 0));
}

#[tokio::test]
async fn the_background_worker_processes_uploads_and_stops_on_shutdown() {
    let r = rig("worker", |_| {}).await;
    enroll_friends(&r).await;
    let (stop, rx) = tokio::sync::watch::channel(false);
    let worker = tokio::spawn(r.fc.clone().run_worker(std::time::Duration::from_millis(20), rx));
    put_photo(&r, "auto.jpg", "FACES:carol+7").await;
    for _ in 0..100 {
        if r.outbox.sent.lock().unwrap().len() == 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(r.outbox.sent.lock().unwrap().len(), 1, "le dépôt WebDAV a déclenché analyse et envoi sans intervention");
    stop.send(true).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), worker).await.expect("arrêt attendu").unwrap();
}

impl Rig {
    /// Embedding normalisé que le moteur simulé produit pour `label`.
    fn engine_embedding(&self, label: &str) -> Vec<f32> {
        random_vec(label)
    }
}

#[tokio::test]
async fn an_uncertain_match_waits_for_one_click_then_goes_out_or_is_dropped() {
    // Seuil d'envoi direct inatteignable : tout visage reconnu tombe dans la zone de précaution.
    let r = rig("pending", |c| c.share_auto_min = 2.0).await;
    enroll_friends(&r).await;
    put_photo(&r, "soiree.jpg", "FACES:bob+p1,carol+p1").await;
    let alice = hex(&ALICE);
    let rep = r.fc.process_photo(&alice, "/soiree.jpg").await.unwrap();
    assert!(rep.shares.is_empty(), "rien ne part sans confirmation : {rep:?}");
    assert_eq!(rep.skipped.iter().filter(|s| s.reason == "en attente de confirmation").count(), 2);
    assert!(r.outbox.sent.lock().unwrap().is_empty());
    assert_eq!(count(&r, "SELECT count(*) FROM face_shares").await, 0);

    // Un retraitement de la même photo ne duplique ni n'envoie.
    r.fc.process_photo(&alice, "/soiree.jpg").await.unwrap();
    assert_eq!(count(&r, "SELECT count(*) FROM face_share_pending").await, 2);
    assert_eq!(count(&r, "SELECT count(*) FROM face_shares").await, 0, "une photo en attente le reste");

    let batches = astro_facecloud::review::pending_batches(&r.db, &alice).await.unwrap();
    let mut named: Vec<_> = batches.iter().map(|b| (b.name.as_str(), b.count)).collect();
    named.sort_unstable();
    assert_eq!(named, [("Bob", 1), ("Carol", 1)]);
    assert!(batches.iter().all(|b| b.sample_path == "/soiree.jpg"));

    // Accepter pour Bob : copie locale ; Carol reste en attente.
    assert_eq!(r.fc.resolve_pending(&alice, &hex(&BOB), true).await.unwrap(), (1, 0));
    assert_eq!(count(&r, "SELECT count(*) FROM face_shares WHERE via = 'local'").await, 1);
    assert_eq!(count(&r, "SELECT count(*) FROM face_share_pending").await, 1);

    // Refuser pour Carol : rien n'est envoyé, le lot disparaît.
    assert_eq!(r.fc.resolve_pending(&alice, &hex(&CAROL), false).await.unwrap(), (0, 1));
    assert_eq!(count(&r, "SELECT count(*) FROM face_share_pending").await, 0);
    assert!(r.outbox.sent.lock().unwrap().is_empty());
}
