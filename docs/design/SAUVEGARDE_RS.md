# Sauvegarde privée par codage à effacement (Reed-Solomon k-sur-n entre amis)

> Document de conception. Statut : **décisions Q1–Q10 tranchées** (§ 9) ; **lot 1 fait** (corps, code, en-tête ARSF :
> crate `crates/astro-erasure`) ; **lot 2 fait** (paquets, index, clé HKDF, extraction : `crates/astro-dav/src/backup_pack.rs`) ;
> **lots 3 et 4 faits** (envoi et restauration, branchés dans `src/private_backup.rs` + `src/private_backup/rs.rs`, actifs par
> défaut pour les gardiens qui annoncent `rs`) ; **lot 5 fait** (réparation nocturne, contrôle de possession, ajout et retrait de
> gardien, ramassage des orphelins, plafond nocturne côté gardien, Q5, raccourci `held_whole` : `src/private_backup/repair.rs`) ;
> **lot 6 fait** (compactage, réécriture des paquets repris d'une restauration, migration des copies complètes :
> `src/private_backup/compact.rs`) ; **lot 7 fait** (interface membre et Capitaine, guide). Tous les lots sont faits ; reste à
> mesurer sur un vrai réseau et sur Raspberry Pi (§ 8.4).
> Périmètre : remplacer, dans `src/private_backup.rs`, la copie complète chez chaque ami par des fragments
> Reed-Solomon répartis entre les gardiens : un « RAID 6 généralisé entre stations amies ».

Les noms de tables, de champs et d'opérations **nouveaux** sont marqués *(proposé)* ; tous les autres existent
dans le code à la date du document.

---

## 1. État actuel exact

### 1.1 Ce qui part, quand, vers qui

