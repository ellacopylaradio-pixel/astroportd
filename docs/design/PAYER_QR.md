# Payer, recevoir et se rencontrer par QR, face à face

Deux membres côte à côte se paient sans saisir d'adresse : l'un montre son code **Recevoir**, l'autre le vise avec **Payer**.
Écran : `crates/astro-ui/web/m-pay.js` (`#/recevoir`, `#/payer`), entrées dans *Mes Ẑen*. Test navigateur :
`~/.astro/bin/python tests/browser/smoke_pay.py [captures]` (caméra simulée qui filme un vrai QR).

## Ce que porte le QR

```
https://<station du bénéficiaire>/#/payer?to=<clé Nostr hex>[&a=<Ẑen entiers>]
```

Rendu par `GET /api/v1/me/zen/receive[?amount=N]` (membre ; `{ url, qr_svg, hex, amount_zen }`, 1 à 1 000 000 Ẑen).

- **Aucun nom dans le QR.** Le payeur voit le nom que *sa* station connaît (récapitulatif de l'envoi) et un mot clair :
  « Léa, votre proche » ou « Cette personne n'est pas dans vos proches : vérifiez son nom ». Un faux QR collé sur celui d'un
  commerçant ne peut donc pas se donner le nom du commerçant.
- Seules la clé et le montant comptent : le payeur peut être sur une autre station de la constellation (le serveur
  accepte tout MULTIPASS connu, `zen_credit::member_party`, contrôle primal compris).
- Un **npub** (QR d'une autre application Nostr) est accepté aussi.
- Ouvert par l'appareil photo du téléphone, le lien ouvre l'application sur la station du bénéficiaire, droit au paiement
  (utile quand payeur et bénéficiaire ont la même station ; sinon, « Payer » dans sa propre application).

## Parcours

- **Recevoir** : QR en grand (noir sur blanc dans tous les thèmes), montant facultatif, et l'écran dit quand l'argent
  arrive (« Paiement reçu : +5 Ẑen », ou « Promesse reçue » si le réseau Ğ1 est coupé), en interrogeant le solde toutes
  les 8 s.
- **Payer** : caméra arrière ; dès le code lu, la caméra s'éteint et le dialogue d'envoi habituel (`m-credit.js`,
  `mode: 'member'`) s'ouvre, destinataire et montant remplis. Même récapitulatif, même confirmation en deux clics, même
  identifiant de versement rejouable. Réseau Ğ1 coupé et île ouverte : l'envoi devient une promesse (`ISLAND.md`).
  Sans caméra, ou caméra refusée : un message dit quoi faire, et « Choisir dans mes proches » reste là.

## Lecteur

`web/scan-cam.js` est partagé avec l'encaissement des Ğ1Billets (`/scan`) : BarcodeDetector, sinon jsQR chargé à la
demande ; `watch(video, onText)` rend `stop()`, et la première lecture attend que l'appelant l'ait reçu.

## Se rencontrer (`#/rencontre`, entrée dans *Proches*)

Trois vues au pouce : **Mon code** (par défaut), **Scanner son code**, **Chercher** parmi les MULTIPASS de la constellation
connus de la station (nom du profil, sinon début de l'email ; jamais l'email entier). Tout code lu ou personne choisie
ouvre la même **fiche** : le nom connu ici, l'état du lien, **Devenir proches** et **Payer**.

- Devenir proches = suivre (liste de contacts kind 3, signée par la station du membre). L'amitié n'existe que si **les deux**
  font le geste : l'autre scanne mon code, ou accepte la demande sur son téléphone (« Léa vient de vous ajouter »,
  demandes des 30 dernières minutes). En attendant, la fiche montre mon code et se met à jour seule.
- API : `GET /api/v1/me/meet[?q=]` (demandes, recherche), `GET /api/v1/me/meet/code` (mon code), `GET|POST
  /api/v1/me/meet/<clé>` (état ; ajout, corps `{ proof }` facultatif). Opérations : `astro_ui::api::MeetOp`.

## Rencontre prouvée ou nomination (anti-Sybil, calibrage de Ğ1-N²)

Un lien né d'un QR scanné face à face ne pèse pas comme un lien choisi dans une liste. `astro_economy::meet` :

- **Preuve de présence** : le code « rencontre » (`<station>/#/rencontre?to=<clé>&m=<tranche>.<signature>`) porte la
  signature, par la clé du membre montré, de la tranche de 2 minutes en cours (événement éphémère kind 27855 reconstruit
  par le vérificateur, jamais publié). L'écran le renouvelle chaque minute ; la preuve vaut 6 minutes : une photo du code
  envoyée au loin expire.
- **Attestation** kind **30855** (remplaçable, `d` = clé rencontrée), signée par celui qui ajoute : `how=qr` + `proof`, ou
  `how=named` (liste, demande acceptée, code de paiement, lien). La station vérifie la preuve avant d'ajouter (code expiré :
  message clair, rien n'est ajouté) ; une nomination n'écrase jamais une rencontre déjà prouvée.
- **Projection** `meetings` (rejouable) : `verified` = preuve signée par la personne rencontrée et datée du moment de
  l'attestation, **revérifiée par chaque station** (`Meeting::from_event`) : se dire « qr » ne suffit pas.
- **Pour Ğ1-N²** : compter `meetings WHERE verified = 1`, et plus fort encore, les rencontres vérifiées **dans les deux
  sens** (A→B et B→A). Les nominations restent visibles mais ne valent pas rencontre.
- Limites : la preuve atteste qu'un écran affichait le code à ce moment, pas que les deux téléphones étaient côte à côte
  (un appel vidéo en direct passerait) ; c'est un coût, pas une impossibilité. Entre deux stations, la demande arrive par la
  synchronisation N¹/N² ; scanner les deux codes marche toujours.

## Mes cercles : niveaux de proximité (`#/cercles`, entrée dans *Proches*)

Cinq cercles, du plus proche au plus lointain : **Famille** (5), **Proche** (4), **Ami** (3), **Connaissance** (2),
**Contact** (1). Des noms, pas des chiffres. Par défaut, un ami réciproque est *Ami* et une personne suivie sans retour
*Contact* : on ne range que les exceptions, d'un menu (écran Mes cercles, ou fiche d'une personne). On ne range que ceux que
l'on suit.

