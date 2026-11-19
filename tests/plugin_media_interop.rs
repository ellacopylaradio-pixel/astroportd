//! Les événements signés par le plugin `media` (Python, sans dépendance) sont acceptés comme n'importe quel événement Nostr :
//! signature Schnorr et identifiant NIP-01 vérifiés par astroportd, tags de l'événement vidéo
//! conformes à ce que la station écrivait avant le plugin. Fixture produite par `media/bin/media_add.py`.

#![allow(clippy::unwrap_used)]

use astro_nostr_proto::Event;
use serde_json::Value;

fn video() -> Event {
    let doc: Value = serde_json::from_str(include_str!("fixtures/plugin_media_events.json")).unwrap();
    serde_json::from_value(doc["video"].clone()).unwrap()
}

#[test]
fn events_signed_by_the_plugin_verify_in_astroportd() {
    let video = video();
    video.verify().unwrap();
    assert_eq!(video.kind, 21);
    let mut tampered = video.clone();
    tampered.content.push('x');
    assert!(tampered.verify().is_err());
}

#[test]
fn the_video_event_carries_the_tags_the_station_side_used_to_write() {
    let video = video();
    let tag = |k: &str| video.tags.iter().filter(|t| t.first().map(String::as_str) == Some(k)).map(|t| t[1..].to_vec()).collect::<Vec<_>>();
    assert_eq!(tag("title")[0], ["Test S01E02 — Pilote"]);
    assert_eq!(tag("url")[0], ["/ipfs/bafkreihdwdcefgh4dqkjv67uzcmw7ojee6xedzdetojuzjevtenxquvyku"]);
    assert!(tag("t").iter().any(|t| t == &["private-copy"]) && !tag("-").is_empty() && tag("i")[0] == ["tmdb:99"]);
    assert_eq!((tag("series_name")[0].clone(), tag("season_number")[0].clone(), tag("episode_number")[0].clone()), (vec!["Test".to_owned()], vec!["1".to_owned()], vec!["2".to_owned()]));
    // Les étiquettes que lisaient les clients quand la station écrivait elle-même ces événements : aucune n'est perdue.
    let mut keys: Vec<String> = video.tags.iter().map(|t| if t[0] == "t" || t[0] == "l" { format!("{}:{}", t[0], t[1]) } else { t[0].clone() }).collect();
    keys.sort();
    keys.dedup();
    let expected = ["-", "L", "dim", "duration", "encrypted", "episode_name", "episode_number", "i", "imeta", "l:Science-Fiction", "l:private-copy-cpi-l122-5", "m", "published_at", "season_number", "series_name", "size", "t:private-copy", "t:regular", "t:science-fiction", "t:series", "t:tv-show", "title", "url", "x", "year"];
    assert_eq!(keys, expected);
}

fn fixture(name: &str) -> Event {
    let doc: Value = serde_json::from_str(include_str!("fixtures/plugin_media_events.json")).unwrap();
    serde_json::from_value(doc[name].clone()).unwrap()
}

#[test]
fn a_track_is_a_signed_nip71_event_whose_imeta_is_audio_and_a_mix_is_a_nip51_playlist_of_tracks() {
    let (track, playlist) = (fixture("music"), fixture("playlist"));
    track.verify().unwrap();
    playlist.verify().unwrap();
    let tag = |e: &Event, k: &str| e.tags.iter().filter(|t| t.first().map(String::as_str) == Some(k)).map(|t| t[1..].to_vec()).collect::<Vec<_>>();
    // Un kind 21 ordinaire : le relai lui applique la règle des vidéos (cercle N¹ de l'auteur).
    assert_eq!(track.kind, astro_nostr_proto::kind::VIDEO);
    let imeta = &tag(&track, "imeta")[0];
    assert!(imeta.iter().any(|v| v == "m audio/mpeg") && imeta.iter().any(|v| v.starts_with("image /ipfs/")), "{imeta:?}");
    assert!(!imeta.iter().any(|v| v.starts_with("dim ")), "aucune image vidéo fabriquée");
    assert!(tag(&track, "t").iter().any(|t| t == &["music"]) && !tag(&track, "-").is_empty());
    assert_eq!(tag(&track, "artist")[0], ["Artiste B"]);
    // La playlist désigne les morceaux par leur id, dans l'ordre, et se protège comme eux.
    assert_eq!(playlist.kind, astro_nostr_proto::kind::VIDEO_SET);
    assert_eq!(tag(&playlist, "e"), vec![vec![track.id.clone()]]);
    assert!(!tag(&playlist, "-").is_empty() && tag(&playlist, "d")[0] == ["mix:mixmixmix01"]);
    let policy = astro_relay::policy::Policy::default();
    assert_eq!(policy.rule(playlist.kind).read, policy.rule(track.kind).read, "une playlist n'est pas plus lisible que ses morceaux");
}

/// La station republie un média (« Partager avec mes amis ») avec **exactement** les étiquettes du plugin : un ami ne voit
/// pas la différence entre un film publié par le plugin et un film partagé depuis la médiathèque.
#[test]
fn the_station_writes_the_same_tags_as_the_plugin() {
    let strip = |e: &Event| e.tags.iter().filter(|t| t[0] != "published_at").cloned().collect::<Vec<_>>();
    let at = |e: &Event| e.tags.iter().find(|t| t[0] == "published_at").map(|t| t[1].parse::<i64>().unwrap()).unwrap();
    let cid = "bafkreihdwdcefgh4dqkjv67uzcmw7ojee6xedzdetojuzjevtenxquvyku";
    let video = video();
    let episode = serde_json::json!({ "type": "episode", "title": "Pilote", "year": 2019, "genres": ["Science-Fiction"], "ids": { "tmdb": 99 },
        "series": { "name": "Test", "season": 1, "episode": 2 }, "source": { "kind": "broadcast", "rights": "private_copy" },
        "technical": { "duration_s": 1500.0, "size": 5_000_000, "mime": "video/mp4", "video": { "width": 1280, "height": 720 } } });
    let (k, tags) = astroportd::media_share::event_tags(&episode, cid, &"a".repeat(64), None, at(&video));
    assert_eq!((k, tags.iter().filter(|t| t[0] != "published_at").cloned().collect::<Vec<_>>()), (video.kind, strip(&video)));
    let track = fixture("music");
    let song = serde_json::json!({ "type": "music", "title": "Titre Deux", "ids": { "youtube": "bbbbbbbbbbb" }, "source": { "kind": "web", "rights": "private_copy" },
        "music": { "artist": "Artiste B", "album": "Mon Mix", "album_artist": "DJ Test", "track": 2 }, "technical": { "duration_s": 245.0, "size": 4_000_000, "mime": "audio/mpeg" } });
    let (k, tags) = astroportd::media_share::event_tags(&song, cid, &"b".repeat(64), Some("bafkreigh2akiscaildcqabsyg3dfr6chu3fgpregiymsck7e7aqa4s52zy"), at(&track));
    assert_eq!((k, tags.iter().filter(|t| t[0] != "published_at").cloned().collect::<Vec<_>>()), (track.kind, strip(&track)));
}