- Un accord par couple (propriétaire, hébergeur) : table `backup_pacts` (`owner_hex`, `host_hex`, `status` ∈
  `pending|accepted|declined|revoked`, `quota_bytes` fixé **par l'hébergeur**, `enabled` choisi par le propriétaire).
  État **local**, tenu par chaque station concernée (`crates/astro-db/src/schema.sql`).
- `run_once()` (envoi nocturne, 20h12 solaire) parcourt les accords `accepted` + `enabled` dont le propriétaire est
  hébergé ici. Pour **chaque** hébergeur, il envoie :
  1. le blob du manifeste uCloud (`manifest_item` : `kv(ucloud_manifest_cid, owner)`), s'il n'y est pas encore ;
  2. jusqu'à `PER_PASS = 100` fichiers privés (`pending_files`) : `ucloud_entries` de type `file`, hors `/Partages/`
     et `/Sauvegardes/`, ni dans `ucloud_sharing`, ni dans `face_shares`, ni dans une invitation active, et pas
     encore dans `backup_items` pour cet hébergeur ; triés par `mtime`.
  Un blob qui ferait dépasser `quota_bytes` est sauté (les plus petits passent encore).
- **Même station** : `adopt_backup_blob` range directement `/Sauvegardes/De_<nom>/<cid>` dans le VFS de l'hébergeur
  (`origin = 'backup'`, `data.backup_of = owner`) ; aucun octet copié.
- **Autre station** : message NIP-59 (gift wrap kind 1059, rumeur `kind::FILE`), tags
  `["t","astro-backup"]`, `["op","item"]`, `["cid",…]`, `["size",…]`, `["station",<peer>]`, contenu
  `ipfs://<cid>`. Cadence `SEND_PACE = 250 ms` entre deux envois. L'hébergeur (`store_item`) vérifie l'accord et
  le quota, rapatrie le blob par `/astro/blob/1.1.0` depuis la station indiquée (`ensure_blob`, reculade
  exponentielle, `MAX_TRIES = 12`, état `kv(backup_inbox_tries, <wrap id>)`), puis l'adopte comme ci-dessus.
- Le propriétaire inscrit `backup_items(owner_hex, host_hex, cid, size, at, missing_since)` **dès l'envoi du
  message** : il n'y a **pas d'accusé de réception**. L'hébergeur tient la même table de son côté.

### 1.2 Messages NIP-59 (`t = astro-backup`)

| `op` | Sens | Rôle |
|---|---|---|
| `request` / `accept` / `decline` / `revoke` | accord | demande, réponse (avec `quota`), retrait |
| `item` | propriétaire → hébergeur | un blob à garder |
| `drop` | propriétaire → hébergeur | blobs à retirer (fichier disparu depuis `GRACE_SECS = 7 j`) |
| `share` | propriétaire → hébergeur | part P3 de la clé de secours (Shamir 2-sur-3, `astro-lifecycle/src/share.rs`) |
| `restore` / `restored` | nouvelle station ↔ hébergeur | restauration : l'hébergeur renvoie le manifeste puis les fichiers |
| `moved` | nouvelle → ancienne Home | déménagement |
| `rescue` / `rescued` / `rescue-denied` / `rescue-closed` | secours P3 | « J'appelle un ami » |
| `attachment` | — | pièce jointe avec `expires` |

### 1.3 `kv` utilisés par la sauvegarde

`backup_inbox` (table `claimed`), `backup_inbox_tries`, `backup_share_sent`, `backup_share_held`,
`backup_restore_job` (renvoi côté ami, clé `<ami>:<propriétaire>`), `backup_rescue_out`, `backup_rescue_in`,
`backup_rescue_asked`, `restore`, `restore_asked`, `restore_home`, `restore_old_home`, `restore_moved`,
`restore_missing`, `moved`, et en lecture `ucloud_manifest_cid` / `ucloud_manifest_rev`.

### 1.4 Restauration actuelle

Carte secrète → nouvelle station → `restore` à chaque proche → chaque ami renvoie de lui-même **le manifeste d'abord**
puis ce qu'il détient (`restored`, `RESTORE_PER_PASS = 40`) ; les fichiers manquants sont redemandés par CID
(`RESTORE_WANT = 200`, `RESTORE_MISSING_ROUNDS = 4` toutes les 6 h). Le pointeur 30078 (`d = ucloud:manifest`,
NIP-44 vers soi) donne `manifest_cid` + `manifest_key` ; le manifeste (UENC v2) contient VFS + trousseau `cid ↔ clé`.

### 1.5 Coûts

- **Place** : `m` amis = `m` copies complètes, soit `m ×` le volume privé. Le premier ami coûte 100 %, chaque
  ami supplémentaire encore 100 %.
- **Réseau** : `m ×` le volume à l'envoi ; à la restauration, n'importe quel ami suffit pour un fichier donné.
- **Messages** : un gift wrap **par fichier et par ami** (des milliers de petites photos = des milliers d'événements
  1059 sur les relais, d'où `SEND_PACE`).
- **Métadonnées** : l'hébergeur voit le nombre de fichiers, la taille exacte de chacun (commentaire assumé dans
  `run_once`), leur CID, et le rythme d'ajout.
- **Sûreté** : avec une disponibilité `p` par ami, `P(restaurable) = 1 − (1−p)^m`.

---

## 2. Modèle proposé

### 2.1 Vue d'ensemble

```
blobs UENC v2 privés (déjà chiffrés, adressés par CID)
        │  concaténés dans l'ordre d'envoi (un blob n'y entre qu'une fois, tous gardiens confondus)
        ▼
flux de sauvegarde ──découpé──▶ paquets de P = 16 Mio (le dernier complété à un multiple de 1 Mio)
        │  chaque paquet chiffré en UENC v2, clé aléatoire par paquet
        ▼
paquet chiffré ──RS(k, n)──▶ n fragments de ⌈P'/k⌉ octets, un par gardien
        │  chaque fragment = en-tête ARSF + données → blob raw, CID = SHA-256
        ▼
gardien i : /Sauvegardes/De_<nom>/<cid du fragment>   (compté dans son quota, comme aujourd'hui)
```

Restent **copiés en entier chez tous les gardiens** (petits et vitaux) :
- le blob du manifeste uCloud (comme aujourd'hui) ;
- l'**index des paquets** *(proposé)* : blob UENC v2 décrit en § 4.3.

### 2.2 Pourquoi des paquets

Coder chaque fichier séparément coûterait, par fichier, `n` blobs, `n` entrées de VFS, `n` messages et un arrondi
de taille ; une photo de 300 Kio en 3-sur-5 deviendrait 5 fragments de 100 Kio. En regroupant dans des paquets
de taille fixe :
- le nombre de messages et d'entrées chez le gardien ne dépend plus que du **volume** (1 fragment par 16 Mio) ;
- la taille de chaque fichier disparaît de la vue du gardien (§ 6) ;
- un gros fichier (vidéo de 4 Gio, ≤ `MAX_BLOB_BYTES = 16 Gio`) s'étale sur plusieurs paquets consécutifs :
  l'index note `(cid, premier paquet, décalage, longueur)`.

16 Mio : fragment de 4 à 8 Mio pour k = 2..4, soit 1 à 2 segments `/astro/blob` (`SEGMENT = 4 Mio`) ; mémoire
d'encodage bornée à `P + (n−k)·P/k` (≈ 32 Mio au pire) — acceptable sur Raspberry Pi. La taille est un paramètre
de l'en-tête, non figé par le code (question ouverte Q2).

### 2.3 Fragments

Code **systématique** : les `k` premiers fragments sont des tranches du paquet chiffré, les `n−k` suivants de la
parité. Indices de fragment `0..=254` sur une matrice de Cauchy fixée une fois pour toutes (§ 4.2) : ajouter un
gardien = calculer une nouvelle ligne de parité **sans toucher** aux fragments existants.

### 2.4 Ce qu'on garde de la déduplication

- **Unicité dans le flux** : un CID n'entre qu'une fois dans les paquets, quel que soit le nombre de gardiens.
  C'est la vraie économie : aujourd'hui `m` amis ⇒ `m` copies.
- **Fichiers déjà détenus par l'ami** : les fichiers partagés (`ucloud_sharing`, `face_shares`, invitations) sont
  déjà exclus par `pending_files` et ce filtre reste. En plus, un blob privé **déjà présent en entier** chez un
  gardien (sonde `HASN`, 4 096 empreintes par flux) est noté `held_whole` dans l'index pour ce gardien : il sert de
  copie supplémentaire à la restauration, mais **ne dispense pas** de le mettre en paquet (sinon le calcul de sûreté
  k-sur-n ne tient plus). Exception possible : très gros blobs déjà tenus en entier par ≥ 2 gardiens (Q5).
- **Suppression** : les paquets sont immuables. Un fichier disparu depuis `GRACE_SECS` (7 jours, inchangé) rend sa
  place « morte » dans son paquet. Le **compactage** nocturne réécrit un paquet dont la part vivante tombe sous 50 % :
  ses blobs vivants repartent dans le flux, puis l'ancien paquet est retiré (`drop` de ses fragments) une fois le
  nouveau acquitté par assez de gardiens. Le propriétaire a toujours les blobs en local : le compactage ne télécharge rien.

---

## 3. Choix de k et n

### 3.1 Règles

- `n` = nombre de gardiens **retenus pour ce paquet** (accord `accepted`, `enabled`, quota restant ≥ un fragment).
  Un fragment par gardien et par paquet, jamais deux : perdre une station ne doit coûter qu'un fragment.
- `k` = le plus grand entier tel que `proba_au_moins_k(dispos, k) ≥ cible`, où `dispos` sont les disponibilités
  mesurées des `n` gardiens (pour mille, fournies par l'étape « disponibilité », non décrite ici) et
  `cible` *(proposé)* = 999 ‰ par défaut (Q3). Au moins `k = 1`.
- Surcoût de place : `n / k` (plus l'en-tête et l'arrondi, < 0,1 % à 16 Mio).
- Cas limites :
  - **1 gardien** : `k = n = 1`, fragment = paquet. C'est la copie complète actuelle, regroupée.
  - **2 gardiens** : `k = 1, n = 2` (miroir) ; le RS n'apporte rien sous 3 gardiens.
  - `k` est **figé par paquet** (dans l'en-tête). Le changer (passer de 2 à 3 pour gagner de la place) impose de
    ré-encoder ; ça se fait au fil du compactage, jamais en bloc.

**Disponibilité ≠ durabilité.** La mesure dit « en ligne quand on la sonde » ; ce qu'on veut, c'est « pas détruite
avant la réparation ». Une station éteinte trois jours n'a rien perdu. La cible porte donc sur la disponibilité au
moment de restaurer, et la réparation (§ 5.4) traite la disparition durable. Les chiffres ci-dessous sont un ordre
de grandeur (disponibilités indépendantes, ce qui est optimiste : deux amis du même village partagent la même coupure).

### 3.2 Exemples (chaque gardien disponible à 90 %)

| Schéma | Place / volume privé | P(restaurable) | Pannes tolérées |
|---|---|---|---|
| Aujourd'hui, 1 ami | 1,00 × | 0,900 | 0 |
| Aujourd'hui, 2 amis | 2,00 × | 0,990 | 1 |
| Aujourd'hui, 3 amis | 3,00 × | 0,999 | 2 |
| RS 2-sur-3 | 1,50 × | 0,972 | 1 |
| RS 2-sur-4 | 2,00 × | 0,996 | 2 |
| RS 3-sur-4 | 1,33 × | 0,948 | 1 |
| RS 3-sur-5 | 1,67 × | 0,991 | 2 |
| RS 3-sur-6 | 2,00 × | 0,999 | 3 |
| RS 4-sur-6 | 1,50 × | 0,984 | 2 |
| RS 4-sur-8 | 2,00 × | 0,9996 | 4 |

Lecture : à place égale (2×), 2 copies complètes donnent 0,990 et tolèrent 1 panne ; RS 3-sur-6 donne 0,999 et en
tolère 3. À sûreté égale (≈ 0,99), on passe de 2× à 1,67× (3-sur-5) ; à 0,999, de 3× à 2×.

---

## 4. Formats

### 4.1 En-tête de fragment `ARSF` v1 *(proposé)*

Le fragment est un blob raw autonome, échangé par `/astro/blob/1.1.0` sans changement (CID `bafkrei…` =
SHA-256 de tout le blob, vérifié à la réception comme aujourd'hui). Octets, entiers gros-boutistes :

| Décalage | Taille | Champ |
|---|---|---|
| 0 | 4 | `"ARSF"` |
| 4 | 1 | version = `0x01` |
| 5 | 1 | code = `0x01` (Cauchy sur GF(2⁸), polynôme `0x11B`, § 4.2) |
| 6 | 1 | `k` (1..=32) |
| 7 | 1 | `index` du fragment (0..=254 ; `< k` = donnée, sinon parité) |
| 8 | 16 | `packet_id` : 16 premiers octets du SHA-256 du paquet chiffré |
| 24 | 8 | `packet_len` : longueur du paquet chiffré (avant bourrage) |
| 32 | 4 | `frag_len` : longueur des données (multiple de 64) |
| 36 | 32 | SHA-256 du paquet chiffré complet (contrôle après décodage) |
| 68 | 4 | réservé = 0 (refusé si ≠ 0 en v1) |
| 72 | `frag_len` | données |

Taille totale = `72 + frag_len`, contrôlée à la lecture. Le paquet est complété par des zéros jusqu'à `k · frag_len`.
`n` n'est pas dans l'en-tête (il peut grandir) : il est dans l'index.

Refus systématiques au décodage (tests hostiles § 8.3) : magie ou version inconnue, `k = 0`, `index ≥ 255`,
`frag_len` incohérent avec la taille du blob ou avec `packet_len` (`k · frag_len < packet_len` ou ≥ `packet_len + k·64`),
fragments d'un même paquet aux en-têtes discordants, deux fragments de même `index`, SHA-256 final faux.

### 4.2 Le code

- Corps GF(2⁸) modulo `x⁸ + x⁴ + x³ + x + 1` (`0x11B`), **le même que `astro-lifecycle/src/share.rs`** ; générateur
  `0x03` pour les tables log/exp (256 + 512 octets, calculées en `const fn`).
- Matrice systématique : `[I_k ; C]` où `C[r][c] = 1 / (x_r ⊕ y_c)` avec `y_c = c` (c ∈ 0..k) et
  `x_r = k + r` pour la ligne de parité d'indice `k + r`. Toute sous-matrice `k × k` est inversible (propriété de
  Cauchy, les `x` et `y` étant distincts) ; d'où la limite `index ≤ 254` et `k ≤ 32` (choix prudent, Q4).
- Décodage : prendre `k` fragments distincts, inverser la sous-matrice `k × k` (Gauss-Jordan dans GF(2⁸)), multiplier.
  Si les `k` fragments de données sont présents, simple concaténation.

### 4.3 Index des paquets *(proposé)*

Blob UENC v2, JSON, copié en entier chez chaque gardien, remplacé à chaque nuit qui change quelque chose :

```json
{ "v": 1, "owner": "<hex>", "rev": 42, "packet_size": 16777216,
  "packets": [ { "id": "<32 hex>", "key": "<64 hex, clé UENC du paquet>", "len": 16777216, "k": 3,
                 "frags": [ { "i": 0, "cid": "bafkrei…", "host": "<hex>" }, … ] } ],
  "blobs":   [ { "cid": "bafkrei…", "packet": 0, "off": 0, "len": 812345, "held_whole": ["<hex>"] } ],
  "legacy":  { "<host hex>": ["bafkrei…", …] } }
```

- Clé UENC de l'index **dérivée** du secret du compte (HKDF-SHA256, info `"astro-backup-index-v1"`) : une station qui
  a restauré le compte depuis la carte secrète la recalcule, sans dépendre du manifeste (pas de cercle).
- Le CID courant de l'index est ajouté au `Manifest` (champ optionnel `backup_index`, `#[serde(default)]`, donc
  `manifest::VERSION` reste 1 et les anciens manifestes se lisent). Si le manifeste est trop ancien, chaque gardien
  renvoie de toute façon l'index qu'il détient ; on prend le `rev` le plus haut **qui se déchiffre**.

### 4.4 Compatibilité

- Une sauvegarde actuelle est le cas dégénéré « 1 sur 1, un paquet par blob, sans en-tête ». Elle reste lisible
  telle quelle : `backup_items` et l'op `item` sont conservés en lecture et en restauration (champ `legacy` de
  l'index). Aucune conversion forcée.
- Migration côté propriétaire : les blobs `legacy` entrent dans le flux de paquets comme les autres ; quand leur
  paquet est acquitté par `≥ k` gardiens **et** que la cible est atteinte, le propriétaire envoie `drop` pour les
  copies complètes (après `GRACE_SECS`, pour garder une marge).
- Négociation : la station de l'ami annonce le support dans `accept` (tag `["rs","1"]` *(proposé)*). Sans ce tag,
  l'ami reste en copie complète (ancienne version d'astroportd) et ne compte pas parmi les `n`.

### 4.5 Vecteurs de test figés (`crates/astro-erasure/tests/vectors.rs` pour 1 à 5 ; 6 dans `crates/astro-dav/tests/backup_pack.rs`, tous faits)

1. Tables : `exp[0..8]`, `log[0x03] = 1`, `mul(0x57, 0x83) = 0xC1` (exemple FIPS-197), `inv(0x53) = 0xCA`.
2. Matrice de Cauchy pour `k = 3`, lignes 3..=5 (9 octets hex figés).
3. Paquet clair de 1 000 octets `i mod 251`, `k = 3, n = 5`, chiffrement **désactivé** pour le vecteur : SHA-256 de
   chaque fragment (en-tête inclus) figés en hex ; décodage depuis les 10 sous-ensembles de 3 fragments.
4. En-tête ARSF complet d'un fragment, octet par octet.
5. Cas `k = n = 1` : fragment = en-tête + paquet, bourrage compris.
6. Index JSON minimal déchiffré avec une clé HKDF issue d'un secret de test fixé (clé privée Nostr `01..20`) : clé,
   JSON et blob UENC v2 (ChaCha20-Poly1305) figés en hex, recoupés par `crates/astro-dav/tests/vector6.py`.

Tout changement de ces valeurs = changement de format = nouvelle version.

---

## 5. Protocole

### 5.1 Nouvelles opérations (`t = astro-backup`) *(proposé)*

| `op` | Sens | Tags | Rôle |
|---|---|---|---|
| `frag` | propriétaire → gardien | `cid`, `size`, `packet`, `i`, `station` | un fragment (ou l'index : `["kind","index"]`) |
| `frag-ack` | gardien → propriétaire | jusqu'à 100 `cid` | fragments rangés (après vérification) |
| `frag-drop` | propriétaire → gardien | jusqu'à 100 `cid` | retirer (compactage, réparation, migration) |

`frag` se traite comme `item` aujourd'hui (même contrôle d'accord et de quota, même `ensure_blob`, même adoption sous
`/Sauvegardes/De_<nom>/`), avec en plus : lecture de l'en-tête ARSF, refus si invalide, refus si un fragment du même
`packet_id` est déjà tenu (un seul fragment par paquet et par gardien).

### 5.2 Envoi (20h12)

1. Construire la liste des blobs privés pas encore en paquet (`pending_files` adapté : jointure sur
   `backup_blobs` *(proposé)* au lieu de `backup_items` par hôte).
2. Remplir des paquets de 16 Mio ; un paquet incomplet attend la nuit suivante, sauf s'il attend depuis 7 jours
   (alors il part, complété au Mio supérieur).
3. Choisir `n` gardiens et `k` (§ 3) ; chiffrer le paquet, encoder, écrire les fragments dans le magasin local
   (référencés dans la base du propriétaire pour que le `CidGate` les laisse sortir), envoyer un `frag` par gardien.
4. Inscrire `backup_frags(owner, packet_id, i, host, cid, sent_at, acked_at)` *(proposé)* ; `acked_at` ne se remplit
   qu'au `frag-ack`.
5. Les fragments locaux sont effacés une fois acquittés (ils sont recalculables depuis les blobs) ; seuls restent
   ceux en attente. Le propriétaire ne garde donc pas `n/k ×` en plus chez lui.
6. Renvoyer l'index (et le manifeste) à tous les gardiens s'il a changé.

Un gift wrap par fragment (16 Mio), et non plus par fichier : quelques dizaines de messages par nuit au lieu de
milliers. `SEND_PACE` reste.

### 5.3 Restauration

1. Inchangé jusqu'au manifeste : carte secrète, `restore` à chaque proche, renvoi du manifeste.
2. Chaque gardien renvoie aussi **l'index** (copie complète). La nouvelle station le déchiffre avec la clé dérivée.
3. La nouvelle station **tire** les fragments (au lieu d'attendre qu'on les pousse) : pour chaque paquet, elle
   sonde par `HASN` les stations des gardiens listés, choisit `k` fragments en préférant les données (pas de
   calcul si les `k` données sont là) puis les gardiens les plus disponibles, et les rapatrie par `GET` sur
   `/astro/blob`. Un fragment absent ou invalide ⇒ elle en demande un autre. Pour les gardiens hors maillage, l'op
   `restored` actuelle (envoi poussé) sert de repli.
4. Décodage dès `k` fragments valides (SHA-256 de chaque fragment = son CID ; SHA-256 du paquet = en-tête) ;
   déchiffrement UENC du paquet ; extraction des blobs selon l'index ; **chaque blob est revérifié par son CID**
   avant d'entrer dans le magasin.
5. Ordre : paquets contenant les blobs les plus récents d'abord (l'index donne la correspondance), pour que le
   membre retrouve vite ses photos récentes. Les blobs `held_whole` ou `legacy` se rapatrient directement.
6. `restore_missing` et ses relances restent la voie de secours pour ce qui n'a pas pu être décodé.

Côté gardien, le `CidGate` laisse sortir les fragments parce qu'ils sont référencés dans sa base (VFS
`/Sauvegardes/…`). Comme aujourd'hui, connaître le CID est la seule capacité nécessaire ; les CID de fragments ne
figurent que dans l'index chiffré.

