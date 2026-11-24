# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

`astroportd` est le cœur souverain d'Astroport.ONE / UPlanet réécrit en Rust : un seul binaire (relai Nostr, uCloud chiffré WebDAV, FaceCloud, agent BRO, économie en Ẑen, maillage P2P, interface web). Workspace Cargo de 19 membres (le binaire `astroportd` + `crates/*`), Rust 2024 (≥ 1.85), AGPL-3.0. Le code, les commentaires et la documentation sont en **français** : écrivez-les en français, avec les accents.

## Commandes

La CI (`.github/workflows/ci.yml`, chaîne figée 1.94.0) exécute exactement ceci ; reproduisez-le avant de livrer :

```bash
cargo clippy --workspace --all-targets --locked -- -D warnings      # aucun avertissement toléré
cargo clippy -p astroportd -p astro-facecloud -p astro-index --all-targets --features onnx --locked -- -D warnings
cargo test --workspace --locked
bash -n install.sh install_sudo.sh build-all.sh && shellcheck install.sh install_sudo.sh build-all.sh
for t in crates/astro-ui/tests/*.mjs; do node "$t"; done            # règles de l'interface
python3 docs/build_guide.py --check                                  # guide HTML à jour
```

**Guide publié** (GitHub Pages sert `docs/`) : `docs/guide.html` (guide du Capitaine) et les annexes `docs/*.html` sont **générés** depuis leurs `.md` — ne jamais les éditer à la main (seul l'en-tête de `guide.html`, avant `<nav class="toc"`, se modifie à la main). `docs/index.html` est la page d'accueil, écrite à la main (grand public et futurs Capitaines). Après toute modification de GUIDE.md : `python3 docs/build_guide.py` (la CI vérifie avec `--check` ; dépendance : `pip install markdown-it-py`). Les règles de l'interface (cockpit, accueil) se testent avec `node crates/astro-ui/tests/*.mjs`. Les **romans-photos** du guide (`docs/img/roman-*.webp`) sont de vraies captures de l'interface sur la station simulée `tests/browser/mock_station.py` (partagée avec `smoke_ui.py`) : après un changement d'un écran raconté, relancez `~/.astro/bin/python docs/roman/roman_photo.py`. La Médiathèque a son test navigateur : `~/.astro/bin/python tests/browser/smoke_media.py [dossier]` (même station simulée), la recherche dans le texte lu aussi : `tests/browser/smoke_search.py`, Mon blog (éditeur, partage en favori `.url`, réponse par adresse Nostr, « Chez mes amis ») : `tests/browser/smoke_blog.py`, le déménagement Google/iCloud (vraie archive Takeout en deux parties) : `tests/browser/smoke_move.py`. Payer / Recevoir / Se rencontrer par QR et Mes cercles (caméra simulée qui filme un vrai QR ; rencontre prouvée ou nomination, kind 30855 ; niveaux de proximité privés, kind 30078 chiffré, `src/proximity.rs` ; toile de confiance N² des rencontres prouvées, `astro_economy::wot`, `GET /api/v1/me/wot` ; `docs/design/PAYER_QR.md`) : `tests/browser/smoke_pay.py`. Partages du Capitaine (Réseau › Partages, `m-services.js` : services ouverts par les amis, ce que la station prête, ports TCP qui écoutent ici lus par `src/local_ports.rs`, entraide sans ligne de commande) : `tests/browser/smoke_services.py`. Corpus d'essai pour vérifier une vraie station (photos libres de Wikimedia Commons dont des portraits de personnalités pour FaceCloud, OCR, PDF, vidéos, dates et lieux fictifs ; `LISEZMOI.md` dit quoi chercher) : `~/.astro/bin/python tests/corpus/fabriquer_corpus.py [dossier] [--envoyer /Essais/Recherche]` (jeton par `ASTRO_TOKEN`, jamais en argument). L'OCR réel (PP-OCRv6) : `ASTRO_OCR_DIR=<modèles> cargo test -p astro-index --features onnx --test ocr_real -- --ignored`. Les schémas animés `docs/img/{ecluses,connexion,plans}.svg` ont leurs styles préfixés (`.ek`, `.cx`, `.pl`) car ils sont insérés tels quels dans guide.html.

```bash
cargo test -p astro-dav                       # un crate
cargo test -p astroportd --test ui_e2e        # un fichier de tests/ du binaire
cargo test -p astro-core solar::              # un module / un test par nom
cargo build --release                         # → target/release/astroportd (lto fat, strip)
cargo build --release --features onnx         # avec l'inférence faciale locale (ONNX Runtime chargé à l'exécution)
./build-all.sh --list | --strict | --targets <t1,t2> | --debug | --test   # archives multi-cibles dans dist/
astroportd doctor --db uplanet.db --game-dir ~/.zen/game                 # diagnostic en lecture seule
```

Les tests ont besoin d'un compilateur C (SQLite et libwebp sont compilés depuis leurs sources). Environ 930 tests (`cargo test --workspace`). Aucun test ne requiert de service externe : base, WebDAV et pairs libp2p sont réels mais locaux.

Les plugins Wasm (`plugins/resonance`) sont **hors workspace** et se compilent pour `wasm32-unknown-unknown` avec `./plugins/build.sh` (produit `plugins/resonance.wasm`, versionné).

## Lints et conventions imposés (`[workspace.lints]`)

- `unsafe_code = deny` (seule exception : l'initialisation de `sqlite-vec`), `clippy::unwrap_used = deny`, `expect_used`/`panic` en warn, `missing_docs = warn`. Les tests autorisent `unwrap` via `#![cfg_attr(test, allow(clippy::unwrap_used))]` en tête du `lib.rs` du crate — pas dans le code de production.
- **Tout montant est un entier de centimes** (`astro_core::Amount`) ; jamais de flottant pour la monnaie.
- Une erreur n'est jamais avalée : elle remonte, ou elle est dite sur le bus d'activité (`astro-core::activity`, flux SSE).
- Les décisions qui dépendent de l'état se prennent **en SQL**, pour que le temps réel et le `replay` convergent (projections indépendantes de l'ordre d'arrivée).
- Les filtres Nostr sont bornés (`MAX_LIST_LEN`, `MAX_TAG_FILTERS`, `MAX_TAG_VALUES` dans `astro-nostr-proto`) et un pair P2P ne peut occuper qu'une fraction des flux d'un tunnel exposé (`astro-p2p::tunnel`).
- Un test hostile (SSRF, boucle infinie, dépassement mémoire) accompagne toute surface exposée.
- Profil dev : les dépendances sont en `opt-level = 2` (scrypt/PBKDF2 resteraient trop lents sinon). En release, `astro-facecloud`, `image` et `ort` sont en `opt-level = 3`, le reste en `"s"`.

## Architecture (vue transversale)

**Principe central : la vérité n'est pas dans la base.** Les sources de vérité sont les événements Nostr (identités, social, registre Ğ1-N² kind 30852, manifestes) et les blobs IPFS chiffrés. `uplanet.db` (SQLite WAL + `sqlite-vec`, index FTS5 `nostr_fts` tenu par triggers pour NIP-50) n'est qu'un **cache rejouable** reconstruit par `astroportd replay`. Toute nouvelle table doit être classée *projection* (rejouable, alimentée par `astro-sync::Projector`) ou *état local* (identités hébergées, files, verrous) dans `crates/astro-db/src/schema.sql`.

**Écriture SQLite** : un acteur unique (un thread, file bornée, une transaction par message, `catch_unwind`) dans `astro-db::writer`, plus un pool de lecteurs (`ReaderPool`). Ne jamais ouvrir de connexion d'écriture ailleurs.

**Graphe de dépendances** (du bas vers le haut) :
- Socle : `astro-core` (formules de crédentiels, KDF, `Amount`, horloge solaire 20h12, bus d'activité), `astro-crypto` (UENC v2, NIP-44/59/98), `astro-nostr-proto` (événements, filtres, kinds), `astro-db`, `astro-erasure` (GF(2⁸) partagé avec Shamir, code de Cauchy et fragments ARSF de la sauvegarde k-sur-n, `docs/design/SAUVEGARDE_RS.md`).
- Services : `astro-relay` (relai Nostr, pare-feu social NIP-42 cercles N1/N2 selon le mode de la station), `astro-sync` (`Projector`, `replay`, crawl N²), `astro-dav` (WebDAV/CardDAV/CalDAV sur uCloud chiffré, tiroir système `/_/{Coffre,Contacts,Calendrier}`, bassin partagé entre plans, liens `/s/<slug>#clé`, invitations `/i/<jeton>`, miniatures ; les films NIP-71 sont publiés par le plugin externe `media`), `astro-facecloud`, `astro-bro`, `astro-index` (recherche multimédia : trait `Indexer` local ou délégué à des Capitaines amis par le service `ai-index`, fermé par défaut, écoute en boucle locale seulement ; partage du calcul : file équitable par station (`sched`, tour de rôle, 503 + `Retry-After`, charge annoncée sur `GET /v1`), station reconnue par le carnet des pairs du tunnel (`astro_p2p::tunnel::PeerBook`), jamais par un en-tête ; côté demandeur, la moins chargée des stations acceptées par le membre et pause jusqu'au délai donné ; catalogue des modèles, `astroportd models`), `astro-p2p` (libp2p, tunnels DRAGON `/x/<service>-<PeerID>`), `astro-economy` (3×1/3, Love Ledger), `astro-lifecycle`, `astro-duniter` (subxt ; `astro-g1-runtime` isole la métadonnée générée), `astro-plugins` (wasmtime, bulle sans capacité).
- `astro-ui` : façade HTTPS (ACME, reverse proxy, `/health`, SSE, API `/api/v1`) qui embarque l'application web de `crates/astro-ui/web/` (HTML/CSS/JS sans framework, CSP `default-src 'none'`, aucun script/style en ligne, pas d'`innerHTML` ; budgets de taille vérifiés par les tests de `crates/astro-ui/src/web.rs`).
- Le binaire racine (`src/`) assemble le tout : `main.rs` (CLI clap : `init`, `serve`, `planes`, `plane-setup`, `multipass`, `doctor`, `replay`, `tunnel`, `parrainer`, `token`, `bridge-copy`, `pool-status`, `plugin`, `ylevel`, `swarm-key`…), `serve.rs` (câblage du démon) et `serve/` (`p2p`, `face`, `tasks` : tâches de fond, `admin`, `ui` : raccordement de l'interface), `ui_backend/` (implémentation de l'API pour `astro-ui` : une structure par domaine — `ucloud`, `faces`, `coop`, `social`, `account`, `onboarding`, `station` — assemblées par `StationUi::into_router` ; côté `astro-ui`, `api/` a un trait et un module de routes par domaine, montés par `api::Routes`), `solar_cron.rs` (nuit de 20h12 : snapshot `VACUUM INTO`, purges, RÊVE, clôture hebdomadaire 3×1/3, kind 30850 `economic-health`), plus un module par fonction (`register`, `invites`, `sharing`, `share_inbox`, `tunnel_policy` (politique des tunnels du Capitaine : mode par service, SSH fermé par défaut sauf ORIGIN, bloc `authorized_keys` balisé), `dnslink` (adresses DNSLink OVH de la station et des MULTIPASS, compatibles `ovh.me.sh`), `assist` (demandes d'assistance entre Capitaines amis : l'accès n'est ouvert qu'à l'ami qui accepte), `forwards` (accès local à chaud vers le service d'un ami, ouvert par `astroportd tunnel connect` ; il n'y a pas de console SSH dans le navigateur), `control` (socket Unix `control.sock` : la CLI `astroportd tunnel …`/`route` parle au démon qui tourne, sections déclarées seulement, propriétaire vérifié) + `swarm_route` (choix de la station qui ouvre un service : annuaire des balises + `ask_services`, accès via `forwards`), `library` (requêtes de la bibliothèque : déjà-là, souvenirs, chronologie, recherche — nom, description et texte lu dans le fichier —, doublons), `search_index` (travailleur de la recherche multimédia, deux tâches : lire le texte des images et PDF (OCR, dans `search_text`), ici ou chez les Capitaines amis que le membre a acceptés — `index_delegation` — ; et regarder les photos et les vidéos (un vecteur SigLIP 2 par photo et par image clé de vidéo — jusqu'à 12, `astro_index::video`, ffmpeg lisant la vidéo à une adresse en boucle locale à jeton unique, format imposé par le type MIME — dans `search_frames` (`t_ms`) + `search_vision_vectors`), **ici seulement** : une station sans le modèle n'a pas de recherche par l'image. La requête devient un vecteur sur place, corrigé de l'attirance des images (`astro_index::siglip::debiased`), et seules les photos qui sortent du lot du membre sont gardées (`stand_out`, z ≥ 3,5, 14 photos au moins ; `library::seen`) : la recherche ne dépend jamais du réseau), `search_query` (compréhension de la recherche tapée : personnes reconnues, villes de plus de 15 000 habitants embarquées — GeoNames CC BY 4.0, `src/data/cities.tsv.gz`, retenues seulement là où le membre a des photos —, mois et années ; une interprétation ajoute une façon d'être trouvé sans jamais retirer le mot ; seuls les mots « à voir » vont à la recherche par l'image ; `library::search` cherche chaque terme partout, classe les meilleurs (`best`) et propose `maybe` quand rien n'est sûr ; une seule case dans Fichiers, écran `m-find.js`), `search_pack` (paquet d'index de chaque membre, `/_/Recherche/index.astroidx` dans son uCloud, écrit au repos au plus toutes les 20 h, emporté par `private_backup`, relu après restauration ; jamais réécrit tant qu'un paquet revenu n'est pas entièrement repris ; le membre peut mettre son analyse en pause — `kv` search_pause, lu par `allowed_sql!` — et effacer ce qui a été calculé : calculs, puis paquet, puis oubli du paquet), `models` (`astroportd models` : `list`, `add`, `remove`, `bench`), `integrity` (contrôle nocturne des blobs), `kubo_mirror` (ORIGIN : **le disque d'abord, Kubo en miroir** — `AutoBlobStore` écrit toujours sur le disque ; le miroir rapatrie, après vérification, les blobs restés dans Kubo, publie le reste par référence au fichier (filestore, `nocopy`) ou par copie quand Kubo ne relit pas le disque, et désépingle ce qui n'est plus référencé ; table `kubo_mirror` (état local), jamais en ẐEN ; `install_sudo.sh` installe kubo et active le filestore), `private_backup` (sauvegarde chiffrée chez un ami qui accepte, envoyée à 20h12), `island` (ISLAND-1 « Ğ1 en différé », `docs/design/ISLAND.md` : pendant une coupure de Duniter, promesses de Ğ1 (30852 `t=island`, crédit 30854 du NODE) plafonnées au dernier solde connu plus les DU, réglées en vraie Ğ1 au retour ; jamais d'ouverture seule sans `--island-after`), `locks` (écran Écluses : mode de la station), `locate` (« Retrouver ma station » : `/api/v1/locate`, portail `/stations`), `privacy_watch` (vigie de `/Public` à 20h12), `bridge` + `plane` (plans multiples, passages entre plans), `person_keys` (une personne dans plusieurs UPlanet), `zen_react`, `upload_worker`…). `src/lib.rs` expose ces modules pour que `tests/` les teste de bout en bout.

**Deux constellations** : pas de `swarm.key` = ORIGIN (`UPLANETNAME` = 64 zéros), `swarm.key` présente = ẐEN (réseau P2P privé). Un `serve` = une constellation ; `astroportd planes` sert ORIGIN et des UPlanet privées dans un seul processus (même NODE, un `serve` complet par plan : base, comptes, P2P, port ; `--front` route par nom d'hôte, `--plane` choisit le plan d'une commande). Entre plans : un plan privé ne verse jamais vers ORIGIN, deux plans privés seulement avec `bridge-copy --cross-swarm` (`bridge::check_passage`), et en bassin partagé une station ne sert que les blobs que sa base référence (`astro-p2p` `CidGate`). Les clés d'un MULTIPASS sont dérivées d'une formule **isolée** `(SALT, PEPPER_<sha256(UPLANETNAME)[:16]>)` : le même couple donne d'autres clés dans une autre constellation (`KeyFormula` dans `astro-core::formula`, vecteurs de compatibilité dans `crates/astro-core/tests/legacy_keygen.rs`).

**Y-Level** : la clé SSH est hachée en SALT/PEPPER → scrypt → graine ed25519 d'où viennent clé SSH jumelle, PeerID IPFS, clé Ğ1 et clé Nostr (`astroportd init` / `ylevel`).

**Données de la station** : clés et identités sont découvertes dans `~/.zen/game` (`--game-dir` ou `ASTRO_GAME_DIR`) par `astro-lifecycle::discovery` ; secrets en fichiers `0600` vérifiés par `doctor`.

## Tests

Les tests d'intégration du binaire sont dans `tests/` (`ui_e2e.rs`, `full_cycle.rs`, `upload_worker.rs`…) et utilisent une vraie base, un vrai WebDAV et de vrais pairs libp2p ; chaque crate a ses propres `tests/` (dont `astro-crypto/tests/nip44_interop.rs` et `uenc_v2_vector.rs`, des vecteurs de compatibilité à ne pas modifier pour faire passer un test). `astro-facecloud` avec `--features onnx` n'est que compilé en CI : la bibliothèque `libonnxruntime` est chargée à l'exécution.

## Dépôt

- Ce dépôt fait partie du workspace `AAA/` (voir le `CLAUDE.md` parent pour l'écosystème Astroport.ONE / UPassport / NIP-101) ; ici, astroportd remplace strfry + `writePolicy`, `gcli` et plusieurs scripts bash d'Astroport.ONE.
- Ne jamais ajouter de `Co-Authored-By` dans les commits (règle du workspace parent, qui prime sur le reste).
- Les fichiers `*.db`, `blobs/`, `*.key`, `swarm.key`, `*.dunikey`, `secrets/` sont ignorés par git : ne les ajoutez jamais.
- La version est dupliquée dans `Cargo.toml` (paquet racine et `[workspace.package]`) : mettez les deux à jour ensemble.


# MISSION CONTEXT & SYSTEM DIRECTIVES : ASTROPORT.ONE / ASTROPORTD

Tu es l'Architecte Principal et Lead Product Engineer d'ASTROPORT.ONE, un écosystème logiciel souverain, décentralisé et sans tiers de confiance, propulsé par le démon tout-en-un écrit en Rust : `astroportd`.

Ton rôle est d'analyser, maintenir, sécuriser et faire évoluer ce codebase avec une rigueur absolue, en conjuguant l'excellence technique des systèmes distribués et l'élégance ergonomique d'un produit grand public.

---

### 1. LES INVARIANTS DOGMATIQUES DU SYSTÈME (À RESPECTER SANS DÉROGATION)

1. **Nostr + IPFS sont les SEULES sources de vérité :**
   La base locale SQLite (`uplanet.db` avec WAL, `sqlite-vec` et FTS5) n'est qu'un **cache de projection rejouable et jetable**. Toute mutation d'état critique doit pouvoir être reconstruite à froid via `astroportd replay`.
2. **Arithmétique financière entière stricte :**
   AUCUN type flottant (`f32`, `f64`, `number` JS avec virgule flottante) n'est toléré pour les montants. Toute valeur monétaire (Ẑen ou Ğ1) est stockée et manipulée sous forme d'entiers 64 bits en centimes (`Amount`).
3. **Cryptographie & Identité Y-Level :**
   - Racine Capitaine : Clé SSH `~/.ssh/id_ed25519` → SHA-512 → Salt/Pepper → Scrypt → Graine Ed25519 maîtresse (PeerID IPFS 12D3Koo = Ğ1 = Nostr secp256k1 via SHA-256).
   - Membre MULTIPASS : Credentials isolés par constellation (`sha256(UPLANETNAME)[..16]`).
   - Tolérance aux pannes : Partage de secret de Shamir SSSS 2-sur-3 (Part 1 QR imprimé, Part 2 station locale, Part 3 amis N¹ via NIP-59).
4. **Étanchéité des Constellations :**
   - `ORIGIN` (Bac à sable public) : `UPLANETNAME` = 64 zéros. 1 Ẑen = 10 centimes de Ğ1.
   - `ẐEN` (Production coopérative privée) : `UPLANETNAME` = SHA-256(swarm.key). 1 Ẑen = 1 €/$/£... Kubo strictement interdit (libp2p PSK isolé).
   - Passages entre UPlanet (`src/bridge.rs`) : privé → ORIGIN toujours refusé ; privé → privé seulement avec `--cross-swarm` ; `bridge-copy` exige `--same-person`. En bassin partagé, une station ne sert que les blobs que sa base référence. Le lien `kind 31903 d="plane-link"` (double signature) relie les clés d'une même personne (chat, carnet).
5. **Format Chiffré uCloud UENC v2 :**
   Format STREAM chunké par blocs de 64 Kio (AES-256-GCM ou ChaCha20-Poly1305). En-tête AAD de 14 octets avec préfixe aléatoire de nonce de 7 octets. Déchiffrement à la volée.

---

### 2. PROTOCOLE NOSTR & PARE-FEU SOCIAL (TIERED GATED RELAY)

- Le relais interne WebSocket et l'API HTTP/WebDAV/CardDAV/CalDAV partagent le **même port unique** (pas de multi-ports complexes).
- Authentification NIP-42 systématique sur WebSocket et NIP-98 sur les requêtes HTTP d'API.
- **Règles d'accès :**
  - Lecture libre : Kinds 0, 1, 3, 10002, 30023, 30800, 30850, plus la présence et la sonnerie visio éphémères (24315, 24316) — liste exacte : `public_kinds` dans `crates/astro-relay/src/policy.rs`.
  - Cercle N¹ (Amis réciproques dans kind 3) : Kinds 21/22 (Vidéos et morceaux en copie privée avec tag NIP-70 `["-"]`) et leurs listes 30005 (playlists NIP-51), réplication automatique FaceCloud, sauvegarde de la Part 3 Shamir.
  - Écriture réservée aux membres du cercle social ($N^1 \cup N^2$ en mode Village, le défaut ; hébergés + amis réciproques en Forteresse ; kinds 0 et 1 ouverts à tous en Place publique — `astro_db::station_mode`, `src/locks.rs`), **sauf** pour le kind 1059 (Gift Wrap NIP-59) ciblant un compte hébergé qui est TOUJOURS admis sans auth préalable (porte d'entrée réseau).
- Registre comptable Ğ1-N² (`kind 30852`) : Transactions chaînées par `prev`. Tentative de fork = gel irréversible du compte.

---

### 3. EXIGENCES FRONT-END & DESIGN SYSTEM (KILLER APP RULES)

1. **Zéro dépendance lourde & CSP Ultra-Stricte :**
   - Vanilla HTML5 / modern CSS / Vanilla JS modulaire (`m-*.js`, `m-*.css`).
   - Aucun framework externe (pas de React, Vue, Tailwind CDN, etc.).
   - Budgets compressés (gzip) servis via `rust-embed`, vérifiés par la CI (`crates/astro-ui/src/web.rs`) : **coque < 20 Ko (60 Ko bruts), écrans à la demande < 320 Ko gzip au cumul (832 Ko bruts, toutes les parts de la langue la plus lourde comprises), le chat à part (< 36 Ko gzip, 96 Ko bruts), chaque écran < 12 Ko hors `m-lang-*`** — les chiffres font foi dans le test, la doc les suit. Relever un budget est une décision : garder l'historique dans le message du test (« X Ko avant telle fonctionnalité »).
2. **Humain d'abord, Technique invisible :**
   - Ne JAMAIS afficher de jargon cryptographique à un membre (proscrire de ses écrans les termes : CID, IPNS, NIP, kind, Hex 64, SS58, Opex/Capex, SSSS, STREAM). **Seule exception : l'espace Capitaine** (*Ma station* : Santé, Membres, Réseau, Économie), écran d'administration où le vocabulaire technique est toléré.
   - Les clés privées sont encapsulées. Les liens de partage avec fragment `#key` sont explicités comme des *"Liens sécurisés privés"*.
   - Conserver l'élégance des thèmes d'origine (Aurore, Sobre, Kraft) et le support sans faille du mode sombre.
   - **Traductions** : le français du code sert de clé. Les phrases communes vont dans `m-lang-<lg>.js` ; un écran qui en a beaucoup (une part de moins de ~800 octets compressés coûte plus qu'elle ne rapporte) a sa part `m-lang-<lg>-<écran>.js`, déclarée dans la liste `parts` du dictionnaire commun et chargée avec l'écran. Un écran s'importe donc **toujours** par `K.mod('m-….js')` (jamais `import()` direct). `node crates/astro-ui/tests/lang_parts.mjs` vérifie les parts (phrase utilisée par son écran, pas de doublon, mêmes clés dans chaque langue).
   - PWA Mobile-First : interactions pensées pour le pouce, zones de clic $\ge 48\text{px}$, raccourcis gestuels.
3. **On suit les conseils de l'ergonome logiciel** (critères de Bastien & Scapin : guidage, charge de travail, contrôle explicite, adaptabilité, gestion des erreurs, homogénéité, signifiance des codes, compatibilité). Toute nouvelle interface est vérifiée dans un vrai navigateur, sur ordinateur et à 400 px de large, avant d'être livrée.

---

### 4. TON RÔLE ET TES RÈGLES DE PRODUCTION DE CODE

- **Sécurité et Qualité Rust :**
  - `#![deny(unsafe_code)]` (seule exception tolérée : initialisation de `sqlite-vec`).
  - Aucun appel non sécurisé à `.unwrap()`. Utiliser la propagation d'erreurs idiomatique (`?`, `thiserror`, `anyhow`).
  - Toute modification de schéma SQL doit maintenir la compatibilité rejouable (`schema.sql`).
- **Style et Langue :**
  - Le code, les commentaires techniques et la documentation doivent être rédigés en **français avec accents**.
  - Ne jamais mentionner ou insérer de tags `Co-Authored-By` dans les commits.
- **Démarche attendue lors d'une demande :**
  1. Diagnostiquer la cause racine en respectant les invariants du système.
  2. Fournir des solutions élégantes, minimales et testables.
  3. Toujours évaluer l'impact UX pour s'assurer que l'application reste instinctive pour un utilisateur néophyte.
