//! Synchronisation par Negentropy (NIP-77) contre un vrai relai `astro-relay` : seules les différences voyagent, et la
//! réconciliation ne porte que sur ce que le lecteur a le droit de lire (vidéos réservées au cercle N¹ de l'auteur).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use astro_nostr_proto::{Event, Filter};
use astro_relay::{DbCircle, Relay, RelayConfig, serve};
use astro_sync::{ClientError, Signer, WsClient};
use common::*;

async fn relay(events: &[Event], hosted: &[&[u8; 32]]) -> (String, TempDir) {
    let dir = TempDir::new("neg");
    let db = open_db(&dir);
    for h in hosted {
        host(&db, h).await;
    }
    for e in events {
        ingest(&db, e).await;
    }
    let circle = Arc::new(DbCircle::new());
    circle.refresh(&db).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    tokio::spawn(serve(listener, Relay::new(db, circle, RelayConfig::default())));
    (url, dir)
}

#[tokio::test(flavor = "multi_thread")]
async fn only_the_differences_travel_and_deletions_are_seen() {
    let notes: Vec<Event> = (0..300u64).map(|n| ev(&BOB, 1, 1_000 + n, vec![], &format!("note {n}"))).collect();
    let (url, _dir) = relay(&notes, &[&BOB]).await;
    // J'en connais 250, plus 5 que le relai n'a pas (supprimés chez lui, ou jamais vus).
    let mut local: Vec<(u64, String)> = notes[..250].iter().map(|e| (e.created_at, e.id.clone())).collect();
    local.extend((0..5u8).map(|n| (500, hex::encode([n; 32]))));
    let filter = Filter { kinds: Some(vec![1]), authors: Some(vec![hex(&BOB)]), ..Filter::default() };
    let r = WsClient::new(Duration::from_secs(10)).sync_as(&url, &url, &filter, &local).await.unwrap();
    assert!(r.negentropy, "le relai astroportd parle NIP-77");
    let mut missing: Vec<&str> = r.missing.iter().map(|e| e.id.as_str()).collect();
    missing.sort_unstable();
    let mut want: Vec<&str> = notes[250..].iter().map(|e| e.id.as_str()).collect();
    want.sort_unstable();
    assert_eq!(missing, want, "les 50 nouveautés, et elles seules");
    assert_eq!(r.gone.len(), 5);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reconciliation_only_covers_what_the_reader_may_read() {
    // Bob suit Alice : sa vidéo est pour elle (cercle N¹), pas pour un inconnu.
    let video = ev(&BOB, 21, 2_000, vec![t(&["title", "Film"]), t(&["x", &"ab".repeat(32)])], "");
    let (url, _dir) = relay(&[follows(&BOB, 1_500, &[&ALICE]), video.clone()], &[&BOB, &ALICE]).await;
    let filter = Filter { kinds: Some(vec![21]), authors: Some(vec![hex(&BOB)]), ..Filter::default() };
    let anon = WsClient::new(Duration::from_secs(10)).sync_as(&url, &url, &filter, &[]).await;
    assert!(matches!(anon, Err(ClientError::AuthRequired)), "sans clé : authentification exigée ({anon:?})");
    let alice = WsClient::new(Duration::from_secs(10)).with_signer(Signer::new(ALICE)).sync_as(&url, &url, &filter, &[]).await.unwrap();
    assert_eq!(alice.missing.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), [video.id.as_str()], "Alice, amie de Bob, la voit");
    let stranger = WsClient::new(Duration::from_secs(10)).with_signer(Signer::new(MALLORY)).sync_as(&url, &url, &filter, &[]).await;
    assert!(stranger.map_or(true, |r| r.missing.is_empty()), "un inconnu n'apprend rien, pas même l'identifiant");
}
