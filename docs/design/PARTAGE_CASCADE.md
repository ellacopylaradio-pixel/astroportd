# Partage N¹ en chaîne, DAG de remix et cascade de Ẑen

> État : **révision économique validée, Voie A retenue ; Lot 1 (structures Rust + Serde, signatures de saut, aplatissement du DAG, calcul exact Voie A, prise en charge `photo`/`plant`/recette dans `media.rs` et `plant_fiche.rs`) implémenté** dans `crates/astro-economy/src/cascade.rs`. Les virements réels (Lot 3) et la liste publique (Lot 2) ne sont pas encore branchés sur le réseau.
>
> Point de départ : un partage de la médiathèque (« Partager avec mes amis », `src/media_share.rs`, kind 21/22) ne laissait qu'une **trace** (qui a copié, de qui je le tiens, qui le repartage, `GET /api/v1/ucloud/media/trace`).

## 1. Ce qu'on veut (et révision économique)

Une œuvre (film, morceau, photo, fiche botanique, capture, production du membre) circule de proche en proche dans les cercles N¹ : A la partage à B, B la repartage à C et D, C à E… Le graphe de diffusion est un **arbre** (plusieurs branches), et le graphe de création (remixes, enrichissements) est un **DAG** (graphe orienté acyclique).

Suite à la revue économique, la règle initiale « 1 saut = 1 Ẑen » (qui rendait une œuvre d'autant plus chère qu'elle voyageait loin et inversait le rapport créateur / passeurs) est **abandonnée**. Les règles retenues sont :

1. **Prix global indépendant de la profondeur `n`** : l'œuvre est gratuite (`Free`), à prix libre / chapeau (`Hat`, défaut recommandé en cercle N¹), ou à prix global fixe (`Fixed`).
2. **Division en points de base (`Bps`, `10_000 = 100 %`)** : un paiement est réparti selon une règle fixe `SplitRule` (par défaut **80 % Création** `8_000 bps` / **20 % Diffusion** `2_000 bps`).
3. **Voie A (lot unique de virements définitifs)** : la station du spectateur dispose de la fiche signée de l'œuvre (table aplatie du DAG) et du jeton de sa branche N¹ (`ShareChain`). Elle émet **un seul lot** de virements définitifs vers chaque bénéficiaire unique (créateurs + parents du DAG + relais de la branche). Aucun séquestre, aucun verrou temporel, aucun relais bloquant.
4. **Contrainte monétaire stricte (`1 Ẑen = 0,10 Ğ1`)** : la plus petite unité transférable sur Duniter v2s est `0,01 Ğ1` = `Amount(1)` (`astro_core::Amount`), soit **`0,10 Ẑen`** (**10 centimes de Ẑen**). L'unité atomique de calcul est donc le **`DeciZen(u64)`** (`1 DeciZen = 0,10 Ẑen = 0,01 Ğ1 = Amount(1)`). Aucun flottant ; tous les calculs se font en entiers non signés (`u64`, produit intermédiaire `u128`), et **tout reste de division euclidienne (poussière) va toujours à l'auteur principal (`author_pubkey`)**, assurant une conservation exacte (`∑ virements == montant total`).
5. **Séparation stricte copie privée / commun** : une œuvre en copie privée (`Rights::PrivateCopy`, CPI L. 122-5) n'est **jamais** monétisable ni remixable dans le DAG économique. Seule une fiche portant `Rights::OpenCommon` (licence SPDX explicite) et **signée** par son auteur peut recevoir des Ẑen.

## 2. Ce qui existe déjà et qu'on réutilise

