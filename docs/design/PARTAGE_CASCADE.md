# Partage N¹ en chaîne et cascade de Ẑen

> État : **conception validée, voie A retenue (§10) ; rien n'est codé.** Le lot 1 (jeton et compteur, sans monnaie) est le prochain.
> Point de départ : aujourd'hui un partage (« Partager avec mes amis », `src/media_share.rs`, kind 21/22) ne laisse qu'une
> **trace** (qui a copié, de qui je le tiens, qui le repartage, `GET /api/v1/ucloud/media/trace`). Il n'y a ni compteur,
> ni prix, ni versement.

## 1. Ce qu'on veut

Une œuvre (film, morceau, capture, production du membre) circule de proche en proche dans les cercles N¹ :
A la partage à B, B la repartage à C et D, C à E… Le graphe est un **arbre** (plusieurs branches), pas une ligne.

- **Le compteur** de la chaîne augmente d'un à chaque saut.
- **Celui qui regarde paie**, et son paiement alimente **sa propre branche**, de lui jusqu'à la source.
- Le montant est le **nombre de maillons de sa branche** : de quoi donner **1 Ẑen à chaque partageur** de la branche ;
  tout **supplément** va à la **source** (l'auteur).
- Le versement se fait **en cascade de MULTIPASS en MULTIPASS**, en retirant 1 Ẑen à chaque relais :
  le spectateur envoie `n` Ẑen à son relais direct, qui en garde 1 et renvoie `n-1` au maillon précédent, etc.
- Chaque maillon **ne connaît que le précédent** (comme un oignon, à la TOR) : personne ne voit la chaîne entière.
- Un partageur peut **choisir de s'inscrire** sur la **liste officielle** (publique, partagée) de la chaîne.

## 2. Ce qui existe déjà et qu'on réutilise

| Besoin | Brique existante |
|---|---|
| Message privé de proche en proche | NIP-59 (`friends_media::send`, étiquettes `op`, `x`, `e`) |
| Identifier l'œuvre sans rechiffrer | CID/empreinte (`x`) : même blob d'un bout à l'autre |
| Virement entre MULTIPASS, sans découvert, sans double dépense | registre `kind 30852` (`astro-economy::ledger`, `prev`/`d` uniques) |
| Écrire une chaîne vérifiable | événements signés chaînés par `prev` (même schéma que le registre) |
| Relier les clés d'une même personne entre plans | `kind 31903 d="plane-link"` (double signature) |
| Liste publique remplaçable | kind 30005 (NIP-51), déjà utilisé pour séries et albums |

## 3. Le jeton de chaîne

Chaque copie transmise de A à B porte un **jeton de saut** signé, que B garde avec la copie :

```
hop = { x: <empreinte de l'œuvre>, n: <rang, 1 = source>, prev: <clé du maillon précédent>, sig: signature du précédent }
```

- À chaque repartage, le partageur fabrique le jeton suivant : `n+1`, `prev` = **sa** clé, signé par lui. Le destinataire
  ne reçoit donc que le jeton de **celui qui lui envoie** : il connaît **un seul** voisin (le précédent), jamais les autres.
- Le jeton est chiffré pour le destinataire (NIP-59) : un tiers qui voit passer l'événement 21/22 ne voit pas la chaîne.
- Le rang `n` est vérifiable : on ne peut pas le faire baisser sans que la signature du précédent ne tombe, ni le monter
  sans l'accord d'un vrai partageur.

**Ce que ça garantit** : un compteur qui ne grandit que d'un par saut, sans graphe central.
**Ce que ça ne garantit pas** : qu'un partageur ne se cache pas (il peut copier le fichier et le renvoyer *comme source*,
rang 1). C'est un manque assumé : la chaîne prouve ce qui a circulé *par elle*.

## 4. Le paiement en cascade

Le spectateur S, à la **première lecture** d'une copie de rang `n`, propose de payer `n` Ẑen (jamais sans geste de sa part :
même confirmation en deux temps que *Payer*).

1. S verse `n` Ẑen à son relais direct R₁ (virement ordinaire du registre, `d` = empreinte + rang, donc **une seule fois par
   œuvre et par spectateur**).
