# G1N² : le LOVE, un dividende universel, des lieux et des Vœux

*État : conception (2026-11-23). Rien n'est codé hormis la toile des rencontres (`astro_economy::wot`). Tout sera **fermé par
défaut**. Les simulations qui fondent chaque règle sont au § 9, les questions encore ouvertes au § 10.*

Cette note décrit l'économie du commun **G1N²** d'une constellation UPlanet et le jeu qui la fait vivre : entretenir des lieux
et réaliser des Vœux, pour faire et pour transmettre le savoir-faire. Elle reprend l'expérience de la Forêt Enchantée
(2021-2022) et de son jeu « 3 2 1 DU » : pas de salaire, le dividende comme part du commun. Son dividende universel
s'appelle le **LOVE**.

## 1. Principes

- **Le LOVE est le dividende universel (DU) de cette UPlanet.** Tout se compte en LOVE, sans jeton : chaque créateur reçoit
  **1 LOVE entier par jour** ; prélèvements, jetons de gratitude,
  Vœux, seuils : tout est exprimé en LOVE. Il n'y a pas d'unité quantitative à émettre ni à convertir, et pas de fonte. Le
  registre tient des soldes en centièmes de LOVE (`astro_core::Amount`, jamais de flottant).
- **Le jeu ne crée aucune monnaie.** La seule création est le LOVE du jour de chaque créateur. Tout le reste (tirages,
  prélèvements, votes, paiement des Bâtisseurs) est un **transfert**. Invariant vérifiable dans le registre chaîné : la somme
  de tous les soldes égale la somme des LOVE créés.
- **Deux économies séparées.** Le **Ẑen** (1 Ẑ = 1 €, la coopérative est la banque centrale de ses Ẑen) paie les PAF et les
  besoins en € ; la **G1N²** paie l'entretien et les Vœux. Aucune conversion entre G1N², Ğ1 et Ẑen. Leur seul lien est un
  **signal fléché** (§ 5.4).
- **Qui crée** : un compte qui entre dans la **toile des rencontres** (§ 6) devient une **identité numérique** (DID,
  kind 30800, comme avec la Ğ1) et crée son LOVE chaque jour.

## 2. Activation et création répartie

- **Activation par constellation : UPlanet 10101.** Le secret de la `swarm.key` (sa dernière ligne, 64 caractères hexa) *est*
  `UPLANETNAME` (`src/serve.rs`). Une constellation dont `UPLANETNAME` **commence par `10101`** porte la G1N². Toute station le
  vérifie sans configuration partagée, et la règle est fixée à la naissance de la constellation, comme la genèse d'une monnaie.
  Une telle clé s'obtient en un million d'essais environ (quelques secondes) ; les 20 bits imposés laissent 236 bits de
  secret. Le jeu s'ouvre ensuite à une **date publiée par la coopérative** (pas d'activation par accident).
- **La création est répartie entre les stations.** Le LOVE d'un joueur est créé par la station qui tient le **bail géographique**
  de son UMAP (`src/geo_lease.rs` : une seule station écrit par case ; si elle se tait 36 h, la suivante reprend). Le joueur
  **s'y inscrit, ou y déménage** son compte le temps de sa résidence. Une seule UMAP de résidence à la fois.
- **Pas de double création.** Le LOVE du jour d'une identité est une écriture du registre chaîné (kind 30852, champ `prev`) :
  deux stations qui créeraient le même jour produiraient une bifurcation, ce qui gèle le compte (règle existante du registre).

## 3. Trois échelles

| Échelle | Ce qu'elle tient | Rôle |
|---|---|---|
| **Personne** (MULTIPASS au cœur de son village N1/N2) | son portefeuille, tenu par son UMAP | crée son LOVE, agit, vote, donne son avis |
| **UMAP** (case de 0,01° × 0,01°, environ 1 km², clé dérivée de `UPLANETNAME` et des coordonnées) | les portefeuilles de ses résidents, la **Boîte à Gratitude**, le **Coffre**, les portefeuilles de ses **Vœux** | toute la vie monétaire du lieu |
| **SECTOR, REGION** | **aucun portefeuille**, ne prélèvent rien | affichent la somme des LOVE en circulation dans leurs UMAP et font connaître les Vœux (§ 5.2) |
| **Constellation** (`swarm.key`) | rien | vote les règles ; ses Capitaines sont les racines de la toile |

