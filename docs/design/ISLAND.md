# ISLAND-1 — « Ğ1 en différé »

Quand une station perd l'accès à Duniter **alors que la chaîne continue de tourner**, ses membres peuvent continuer
d'échanger des Ğ1 : ils se font des **promesses**, tenues dans le registre Ğ1-N², et réglées en vraie Ğ1 au retour.

Code : `crates/astro-economy/src/island.rs` (règles, fonction pure), `src/island.rs` (station),
`astro_duniter::Chain::ud_status` (DU et adhésion lus sur la chaîne).

## Principe : aucune création de monnaie

Un MULTIPASS hébergé peut promettre au plus :

```
crédit(t) = dépensable au dernier instantané + DU × périodes de DU entièrement écoulées depuis l'instantané
```

- *dépensable* : solde transférable moins la réserve de la station (`RESERVE_G1_CENTS`) ;
- *DU* : seulement si le portefeuille porte une identité **membre** ; DU en vigueur, période et fin d'adhésion lus sur la
  chaîne (`CurrentUd`, `UdCreationPeriod`, `Identity`, `Membership`) ; jamais au-delà de la fin d'adhésion connue, ni de
  30 jours (`MAX_UD_SECS`) ;
- les promesses **reçues ne sont pas dépensables** avant leur règlement : une promesse non tenue ne se propage pas.

Ces DU ne sont pas créés par l'île : la chaîne les verse pendant la coupure. L'île permet seulement de les dépenser
d'avance.

## Transparent pour les membres

Le membre ne change rien à ses gestes : **« Envoyer des Ẑen »** (Mes Ẑen, chat) bascule seul en promesse quand l'île est
ouverte (`zen_credit::member_island`, via le trait `island::Deferred`). L'écran l'avertit **avant** (bandeau du dialogue),
**pendant** (« Promis, réglé au retour du réseau », « Vous pourrez encore promettre ») et **après** (« Promesse … réglée en
Ğ1 au retour du réseau »). Même confirmation en deux clics, même identifiant de versement : rejouer après une panne rend la
même promesse. Pendant l'île, la trésorerie (CASH) attend le retour du réseau. *Mes Ẑen* montre une carte « Ğ1 en
différé » seulement quand l'île est ouverte ou que des promesses restent à régler ; `#/island` liste les promesses.

## Plusieurs stations en panne

Une coupure en touche souvent plusieurs. Chaque station ouvre **sa** île ; les promesses circulent entre elles comme les
autres événements (maillage, synchronisation N¹/N²). Chaque promesse **embarque l'ouverture de crédit signée par le NODE
du payeur** (contenu du 30852) : la station du destinataire vérifie seule le crédit, sans avoir jamais vu le 30854. Un
crédit embarqué qui vise un autre membre ou une autre île est ignoré. L'écran du Capitaine liste les **stations voisines
en île** (connues ou non).

## Événements

| Kind | Signé par | Rôle | Tags |
|---|---|---|---|
| 30854 | NODE | ouverture de crédit (une par membre et par île) | `d`, `t=island`, `island`, `p` (membre), `g1`, `balance`, `ud`, `ud_period`, `ud_until`, `at` |
| 30852 | membre (clé détenue par la station) | promesse | forme N² (`d`, `p`, `amount`, `prev`, `t=g1-n2`) + `t=island`, `island`, `g1` (portefeuille du destinataire) |