### 5.4 Réparation nocturne

À chaque 20h12, pour chaque paquet vivant :
- **Contrôle de présence** : `HASN` groupé par station gardienne (un flux pour 4 096 fragments).
- **Contrôle de possession** (échantillon, ex. 1 % des fragments par nuit) : `GET` de 4 Kio à un décalage tiré au
  hasard, comparé au fragment **recalculé localement**. Un gardien qui ne répond pas juste perd sa note de
  disponibilité et le fragment est considéré perdu.
- Un fragment est **à refaire** si : accord `revoked`/`declined`, réponse fausse, ou gardien absent depuis
  `REPAIR_AFTER` *(proposé, 14 jours, Q6)*. Une absence plus courte ne déclenche rien (pas de tempête de réparation
  après une coupure de courant).
- Réparer = recalculer **le même indice** (ou un indice neuf si le gardien remplaçant tient déjà un fragment de ce
  paquet — interdit) depuis les blobs locaux et l'envoyer à un autre gardien. Le propriétaire a les données : pas
  besoin de rassembler `k` fragments sur le réseau. Si la station du propriétaire est morte, personne ne répare :
  c'est la restauration qui fait office de réparation.
- Alerte au membre si un paquet tombe à `k` fragments sûrs (plus aucune marge), bloquant la suppression des
  copies `legacy` restantes.