| Besoin | Brique existante |
|---|---|
| Message privé de proche en proche | NIP-59 (`sharing.rs`, `friends_media::send`, étiquettes `op`, `x`, `e`) |
| Identifier l'œuvre sans rechiffrer | CID/empreinte (`x`) : même blob d'un bout à l'autre |
| Unité monétaire entière Ğ1 | `astro_core::Amount` (centimes de Ğ1 = `DeciZen`) |
| Virement entre MULTIPASS, sans découvert, sans double dépense | registre `kind 30852` (`astro-economy::ledger`, `prev`/`d` uniques) |
| Signatures BIP-340 reconstructibles hors relai | `astro_nostr_proto::UnsignedEvent` / `Event::verify` (même modèle que `astro_economy::meet`) |
| Relier les clés d'une même personne entre plans | `kind 31903 d="plane-link"` (double signature) |
| Liste publique remplaçable | `kind 30005` (NIP-51), déjà utilisé pour séries et albums |
| Fiches plantes privées et recherche | `src/plant_fiche.rs` (`/_/Plantes/`), `src/media.rs` (`media_index`, `search_text`) |

## 3. Le DAG de création, le Remix et les Communs

Une création n'est plus isolée. Une fiche `<nom>.media.json` (`media/v1`) peut embarquer un bloc optionnel `commons` (`CommonsBlock`), rétrocompatible avec les fiches existantes :

- **Générique pondéré (`credits`)** : une œuvre audiovisuelle ou documentaire est souvent collective (réalisation, image, son, montage, matériel ou lieu en bien commun). Chaque entrée porte `{ pubkey, role, bps, attest_sig? }`.
- **Parents du DAG (`sources`)** : une œuvre dérivée cite 1 à N œuvres parentes (`SourceRef` : `sha_plain`, `cid`, `author_pubkey`, `event_id?`, `role`, `share_bps`).
  - Invariant : `∑ credits.bps + ∑ sources.share_bps == 10_000`.
  - Chaque parent doit autoriser le remix (`allow_remix: true`) et fixer un plancher amont `remix_min_bps`. L'enfant doit allouer `share_bps >= parent.remix_min_bps`.
  - La profondeur créative est bornée (`generation = max(parents) + 1 <= MAX_DAG_DEPTH`, 8). Les cycles sont impossibles par SHA-256.
- **Deux formes de remix (`RemixKind`)** :
  - `Rendered` : nouveau média encodé (montage, mashup, étalonnage) avec son propre fichier et CID.
  - `Recipe` : composition **non destructive** (`tracks: Vec<CompositionTrack>`) qui associe des CID existants sans dupliquer les octets (nouvelle bande son, doublage, sous-titres, ou fiche documentaire rattachée à une photo). Dans `src/media.rs`, une fiche `Recipe` n'exige pas de fichier média propre (`file` optionnel).