2. R₁ garde 1 Ẑen et verse `n-1` à R₂ (le maillon qu'il connaît par **son** jeton `prev`), avec le même `d`.
3. … jusqu'à la source, qui reçoit le reste : **1 Ẑen** pour son propre maillon **plus le supplément**.

Si le prix demandé par la source dépasse `n` (œuvre payante), l'écart s'ajoute au **dernier** versement, à la source.

**Qui fait ces virements ?** La station du relais, avec la clé du MULTIPASS qu'elle héberge, **dès qu'elle reçoit** le
versement entrant : c'est une règle automatique, tracée au journal (`audit`), pas un geste manuel. Un relais ne peut ni
garder plus de 1 Ẑen, ni renvoyer moins de `n-1` : la règle est dans le code de la station, et le registre refuse tout ce qui
n'est pas un virement valide.

Montants en **centimes entiers** (règle du dépôt) ; le Ẑen entier de la règle est donc 100 centimes, sans arrondi.

## 5. Quand un relais s'arrête

Trois cas, et une règle : **l'argent ne reste jamais bloqué chez un maillon qui ne répond plus.**

1. **Relais hors ligne un moment** : son versement entrant attend dans le registre (le virement est déjà fait vers lui) ; sa
   station fait suivre au retour. Le versement suivant porte un délai (`expire`) ; passé ce délai, voir 2.
2. **Relais disparu ou qui refuse de relayer** : au bout du délai, le versement **saute** le maillon : le *spectateur* (ou
   le dernier relais qui a transmis) renvoie `n-k` directement au maillon suivant **connu par son jeton**. Le maillon perdu
   garde ce qu'il avait déjà reçu ; il perd la suite. Aucun Ẑen n'est détruit.
3. **MULTIPASS qui déménage vers un autre Astroport** : sa clé ne change pas, seul son hébergeur change ; le virement vise
   la **clé**, pas la station, donc la cascade continue. Le lien `plane-link` (même personne, deux plans) permet à la
   station de destination de reprendre les jetons en attente. Un MULTIPASS **supprimé** (`member_purge`) est traité comme
   le cas 2.

À décider (§9) : qui avance le délai quand personne ne réclame.

## 6. La liste officielle (facultative)

Un partageur peut **s'inscrire** : il publie un événement `kind 30005` remplaçable, `d = chaîne:<empreinte>`, qui contient
sa clé et le rang de son jeton (pas celui des autres). La liste de l'œuvre est la **réunion** de ces inscriptions.

- Inscrit ou non, il reçoit son Ẑen : l'inscription ne change **pas** le paiement, elle donne la **visibilité** (« cette
  œuvre a été relayée par 12 membres ») et un classement des œuvres qui circulent.
- Le compteur affiché de l'œuvre = le plus grand rang inscrit (preuve : sa signature en chaîne), pas la somme.
- Retirer son inscription = supprimer l'événement (NIP-09) ; les versements passés restent.

## 7. Vie privée

- Le spectateur ne connaît que **son** relais direct ; chaque relais ne connaît que **le précédent** et le suivant à qui il
  a envoyé.
- Les virements du registre sont visibles de ceux qui lisent le registre : un observateur peut **rapprocher** des virements
  de même `d` et reconstituer la chaîne. La protection TOR ne tient donc que contre les **maillons**, pas contre un
  lecteur du registre. À dire au membre sans détour (« ce que voient les autres »).
- Le paiement est facultatif et expliqué avant (jamais prélevé en silence).

## 8. Découpage en lots

| Lot | Contenu | Argent ? |
|---|---|---|
| 1 | jeton de saut dans l'événement de partage ; compteur affiché dans la trace ; aucune monnaie | non |
| 2 | inscription à la liste officielle (kind 30005) | non |
| 3 | paiement du spectateur et cascade automatique entre stations amies, tests sur le registre en mémoire | oui |
| 4 | délai, saut du maillon absent, reprise après déménagement | oui |

Les lots 1 et 2 sont sans risque et donnent déjà le compteur demandé. Le lot 3 ne part qu'avec des **tests de propriété**
sur le registre (somme conservée, aucun solde négatif, un seul paiement par `d`).

## 9. Questions ouvertes

1. **Le délai** (§5) : durée, et qui réclame quand tout le monde dort ? Proposition : la station du *spectateur* relance.
2. **Fraude au rang** : un partageur peut-il se présenter comme source pour toucher le supplément ? Atténuation : le
   supplément ne va qu'au maillon dont le jeton remonte à la **clé de l'auteur** de la fiche (`file`/`sha` signés).