### 5.5 Changement de la liste des gardiens

- **Ajout** : le nouveau gardien reçoit l'index, puis un fragment de parité **d'indice neuf** pour chaque paquet
  existant (dans la limite de son quota, au fil des nuits). `k` ne change pas ; `n` et la sûreté montent.
- **Retrait** (`revoke` par l'un ou l'autre) : ses fragments sont réparés ailleurs (§ 5.4), puis `frag-drop`.
- **Passage à un `k` plus grand** (plus de gardiens, on veut gagner de la place) : uniquement pour les nouveaux
  paquets et par le compactage.
- **Quotas inégaux** : un gardien à petit quota ne reçoit des fragments que pour une partie des paquets ; `n` varie
  donc par paquet. Le choix des gardiens d'un paquet privilégie ceux qui ont le plus de quota restant et la
  meilleure disponibilité, en gardant `k` constant tant que possible.

---

## 6. Sécurité

### 6.1 Ce qu'apprend un gardien seul

- **Rien du contenu** : les blobs sont déjà UENC v2 (clé par fichier dans le trousseau, lui-même dans le manifeste
  chiffré), et le paquet est rechiffré avec sa propre clé ; un fragment de données est une tranche de ce
  chiffré, un fragment de parité une combinaison linéaire de chiffré.
- **Moins de métadonnées qu'aujourd'hui** : plus de taille par fichier, plus de nombre de fichiers, plus de CID de
  blob (seulement des CID de fragments). Restent visibles : le volume total à 16 Mio près, le rythme des nuits
  (combien de paquets par nuit), l'identité du propriétaire (l'accord est nominatif), le `k` (en-tête).
  Le chiffrement du paquet sert précisément à masquer les débuts de blob (`"UENC" | 0x02 …`) qui, sinon, laisseraient
  lire les tailles dans les fragments de données.
- Les autres gardiens ne lui sont connus que via l'index — chiffré. Il ne sait pas avec qui il partage la garde.

### 6.2 Coalition

- `≥ k` gardiens réunis reconstruisent le **paquet chiffré**, rien de plus sans la clé du paquet (dans l'index,
  chiffré par une clé dérivée du secret du compte).
- Même la coalition de tous les gardiens n'a pas le secret du compte. Seul risque croisé : un ami qui garde aussi
  la part **P3** ; P3 seule ne donne rien sans P1 (Shamir 2-sur-3, inchangé).
- Pour masquer jusqu'au volume, on pourrait envoyer des paquets factices ; non proposé (coût réel pour un gain faible).

### 6.3 Anti-abus

- Le **quota reste celui que l'ami a fixé** (`backup_pacts.quota_bytes`), compté en octets de fragments (en-tête
  compris) côté gardien ; un `frag` qui le dépasse est ignoré sans bruit, comme `item` aujourd'hui.
- Un seul fragment par `packet_id` et par propriétaire chez un gardien ; taille d'un fragment bornée
  (`72 + 16 Mio`, réglable mais plafonnée).
- Débit : pas plus de `X` fragments reçus par nuit et par propriétaire *(proposé)*, le reste différé.
- Un propriétaire malveillant ne peut faire stocker que ce qu'autorise le quota, comme aujourd'hui.

### 6.4 Intégrité

- Par fragment : CID = SHA-256 du blob entier (en-tête inclus), vérifié par `/astro/blob` à la réception.
- Par paquet : SHA-256 dans l'en-tête, vérifié après décodage ; puis authentification UENC v2 bloc à bloc.
- Par blob : CID revérifié après extraction.
- Un gardien qui renvoie un fragment faux est détecté avant décodage (CID) ; on en prend un autre. Un fragment
  dont le CID est juste mais l'en-tête incohérent avec les autres est écarté (§ 4.1).

---

## 7. Bibliothèque Rust

| Option | Licence | `unsafe` | Pi (aarch64) | Remarques |
|---|---|---|---|---|
| `reed-solomon-erasure` 6.x | MIT | pur Rust par défaut ; C+SIMD derrière `simd-accel` | correct sans SIMD | peu maintenu ; matrice Vandermonde propre à la crate |
| `reed-solomon-simd` 3.x | MIT/Apache | `unsafe` interne (intrinsèques), API sûre | NEON, très rapide | algorithme Leopard (GF(2¹⁶), O(n log n)) ; format lié à la crate ; tailles de fragment paires |
| **Maison** sur GF(2⁸) | AGPL (notre code) | aucun | ~100–300 Mo/s par ligne de parité estimés, à mesurer | ~250 lignes, `share.rs` a déjà `mul`/`inv` |