- **Aplatissement déterministe (`flatten_dag`)** : à la publication, les parts des parents sont résolues récursivement en une table aplatie `flattened_payouts` (`sum == 10_000 bps`), en fusionnant les diamants par `pubkey` (un seul virement par bénéficiaire) et en attribuant tout reste d'arrondi de `bps` à `author_pubkey`. Toute station qui possède les fiches parentes peut revérifier cette table (`verify_flattened_dag`) avant de payer.
- **Signature d'auteur (`CommonsBlock::sign` / `verify_signature`)** : signature Schnorr BIP-340 (kind éphémère `27_856`, jamais publié) sur la sérialisation canonique du bloc hors `signature`. Sans signature valide, l'œuvre reste en Lots 1–2 (compteur et filiation seuls, jamais d'argent).

### Cas d'usage phare : la base commune botanique

1. **Alice** photographie une plante et publie la **Fiche A** (`type: "photo"`, `file: "ortie.jpg"`, licence `CC-BY-SA-4.0`, `allow_remix: true`, `remix_min_bps: 4000`).
2. **Bob** identifie la plante (via sa fiche privée `src/plant_fiche.rs`, convertie sans ses notes personnelles ni son GPS fin — seule la maille UMAP `0,01°` est retenue via `Fiche::to_commons_plant_data`) et publie la **Fiche B** (`type: "plant"`, bloc `plant: PlantData`, `remix: Recipe`, `sources: [Alice, 4000 bps]`, `credits: [Bob, 6000 bps]`). Zéro duplication de la photo dans le bassin commun.
3. **Bob** partage la Fiche B à **Charlie** (saut 1), qui la partage à **David** (saut 2).
4. **David** verse `25 DeciZen` (**2,50 Ẑen** = `0,25 Ğ1`) :
   - Diffusion (20 %) = `5 DeciZen` → **Charlie** (relais unique hors auteur principal Bob) ;
   - Création (80 %) = `20 DeciZen` → **Alice** `8 DeciZen` (40 %), **Bob** `12 DeciZen` (60 %).
   - Si 3 relais se partagent `5 DeciZen`, chacun reçoit `1 DeciZen` (`0,10 Ẑen`), et le reste (`2 DeciZen`) remonte à **Bob** (auteur principal). Somme conservée : `8 + 14 + 1 + 1 + 1 = 25 DeciZen`.

## 4. Le jeton de chaîne N¹ (`ShareChain`, Lot 1)

Chaque copie transmise de proche en proche porte un `ShareChain` chiffré pour le destinataire (NIP-59) :

```
ShareChain = {
  v: 1,
  work_sha: <SHA-256 du contenu clair>,
  work_cid: <CID du blob>,
  fiche_cid: <CID de la fiche .media.json>,
  origin_pubkey: <clé Nostr de la source>,
  hops: [
    { rank: 1, from_pubkey: A, to_pubkey: B, at, sig },
    { rank: 2, from_pubkey: B, to_pubkey: C, at, sig },
    ...
  ]
}
```

- Chaque saut est signé par `from_pubkey` (kind éphémère `27_857`) en incluant `work_sha`, `rank`, `from_pubkey`, `to_pubkey`, `at` et la signature du saut précédent (`"genesis"` pour le rang 1).
- `ShareChain::verify` contrôle : longueurs (`hops.len() <= MAX_CHAIN_HOPS`), formats hex64, enchaînement strict `hops[0].from_pubkey == origin_pubkey` et `hops[i].from_pubkey == hops[i-1].to_pubkey`, interdiction des auto-sauts (`from != to`), rangs consécutifs `1..=k` et validité de toutes les signatures BIP-340.
- Le destinataire voit sa **lignée** (de la source à lui), jamais les autres branches de l'arbre.
- Un partageur malveillant peut toujours ré-encoder le fichier et repartir comme source (manque assumé) ; en revanche, il ne peut ni raccourcir ni rallonger une chaîne signée, ni toucher le paiement d'une œuvre dont la fiche est signée par un autre auteur (`chain.origin_pubkey == commons.author_pubkey`).

## 5. Paiement Voie A (`compute_payout_batch_with_eligible`, Lot 3)

1. La station du spectateur vérifie `commons.verify_signature()`, `chain.verify()`, `chain.work_sha == commons.content_sha` et `chain.origin_pubkey == commons.author_pubkey`.
2. Elle vérifie que `PricingMode` autorise le montant (ex. `>= price_deci_zen` pour `Fixed`, `>= min_deci_zen` pour `Hat`).
3. **Filtrage WoT des relais (anti-Sybil intra-diffusion)** : seuls les relais uniques de `chain.hops` (hors `author_pubkey`) appartenant à la toile des rencontres prouvées (`astro_economy::wot::Wot`) sont éligibles à la tranche Diffusion. Un relais hors WoT est listé dans `filtered_relays` et reçoit `0` ; s'il ne reste aucun relais éligible, 100 % rejoint la tranche Création.
4. Avant paiement (hors mode île), l'aperçu vérifie que chaque bénéficiaire est joignable sur le plan et atteint le dépôt existentiel Ğ1 (`1,00 Ğ1 = 100` centimes).
5. Calcul entier (`DeciZen(u64)`, produits intermédiaires `u128`) :
   - Relais uniques de la branche (hors `author_pubkey`, dédoublonnés pour déjouer les boucles `B → C → B`). S'il n'y a aucun relais tiers, toute la somme va à la Création.
   - `diffusion_pool = total * diffusion_bps / 10_000` ; `creation_pool = total - diffusion_pool`.
   - Chaque bénéficiaire de `flattened_payouts` reçoit `creation_pool * bps / 10_000`.
   - Chaque relais unique reçoit `diffusion_pool / nb_relays`.
   - Les lignes sont fusionnées par `pubkey` (un créateur qui serait aussi relais ne reçoit qu'un seul virement).
   - `remainder = total - ∑ parts` est ajouté à `author_pubkey`.
4. Un maillon absent est payé directement sur sa clé et retrouve son dû à son retour (ou via `plane-link` après déménagement).
5. **Frontière des plans** : un virement ne traverse pas un plan privé (`bridge::check_passage`). La station du spectateur l'indique avant paiement si un bénéficiaire n'est pas joignable dans ce plan.

## 6. La liste officielle (facultative, Lot 2)

Un partageur peut **s'inscrire** en publiant un événement remplaçable `kind 30005`, `d = chaine:<work_sha>`, portant sa clé et le rang prouvé de son jeton.

- L'inscription est indépendante du paiement (elle ne change pas les parts, elle rend la circulation visible).
- Le compteur public se lit filtré par la toile des rencontres prouvées (`astro_economy::wot`) pour neutraliser les faux comptes qui gonfleraient la profondeur sans gain financier.
- Retirer son inscription = suppression NIP-09 (`kind 5`).

## 7. Vie privée

- Le spectateur ne connaît que sa propre lignée, jamais l'arbre entier.
- Les virements du registre `kind 30852` sont lisibles par ceux qui lisent le registre : un observateur peut rapprocher les virements d'un même lot. Cela est dit clairement au membre avant confirmation.
- Le paiement est toujours déclenché par un geste explicite en deux temps (comme *Payer*), jamais prélevé en silence.
- Pour les fiches botaniques publiées vers le commun (`PlantData`), les notes personnelles sont retirées et la position est arrondie à la maille UMAP (`0,01°`).

## 8. Découpage en lots

| Lot | Contenu | Argent ? | Statut |
|---|---|---|---|
| **1** | Structures Rust/Serde (`DeciZen`, `Bps`, `SplitRule`, `Rights`, `PricingMode`, `Credit`, `SourceRef`, `RemixKind`, `CommonsBlock`, `PlantData`, `Hop`, `ShareChain`), signatures BIP-340, aplatissement et vérification du DAG (`flatten_dag`), calcul exact Voie A (`compute_payout_batch`), prise en charge `photo`/`plant`/recette dans `src/media.rs` et passerelle `src/plant_fiche.rs`, tests unitaires et hostiles | non (moteur prêt) | **fait** (`astro_economy::cascade`) |
| **1b** | Transport du `ShareChain` dans les enveloppes NIP-59 (`sharing.rs`, `share_inbox.rs`) et stockage local (`kv` scope `share_chain`), exposition dans `MediaShare::trace` et `GET /api/v1/ucloud/media/chain` | non | **fait** (`src/cascade_share.rs`, `src/sharing.rs`, `src/share_inbox.rs`) |
| **2** | Inscription et retrait sur la liste officielle (`kind 30005`, `d = chaine:<work_sha>` / NIP-09) avec vérification de la chaîne prouvée et filtrage par `astro_economy::wot` (`POST /api/v1/ucloud/media/chain/register`) | non | **fait** (`src/cascade_share.rs`, `src/media_share.rs`) |
| **3** | Émission effective du `PayoutBatch` (Voie A) via `ZenCredit` (`cascade_preview` / `cascade_pay`, suivi d'état rejouable dans `kv`, mode île, contrôle frontière de plan, `POST /api/v1/ucloud/media/pay/preview` et `POST /api/v1/ucloud/media/pay`) et tests de bout en bout | oui | **fait** (`src/zen_credit.rs`, `src/ui_backend/ucloud.rs`) |
| **4** | Publication vers le commun / remix « recette » non destructif (`publish_commons_or_remix`, dont passerelle botanique depuis `plant_fiche.rs`, `POST /api/v1/ucloud/media/remix`) et contresignature BIP-340 de générique (`Credit::attest`, `CREDIT_SIG_KIND = 27_858`) | oui | **fait** (`src/cascade_share.rs`, `crates/astro-economy/src/cascade.rs`) |