- **Privé** (`src/proximity.rs`) : un seul événement kind 30078 `d=uplanet/proximity`, signé par le membre, contenu chiffré
  pour lui seul (NIP-44 vers soi-même), seulement les exceptions. Jamais dans le kind 3 : ni le relai ni la personne rangée
  ne savent où elle l'est. L'événement voyage avec les autres (rejouable).
- API : `GET /api/v1/me/circles` (`levels`, `people` : `level`, `ranked`, `friends`, `met_verified`), `POST
  /api/v1/me/circles` `{ hex, level: 1..5 | null }` ; la fiche (`GET /api/v1/me/meet/<clé>`) porte `level`.
- **Branché** : au moment de payer, Famille/Proche/Ami rassurent (« Léa, votre proche (Famille) »), Connaissance demande de
  vérifier le nom, Contact ou inconnu avertit.
- **Branché** : *Mes gardiens* (Mon compte › Ma sécurité) propose d'abord les proches sur un **autre Astroport** (sur la même
  station, un gardien tomberait avec elle), puis du cercle le plus proche au plus lointain, puis rencontrés en personne
  (`private_backup::guardian_candidates`) ; si tous les proches sont sur la station, l'écran le dit et propose *Se rencontrer*.

## Toile de confiance N² (`astro_economy::wot`, carte dans *Mes cercles*)

Le graphe des rencontres prouvées reçoit la mécanique de la toile Ğ1 de Duniter v2s, pour qu'une économie Ğ1-N² puisse
un jour verser un dividende universel **sans** la chaîne (mode autonome ; rien n'est versé aujourd'hui).

- **Certification** = rencontre prouvée A → B (`meetings.verified = 1` : A a scanné le code de B). Une nomination ne
  certifie jamais.
- **Racines** (genèse) : les Capitaines de la constellation (`identities.role = 'captain'`), membres d'office.
- **Règles Ğ1** (`Params::G1`) : 5 certifications de membres ; 80 % des référents l'atteignent en 5 pas au plus, par des
  membres ; référent = a émis et reçu ⌈N^(1/5)⌉ certifications de membres ; validité 2 ans ; 100 au plus par émetteur
  (les plus récentes).
- **Calcul pur, rejouable, sans table** : depuis les racines, chaque tour ajoute tous les candidats qui remplissent les
  règles face aux membres du tour précédent ; l'ordre d'arrivée ne compte pas. Des faux comptes qui se certifient entre eux
  n'entrent jamais (aucun référent ne les atteint).
- API membre : `GET /api/v1/me/wot` → `{ member, root, certs, needed, reached_pct, mutual, certifiers, web }`.
- Limites : les racines dépendent des Capitaines que la station connaît (deux stations au même horizon trouvent la même
  toile) ; les paramètres de la Ğ1 sont exigeants pour un petit village (5 rencontres) : à régler par constellation si besoin.

## Suite prévue

Brancher les autres droits sur les cercles, un par un, chacun dit à l'écran : gardiens de la sauvegarde et des parts de
clé (fait : conseil du gardien, ci-dessus), photos partagées seules (Proche et plus), sonnerie des appels la nuit (Famille), confiance dans les promesses
d'île d'une station (cercle de son Capitaine), poids des rencontres pour Ğ1-N² (fait : toile ci-dessus ; reste le dividende universel N²).