3. **Œuvres du membre sans fiche de licence** : la licence (CC…) devient-elle obligatoire pour entrer dans la chaîne payante ?
4. **Plans différents** : un virement entre deux plans privés est interdit (`bridge::check_passage`). La cascade s'arrête donc
   à la frontière d'un plan ; à confirmer que c'est voulu.
5. **Abus** : un spectateur qui crée 50 faux comptes pour s'inscrire en chaîne et se payer ? L'anti-Sybil général du projet
   (`project_g1n2_troczen_extension`) n'est pas résolu ; la cascade n'en dépend que si le lot 3 est ouvert à tous.

## 10. Revue externe (relecture du §1 à §9) : ce qu'on retient, ce qu'on corrige

**Retenu, parce que c'est juste**

- **Ancrer le supplément dans la fiche d'œuvre.** Le supplément ne va qu'au maillon dont la chaîne remonte à la clé qui a
  **signé la fiche** (celle du fichier `.media.json`, `sha` compris). Sans fiche signée par l'auteur : compteur et liste
  officielle seulement (lots 1 et 2), **jamais d'argent** (lot 3). C'est la réponse à la question 2 et à la question 3 du §9.
- **Frontière des plans** (question 4) : la cascade s'arrête au bord du plan, c'est voulu. La station du spectateur le **dit**
  avant de payer (« l'auteur ne peut pas être payé depuis ce plan ») plutôt que de laisser le versement échouer.
- **Faux comptes** (question 5) : si le spectateur est aussi les faux relais, il se paie lui-même, donc il ne gagne rien ; il
  perd au contraire le prix de l'œuvre et le Ẑen du premier vrai relais. Le seul gain possible est le **compteur gonflé** de
  la liste officielle, pas de l'argent. Le compteur public se lira donc avec un filtre de confiance (toile des rencontres
  prouvées, `astro_economy::wot`) et non comme un chiffre brut.
- **Le rang affiché** vient de la plus longue chaîne **valide** vue par la station, pas d'une déclaration.

**Corrigé, parce que ça ne tient pas tel quel**

- **« Annuler son virement » est impossible.** Le registre (kind 30852) ne connaît ni séquestre, ni échéance, ni annulation :
  un virement fait est fait, `prev` chaîné, jamais défait. Toute règle de saut qui repose sur « S annule » ou « virement à
  verrou temporel » demande de **créer** ces mécanismes dans le registre : c'est un chantier à part, à ne pas supposer.
- **Le saut d'un maillon absent oblige à connaître la clé du suivant.** Donc, entre la vie privée « à la TOR » (§3) et la
  résilience (§5), il faut choisir. Deux voies :
  - **Voie A, chaîne connue du spectateur** : le jeton porte toutes les clés de la branche (chiffré pour le spectateur,
    NIP-59, invisible du reste du monde). Le spectateur verse **directement** 1 Ẑen à chaque maillon et le supplément à la
    source, en **un lot** de virements. Plus de cascade, plus de délai, plus de relais bloqué, pas de séquestre à inventer.
    On perd : « chaque maillon ne connaît que le précédent » (le spectateur voit la branche, pas les autres branches).
  - **Voie B, cascade à l'aveugle** (§4) : on garde l'oignon, et on accepte qu'un relais absent **bloque** le versement
    jusqu'à son retour, ou qu'il faille inventer le séquestre ci-dessus.
- **Ma recommandation : voie A pour le lot 3**, et l'oignon réservé à ce qu'il protège réellement (qui a *partagé* avec qui),
  pas à ce qui sert à *payer*. Les lots 1 et 2 restent identiques dans les deux voies : le jeton y porte déjà la chaîne
  signée maillon par maillon (chaque ajout signe le maillon précédent), seule sa lecture par le spectateur change.

**Décision prise : voie A.** Le registre ne garde que des virements définitifs, sans séquestre ni verrou temporel ; le spectateur
connaît sa lignée (de lui à l'auteur), jamais l'arbre entier ; un maillon absent est payé sur sa clé et retrouve son dû à son retour
(ou par `plane-link` s'il a déménagé).