Les deux crates sont compatibles AGPL. Mais l'enjeu n'est pas la vitesse : à 16 Mio par paquet et 2 lignes de
parité, l'encodage coûte `(n−k) × 16 Mio` multiplications par table, de l'ordre de 0,1 à 0,3 s par paquet sur un Pi 4
(à mesurer) ; une première sauvegarde de 50 Gio = 3 200 paquets ≈ 10 à 15 min de calcul, réparti sur plusieurs nuits
de toute façon (débit réseau). La décision porte sur **le format** : nos fragments doivent rester décodables dans
10 ans, sans dépendre des choix internes d'une crate (matrice, corps, alignement).

**Retenu et fait (lot 1) : implémentation maison**, Cauchy systématique sur GF(2⁸) `0x11B` avec tables log/exp, dans un module
dédié **sans `unsafe`** — en pratique le petit crate `crates/astro-erasure` (dépendances : `sha2`, `thiserror`),
plutôt qu'un module d'`astro-dav` : `astro-lifecycle` (Shamir) n'a pas à dépendre du WebDAV, et le format doit vivre
indépendamment du reste, avec vecteurs figés
(§ 4.5). On en profite pour sortir `mul`/`inv` de `share.rs` vers ce module commun (Shamir et RS partagent le corps).
Garder `reed-solomon-simd` en réserve si la mesure sur Pi montre un goulot ; mais il faudrait alors un nouvel
identifiant de code (`code = 0x02`), jamais réinterpréter `0x01`.

---

## 8. Plan de mise en œuvre

### 8.1 Rejouable ou local