Les LOVE ne passent d'une UMAP à l'autre que par les joueurs : réalisation de Vœux et dons volontaires.

## 4. L'UMAP

### 4.1 La Boîte à Gratitude

- **Quantité finie et entretenue** : **10 R par résident** (R = 3 : 30 LOVE par résident ; R = 5 : 50 LOVE).
- **Liste des actions** : tenue par les habitants. Quand le lieu devient plus complexe à maintenir, la liste s'allonge.
- **Tirage** : une action d'entretien donne un tirage en aveugle, jetons de 1 à 21, **moyenne R** (30 LOVE en 10 jetons pour
  R = 3). Le hasard évite de marchander la valeur d'une tâche.
- **Preuve sur place** : l'action est **attestée par un autre résident présent**, qui scanne le code « Se rencontrer » de
  celui qui a agi (avec l'UMAP et l'action). Pas de matériel en plus ; un QR imprimé, lui, se photographierait et se rejouerait.
- **Compteur de gratitude** : LOVE reçus en gratitude − LOVE rendus. **Au-delà de 4 R, on continue d'entretenir, mais le jeton va
  au Vœu de son choix**, pas dans son portefeuille : le vote est obligatoire et ne coûte rien au joueur. Chaque journée de
  vacances (résidence sans action) rend R au compteur.
- **Pourquoi** : celle ou celui qui fait toujours tout avant les autres est arrêté au plafond, chacun garde sa part
  d'entretien, et les plus actifs deviennent ceux qui orientent le commun vers les Vœux.

### 4.2 Le Coffre

- **Prélèvement quotidien** sur chaque résident, selon sa journée (règle « 3 2 1 DU ») :

  | Journée | Prélevé |
  |---|---|
  | aucune action | **3 LOVE** |
  | au moins une action d'entretien (et son tirage) | **2 LOVE** |
  | participation à la réalisation d'un Vœu | **1 LOVE** |
  | **personne fragile** : R = 1, son LOVE va à l'UMAP, elle est en vacances (gîte, couvert, services du lieu) | **1 LOVE** |

- Il **remet la Boîte à son niveau** chaque soir à 20h12.
- Il garde **28 jours de réserve** ; au-delà, son surplus **abonde les Vœux de l'UMAP** : une part de base par Vœu ouvert, plus
  selon les votants distincts, (base + Σ √votes)², jamais au-delà de ce qui manque au Vœu.
- **R s'ajuste seul** tous les 28 jours : il baisse si le Coffre dépasse deux fois sa réserve, il monte s'il tombe sous la
  moitié (facteur de 1/3 à 2 sur la grille 3 / 2 / 1). C'est le niveau optimal : le lieu prélève ce que sa Boîte et ses Vœux
  peuvent rendre. Les habitants voient R et ce que le lieu rend (personnes nourries, nuitées, ateliers).
