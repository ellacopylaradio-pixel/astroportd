//! Interopérabilité de Negentropy avec l'implémentation de référence (hoytech/negentropy, celle de strfry) : les messages
//! sont rejoués un à un et doivent être identiques octet pour octet. Vecteurs produits par `fixtures/negentropy_vectors.mjs`.

#![allow(clippy::unwrap_used)]

use astro_nostr_proto::negentropy::{Item, Negentropy, Storage};
use serde_json::Value;
use sha2::{Digest, Sha256};

fn storage(v: &Value) -> Storage {
    Storage::new(v.as_array().unwrap().iter().map(|e| Item { timestamp: e[0].as_u64().unwrap(), id: Sha256::digest(e[1].as_str().unwrap().as_bytes()).into() }).collect())
}

#[test]
fn every_message_matches_the_reference_byte_for_byte() {
    let doc: Value = serde_json::from_str(include_str!("fixtures/negentropy_vectors.json")).unwrap();
    for run in doc["runs"].as_array().unwrap() {
        let limit = run["limit"].as_u64().unwrap() as usize;
        let msgs: Vec<Vec<u8>> = run["msgs"].as_array().unwrap().iter().map(|m| hex::decode(m.as_str().unwrap()).unwrap()).collect();
        let (mut client, mut relay) = (Negentropy::new(storage(&doc["a"]), limit), Negentropy::new(storage(&doc["b"]), limit));
        assert_eq!(client.initiate(), msgs[0], "premier message (limite {limit})");
        let (mut have, mut need) = (Vec::new(), Vec::new());
        let mut i = 0;
        loop {
            let answer = relay.reconcile(&msgs[i]).unwrap().next.unwrap();
            assert_eq!(answer, msgs[i + 1], "réponse du relai n° {} (limite {limit})", i / 2 + 1);
            let round = client.reconcile(&answer).unwrap();
            have.extend(round.have.iter().map(hex::encode));
            need.extend(round.need.iter().map(hex::encode));
            match round.next {
                Some(next) => {
                    assert_eq!(next, msgs[i + 2], "message n° {} du client (limite {limit})", i / 2 + 2);
                    i += 2;
                }
                None => {
                    assert_eq!(i + 2, msgs.len(), "même nombre d'allers-retours que la référence");
                    break;
                }
            }
        }
        have.sort();
        need.sort();
        let list = |k: &str| run[k].as_array().unwrap().iter().map(|x| x.as_str().unwrap().to_owned()).collect::<Vec<_>>();
        assert_eq!((have, need), (list("have"), list("need")));
    }
}