| Donnée | Nature | Où |
|---|---|---|
| Accord, quota | local (inchangé) | `backup_pacts` |
| Pointeur du manifeste | **rejouable** (Nostr 30078) | relais |
| Manifeste, index des paquets | **rejouables** (blobs chiffrés chez tous les gardiens, clé du manifeste dans le pointeur, clé de l'index dérivée du secret) | gardiens |
| Paquets, fragments envoyés, accusés, disponibilités | **local** au propriétaire, reconstructible depuis l'index + sondes `HASN` | `backup_blobs`, `backup_frags` *(proposés)* |
| Fragments tenus pour un ami | local au gardien (VFS `/Sauvegardes/`, donc aussi dans **son** manifeste) | gardien |

Règle : rien de ce qui est nécessaire à la restauration ne vit seulement dans une table locale ; les tables locales
sont des caches de l'index.

### 8.2 Lots

1. **Corps et code** *(fait)* : `crates/astro-erasure` (`gf`, `arsf`, `encode`/`decode`), `share.rs` utilise
   `astro_erasure::gf::{mul, inv}` ; vecteurs 1–5 recoupés par une implémentation Python indépendante ; débit mesuré
   en release (i5-1235U, un cœur) : ≈ 320–345 Mio/s de paquet à l'encodage (3-sur-5, 4-sur-6), ≈ 50 ms par paquet de
   16 Mio au décodage sans les données de tête ; le test 16 Mio tourne aussi en debug (≈ 1 s). Reste à mesurer sur Pi.
   Plan d'origine : module GF(2⁸) + Cauchy, `encode/decode`, en-tête ARSF ; vecteurs § 4.5 ; tests de propriété
   (toutes les combinaisons de `k` fragments parmi `n` pour `k ≤ 6, n ≤ 10`, tailles 0, 1, 63, 64, 65, 16 Mio).
   Aucun branchement dans `private_backup.rs`. Déplacer `mul`/`inv` de `share.rs` (ses tests doivent passer tels quels).
2. **Paquets et index** *(fait)* : module `astro_dav::backup_pack` (et non `astro-erasure`, qui garde le format pur :
   le paquetage dépend d'`astro-crypto` pour UENC v2 et du calcul de CID d'`astro-dav` ; `astro-erasure` ne dépend de
   rien du projet, pas de cycle ; nouvelles dépendances d'`astro-dav` : `astro-erasure`, `hkdf`, déjà dans le workspace).
   `PackBuilder` (flux blob par blob, SHA-256 de chaque blob vérifié contre son CID `bafkrei…` en le lisant, un CID
   une seule fois, y compris ceux déjà empaquetés, paquets scellés et rendus à un `FragmentSink` dès qu'ils sont
   pleins : mémoire = un paquet clair + son chiffré + ses `n` fragments), `PackIndex` (`seal`/`open`/`best`),
   `index_key`, `open_packet` + `Extractor` (blobs revérifiés par CID, blob étalé rejeté si un paquet manque).
   Champ `Manifest::backup_index` (`#[serde(default)]`, omis quand vide : les manifestes sans sauvegarde restent
   identiques octet pour octet). Tests : `crates/astro-dav/tests/backup_pack.rs` ; vecteur 6 recoupé par
   `crates/astro-dav/tests/vector6.py` (`cryptography`). **Écarts et précisions** :
   - **16 Mio = taille du paquet chiffré.** La charge utile d'un paquet plein est `PAYLOAD` = 16 Mio − 14 − 256 × 16
     = 16 773 106 octets, pour que le chiffré fasse exactement `PACKET_SIZE` et qu'un fragment `k = 1` tienne sous
     `MAX_FRAG_LEN` sans toucher au format ARSF. Le dernier paquet est complété (zéros en clair) pour que son chiffré
     tombe sur le Mio supérieur. `packets[].len` de l'index = longueur chiffrée (= `packet_len` ARSF).
   - **Secret de la clé d'index** : la clé privée Nostr du MULTIPASS (`sha256(graine)`, graine = scrypt de la formule
     isolée `(SALT, PEPPER_<sha256(UPLANETNAME)[:16]>)`), en IKM de HKDF-SHA256 sans sel, `info = "astro-backup-index-v1"`.
     La station hébergeuse l'a (elle signe), la carte secrète la recalcule, elle est isolée par constellation, et un
     gardien qui voudrait éprouver un SALT/PEPPER faible doit payer scrypt (ce qui ne serait pas le cas avec HKDF
     directement sur SALT/PEPPER).
   - `frags[].host` est optionnel (rempli au lot 3) ; `held_whole` omis quand vide ; seuls les CID `bafkrei…`
     (SHA-256 brut) entrent dans le flux — une racine `bafybei…` n'est pas vérifiable bloc à bloc, elle est refusée.
   - Après une erreur au milieu d'un blob (CID faux, lecture coupée), le constructeur est hors d'usage
     (`Poisoned`) : des octets peuvent déjà être dans un paquet scellé ; on recommence le flux.
   - Reste pour le lot 3 : `load_manifest` remet `backup_index` à `None` (il lit la base) ; il faudra y reporter le
     CID courant de l'index avant publication.
3. **Envoi** *(fait)* : `src/private_backup/rs.rs`. Tables d'état local `backup_packets`, `backup_blobs`, `backup_frags`,
   `backup_index`, `blob_copies` (`schema.sql`) ; ops `frag` (avec `["kind","index"]` pour l'index), `frag-ack`, `frag-drop` ;
   `["rs","1"]` dans `accept` (et dans `restored`) ; `k` = plus grand tel que `proba_au_moins_k ≥ 999 ‰` (`choose_k`). Tests :
   `two_stations_…`, `every_packet_goes_to_all_coded_guardians_…`, `a_cut_in_the_middle_of_the_night_…`,
   `a_shared_file_stays_out_of_packets_…` dans `src/private_backup.rs`. **Écarts et précisions** :
   - **Actif par défaut** (décision de Fred) pour tout gardien d'une **autre station** qui annonce `rs` ; les gardiens sans
     `rs` et ceux de cette même station gardent la copie complète (`item`, zéro copie en local). Les copies `legacy` déjà
     faites ne sont pas retirées (lot 6).
   - Défaut corrigé : `set()` désactivait les autres accords, si bien que les nouveaux fichiers n'allaient qu'au dernier
     gardien choisi. Désormais chaque gardien reste actif et **chaque paquet va à tous les gardiens retenus**.
   - Table de plus que prévu : `backup_packets` (clé, `k`, longueur, rang) ; `backup_index` porte le CID courant de l'index
     (colonne `index_cid` : le `CidGate` le laisse sortir, comme `backup_frags.cid`) et `load_manifest` le reporte dans
     `Manifest::backup_index`. `backup_frags` sert des deux côtés, comme `backup_items` ; sa clé
     `(owner, host, packet_id)` impose Q9.
   - Une station = un fragment par paquet : les gardiens sont regroupés par PeerID de leur station d'accueil (DID), le plus
     présent puis le plus spacieux est retenu. Disponibilité sans 7 jours de recul : **900 ‰ supposés** pour choisir `k`.
     Un gardien dont le quota restant ne tient pas son fragment sort du passage et `k` est recalculé.
   - **Pas d'attente du paquet plein** (§ 5.2 point 2) : chaque passage scelle ce qu'il a (au plus 4 paquets pleins par
     passage, la nuit enchaîne), le dernier complété au Mio supérieur. À reprendre si le surcoût des petits paquets se voit.
   - Mémoire bornée même pour une vidéo de plusieurs Gio : les fragments d'un paquet scellé sont écrits dans le magasin
     avant la lecture suivante du blob (lecteur `Throttle`). Coupure en plein encodage : tout est inscrit en une transaction
     après coup ; les fragments déjà écrits restent orphelins dans le magasin et le passage suivant recommence avec une clé
     neuve (fuite de place bornée, à ramasser au lot 5). Coupure en plein **envoi** : `sent_at` NULL ⇒ renvoi au passage
     suivant sans réencoder ; non acquitté depuis 24 h ⇒ renvoi ; le gardien dédoublonne (même CID : nouvel accusé).
   - Côté gardien : accord, quota (copies + fragments, en-têtes compris), taille ≤ `72 + 16 Mio` (index ≤ 64 Mio), blob
     rapatrié puis vérifié (en-tête ARSF du `packet`/`i` annoncés ; index = UENC v2), taille réelle recomptée ; un refus
     retire ce que le message a apporté et prévient le Capitaine. L'index précédent est remplacé à l'arrivée du suivant.
     Débit par nuit (§ 6.3, `X` fragments) : fait au lot 5 (`NIGHT_FRAGS` = 512).
   - L'index est rescellé quand son contenu change (empreinte sans la révision) ; `legacy` n'y liste que des fichiers du VFS.
   - Seuls les CID `bafkrei…` entrent dans les paquets ; un autre CID part en copie complète même chez un gardien `rs`.
   - **Fichiers partagés** (décision de Fred) : sur la voie k-sur-n, un fichier partagé (lien, ami, FaceCloud, invitation)
     n'est plus exclu d'office. Seuls `/Partages/` et `/Sauvegardes/` le sont toujours. Chaque nuit, une sonde `HASN` sur
     NOTRE CID (4 096 par flux) interroge les stations distinctes des amis réciproques hébergés ailleurs ; les « oui » vont
     dans `blob_copies` (une station = une copie). Le fichier reste hors paquet seulement si
     `proba_au_moins_k(dispos des stations, 1) ≥ 999 ‰` avec des vérifications de moins de 7 jours (disponibilité d'une
     station = la meilleure de ses comptes, 900 ‰ sans observation) ; sinon il entre au paquet suivant. Ces copies
     alimentent `held_whole` (comptes des stations qui les tiennent) pour les blobs en paquet. Lecture pour la Médiathèque :
     `PrivateBackup::file_copies`. Les gardiens `legacy` gardent l'ancien filtre. Vérification des chemins de copie :
     partage entre amis et FaceCloud = **même CID** (le destinataire rapatrie notre blob et le range sous `/Partages/`) ;
     lien `/s/` = **rechiffré** avec la clé du lien (autre CID, servi par notre station seule) : zéro copie, le fichier
     entre en paquet ; invitation `/i/` réclamée = même blob et même CID, mais le compte est créé **sur notre station** :
     zéro copie distincte.
4. **Restauration** *(fait)* : tests `five_guardians_two_off_…` (restauration complète) et
   `three_guardians_off_in_three_of_five_…` (échec propre, relances, reprise quand un gardien revient). **Écarts** :
   - Le gardien renvoie l'index qu'il tient avec le manifeste (`restored`, `["kind","index"]`), jamais ses fragments d'office
     (sinon le gain `n/k` disparaîtrait au réseau) ; ils sont tirés par la nouvelle station. La nouvelle station garde le
     `rev` le plus haut qui se déchiffre, **reconstruit ses tables locales depuis l'index** (`import_index` : rien ne repart
     en paquet, l'accord `rs` des gardiens est connu, le prochain index prend la suite), puis tire les paquets dès que
     l'index est là, sans attendre le manifeste.
   - Suites de paquets liés par un blob étalé traitées d'un bloc, les suites des blobs les plus récents d'abord (`mtime`
     du manifeste rétabli) ; fragments déjà là, puis données, puis gardiens les plus présents ; une sonde `HASN` par
     station (`with_probe`, câblée sur `astro_p2p::blob::has_many` dans `serve.rs`), puis `GET` par le rapatriement
     existant ; chaque blob extrait est réécrit et **revérifié par son CID** au `commit`. Fragments tirés effacés après
     décodage.
   - Échec (moins de `k` fragments) : les gardiens injoignables sont priés par CID de renvoyer leur fragment (op `restore`,
     ils répondent `restored` + `src = frag`), le Capitaine est prévenu, nouvel essai toutes les 6 h (24 h en état `late`)
     ou dès qu'un index ou un fragment arrive ; abandon au bout de 30 jours. État : `kv restore_rs`. `restore_missing` et la
     fusion tardive (`late`, `merge_into`) restent inchangés ; le suivi du membre (`m-guard.js`) aussi, sauf la phrase
     « vos nouveaux fichiers iront chez lui » remplacée.
5. **Réparation et contrôle de possession** *(fait)* : `src/private_backup/repair.rs`, appelé par `sync_night` (20h12) **avant**
   l'envoi de la nuit, qui emporte les fragments refaits et l'index rescellé. Tests : `a_guardian_who_says_yes_then_serves_wrong_bytes_…`,
   `an_absence_of_13_days_does_nothing_15_days_triggers_a_repair`, `a_revoked_guardian_is_repaired_elsewhere_at_once`,
   `a_new_guardian_gets_a_fresh_parity_of_every_packet_and_safety_rises`, `fragments_left_by_a_cut_are_collected_once`,
   `a_guardian_takes_a_bounded_number_of_fragments_per_night_and_owner`, `restoration_fetches_a_blob_held_whole_by_a_friend_directly`,
   `a_very_big_blob_held_whole_by_two_friend_stations_stays_out_of_packets` (`src/private_backup.rs`) ;
   `un_fragment_se_recalcule_depuis_le_clair_et_l_en_tete` (`crates/astro-dav/tests/backup_pack.rs`). **Écarts et précisions** :
   - **Recalcul d'un fragment.** Le paquet chiffré dépend d'un préfixe de nonce aléatoire (en-tête UENC v2, 14 octets) : sans lui,
     on ne retrouve pas les mêmes octets. Il est gardé **en local** (`backup_packets.head_hex`, jamais dans l'index, rien ne
     change au format) ; `astro_crypto::uenc::reseal_to_vec` rechiffre avec cet en-tête et `backup_pack::refragment` refuse de
     rendre quoi que ce soit si le SHA-256 du chiffré recalculé n'est pas l'identifiant du paquet (un autre clair sous le même
     nonce ne sort jamais). Le clair est reconstitué depuis `backup_blobs` (positions dans le flux, blobs étalés compris).
     **Limite** : un paquet repris d'une restauration (`import_index`) n'a pas cet en-tête ; il n'est ni réparé ni contrôlé ici
     (le Capitaine est prévenu). Le compactage (lot 6) le réécrira.
   - **Présence** : `HASN` par station (4 096 par flux). « Non » d'une station qui répond ⇒ perdu aussitôt ; station muette ⇒
     rien. `backup_frags.seen_at`, `lost_at` (colonnes ajoutées, migration `ADDED_COLUMNS`).
   - **Possession** : ⌈1 %⌉ des fragments sûrs par nuit, au moins un, tirés par `ORDER BY random()` ; 4 Kio à un décalage tiré
     par `getrandom`, lus par `astro_p2p::blob::fetch_range` (nouvelle, plafonnée à 1 Mio, `Ok(None)` = la station répond mais
     ne sert pas la plage) branché par `with_sample` dans `serve.rs`. Mauvaise réponse ⇒ fragment perdu, lignes
     `guardian_presence` du gardien effacées sur la fenêtre de 30 jours (sa disponibilité retombe à zéro et remonte avec sa
     présence), `frag-drop`, Capitaine prévenu. Une station injoignable n'est pas contrôlée.
   - **Absence** : dernier signe de vie = le plus récent de `seen_at`, `acked_at` et du dernier jour de `guardian_presence` ;
     perdu seulement s'il date de plus de `REPAIR_AFTER` = 14 jours. `frag-drop` lui est envoyé (traité à son retour).
   - **Retrait** : accord qui n'est plus `accepted` ⇒ perdu tout de suite (le gardien a déjà tout retiré au `revoke`).
   - **Réparation = extension.** Un seul mécanisme pour § 5.4 et § 5.5 : chaque gardien `rs` retenu (un par station) qui n'a
     **aucune** ligne pour un paquet, et dont la station n'en tient pas de fragment sûr, reçoit un fragment d'**indice neuf**
     (`max(i) + 1`, ≤ 254) dans son quota — jamais l'indice perdu, que l'ancien gardien tient peut-être encore. Les paquets
     blessés passent d'abord ; au plus 64 fragments recalculés par membre et par nuit. Le fragment perdu reste en base (hors
     index, hors quota) pour qu'on ne le renvoie pas au même gardien ; l'envoi et le `frag-ack` sont ceux de l'envoi ordinaire.
   - **Alerte** : un paquet qui a **perdu** un fragment et n'a plus que `k` fragments sûrs (acquittés, non perdus) ⇒ message au
     membre (sans jargon : « n'a plus de marge… en ajouter un aide ») et au Capitaine, une seule fois (`kv backup_rs_alert`),
     levée quand la marge revient. Un compte à un seul gardien (k = n = 1) n'est donc pas alerté chaque nuit.
   - **Orphelins** : chaque fragment est journalisé (`kv backup_spool`) **avant** d'être écrit dans le magasin ; la transaction
     qui l'inscrit efface la ligne. La nuit, une ligne restée dont le blob n'est référencé nulle part (fragment non acquitté,
     VFS, index courant) ⇒ blob effacé ; de même un fragment acquitté encore présent (coupure entre l'accusé et l'effacement).
   - **Plafond côté gardien** (§ 6.3) : `NIGHT_FRAGS` = 512 fragments rangés par 24 h glissantes et par propriétaire ; au-delà, le
     message n'est pas réclamé (repris au passage suivant), rien n'est rapatrié. 512 × 16 Mio = 8 Gio/jour au pire : une première
     sauvegarde de 50 Gio en 3 sur 5 (≈ 3 200 fragments par gardien) passe en une semaine, l'envoi normal n'est jamais freiné,
     et un propriétaire en boucle ne fait pas télécharger plus de 8 Gio par jour à l'ami.
   - **Q5** : un blob d'au moins 256 Mio (`BIG_BLOB`) entre dans la sonde de copies de nuit ; tenu en entier par au moins deux
     stations amies vérifiées depuis moins de 7 jours (`blob_copies`), il reste hors des paquets. Il revient alors à la
     restauration par la voie des copies (`restore_missing`), comme les fichiers partagés couverts du lot 3.
   - **Raccourci `held_whole`** : avant de tirer les paquets, la nouvelle station rapatrie directement chaque blob absent qu'une
     station amie tient en entier (`pull_whole`, CID vérifié) ; les paquets ne sont tirés que pour le reste.
   - **Corrigé après le lot 5** : une plage fausse ne condamne plus. Une autre plage, tirée au hasard, est demandée aussitôt ;
     seules **deux** réponses fausses font un menteur. Une plage fausse suivie d'un silence rend le fragment **suspect**
     (`kv backup_rs_suspect`), contrôlé en priorité une autre nuit sur une seule plage ; une erreur de transport n'est jamais
     une preuve. Test : `one_wrong_range_is_retried_on_another_two_wrong_ranges_condemn`.
6. **Compactage et migration `legacy`** *(fait)* : `src/private_backup/compact.rs`, appelé à chaque passage de `rs_once` avant
   l'envoi des fragments. Tests : `a_mostly_dead_packet_is_compacted_step_by_step_and_nothing_is_ever_lost`,
   `a_packet_back_from_a_restoration_is_rewritten_and_becomes_checkable_again`,
   `full_copies_are_dropped_once_packets_are_safe_and_never_without_margin` (`src/private_backup.rs`). **Écarts et précisions** :
   - **Place morte** : `backup_blobs.gone_at` (colonne ajoutée) note qu'un fichier n'existe plus ; mort après `GRACE_SECS`.
     Une suite de paquets liés par un blob étalé est réécrite d'un bloc si sa part vivante (en octets de blobs, morts compris
     au dénominateur — pas la capacité du paquet, sinon un petit paquet complété au Mio serait réécrit chaque nuit) tombe sous
     50 %, si elle n'a plus de blob, ou si un de ses paquets n'a pas d'en-tête local (**paquet repris d'une restauration** :
     réécrit même tout vivant, il redevient contrôlable et réparable). Au plus `PASS_BYTES` de blobs vivants par passage,
     **un seul compactage en cours par compte**, rien pendant une restauration, rien si un blob vivant manque ici.
   - **Quatre étapes sans perte** : (1) paquets neufs `staged = 1`, positions dans `backup_compact` (nouvelle table), anciens
     paquets dans `kv backup_compact_from`, en une transaction — l'index publié décrit toujours l'ancienne disposition (les
     paquets neufs y figurent sans blob, pour qu'une station qui restaure puisse les retirer) ; (2) envoi et accusés
     ordinaires ; (3) **bascule** en une transaction quand chaque paquet neuf a au moins `k` fragments acquittés, et `k + 1`
     s'il en a reçu davantage : blobs déplacés, morts oubliés, anciens paquets et leurs fragments sortis des tables, fragments
     mis dans la file `kv backup_rs_drop` avec la révision d'index du moment ; (4) `frag-drop` seulement quand un index de
     révision plus haute a été **envoyé à tous les gardiens** `rs`. Le test vérifie, à chaque étape, qu'une station neuve
     qui ne dispose que de ce que tiennent les gardiens (index de plus haute révision, fragments réellement présents) décode
     chaque fichier vivant ; une coupure en pleine construction reprend sans rien reconstruire.
   - Le recalcul d'un fragment (`rebuild`) lit aussi `backup_compact` : un paquet neuf se répare et se contrôle avant la bascule.
   - **Migration** : une copie complète (`backup_items`) chez un gardien `rs` d'une autre station est retirée (`drop`) quand son
     blob est en paquet depuis au moins `GRACE_SECS` et que **chaque** paquet sur lequel il s'étale est basculé, a **plus de `k`**
     fragments sûrs et atteint 999 ‰ (disponibilités mesurées, 900 ‰ pour un gardien inconnu). Jamais sans marge. Le manifeste
     et l'index ne sont pas concernés (Q10) ; un gardien sans `rs` garde ses copies (il les recevrait de nouveau).
7. **Interface et guide** *(fait)*. **Moi › Sauvegarde › Mes gardiens** (`m-guard.js`, champ `coded` de `GET /api/v1/backup`,
   `PrivateBackup::coded_view`) : « Sécurité estimée : 99,9 % (objectif 99,9 %) » — le plus faible des paquets, `k` parmi
   les gardiens qui tiennent un fragment sûr —, « 3 de vos 5 gardiens suffisent pour tout retrouver », « Vos amis gardent
   1,7 fois vos fichiers au lieu de 5 » (octets de fragments / octets de blobs ; masqué quand il n'y a rien d'économisé, en
   miroir `k = 1`), réparation, absence de marge et rangement en cours, en mots simples. **Médiathèque** (`m-media.js`) :
   `GET /api/v1/ucloud/media/safety` (`PrivateBackup::media_copies`, quelques lectures groupées pour tous les fichiers
   partagés, jamais une par fichier ; `file_copies` est aussi groupé) ; pastille 🟢/🟡/⏳ dans la liste, phrase dans la fiche
   (« 🟢 En sécurité : 3 copies chez vos amis · vérifié il y a 3 h », « 🟡 1 copie chez Paul + votre sauvegarde »).
   **Ma station › Santé › Détails techniques** (`m-machine.js`, champ `backup_rs` de `/api/v1/dashboard`, Capitaine seulement,
   `private_backup::captain_summary`) : paquets, en compactage, fragments acquittés / en attente / perdus, réparations,
   retraits en file, gardiens fautifs, contrôles à finir, fragments gardés pour des amis. Traductions en/es ; test navigateur
   `tests/browser/smoke_backup_rs.py` (1280 et 400 px). GUIDE.md (#secours, Médiathèque), README (limites), DEVELOPPER.md.

### 8.4 Reste à mesurer (vrai réseau, Raspberry Pi)

- Débit d'encodage, de décodage et de rechiffrement d'un paquet de 16 Mio sur Pi 4 / Pi 5 (estimé 0,1 à 0,3 s par paquet) ;
  mémoire réelle pendant un compactage de `PASS_BYTES`.
- Temps d'une première sauvegarde de 50 Gio et d'une restauration complète avec des gardiens derrière des box domestiques
  (relais, `HASN`, `GET` par segments de 4 Mio).
- Disponibilités réelles des gardiens et corrélation des pannes (même village, même fournisseur) : la cible 999 ‰ suppose
  des pannes indépendantes.
- Fréquence réelle des compactages (taux de suppression des membres) et surcoût temporaire de place chez les gardiens pendant
  une bascule (ancien et nouveau paquet coexistent jusqu'au retrait).
- Faux positifs du contrôle de possession sur des liaisons instables (le second essai devrait les absorber).

### 8.3 Tests hostiles

- Gardien qui renvoie un fragment au CID juste mais à l'en-tête d'un autre paquet ou d'un autre `k`.
- Deux fragments de même indice ; `k` fragments dont un de données dupliqué en parité.
- `frag_len` gonflé (bombe mémoire), `packet_len` énorme, réservé ≠ 0, version 2.
- Index forgé par un gardien (ne se déchiffre pas ⇒ ignoré) ; index ancien (`rev` plus bas) rendu par un gardien
  resté hors ligne : on prend le plus haut déchiffrable, et l'ancien reste une restauration partielle valide.
- Gardien qui répond `HAS` oui puis sert des octets faux au `GET` (contrôle de possession, puis CID).
- Propriétaire qui envoie plusieurs fragments du même paquet au même gardien, ou dépasse le quota.
- Coupure en plein encodage nocturne (fragments à moitié écrits) : reprise idempotente.
- Restauration pendant qu'une réparation tourne encore chez l'ancienne station (déménagement, `moved`).
- Perte de `k − 1` gardiens puis d'un `k`-ième : alerte, aucun effacement de `legacy`.

---

## 9. Décisions de Fred (tranchées)

- **Q1** — Pas de copie complète « principale » par défaut ; option possible plus tard.
- **Q2** — Paquets de **16 Mio** (`astro_erasure::PACKET_SIZE`).
- **Q3** — Cible **999 ‰**, et elle est **montrée** au membre.
- **Q4** — `k ≤ 32`, `index ≤ 254`, GF(2⁸) (pas de GF(2¹⁶) en v1).
- **Q5** — Oui : les très gros blobs déjà tenus en entier par plusieurs gardiens peuvent sortir des paquets.
- **Q6** — `REPAIR_AFTER` = **14 jours**.
- **Q7** — Oui : le paquet est rechiffré. Reconsidéré puis confirmé : le contenu est déjà illisible sans cela, mais sans
  ce chiffrement les en-têtes `UENC` visibles dans les fragments de données livreraient au gardien le nombre, la taille
  (donc la nature probable) et le rythme des fichiers. Coût : une passe par paquet (~15 ms avec AES matériel, ~100 ms
  estimés sur Raspberry Pi 4), une seule fois par paquet.
- **Q8** — Clé de l'index dérivée du secret du compte par **HKDF**.
- **Q9** — Une station = **un seul fragment** par paquet.
- **Q10** — Manifeste et index restent en **copie complète** chez chaque gardien.

Texte d'origine des questions :

- **Q1** — Remplacer complètement la copie complète, ou garder une **copie complète chez un ami « principal »** plus
  des fragments chez les autres (restauration rapide d'un seul endroit) ?
- **Q2** — Taille de paquet : 16 Mio (proposé), ou 8 Mio pour des Pi à faible mémoire et des liaisons lentes ?
- **Q3** — Cible de sûreté par défaut (999 ‰ ?) et faut-il la montrer au membre (« sûreté : très bonne ») ou la
  garder interne ?
- **Q4** — Plafonds `k ≤ 32`, `index ≤ 254` : suffisants pour une constellation, ou faut-il prévoir GF(2¹⁶) dès v1 ?
- **Q5** — Les très gros blobs déjà détenus en entier par plusieurs gardiens (films partagés puis gardés en privé)
  peuvent-ils sortir des paquets ?
- **Q6** — Délai `REPAIR_AFTER` (14 jours ?) avant de considérer un gardien comme perdu.
- **Q7** — Chiffrer le paquet en plus des blobs (proposé, pour masquer les tailles) : accepté malgré le coût CPU ?
- **Q8** — Clé de l'index dérivée du secret du compte (HKDF) : acceptable vis-à-vis du protocole Y-Level et de la
  rotation de clé, ou préférer une clé aléatoire stockée dans le manifeste ?
- **Q9** — Les gardiens d'une **même station** (même base) : comptent-ils pour plusieurs `n` ? Proposé : non, une
  station = un seul fragment par paquet, puisqu'elle brûle d'un coup.
- **Q10** — Faut-il conserver la sauvegarde en copie complète pour le seul manifeste et l'index (proposé) ou les
  coder aussi (ils sont petits : non recommandé) ?
