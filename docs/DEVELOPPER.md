# astroportd — référence technique

Pour les contributeurs et les curieux du dedans. L'utilisation pas à pas est dans le [guide du Capitaine](GUIDE.md) ;
la présentation générale dans le [README](../README.md).

- [1. Principes d'architecture](#1-principes-darchitecture)
- [2. Vue d'ensemble](#2-vue-densemble)
- [3. Cartographie des crates](#3-cartographie-des-crates)
- [4. Construire et tester](#4-construire-et-tester)
- [5. Toutes les commandes](#5-toutes-les-commandes)
- [6. La nuit de 20h12](#6-la-nuit-de-20h12)
- [7. Sécurité](#7-sécurité)
- [8. Écrire une extension WebAssembly](#8-écrire-une-extension-webassembly)
- [9. Films et musique : la copie privée](#9-films-et-musique--la-copie-privée)
- [10. Où vit chaque fonction](#10-où-vit-chaque-fonction)
- [11. Écrire un client compatible](#11-écrire-un-client-compatible)

---

## 1. Principes d'architecture

### La vérité n'est pas dans la base

Il n'existe qu'**une** source de vérité, répliquée par la constellation : les **événements Nostr** (identités, réseau
social, registre Ğ1-N², manifestes) et les **blobs IPFS** (fichiers chiffrés). `uplanet.db` n'est qu'un **cache
rejouable** : on peut la supprimer, puis `astroportd replay` la reconstruit à l'identique. Chaque projection est
*indépendante de l'ordre d'arrivée* : le temps réel et le replay convergent vers le même état. Toute nouvelle table est
classée *projection* (rejouable) ou *état local* (identités hébergées, files, verrous) dans
`crates/astro-db/src/schema.sql`.

### Un seul écrivain SQLite

SQLite (WAL, `sqlite-vec` pour la recherche vectorielle cosinus, FTS5 pour NIP-50) sert d'index local. Toutes les
écritures passent par **un acteur unique** (un thread, une file bornée, une transaction par message) : pas de
`SQLITE_BUSY`, contre-pression naturelle, lecteurs jamais bloqués.

### Les clés restent chez vous

- Secrets en fichiers `0600` (vérifiés par `doctor`), effacés de la mémoire à la destruction (`zeroize`), jamais écrits
  dans la base ni dans les journaux.
- Fichiers chiffrés avant de quitter la machine : **UENC v2** (AES-256-GCM ou ChaCha20-Poly1305, flux par blocs de
  64 Kio, lecture partielle `Range`). Le clair ne touche jamais le disque. Un lien partagé porte sa clé dans le fragment
  `#…`, qu'un navigateur n'envoie jamais au serveur.
- HTTPS automatique (Let's Encrypt, HTTP-01) sans Nginx ni Certbot ; ou derrière votre reverse proxy, sur le port 8090.

### Une monnaie comptable

Le **Ẑen** est l'unité de compte interne de la coopérative, adossée à la Ğ1 (Duniter v2s). Tout montant est un
**entier de centimes** (`astro_core::Amount`) : le flottant est banni de la monnaie.

### Ne jamais détruire un compte

Un compte en défaut de paiement n'est pas détruit : 7 jours de loyer, 7 jours de grâce, puis *suspension* (clés et
données intactes, réactivées au paiement). Quand la trésorerie ne couvre plus les charges, la station passe en
**Bénévolat Actif** (Love Ledger) plutôt que de s'arrêter.

### Identité Y-Level

La clé SSH est hachée (sha512) en `SALT`/`PEPPER`, un scrypt en tire une graine ed25519, d'où viennent une clé SSH
jumelle, le PeerID IPFS, la clé Ğ1 et la clé Nostr. `astroportd ylevel --verify` le prouve. Les MULTIPASS, eux, sont
dérivés d'une formule **isolée** `(SALT, PEPPER_<sha256(UPLANETNAME)[:16]>)` : mêmes secrets, autres clés dans une
autre constellation (`astro-core::formula`, vecteurs dans `crates/astro-core/tests/legacy_keygen.rs`).

---

## 2. Vue d'ensemble

```
        navigateur / mobile (PWA)        clients Nostr        WebDAV · CardDAV · CalDAV
                    │                         │ NIP-11/WS               │
                    ▼                         ▼                         ▼
   ┌────────────────────────────  astroportd  ───────────────────────────────────┐
   │  astro-ui  façade HTTPS (ACME, proxy, app web, /health, SSE, /stations)      │
   │  astro-relay  relai Nostr, pare-feu social NIP-42   astro-dav  uCloud chiffré│
   │  astro-bro  mémoire, bus         astro-facecloud  visages   astro-plugins    │
   │  astro-economy  3×1/3, Love Ledger              astro-lifecycle  comptes     │
   │  astro-sync  projections & replay    astro-p2p  maillage libp2p, tunnels     │
   │  astro-index  recherche multimédia (OCR, SigLIP)                             │
   ├────────────┬───────────────────────────────┬─────────────────────────────────┤
   │ astro-db   │ astro-crypto · astro-core     │ astro-duniter  (Ğ1, subxt)      │
   │ SQLite+vec │ astro-nostr-proto             │ astro-g1-runtime                │
   └────────────┴───────────────────────────────┴─────────────────────────────────┘
          │ cache rejouable                │ vérité : événements Nostr + blobs IPFS (Kubo ou disque)
          ▼                                ▼
      uplanet.db                  relais de la constellation  ·  pairs libp2p
```

Le relai Nostr, l'application web et WebDAV/CardDAV/CalDAV partagent **le même port**.

---

## 3. Cartographie des crates

| Crate | Rôle |
|---|---|
| **`astro-core`** | Formules de crédentiels (`KeyFormula`), dérivations Ğ1/IPFS/Nostr (scrypt, PBKDF2), SS58 préfixe 4450, `Amount`, semaine ISO, horloge solaire 20h12, bus d'activité (SSE). |
| **`astro-crypto`** | UENC v2, NIP-44 v2, NIP-59 (gift wrap), NIP-98 durci (anti-rejeu, liaison URL/méthode/`payload`). |
| **`astro-nostr-proto`** | Événements, filtres bornés, messages NIP-01, kinds du projet, Schnorr secp256k1. |
| **`astro-db`** | Schéma, acteur d'écriture, pool de lecteurs WAL, `sqlite-vec` (mémoire 1024D, visages 512D), événements (remplaçables, NIP-09, NIP-40), FTS5 (NIP-50), `station_mode`. Base en `0600`. |
| **`astro-relay`** | Relai WebSocket en **pare-feu social** : kinds publics en lecture libre, le reste au cercle authentifié (NIP-42) selon le mode ; NIP-01, 09, 11, 17, 40, 42, 45, 50, 59, 70 ; anti-abus. |
| **`astro-sync`** | `Projector` (événement → SQL, indépendant de l'ordre), `replay`, crawler N², client WebSocket. |
| **`astro-dav`** | WebDAV classe 1+2 sur uCloud chiffré, manifeste ancré sur Nostr/IPFS, tiroir `/_/` (coffre, contacts, agenda), liens `/s/<slug>#clé`, invitations `/i/<jeton>`, miniatures WebP, bassin partagé entre plans, jetons par appareil, CardDAV, CalDAV, `sync-collection`. |
| **`astro-facecloud`** | Visages : file de photos, moteur Brain HTTP ou ONNX local, seuils 0,82 / 0,55, partage automatique (≥ 0,88, seul avec le propriétaire). |
| **`astro-bro`** | Agent BRO : mémoire `bge-m3` 1024D, cycle RÊVE (condensation à 150 souvenirs), garde-fous anti-secrets. |
| **`astro-index`** | Recherche multimédia : OCR (local ou délégué à des Capitaines amis par `ai-index`, fermé par défaut), vecteurs SigLIP 2 locaux, file équitable par station. |
| **`astro-p2p`** | libp2p (TCP/QUIC, Noise, Yamux, `swarm.key`, circuit relay, DCUtR), tunnels DRAGON `/x/<service>-<PeerID>`, échange de blobs avec écluse par CID, routage Power-Score. |
| **`astro-duniter`** · **`astro-g1-runtime`** | Client Duniter v2s natif (subxt) : soldes, virements avec mémo, bascule de nœud avant envoi ; métadonnées du runtime isolées. |
| **`astro-economy`** | Registre Ğ1-N² (kind 30852, chaîné par `prev`), règle des 3×1/3, Love Ledger. |
| **`astro-lifecycle`** | Découverte des identités (NODE, Capitaine, MULTIPASS), cycle hebdomadaire loyer → grâce → suspension, déménagement, clé de secours Shamir 2 sur 3. |
| **`astro-plugins`** | Runtime WebAssembly isolé (wasmtime). |
| **`astro-ui`** | Façade web : ACME, TLS, reverse proxy, application embarquée (sans framework, PWA, coque < 20 Ko gzip), pages publiques, API `/api/v1`, `/health`, SSE. |

Le binaire racine (`src/`) assemble le tout ; `src/lib.rs` expose ses modules pour les tests de bout en bout.

---

## 4. Construire et tester

```bash
# Prérequis : Rust ≥ 1.85, un compilateur C (SQLite et libwebp sont compilés depuis leurs sources)
cargo build --release                       # → target/release/astroportd
cargo build --release --features onnx       # avec l'inférence locale (visages, embeddings)

./build-all.sh --list      # chaîne d'outils de chaque cible, et comment installer ce qui manque
./build-all.sh             # dist/astroportd-<version>-<cible>.tar.gz + SHA256SUMS
./build-all.sh --strict    # échoue si une cible est sautée (CI)
```

Ce que la CI exécute, à reproduire avant toute pull request :

```bash
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo clippy -p astroportd -p astro-facecloud -p astro-index --all-targets --features onnx --locked -- -D warnings
cargo test --workspace --locked
bash -n install.sh install_sudo.sh build-all.sh && shellcheck install.sh install_sudo.sh build-all.sh
for t in crates/astro-ui/tests/*.mjs; do node "$t"; done
python3 docs/build_guide.py --check         # guide HTML à jour (pip install markdown-it-py)
```

Aucun test ne requiert de service externe : base, WebDAV et pairs libp2p sont réels mais locaux.

**Conventions** : montants en centimes entiers ; une erreur n'est jamais avalée (elle remonte, ou elle est dite sur le
bus d'activité) ; les décisions qui dépendent de l'état se prennent **en SQL** ; toute surface exposée a son test hostile
(SSRF, boucle infinie, dépassement mémoire) ; `unsafe` interdit (hors `sqlite-vec`), `unwrap` interdit en production ;
code, commentaires et documentation en français.

---

## 5. Toutes les commandes

| Commande | Rôle |
|---|---|
| `init [--mode origin\|zen] [--found-constellation] [--join <jeton>] [--systemd] [--yes]` | Assistant : Y-Level, ORIGIN/ẐEN, PAF, dossiers, service systemd. |
| `serve` | Le démon. Options utiles : `--domain` + `--acme-email` (HTTPS), `--public-domain` (derrière un proxy), `--swarm-key`, `--p2p-expose nom=hôte:port`, `--ollama`, `--max-accounts`, `--register-per-hour`, `--no-register`, `--face-engine`/`--face-models`, `--embed-models`, `--lat/--lon`, `--no-nightly`. |
| `doctor` · `face-check` · `embed-check` | Diagnostic en lecture seule (code 1 si échec) ; vérification réelle de l'inférence locale. |
| `ylevel [--verify]` | Correspondance SSH ↔ PeerID ↔ Ğ1 ↔ Nostr. |
| `multipass create <email> [--captain] [--lang] [--json]` · `list` · `card <email> [--part N] [--svg]` · `recover -` | Comptes, cartes de secours, restauration. |
| `parrainer [--hours N]` | Jeton à usage unique pour faire entrer une station dans sa constellation. |
| `token <email>` | Jeton de connexion (web, WebDAV). |
| `replay --db … [--from ws://… --hosted <hex>] [--blobs … --secrets …]` | Reconstruit la base (et uCloud) depuis les événements. |
| `tunnel list \| connect \| close \| policy \| mode \| allow \| revoke \| share \| unshare` · `route <service> --mode closest` | Services ouverts par la constellation et politique de tunnels, via `control.sock`. |
| `id` · `swarm peers \| connect \| addrs listen` · `bootstrap` · `p2p ls \| forward \| close` | Même nomenclature que `ipfs`. |
| `repo stat \| gc \| verify` | Bassin de blobs : poids, orphelins, intégrité. |
| `swarm-key` | Clé d'un réseau privé de constellation. |
| `plane-setup list \| add \| remove` · `planes [--check]` · `--plane <nom>` | Plusieurs UPlanet dans un seul processus. |
| `pool-status [--sweep]` · `bridge-copy --same-person [--cross-swarm]` | Bassin partagé entre plans ; adoption sans copie. |
| `pull-webdav <compte> list \| add \| remove \| run` | Reprise depuis Nextcloud, ownCloud… |
| `models list \| add \| remove \| bench` | Catalogue des modèles locaux. |
| `plugin [--dir plugins] list \| call <nom> '<json>'` | Extensions Wasm. |
| `relay` · `dav` | Le relai seul, le WebDAV seul. |

---

## 6. La nuit de 20h12

Les travaux d'entretien se calent sur **20h12 solaire vrai** de la station (équation du temps de Spencer, ~30 s). Le
démon refuse une horloge antérieure à 2026 (Raspberry Pi sans pile RTC). Il faut les coordonnées (`--lat/--lon` ou la
position du NODE). Chaque étape est indépendante ; un échec est dit sur le bus et la nuit continue :

1. garde-fou RTC et verrou du jour ;
2. snapshot `VACUUM INTO` (7 conservés) ;
3. purges (réclamations, liens d'invités, événements NIP-40 expirés, journal DAV) ;
4. cycle RÊVE de la mémoire de BRO ;
5. sauvegarde privée chez les amis, puis contrôle d'intégrité des blobs ;
6. vigie « vie privée » de `/Public` (prévient, ne modifie rien) ;
7. retour des billets papier expirés à leur émetteur ;
8. clôture hebdomadaire 3×1/3 (rattrapage jusqu'à 5 semaines) ;
9. santé économique (kind 30850, `d = economic-health`).

Une station éteinte à 20h12 rattrape au démarrage la nuit du jour et celle de la veille.

### Sauvegarde k-sur-n entre amis

Conception complète, décisions et écarts : [`docs/design/SAUVEGARDE_RS.md`](design/SAUVEGARDE_RS.md).

- **Formats** (figés, vecteurs dans `crates/astro-erasure/tests/vectors.rs` et `crates/astro-dav/tests/backup_pack.rs`,
  recoupés par `crates/astro-dav/tests/vector6.py`) : fragment **ARSF v1** (en-tête de 72 octets : magie, version, code
  Cauchy GF(2⁸) `0x11B`, `k`, indice, `packet_id`, longueurs, SHA-256 du paquet) ; paquet = flux de blobs rechiffré en
  UENC v2 (16 Mio) ; **index** JSON chiffré UENC v2 par une clé HKDF du secret du compte (`astro-backup-index-v1`),
  copié en entier chez chaque gardien, CID porté par le manifeste (`backup_index`).
- **Opérations** (`t = astro-backup`) : `frag` (un fragment, ou l'index avec `["kind","index"]`), `frag-ack`, `frag-drop`
  (compactage, réparation, retrait) ; `["rs","1"]` dans `accept` ; `item`/`drop` restent pour les copies complètes.
- **Code** : `crates/astro-erasure` (corps, code, ARSF), `astro_dav::backup_pack` (paquets, index, extraction),
  `src/private_backup/rs.rs` (envoi, restauration), `repair.rs` (présence, possession, réparation), `compact.rs`
  (compactage, migration des copies complètes). Tables d'état local : `backup_packets`, `backup_blobs`, `backup_frags`,
  `backup_compact`, `backup_index`, `blob_copies` (caches de l'index : une restauration les reconstruit).

---

## 7. Sécurité

- **Pare-feu social** : contenus privés servis aux seuls membres authentifiés du cercle, selon le mode (Forteresse :
  hébergés et amis réciproques ; Village : + N¹ et N² ; Place publique : kinds 0 et 1 ouverts). Un gift wrap NIP-59
  d'un inconnu n'est accepté que vers un compte hébergé, avec limite de débit.
- **Authentification** : NIP-98 (anti-rejeu persistant, fenêtre 120 s, liaison URL + méthode + corps), code PASS
  (verrou après 5 échecs) ou jeton `HMAC(secret de station, compte)`. Aucun cookie.
- **`/api/v1/locate`** : 20 recherches par minute et par adresse, ne révèle que le DID public.
- **SSRF** : `#upload <URL>` ne joint que des adresses publiques, vérifiées avant connexion et à chaque redirection.
- **Application web** : CSP `default-src 'none'`, aucun script ni style en ligne, aucun `innerHTML`.
- **Tunnels** : politique par service (fermé, nominatif, Capitaines amis, réseau) ; SSH fermé par défaut en ẐEN ;
  sessions journalisées et coupées au retrait.

---

## 8. Écrire une extension WebAssembly

Une extension est un `.wasm` déposé dans `plugins/`, exécuté dans une **bulle** :

| Garantie | Mécanisme |
|---|---|
| Aucune capacité | Ni WASI ni fonction hôte : un module qui *importe* quoi que ce soit est refusé au chargement. |
| Calcul borné | 50 millions d'instructions par appel (`OutOfFuel`). |
| Mémoire bornée | 16 Mio par instance ; entrée et sortie ≤ 1 Mio. |
| Sans état, déterministe | Instance neuve à chaque appel, ni horloge ni aléa. |

**ABI v1** (JSON UTF-8 dans la mémoire linéaire) :

| Export | Signature | |
|---|---|---|
| `memory` | mémoire | |
| `alloc` | `(len: i32) -> i32` | réserve `len` octets |
| `plugin_call` | `(ptr: i32, len: i32) -> i64` | `(adresse << 32) \| longueur` d'une enveloppe `{"ok": …}` ou `{"error": "…"}` |
| `dealloc` | `(ptr: i32, len: i32)` | facultatif |
| `plugin_manifest` | `() -> i64` | facultatif : `{"version","description"}` |

```rust
#[unsafe(no_mangle)]
pub extern "C" fn plugin_call(ptr: i32, len: i32) -> i64 {
    let input = unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) };
    pack(envelope(handle(input)))   // handle : JSON entrant → Result<Value, String>
}
```

```bash
rustup target add wasm32-unknown-unknown
./plugins/build.sh                         # → plugins/resonance.wasm
astroportd plugin --dir plugins call resonance '{"fn":"resonance","a":"pain","b":"pain"}'
```

Exemple complet : `plugins/resonance` (crate `cdylib` hors workspace, `opt-level = "z"`, `panic = "abort"`). Modules
hostiles de démonstration : `crates/astro-plugins/tests/runtime.rs`.

---

## 9. Films et musique : la copie privée

astroportd ne traite pas les médias : le plugin système
[`media`](https://github.com/papiche/astroportd-plugins/tree/main/media) sonde, réencode et dépose dans l'uCloud du
membre (`/Vidéos/Films`, `/Vidéos/Séries/<série>/Saison NN`, `/Musique/<artiste>/<album>`), pose l'attestation de copie
privée (CPI, art. L. 122-5, 2°) et publie l'événement **NIP-71** (kind 21 ou 22, `imeta` NIP-94, tag NIP-70 `["-"]`) ;
un mix devient une playlist NIP-51 kind 30005.

Le relai ne livre ces kinds 21/22/30005 qu'à l'**auteur** et à ceux qu'il **suit** (N¹, lu dans son dernier kind 3),
jamais au reste du N². Le fichier reste chiffré ; pour qu'un ami regarde, le titulaire envoie un lien `/s/<slug>#clé`.
Le titulaire retrouve tout dans *Fichiers › Médiathèque* (lecture en flux `/play/<jeton>`, 12 h, `Range`).

---

## 10. Où vit chaque fonction

| Fonction | Code | Guide |
|---|---|---|
| Accueil des membres | `web/m-home.js` | [étape 7](GUIDE.md#etape-7) |
| Cockpit du Capitaine | `web/m-cockpit.js`, `tests/cockpit_rules.mjs` | [étape 7b](GUIDE.md#etape-7b) |
| Écluses et modes | `src/locks.rs`, `astro_db::station_mode` | [écluses](GUIDE.md#ecluses) |
| Portail `/stations` | `src/locate.rs`, `web/stations.js` | [annuaire](GUIDE.md#annuaire) |
| Conversations et visio | `src/chat.rs`, `src/turn.rs`, `src/presence.rs` | [visio](GUIDE.md#visio) |
| Coffre et agents (MCP), navigateur prêté | `src/agents.rs`, `src/agent_browser/` ([conception](design/SESSION_AGENT.md)) | [étape 8bis-b](GUIDE.md#etape-8bis-b) |
| Naviguer ensemble (co-surfing) | `src/cosurf.rs`, `web/m-cosurf.js`, `web/m-screen.js` | [cosurf](GUIDE.md#cosurf) |
| Site public et vigie | `src/public_site.rs`, `src/privacy_watch.rs` | [étape 8bis-m](GUIDE.md#etape-8bis-m) |
| Bibliothèque et recherche | `src/library.rs`, `src/search_*.rs` | — |
| Ranger et reprendre | `src/organize.rs`, `src/webdav_pull.rs` | [étape 8bis-d](GUIDE.md#etape-8bis-d) |
| Sauvegarde privée, clé de secours | `src/private_backup.rs`, `astro-lifecycle` | [secours](GUIDE.md#secours) |
| Intégrité du stockage | `src/integrity.rs` | — |
| Tunnels, assistance | `src/tunnel_policy.rs`, `src/assist.rs` | [étape 8ter](GUIDE.md#etape-8ter) |
| Adresses DNSLink | `src/dnslink.rs` | [étape 9b](GUIDE.md#etape-9b) |
| Économie, billets, OpenCollective | `src/settlement.rs`, `src/billet.rs`, `src/oc_emission.rs` | [partie 3](GUIDE.md#partie-3) |
| Plusieurs UPlanet | `src/plane.rs`, `src/bridge.rs` | [étape 8bis-i](GUIDE.md#etape-8bis-i) |
| Mémoire de BRO | `crates/astro-bro`, `src/member_ai.rs` | — |

---

## 11. Écrire un client compatible

astroportd ne parle que des protocoles ouverts : un client Nostr, un client WebDAV ou une application de contacts
existante fonctionne déjà. Cette section dit ce qu'il faut savoir pour aller plus loin. La référence exécutable reste
le code et ses tests (cités ici) : en cas de doute, ils font foi.

### Une seule adresse

Tout passe par **la même adresse et le même port** (8090 en local, 443 en HTTPS) :

| Le client envoie… | Il reçoit… |
|---|---|
| une requête WebSocket | le relai Nostr (NIP-01) |
| `Accept: application/nostr+json` | le document NIP-11 du relai (kinds acceptés, limites) |
| une requête HTTP ordinaire | l'application web, l'API `/api/v1`, WebDAV, CardDAV (`/_/Contacts`), CalDAV (`/_/Calendrier`) |

### S'authentifier

- **WebSocket** : le relai envoie un défi `AUTH` ; répondez par un événement kind **22242** signé (NIP-42). Sans lui, seuls
  les kinds publics se lisent : 0, 1, 3, 30023, 30800, 30850, plus présence et sonnerie visio (24315, 24316). La liste
  exacte est `public_kinds` dans `crates/astro-relay/src/policy.rs`.
- **API HTTP** : en-tête `Authorization: Nostr <base64>` portant un kind **27235** (NIP-98) lié à l'URL exacte, à la
  méthode et à l'empreinte du corps (tag `payload`). Fenêtre de 120 s, chaque événement n'est accepté qu'une fois.
- **WebDAV / CardDAV / CalDAV** : authentification Basic avec un **jeton d'appareil** (créé dans *Moi › Mes appareils*
  ou par `astroportd token <email>`), révocable et confiné à un compte.

L'écriture sur le relai n'est permise qu'aux membres du cercle, selon le mode de la station. Une exception : un
**gift wrap** (kind 1059) destiné à un compte hébergé est toujours accepté, sans authentification. C'est la porte
d'entrée pour écrire à quelqu'un depuis n'importe où.

### Les kinds du projet

Les kinds standard (0, 1, 3, 5, 7, 13, 14, 1059, 21, 22, 30005, 30023, 30078) suivent leurs NIP. Ceux qui sont propres à
UPlanet (constantes dans `crates/astro-nostr-proto/src/lib.rs`) :

| Kind | Usage | Lecture |
|---|---|---|
| **30800** | DID du compte : clés Ğ1/IPNS, station d'accueil (`home:station`). Sert à « retrouver ma station ». | publique |
| **30850** | Balise de station (services, adresses P2P) ; `d = economic-health` : santé économique. | publique |
| **30851** | Émission de Ẑen. | cercle |
| **30852** | Registre Ğ1-N² : écritures chaînées par `prev`. Un embranchement gèle le compte. | cercle |
| **31903** | Paramètres d'application ; `d = plane-link` : lien doublement signé entre les clés d'une même personne dans deux UPlanet. | cercle |
| **25050** | Signalisation d'appel visio (offre, réponse, candidats ICE). | cercle |
| **24315 / 24316** | Présence visio / sonnerie (éphémères). | publique |
| **7** avec `+N` | Réaction qui transfère N Ẑen à l'auteur (micro-don). | — |
| **21 / 22 / 30005** | Vidéos, morceaux, playlists en copie privée : réservés à l'auteur et à son N¹. | N¹ |

### Messages privés

Utilisez **NIP-17** : un kind 14 scellé (kind 13, NIP-44 v2) puis enveloppé (kind 1059, NIP-59). Le kind 4 n'est gardé
que pour la compatibilité. Implémentation et vecteurs : `crates/astro-crypto` (`tests/nip44_interop.rs`).

### Lire un fichier partagé

Un lien `/s/<slug>#<clé>` désigne un fichier chiffré en **UENC v2** : flux de blocs de 64 Kio (AES-256-GCM ou
ChaCha20-Poly1305), en-tête de 14 octets servant de données associées, préfixe de nonce aléatoire de 7 octets. Le
client télécharge le blob (lecture partielle `Range` possible), puis déchiffre bloc par bloc avec la clé du fragment,
qui ne doit **jamais** partir sur le réseau. Le vecteur de référence est `crates/astro-crypto/tests/uenc_v2_vector.rs` :
un client compatible doit le reproduire octet pour octet.

### Dériver les clés d'un MULTIPASS

Un client qui recrée les clés à partir des secrets du compte suit `KeyFormula` (`crates/astro-core/src/formula.rs`) :
le poivre est **isolé par constellation**, `PEPPER_<sha256(UPLANETNAME)[:16]>`, avec `UPLANETNAME` = 64 zéros en ORIGIN.
Les vecteurs de `crates/astro-core/tests/legacy_keygen.rs` vérifient la compatibilité avec les scripts d'Astroport.ONE.

### Parler à une station en P2P

Les stations d'une constellation ẐEN forment un réseau libp2p privé (clé partagée `swarm.key`). Un service exposé s'ouvre
sur le protocole `/x/<service>-<PeerID>` (« DRAGON »), compatible avec `ipfs p2p`. Le Capitaine décide, service par
service, qui peut l'ouvrir.

### Vérifier sa compatibilité

1. Lancez une station locale : `astroportd serve --db /tmp/t.db --blobs /tmp/blobs`.
2. Créez un compte : `astroportd multipass create test@exemple.org --json`.
3. Rejouez les vecteurs de compatibilité (UENC v2, NIP-44, dérivation) dans votre langage.
4. Testez l'authentification NIP-42 et NIP-98, puis un aller-retour NIP-17 entre deux comptes.

Un client réussi ? Ouvrez une [issue](https://github.com/papiche/astroportd/issues) pour qu'il soit cité.
