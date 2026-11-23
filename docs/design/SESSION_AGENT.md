# Session de navigateur confiée à un agent (MCP)

> État : **lots 1 à 6 livrés** (navigateur jetable, porte CDP, multiplexeur, écran dans l'application, garde de navigation,
> outils MCP, agent sur une autre station, co-surfing entre membres, § 11). Les écarts entre cette
> conception et la réalisation sont au § 12. Prolonge `src/agents.rs` (registre kind 31903 `d = agents`, serveur MCP `POST /mcp`).

## 1. Objectif

Un agent (OpenClaw, BRO, Claude Code…) doit agir sur un site où le membre a un compte (LinkedIn, impots.gouv.fr…) **sans
jamais recevoir son mot de passe**. Au lieu d'ouvrir `agents.kdbx`, la station :

1. lance un navigateur **jetable** (profil vierge, détruit à la fin) ;
2. laisse le membre s'y connecter lui-même (mot de passe, captcha, SMS) ;
3. **prête** ce navigateur à l'agent, par le protocole de débogage de Chromium (CDP), pour une durée bornée ;
4. le **détruit** à l'échéance ou quand le membre reprend la main.

L'agent peut tourner :

- **ici** : même machine, autre compte Unix (`/home/openclaw`) ;
- **ailleurs dans la constellation** : une station « LAN » (machine GPU d'un ami, par exemple) où le MULTIPASS du membre
  **n'est pas** hébergé.

Dans les deux cas, l'agent reçoit la même réponse : une adresse `ws://127.0.0.1:<port>/cdp/<jeton>` sur **sa** machine.

## 2. Ce qu'on promet, et ce qu'on ne promet pas

| Garanti | Non garanti |
|---|---|
| Le mot de passe et le second facteur ne quittent jamais la station du membre. | Pendant l'accord, l'agent peut faire **sur ce site** tout ce que le membre pourrait y faire (publier, supprimer, changer un réglage). |
| La session cesse d'exister à l'échéance (processus tué, profil effacé, tunnel coupé). | Le secret absolu des cookies : le filtre (§ 5) retire les voies directes, mais un agent qui exécute du JavaScript dans la page lit ce que la page lit (cookies non `HttpOnly`, `localStorage`). |
| Seul l'agent déclaré, avec son jeton, depuis la station déclarée, peut piloter. | Qu'un site ne détecte pas l'automatisation. |
| Tout est tracé (`audit.agent`) : ouverture, remise, fin, nombre de commandes, sites visités. | |

La vraie frontière est donc : **durée courte + un seul site + traçabilité + coupe-circuit**. La documentation membre le dit
en clair (§ 9) ; on n'écrit pas « sécurité absolue ».

## 3. Corrections au premier jet

| Premier jet | Retenu | Pourquoi |
|---|---|---|
| `--remote-debugging-port=0` | `--remote-debugging-pipe` (CDP sur les descripteurs 3/4) | Un port de débogage en boucle locale est ouvert à **tout utilisateur de la machine**, `openclaw` compris, sans jeton ni filtre. Avec le tube, seul `astroportd` parle à Chromium. |
| Remettre l'URL CDP de Chromium | Remettre l'adresse d'une **porte CDP** tenue par `astroportd` | Jeton par session, une seule connexion, filtre des commandes, coupure instantanée. |
| `/tmp/astro_agent_linkedin_123` | `<data_dir>/agent-browsers/<id>/` en `0700`, balayé au démarrage | `/tmp` est partagé ; un plantage y laisserait des cookies de session. |
| SIGKILL du PID | SIGKILL du **groupe de processus** | Chromium lance des processus enfants (rendu, GPU) qui survivraient au seul PID. |
| La station A autorise le PeerID de B « reçu dans la demande » | PeerID de B **déclaré par le membre** sur l'agent, vérifié par le carnet des pairs du tunnel | Règle de la maison : une station est reconnue par `PeerBook`, jamais par ce qu'elle déclare. |
| B fait `astroportd tunnel connect` | Le **relais MCP** de B ouvre l'accès local lui-même | `control.sock` vérifie le propriétaire : le compte `openclaw` ne peut pas s'en servir. OpenClaw ne doit voir aucune différence entre local et distant. |

## 4. Déroulé

```
OpenClaw          astroportd (station A, MULTIPASS)                 Membre (PWA)
   │ demander_acces{site, raison, mode:"navigateur"}                     │
   ├────────────────────────►│ état pending ── activité ───────────────►│
   │                         │◄──────────── « Me connecter pour lui » ───┤
   │                         │ lance Chromium (tube CDP), ouvre site     │
   │                         │ état login ── écran du navigateur ──────►│ se connecte (2FA)
   │                         │◄──────────── « C'est prêt, lui confier » ─┤
   │                         │ ouvre la porte CDP, jeton 256 bits        │
   │ statut_acces            │ état granted                              │
   ├────────────────────────►│                                           │
   │◄── {state:"granted", until, browser_ws_endpoint}                    │
   │ chromium.connectOverCDP(endpoint) ─► porte CDP ─► filtre ─► tube    │
   │                         │      bandeau « OpenClaw agit… 52 min » ──►│
   │ rendre_acces  (ou échéance, ou « Reprendre la main »)               │
   ├────────────────────────►│ ferme la porte, killpg, efface le profil  │
```

États d'une demande (`agents::State`) : `Pending` → `Login` → `Granted` → (`expired` | `revoked`), ou `Denied`.
Le mode `accord` actuel (sans navigateur) reste le défaut : aucun agent existant ne change de comportement.

## 5. Briques (station A)

### 5.1 `src/agent_browser/launch.rs` — le navigateur jetable

- Trait `BrowserLauncher` (`launch(profile_dir, site, headed) -> BrowserProc`) : `ChromiumLauncher` en production,
  `FakeLauncher` (CDP simulé en mémoire) pour les tests. Aucun test de la CI n'exige Chromium ; un test réel est `#[ignore]`.
- Drapeaux : `--remote-debugging-pipe --user-data-dir=<dir> --no-first-run --no-default-browser-check
  --disable-sync --disable-extensions --password-store=basic`, plus `--headless=new` si la station n'a pas d'écran.
- Descripteurs 3/4 passés par la crate `command-fds` (son `unsafe` reste hors du workspace : `unsafe_code = deny` tenu).
  Groupe de processus par `CommandExt::process_group(0)` (std, sûr) ; arrêt par `nix::sys::signal::killpg(SIGKILL)`.
- `BrowserProc` : `Drop` = killpg + `remove_dir_all` du profil. Au démarrage du démon : balayage de `agent-browsers/`
  (pidfile → killpg, puis effacement).
- Binaire cherché : `chromium`, `chromium-browser`, `google-chrome` ; absent → `demander_acces` en mode navigateur répond
  « pas de navigateur sur cette station » (erreur d'outil), et `doctor` le signale.

### 5.2 `src/agent_browser/cdp_gate.rs` — la porte CDP

Serveur WebSocket sur `127.0.0.1:0`, chemin `/cdp/<64 hex>` comparé en temps constant ; **une seule** connexion acceptée
par session ; trames bornées (16 Mio) ; tout le reste → 404.

Filtre, dans les deux sens :

- **Commandes refusées** (réponse d'erreur CDP, comptée dans l'audit) : `Storage.getCookies`, `Storage.setCookies`,
  `Network.getCookies`, `Network.getAllCookies`, `Network.setCookie(s)`, `Network.deleteCookies`, `IO.*`, `SystemInfo.*`,
  `Browser.setDownloadBehavior` hors du profil, `Target.exposeDevToolsProtocol`, `Tracing.*`.
- **Navigation bornée** : `Page.navigate` et `Target.createTarget` n'acceptent que le site accordé et ses sous-domaines
  (+ hôtes ajoutés par le membre, ex. `accounts.google.com` pour une connexion OAuth). Les navigations par clic sont
  bornées par la session propre d'`astroportd` sur le tube (`Fetch.enable` sur les requêtes `Document`, hors site → échec).
- **Événements nettoyés** : en-têtes `Cookie` / `Set-Cookie` retirés de `Network.*ExtraInfo` et `Network.responseReceived`.
- **Comptage** : nombre de commandes par domaine et hôtes visités, écrits à la fin de session dans `audit.agent`.

Fermer la porte coupe l'agent **immédiatement**, en local comme à travers un tunnel (la porte est la cible du tunnel).

### 5.3 Écran de connexion du membre

- **Lot 1** : station avec écran (ordinateur de bureau) → Chromium s'ouvre en fenêtre sur cet écran.
- **Lot 2** : station sans écran (Raspberry Pi) ou membre sur son téléphone → écran `m-agent-session.js` : la station
  diffuse la page par `Page.startScreencast` (JPEG) et rejoue les gestes par `Input.dispatch*`, sur une WebSocket
  authentifiée `/api/v1/agents/session/<id>/screen`. Le même écran sert ensuite à **regarder l'agent travailler**.
  Pas de VNC, aucune dépendance : on tient déjà le tube CDP.

### 5.4 `src/agents.rs` — extensions

- `AgentRecord` gagne `station: Option<String>` (PeerID où tourne l'agent ; `None` = cette station) et
  `extra_hosts: Vec<String>`. Champs `#[serde(default)]` : les registres kind 31903 déjà publiés restent lisibles.
- Nouveau `sessions: Mutex<HashMap<String, Session>>` (état local, jamais en base : une session ne survit pas au démon).
- API membre : `POST /api/v1/agents/decide` avec `approve` → lance le navigateur (état `Login`) ;
  `POST /api/v1/agents/ready {id}` → ouvre la porte (état `Granted`) ; `POST /api/v1/agents/stop {id}` → coupe.
- MCP :
  - `demander_acces` : argument facultatif `mode` (`"accord"` par défaut, ou `"navigateur"`).
  - `statut_acces` en mode navigateur, une fois accordé :
    `{"state":"granted","until":1764000000,"browser_ws_endpoint":"ws://127.0.0.1:45678/cdp/<jeton>"}`
    (le jeton n'est rendu **qu'à l'agent qui a fait la demande**, jamais au journal ni au flux d'activité).
  - `rendre_acces {id}` : l'agent a fini, la station coupe tout de suite.

## 6. Agent sur une autre station de la constellation

Station **A** héberge le MULTIPASS ; station **B** (LAN) fait tourner OpenClaw. B n'a aucun compte du membre.

```
OpenClaw ──POST /mcp──► astroportd B ══ P2P /x/agents-mcp-<A> ══► astroportd A   (MCP relayé)
OpenClaw ──ws 127.0.0.1:P/cdp/<jeton>──► forward B ══ /x/cdp-<id8>-<A> ══► porte CDP A ─► Chromium
```

1. **Déclaration** : à l'enrôlement, le membre indique sur quelle station tourne l'agent (choix dans la liste des stations
   de la constellation, enregistré dans `AgentRecord.station`).
2. **Relais MCP sur B** : B reçoit `POST /mcp` avec un jeton `ag1_<16 hex du compte>_…` d'un compte qu'elle n'héberge pas.
   Elle trouve la station hôte par l'annuaire des balises (`swarm_route`) ou, à défaut, par le paramètre `station=<PeerID>`
   de l'URL MCP configurée dans OpenClaw (simple **routage**, jamais une autorisation), puis transmet le corps JSON-RPC tel
   quel par le service P2P `agents-mcp` de A.
3. **Sur A** : le service `agents-mcp` est exposé avec un `PeerBook` ; A vérifie **trois** choses : le jeton, que le pair
   du flux est bien `AgentRecord.station`, et la politique des tunnels (garde : pair ∈ stations déclarées d'au moins un
   agent ; aucun réglage à faire par le Capitaine).
4. **Remise** : à l'accord, A expose la porte CDP sous `cdp-<8 hex de la demande>` avec
   `ExposeOpts { allow: {B}, max_streams: 4, gate: session vivante, watch_every: 5 s }` et répond à B
   `{"state":"granted","until":…,"browser":{"p2p":{"service":"cdp-1a2b3c4d","peer":"<A>"},"path":"/cdp/<jeton>"}}`.
5. **Réécriture par B** : B ouvre `Forwards::open(id, A, "cdp-1a2b3c4d", until, now)` (boucle locale, inscrit dans
   `LocalBinds` : anti-cascade déjà tenue) et rend à OpenClaw
   `{"state":"granted","until":…,"browser_ws_endpoint":"ws://127.0.0.1:<port>/cdp/<jeton>"}` — **la même forme qu'en local**.
6. **Fin** : A ferme la porte (coupure immédiate du flux), retire l'exposition, tue le navigateur ; l'accès local de B
   tombe à `until` par son minuteur existant.

Défense en profondeur : même si B est compromise, elle n'obtient qu'un navigateur, sur un site, pour une durée, déjà
accordés par le membre — jamais d'identifiant.

## 6 bis. Quel agent, quelle voie

La porte CDP suppose que l'agent tourne **sur une machine de la constellation** (il se connecte à `127.0.0.1`). Les agents
hébergés par leur éditeur (ChatGPT, claude.ai, Le Chat…) tournent dans un nuage qui ne voit pas cette adresse. D'où deux voies :

| Agent | Où il tourne | Voie |
|---|---|---|
| OpenClaw, Claude Code, Claude Desktop, Codex CLI, script Playwright | une station ou un poste de la constellation | **CDP** : `browser_ws_endpoint` + `connectOverCDP` (contrôle complet, rapide) |
| ChatGPT, claude.ai, tout client « MCP distant » | nuage de l'éditeur | **Outils de navigation MCP** : la station pilote elle-même le navigateur prêté, l'agent ne fait qu'appeler des outils |

**Outils de navigation MCP** (lot 5), disponibles seulement pendant un accord `granted`, exécutés par `astroportd` à
travers la même porte CDP et le même filtre : `naviguer {id, url}`, `lire_page {id}` (texte et arbre d'accessibilité
abrégé, éléments numérotés), `cliquer {id, element}`, `saisir {id, element, texte}`, `capture {id}` (image JPEG). Pas de
`evaluer_js` : c'est la voie la plus sûre, et la seule possible pour un agent du nuage. Elle exige que `/mcp` soit joignable
en HTTPS (domaine de la station ou `dnslink`) ; le jeton `ag1_…` reste la seule clé.

Configuration type d'un agent local :

```bash
# Claude Code (poste ou station de la constellation)
claude mcp add --transport http astroport https://<station>/mcp --header "Authorization: Bearer ag1_…"
```

```js
// OpenClaw / script : après statut_acces → granted
const browser = await chromium.connectOverCDP(endpoint);   // endpoint = browser_ws_endpoint
const page = browser.contexts()[0].pages()[0];              // déjà connecté au site par le membre
```

## 7. Audit et activité

`audit.agent` gagne `session_open` (navigateur lancé), `session_ready` (remis), `session_end`
(`reason` : `expired` | `revoked` | `returned` | `crash`, `cdp_calls` par domaine, `denied_calls`, `hosts`, `remote_peer`).
Le flux d'activité du membre dit « OpenClaw agit sur linkedin.com pour vous » au début et « OpenClaw a rendu la main »
à la fin. Rien de tout cela n'est rejouable ni n'a besoin de l'être (état local).

## 8. Tests (un test hostile par surface)

- `cdp_gate` : mauvais jeton, second client, trame géante, `Storage.getCookies`, `Page.navigate` vers un autre hôte,
  en-têtes `Cookie` dans les événements → tous refusés ou nettoyés.
- `agents` : `statut_acces` d'un autre agent ne voit pas l'adresse ; `stop` ferme la porte et tue le `FakeLauncher` ;
  échéance → `expired` et profil effacé ; registre ancien (sans `station`) toujours lisible.
- `tests/agent_session_p2p.rs` : deux vrais pairs libp2p locaux ; relais MCP de B → A ; pair C non déclaré refusé ;
  coupure de A ⇒ connexion de B close en moins de 5 s.
- Chromium réel : `cargo test -p astroportd --test agent_browser_real -- --ignored`.

## 9. Ce que voit le membre

Écran `Moi › Appareils › Mes agents`, sans jargon :

1. « **OpenClaw** voudrait agir sur **linkedin.com** pour vous : *publier le résumé de la semaine*. »
   [ Me connecter pour lui ] [ Refuser ]
2. Le site s'ouvre ; le membre se connecte comme d'habitude. « Connecté ? » [ C'est prêt, lui confier pour 1 h ▾ ]
3. Bandeau : « OpenClaw agit sur linkedin.com — reste 52 min » [ Regarder ] [ **Reprendre la main** ]
4. « OpenClaw a rendu la main. Le navigateur prêté a été détruit. »

Phrase d'honnêteté affichée à l'étape 2 : « OpenClaw ne connaîtra pas votre mot de passe. Pendant le temps choisi, il
pourra faire sur ce site ce que vous pourriez y faire vous-même. »

## 10. Lots

| Lot | Contenu | Livrable vérifiable |
|---|---|---|
| 1 | `launch` + `cdp_gate` + états `Login`/`Granted` + MCP (`mode`, `browser_ws_endpoint`, `rendre_acces`), local, fenêtre sur l'écran de la station | Playwright sur la même machine (compte `openclaw`) publie un message de test |
| 2 | Écran `m-agent-session.js` (diffusion + gestes), bandeau « Reprendre la main » | Connexion depuis un téléphone à 400 px vers une station sans écran |
| 3 | `AgentRecord.station`, service `agents-mcp`, relais MCP, exposition `cdp-<id>` | OpenClaw sur une station LAN pilote le navigateur de la station du membre |
| 4 | Hôtes supplémentaires (OAuth), bornage des navigations par clic, résumé d'audit dans l'écran | Connexion « via Google » acceptée, autre hôte refusé |
| 5 | Outils de navigation MCP (`naviguer`, `lire_page`, `cliquer`, `saisir`, `capture`) | claude.ai ou ChatGPT (connecteur MCP distant) publie via la station |

Documentation à tenir à chaque lot : en-tête de `src/agents.rs`, section « Agents et serveur MCP » de `docs/GUIDE.md`
(puis `python3 docs/build_guide.py`), ligne « Coffre et agents (MCP) » de `docs/DEVELOPPER.md`, `RELEASE_NOTES.md`.

## 11. Lot 6 : co-surfing entre membres (livré : `src/cosurf.rs`, écran `m-cosurf.js`)

Même mécanique, mais le navigateur est prêté **à un humain** plutôt qu'à un agent : pendant une conversation (ou dans un
groupe), un membre dit « aide-moi sur ce site » ; son ami voit la page en direct, et peut prendre la souris si on la lui donne.

- **Ce qui existe déjà** : le navigateur jetable, l'écran diffusé (`Screen` : images JPEG à la demande, gestes rejoués), la
  garde de navigation, le relais P2P entre stations, la visio du chat (`m-call.js`, `turn.rs`).
- **Ce qu'il faut ajouter** :
  1. une session « co-surfing » lancée depuis une conversation (bouton *Naviguer ensemble*), sans agent ni jeton : invités =
     participants de la conversation, amis réciproques seulement ;
  2. diffusion de l'écran à un ami hébergé **ailleurs** : sa station relaie le flux d'images et les gestes par le maillage
     (service `cosurf` à côté de `agents-mcp`, invité reconnu par sa clé Nostr signée, station par le maillage) ;
  3. trois niveaux par invité, choisis et retirés par le propriétaire à tout instant : **regarder**, **montrer** (pointeur
     visible, sans clic), **prendre la main** (clics et frappe) ; un seul pilote à la fois ;
  4. la visio du chat en option, à côté de l'écran partagé.
- **À dire clairement** : l'ami qui « prend la main » peut faire ce que le propriétaire pourrait faire sur ce site, mots de passe
  compris (la personne aidée peut être handicapée ou ne pas savoir taper : c'est voulu) ; ce qui est tapé passe par sa station.
  « Tout faire pour moi » est proposé d'emblée au lancement, et le membre peut changer d'avis à tout instant.
- **Réalisé** : invitation par la conversation (lien `#/cosurf?id=…&station=…`, cliquable dans le chat), entrée par *Partager*
  du téléphone, demandes relayées signées NIP-98 (kind 27235, fraîcheur 60 s, usage unique), règles ouvertes de la garde
  (tout site public, jamais le réseau local ni une adresse IP), une heure au plus, une session par membre, 8 invités.

## 12. Écarts entre la conception et la réalisation

- Numéros de commande réservés à la station : à partir de `1 << 30` (Chromium n'accepte que 32 bits signés).
- L'écran du membre passe par un flux SSE d'images (`GET /api/v1/agents/screen`) et des gestes en `POST`, avec
  l'authentification habituelle de l'application, plutôt que par une WebSocket dédiée.
- La station d'origine de l'agent distant est donnée par `?station=<PeerID>` dans l'adresse MCP ; la découverte automatique
  par l'annuaire n'est pas faite (l'écran *Mes agents* affiche l'adresse à copier).
- Le bornage des navigations par clic (lot 4) est fait par `Fetch` sur les documents de la page principale, plus la fermeture
  des fenêtres ouvertes hors site ; les cadres intérieurs ne sont pas bornés.
- Chromium installé en snap : non essayé (le confinement snap interdit les dossiers cachés comme `~/.zen`).
- En mode `planes` (un NODE pour plusieurs plans), le service `agents-mcp` n'est servi que par le premier plan démarré.