- Identifiant d'île : `<16 premiers hex du NODE>-<instant d'ouverture>`. Une ouverture signée par une autre clé que le
  NODE de ce préfixe est ignorée.
- 30853 n'est **pas** réutilisé (ancienne attestation de build, retirée).
- Une promesse d'île n'entre pas dans `ledger_n2` : le futur registre N² ne la voit pas.

## Validation (indépendante de l'ordre d'arrivée)

`astro_economy::island::validate` travaille sur l'**ensemble** des promesses d'un payeur : chaîne `prev` depuis
`genesis`, signature, île, destinataire Ğ1, horodatage (pas avant l'instantané, pas plus de 10 min dans le futur),
pas de `t=mint`, total ≤ crédit à l'instant de chaque promesse.
**Fourche** (deux promesses sur le même `prev`, ou un `d` réutilisé) : payeur gelé, aucune des deux branches acceptée.
La station recalcule l'état (`island_promises.state`) à chaque passage : le temps réel et le replay convergent.

## Cycle de vie (tâche de fond, toutes les 5 min)

1. **Chaîne joignable** : instantané `g1_snapshot` toutes les 6 h ; l'île ouverte se **ferme** ; règlement.
2. **Chaîne injoignable** : `down_since` est noté. Si l'ouverture automatique est réglée, l'île s'ouvre seule passé ce
   délai. Le Capitaine la règle dans **Ma station › Économie › Île** (`kv` `island/auto_after`, prioritaire) ;
   `--island-after <minutes>` ne sert que de valeur par défaut. Sans l'un ni l'autre : **jamais seule**, seulement à la main.
3. **Règlement** (station du payeur, qui détient sa clé Ğ1) : promesses acceptées des îles fermées, dans l'ordre de
   chaque chaîne, mémo `UPLANET:ISLAND:<id>`.
   - noté `sending` **avant** l'envoi ; `paid` avec le bloc ;
   - solde insuffisant (le membre a dépensé ailleurs) ou refus de la chaîne : `short`, payeur et Capitaine prévenus,
     retenté ; les promesses suivantes du même payeur attendent ;
   - issue inconnue, ou `sending` resté après un arrêt : **jamais rejoué**, payeur bloqué, le Capitaine vérifie.

## Tables

| Table | Nature | Contenu |
|---|---|---|
| `g1_snapshot` | état local | dernier instantané par MULTIPASS |
| `island_credits` | projection (30854) | crédits ouverts |
| `island_promises` | projection (30852 `t=island`) + état recalculé | promesses |
| `island_settle` | état local | règlements (jamais effacés par un replay) |
| `kv` scope `island` | état local | `current`, `down_since`, `snapshot_at` |

## API

| Route | Accès | Rôle |
|---|---|---|
| `GET /api/v1/island` | membre | crédit disponible, promesses envoyées et reçues (station garante, `known_station`) |
| `POST /api/v1/island/pay` | membre | `{ to: <hex>, g1: <adresse>, amount: "12.34" }` |
| `GET /api/v1/station/island` | Capitaine | état : ouverte, instantané, panne, compteurs |
| `POST /api/v1/station/island/open` · `/close` | Capitaine | ouverture / fermeture à la main |
| `POST /api/v1/station/island/auto` | Capitaine | `{ minutes: 1..1440 \| null }` : ouverture automatique |
| `POST /api/v1/me/zen/preview` · `/send` | membre | l'envoi habituel ; île ouverte → aperçu `island` puis promesse |

Écrans : `m-island.js` (`#/island` membre, `#/ile` Capitaine), `m-credit.js` (avertissements), `m-money.js` (carte).
Test navigateur : `~/.astro/bin/python tests/browser/smoke_island.py [captures]`.

## Limites connues (à traiter avant d'activer en production)

- **Confiance entre stations** : une clé inconnue peut s'inventer une île et promettre ce qu'elle veut. Le destinataire
  voit `known_station` ; une promesse ne vaut que ce que vaut la station qui la garantit. Pas de liste d'autorités.
- **Dépense hors île** : un membre peut dépenser sa Ğ1 ailleurs (Cesium en 4G) pendant la coupure ; sa promesse reste
  alors `short` jusqu'à réapprovisionnement. La station prévient, elle ne peut pas l'empêcher.
- **Instantané** : jusqu'à 6 h d'âge au moment de la coupure.
- **Réévaluation du DU** pendant la coupure ignorée (prudent : le DU ne baisse pas).
- **Contrôle primal** : pendant l'île, l'indexeur Duniter est injoignable comme la chaîne ; le portefeuille du destinataire
  est celui de son identité connue, sans vérifier qu'il vient de cette UPlanet.
- **Chaîne arrêtée** (pas seulement injoignable) : les DU présumés ne sont pas versés ; c'est le cas ISLAND-2, non traité.