- **Avance du dépôt d'arrivée** : pour un joueur qui n'est pas encore créateur, le Coffre (alimenté au préalable par les
  sommes versées à l'UMAP) avance sa part de Boîte ; il la rembourse sur ses premiers LOVE.

## 5. Les Vœux

### 5.1 Vie d'un Vœu

1. **Émis** sur une UMAP, avec un **dossier** : le plan de réalisation, partageable dans le cercle N². Fiche versionnée comme
   les biens communs et les savoirs (`COMMUNS.md`, kinds 30500-30505). Un Vœu sert à **faire** (un abri, un four, un moulin)
   ou à **transmettre un savoir-faire** (un atelier, une formation).
2. **Soutenu** : sa cagnotte (portefeuille propre, tenu par l'UMAP) monte avec la gratitude au-delà de 4 R, les dons
   volontaires et l'abondement du Coffre.
3. **Visible par paliers dynamiques** (§ 5.2).
4. **Réalisé** : les **Bâtisseurs** choisissent le moment et fixent le plan. La réalisation est **filmée et documentée** pour
   partager le savoir-faire (module Média, Mon blog).
5. **Payé** aux Bâtisseurs à la **réalisation documentée, confirmée par au moins un avis** d'un votant ou d'un usager présent
   sur place, **pas d'un Bâtisseur**.
6. **Évalué** : l'avis nourrit la toile des talents (§ 6).

Un Vœu irréalisable ne fait de mal à personne : sa cagnotte attend et, à l'échéance, revient au Coffre de son UMAP.

### 5.2 Paliers de visibilité dynamiques

Un Vœu est visible dans son UMAP, puis dans son SECTOR, puis dans la REGION, à mesure qu'il reçoit des votes de **personnes
distinctes** :

- SECTOR à **⌈√ résidents de l'UMAP⌉** votants ; REGION à **⌈√ résidents du SECTOR⌉** ;
- un Vœu sans nouveau votant pendant 28 jours **redescend d'un palier**.

Les seuils suivent la population : une petite UMAP n'a pas besoin de 25 votants pour se faire connaître. Les **merveilles**
(Vœux trop grands pour une seule UMAP) n'existent que par cette visibilité.

### 5.3 Comme un jeu de civilisation

| Jeu de civilisation | G1N² |
|---|---|
| Ville, province, empire | UMAP, SECTOR, REGION |
| Influence qui s'étend | paliers de visibilité d'un Vœu |
| Arbre technologique | chaîne de production, de la graine à l'assiette ; savoir-faire transmis |
| Merveilles | grands Vœux réalisés par plusieurs UMAP |
| Tour de jeu | le 20h12 ; la saison de 28 jours |
| Brouillard de guerre | visibilité N1/N2, puis géographique |
| Culture, rayonnement | Vœux réalisés, visités, documentés ; talents reconnus |

### 5.4 La part en € d'un Vœu : un signal fléché

Un Vœu peut avoir besoin d'€ (matériaux, machines) : il porte alors une **jauge en Ẑen**, séparée de sa cagnotte en LOVE. Une
identité numérique peut **flécher** une partie de ses LOVE vers le compte de la coopérative (`UPLANETNAME_G1`) pour signaler où
les € sont attendus. Ce n'est **pas un achat** de Ẑen : il n'existe aucun taux de change. Les LOVE fléchés vont aux Bâtisseurs
quand la coopérative finance la jauge Ẑen, et reviennent au Vœu (puis à son UMAP) sinon : ni taux de change, ni puits.

## 6. Les toiles

- **Toile des rencontres** (fait : `crates/astro-economy/src/wot.rs`, `GET /api/v1/me/wot`, carte dans *Mes cercles*,
  `PAYER_QR.md`). Une rencontre prouvée par QR = une certification ; racines = Capitaines de la constellation ; règles de la
  toile Ğ1 (5 certifications, distance 5, 80 % des référents, 2 ans, 100 par émetteur). Y entrer fait de son compte une
  identité numérique créatrice du LOVE.
- **Toiles des talents** (« savoir » et « savoir-faire » : construire un abri, conduire, greffer…). Après la réalisation d'un
  Vœu, un avis favorable des votants et des usagers **certifie** les Bâtisseurs. Même moteur, seuil propre au talent (5 comme
  la Ğ1, 12 pour un « permis »). Certification **positive et périssable** (2 ans) : ne pas certifier n'est pas une mauvaise
  note. L'avis ne décide pas du paiement (au-delà du premier, qui confirme la réalisation) : un jugement qui ne retient pas
  d'argent reste honnête.

## 7. Analyse systémique

**Bilan d'une journée** (R = 3, jeton moyen 3 LOVE, 1 LOVE créé) ; la somme vaut toujours +1 :

| Journée | Joueur | Coffre |
|---|---|---|
| sans action | +1 − 3 = −2 | +3 |
| entretien + tirage | +1 − 2 + 3 = +2 | +2 − 3 (remise à niveau) = −1 |
| réalisation d'un Vœu | +1 − 1 = 0 (payé plus tard par le Vœu) | +1 |
| personne fragile | +1 − 1 = 0 | +1 |

**Boucles de rétroaction.**

| Boucle | Type | Effet | Ce qui la tient |
|---|---|---|---|
| Coffre ↔ Boîte | équilibrante | la Boîte reste pleine | réserve de 28 jours |
| Coffre → R | **équilibrante** | le lieu prélève ce qu'il peut rendre | R auto-ajusté (sans lui, le Coffre est un puits, § 9.2 et 9.4) |
| Coffre → Vœux → Bâtisseurs | équilibrante | la monnaie revient aux joueurs | abondement du surplus |
| Plafond 4 R → gratitude au Vœu | équilibrante | rotation de l'entretien | vote reçu en gratitude, jamais payé de sa poche (§ 9.3) |
| Votes → visibilité → plus de votes | renforçante | un Vœu populaire s'étend | paliers par votants distincts, redescente à 28 jours |
| Réalisations → certifications | renforçante | quelques Bâtisseurs reconnus | certifications périssables, plafonnées |

**R est un facteur d'échelle.** Jeton, Boîte et plafond étant proportionnels à R, R ne change pas l'équilibre : il règle le
poids du lieu dans la vie du résident (le prix d'une journée sans action). L'équilibre réel dépend de la longueur de la liste
rapportée aux résidents.

**La motivation n'est pas l'enrichissement.** Dans toutes les configurations saines, l'avoir des actifs égale à peu près celui
des passifs, ou un peu moins (§ 9.4) : la boucle est fermée, la monnaie revient par les Vœux. On est motivé par ce que l'on oriente (la
gratitude au-delà de 4 R choisit les Vœux), par la reconnaissance (talents) et par la visibilité de ce qu'on réalise : c'est
l'esprit de « 3 2 1 DU ». Le modèle ne mesure pas cette motivation ; seule une expérience réelle le dira.

## 8. Garde-fous

- Aucune note individuelle publiée ; la répartition repose sur des personnes distinctes et des certifications positives et
  périssables. Rien ne doit ressembler à un « crédit social » (mise en garde du document de la Forêt Enchantée).
- Règles et seuils publics et votés ; on ne sanctionne pas, on remercie.
- Chacun voit ce que le système sait de lui.
- Tout est fermé par défaut ; la G1N² n'est jamais présentée comme active tant qu'elle ne l'est pas.

## 9. Simulations

Scripts : `docs/design/sim_du/` (Python 3 + numpy). `love_sim.py` est le modèle, aux règles de cette note, avec un
interrupteur par alternative écartée ; `scenarios.py` reproduit les tableaux 9.3 à 9.6 (moyennes sur 4 graines) :
`python3 docs/design/sim_du/scenarios.py [vote|regulation|paliers|love]`. Ordres de grandeur et sens de variation fiables,
pas les décimales (prix fixes, réseaux stylisés, motivation fixée d'avance). **Conservation exacte** : la somme de tous les
avoirs égale toujours la somme des LOVE créés.

### 9.1 Chemin parcouru (modèles exploratoires, retirés)

- La formule actuelle de TrocZen **s'éteint** (100 le premier jour, puis 0 dès l'année 9).
- Le DU de départ (100 ou 11) ne change rien aux résultats relatifs.
- Une création qui dépend du réseau doit être bornée (racine, plafond) ; compter les clients à sens unique déjoue la triche.
- Le niveau de résidence R n'indique pas la valeur : il multiplie les Vœux mais exclut les fragiles.
- Une répartition par « score de village » ou par financement quadratique délaisse le lieu le plus pauvre.

### 9.2 Le commun sans retour est un puits (modèle exploratoire en cascade UMAP → SECTOR → REGION, retiré)

1 REGION, 3 SECTOR, 9 UMAP de 6 à 40 résidents, 15 % de fragiles, 3 ans. Sans boucle de retour : **96 % de la monnaie
immobilisée** dans les communs, 59 % des membres sans 3 LOVE, fragiles en retard 75 % des jours. C'est ce qui a conduit à tout
garder dans l'UMAP et à ajuster R.

### 9.3 Le vote au-delà de 4 R (`scenarios.py vote`)

| Vote obligatoire au-delà de 4 R | Entretien | Membres sans 3 LOVE | Retards des fragiles |
|---|---|---|---|
| payé de son portefeuille, R fixe | **11 %** | 72 % | 91 % |
| reçu en gratitude, R fixe | 44 % | 20 % | 67 % |
| payé de son portefeuille, R auto-ajusté | 37 % | 0 % | 8 % |
| **reçu en gratitude, R auto-ajusté** | **44 %** | **0 %** | 4 % |

Payé de sa poche, le vote obligatoire fait chuter l'entretien dès que l'argent manque ; reçu en gratitude, il ne coûte rien
au joueur et garde l'entretien dans tous les cas.

### 9.4 Tout dans l'UMAP, avec ou sans R auto-ajusté (`scenarios.py regulation`)

| Réglage | Entretien | Membres sans 3 LOVE | Retards fragiles | Monnaie chez les membres | Au Coffre | Jours de Vœu | Actifs / passifs |
|---|---|---|---|---|---|---|---|
| R fixe, peu de Vœux | 0,49 | 64 % | 98 % | 0 % | **93 %** | 2 % | 0,96 |
| R fixe, Vœux moyens | 0,44 | 20 % | 67 % | 2 % | 88 % | 11 % | 1,18 |
| R fixe, beaucoup de Vœux | 0,38 | 4 % | 19 % | 20 % | 25 % | 21 % | 1,50 |
| **R auto-ajusté, peu de Vœux** | **0,48** | **0 %** | 6 % | 61 % | 31 % | 3 % | 0,82 |
| **R auto-ajusté, Vœux moyens** | **0,44** | **0 %** | 4 % | 85 % | 7 % | 11 % | 0,82 |
| + **fragiles à R = 1** | 0,44 | 0 % | **0 %** | 84 % | 8 % | 11 % | 0,80 |

Avec R fixe, le Coffre est un puits sauf si un jour sur cinq va à la réalisation de Vœux ; **R auto-ajusté referme la boucle
dans tous les cas**, chaque UMAP trouvant son niveau. Dans les configurations saines, les plus actifs finissent même un peu
moins riches que les passifs (0,8) : au-delà de 4 R, leur gratitude part aux Vœux. L'écart en faveur des actifs n'apparaît
que quand l'argent manque.

### 9.5 Paliers de visibilité, 10 % de merveilles dix fois plus coûteuses (`scenarios.py paliers`)

| Paliers | Merveilles réalisées | Vœux réalisés | Vœux par tête, petites UMAP | Vœux par tête, grandes UMAP |
|---|---|---|---|---|
| aucun (UMAP seule) | 55 % | 766 | 5,9 | 4,0 |
| fixes (5 / 25 votants) | 67 % | 684 | 5,3 | 3,5 |
| **dynamiques (√ population, redescente à 28 j)** | **72 %** | 703 | **6,0** | 3,6 |

La visibilité sert aux merveilles ; les paliers dynamiques en réalisent le plus et ne pénalisent pas les petites UMAP, à
l'inverse des paliers fixes. Prix : un peu moins de petits Vœux, la gratitude se répartissant plus loin.

### 9.6 Compter en LOVE sans jeton, 6 ans, 30 % d'arrivées en année 2 sans avoir (`scenarios.py love`)

| Variante | Avoir des nouveaux / des anciens | Membres sans 3 LOVE | Inégalité des avoirs (Gini) |
|---|---|---|---|
| **en LOVE, sans réévaluation** | **1,00** | 0 % | 0,13 |
| croissance de 10 %/an (vue en LOVE) | 1,02 | 0 % | 0,13 |

Les nouveaux venus rattrapent les anciens par la circulation du commun (prélèvements, Vœux, R auto-ajusté), pas par la
croissance du DU : **le comptage en LOVE sans jeton suffit.** Limite : le marché simulé (chacun dépense 5 % de son avoir dans son
SECTOR) égalise fortement ; un marché réel le fera moins, ce qu'une expérience réelle devra observer.

### 9.7 Ce que les simulations ne disent pas

- si les règles font naître l'envie d'entretenir et de bâtir (la motivation est fixée d'avance dans le modèle) ;
- le comportement d'un vrai marché, de vraies chaînes de production, de vrais tricheurs ;
- l'activation par préfixe et la création répartie, qui relèvent de la sécurité, pas de l'économie (§ 10).

## 10. Questions ouvertes

Tranché : préfixe `10101` sur `UPLANETNAME` ; date d'ouverture dans la configuration coopérative (§ 11.1).

1. **Numéros des nouveaux kinds** (30856 Vœu, 30857 action attestée, 30858 avis) : à confirmer, et à reporter dans
   `nostr-nips/` si on les publie.
2. **Statut de personne fragile** : demandé par la personne et confirmé par un second résident (proposition § 11.4), ou
   décidé par l'assemblée du lieu ?
3. **Changement de station du bail** pendant une journée : vérifier par un test qu'aucune double création n'est possible
   (le `d = love:<date>` unique par auteur devrait suffire).
4. **Seuils** : la forme ⌈√ population⌉ et la redescente à 28 jours sont simulées, les valeurs restent à voter.
5. **Expérience réelle** : un lieu pilote, avec l'application qui enregistre actions, tirages et Vœux, pour mesurer ce que les
   simulations ne savent pas (la motivation).

## 11. Spécification de mise en œuvre

**Noms dans le code** : le membre voit « LOVE » ; le code préfixe tout par `g1n2_` (tables, modules, routes) pour ne pas
confondre avec l'existant : le **Love Ledger** (`astro_economy::love`, table `love_ledger`) enregistre le bénévolat du
Capitaine quand sa station est en déficit, et le rôle d'identité `love` est la clé LOVE de chaque MULTIPASS (extension Ğ1-N²
de TrocZen), porteur naturel du compte en LOVE.

### 11.1 Activation

- `g1n2::enabled(uplanet_name)` : vrai si `UPLANETNAME` commence par `10101` (constante `G1N2_PREFIX`). Jamais en ORIGIN.
- **Ouverture** : clé `G1N2_OPEN_AT` (date ISO) de la configuration coopérative (kind 30800 `d=cooperative-config`, signée par
  la clé `UPLANETNAME_G1`, `src/coop_config.rs`). Avant cette date, rien n'est créé ; l'écran dit « le LOVE s'ouvrira le … ».
- `astroportd swarm-key --prefix 10101` : génère une clé portant le préfixe (essais successifs).

### 11.2 Comptes

| Compte | Clé qui signe | Tenu par |
|---|---|---|
| Résident | sa clé Nostr (MULTIPASS hébergé) | la station du bail de son UMAP |
| Boîte, Coffre de l'UMAP | dérivées du secret de l'UMAP : HKDF(secret UMAP, `g1n2/boite`), `g1n2/coffre` | la station du bail |
| Vœu | HKDF(secret UMAP, `g1n2/voeu/<id>`) | la station du bail de l'UMAP du Vœu |
| Coopérative (signal fléché) | `UPLANETNAME_G1` | la coopérative |

Le secret d'une UMAP se dérive de `UPLANETNAME` et des coordonnées (`geo_lease.rs`) : la station qui reprend le bail retrouve
les mêmes comptes.

### 11.3 Écritures du registre (kind 30852, `t=love`)

Même validateur que le registre Ğ1-N² (`astro_economy::ledger` : signature, chaînage `prev`, `d` unique par auteur, fourche =
gel, pas de découvert), paramétré par l'étiquette `t=love` pour ne jamais mêler LOVE et Ẑen. Montant en centièmes de LOVE.
Chaque écriture porte `op` :

| `op` | De → vers | Règle propre |
|---|---|---|
| `du` | création → résident | `d = love:<AAAA-MM-JJ>` (une par jour et par identité : la double création est impossible, même entre deux stations) ; l'auteur est dans la toile des rencontres ce jour-là ; signée par la station du bail |
| `prelevement` | résident → Coffre | `d = prel:<date>` ; montant selon la journée (§ 4.2) × facteur R de l'UMAP ; `fragile` → 1 LOVE |
| `remise` | Coffre → Boîte | 20h12 ; jusqu'à 10 R par résident |
| `gratitude` | Boîte → résident, ou → Vœu au-delà de 4 R | cite l'attestation d'action (`e`) ; montant tiré dans le sac de jetons du jour |
| `don` | résident → Vœu | volontaire |
| `abondement` | Coffre → Vœu | au-delà de la réserve ; (1 + Σ √votes)² ; jamais plus que ce qui manque |
| `paiement` | Vœu → Bâtisseurs | cite la réalisation documentée et au moins un avis d'un non-Bâtisseur |
| `retour` | Vœu → Coffre | à l'échéance (180 jours) d'un Vœu non réalisé |
| `fleche` / `fleche-retour` | résident → coopérative → Bâtisseurs ou Vœu | signal fléché (§ 5.4), cite le Vœu |

Invariant (test) : somme des soldes `t=love` = somme des `op=du`.

### 11.4 Événements nouveaux (numéros à confirmer)

| Kind | Objet | Contenu |
|---|---|---|
| 30856 | **Vœu** (remplaçable, `d` = id) | `umap`, coût en LOVE, coût en Ẑen facultatif, échéance, état (`ouvert`, `chantier`, `realise`, `expire`), dossier (Markdown, versions comme `COMMUNS.md`), lien vers la documentation de la réalisation |
| 30857 | **Action attestée** | signée par le **résident témoin** : `p` = qui a agi, preuve de présence « Se rencontrer » de celui-ci (`astro_economy::meet`), `umap`, action de la liste |
| 30858 | **Avis** | signé par un votant ou un usager présent : `e` = Vœu, favorable ou non, talent(s) certifié(s) ; un avis favorable certifie les Bâtisseurs dans la toile du talent (2 ans) |

La liste des actions et le statut de personne fragile sont tenus par les habitants dans la fiche de l'UMAP (événement
remplaçable signé par la clé de l'UMAP, à la demande d'un résident et confirmé par un second).

### 11.5 Projections (rejouables, `schema.sql`)

`g1n2_balance` (compte, solde), `g1n2_resident` (identité, UMAP, fragile, compteur de gratitude, avance restant due),
`g1n2_umap` (facteur R, réserve, dernière remise), `g1n2_voeu` (Vœu, état, palier, dernier votant), `g1n2_action`
(attestations du jour). Toutes alimentées par `astro-sync::Projector` à partir des événements ci-dessus.

### 11.6 Le 20h12 de la station du bail, pour chacune de ses UMAP

1. `du` de chaque résident créateur (toile du jour) ;
2. `prelevement` selon la journée (action attestée → 2 ; jour de chantier attesté par l'équipe → 1 ; fragile → 1 ; sinon 3) ;
   les vacances rendent R au compteur ;
3. `remise` de la Boîte ;
4. Vœux : passage en chantier (cagnotte ≥ coût), `retour` des Vœux échus, recalcul des paliers ;
5. `abondement` au-delà de la réserve ;
6. tous les 28 jours, facteur R : −1/6 si Coffre > 2 × réserve, +1/6 si Coffre < réserve / 2, borné à [1/3, 2].

Les tirages de `gratitude` se font au fil de la journée, dès l'attestation.

### 11.7 Valeurs par défaut (votables par la constellation)

| Paramètre | Valeur |
|---|---|
| R de départ | 3 |
| Boîte | 10 R par résident |
| Sac de jetons | 1, 1, 1, 2, 2, 2, 3, 4, 5, 9 (× R / 3) : moyenne R, de 1 à 21 possibles |
| Plafond de gratitude | 4 R |
| Prélèvement | 3 / 2 / 1 (× R / 3 × facteur), fragile 1 |
| Réserve du Coffre | 28 jours |
| Facteur R | pas de 1/6, entre 1/3 et 2, tous les 28 jours |
| Échéance d'un Vœu | 180 jours |
| Paliers | SECTOR ⌈√ résidents de l'UMAP⌉, REGION ⌈√ résidents du SECTOR⌉ votants, redescente après 28 jours sans votant |
| Avis pour payer | au moins 1, d'un non-Bâtisseur |
| Toile des rencontres | 5 certifications, 5 pas, 80 %, 2 ans, 100 par émetteur |

### 11.8 Arrivée et départ

- **Arrivée** : inscription ou déménagement sur la station du bail ; la part de Boîte (10 R) est avancée par le Coffre et
  remboursée sur les premiers LOVE.
- **Départ** (déménagement, `moved`) : la part de Boîte revient au partant, moins l'avance restant due ; le compteur de
  gratitude s'éteint ; son solde le suit dans sa nouvelle UMAP (écriture `don` à lui-même, signée par l'ancienne station).

### 11.9 Tests à écrire

- conservation (somme des soldes = somme des `du`) après un `replay` ;
- double création impossible : deux stations, changement de bail en cours de journée ;
- hostile : attestation par soi-même, avis d'un Bâtisseur, tirage sans attestation, `paiement` sans avis, `abondement` au-delà
  du manque, Vœu d'une autre UMAP payé par ce Coffre, ouverture avant `G1N2_OPEN_AT`, constellation sans préfixe ;
- 20h12 rejoué deux fois le même jour : aucun effet (idempotence par `d`) ;
- `love_sim.py` sert d'oracle pour un test de bout en bout sur une UMAP simulée.

### 11.10 Lots

1. **Registre LOVE** : activation, `du` et invariant de conservation, projection des soldes, écran « Mon LOVE ».
2. **UMAP** : Boîte, Coffre, prélèvement, remise, facteur R, liste des actions, attestation (30857), statut fragile.
3. **Vœux** (30856) : dossier, cagnotte, gratitude au-delà de 4 R, abondement, paliers, partage N², jauge Ẑen et signal fléché.
4. **Réalisation, avis (30858) et toiles des talents** (moteur `wot` par talent).
5. **Écrans** : Mon lieu, Vœux, Mes talents (membre) ; ouverture et règles (Capitaine).
