# astroportd face aux autres solutions

Comparatif honnête : ce que fait astroportd, ce que font les grands services propriétaires et les solutions libres
auto-hébergées, et ce qu'astroportd **ne** fait **pas** encore. Les limites sont détaillées dans le
[README](../README.md#limites-connues).

## Architecture et stockage

| | astroportd | Services propriétaires (Google, iCloud, Dropbox) | Libre auto-hébergé (Nextcloud…) |
|---|---|---|---|
| **Source de vérité** | Les événements Nostr et les blobs IPFS, répliqués ; la base locale n'est qu'un cache, reconstruit par `astroportd replay`. | Les serveurs de l'entreprise. | La base du serveur (MySQL/PostgreSQL) : sans sauvegarde externe, elle est le point unique de défaillance. |
| **Fichiers** | Chiffrés avant de quitter l'appareil (UENC v2) ; la station ne stocke que des blocs illisibles. | Lisibles par le fournisseur. | En clair sur le disque, sauf module de chiffrement aux fonctions réduites. |
| **Réseau** | Maillage P2P libp2p, tunnels sans ouvrir de port sur la box, découverte locale. | Client-serveur. | Client-serveur ; demande domaine et ports ouverts. |
| **Installation** | Un seul binaire Rust, du Raspberry Pi au serveur GPU. | — | Pile PHP + base + cache + serveur web. |

## Identité et récupération

| | astroportd | Propriétaire | Libre auto-hébergé |
|---|---|---|---|
| **Compte** | MULTIPASS : un secret donne la clé Nostr, le portefeuille Ğ1 et l'adresse IPNS. Un email ou un simple pseudo suffit. | Email ou téléphone obligatoire. | Géré par l'administrateur. |
| **Connexion** | Signatures (NIP-42, NIP-98) ou code PASS local ; aucun cookie. | Mot de passe, cookies, OAuth. | Mot de passe, cookies, 2FA. |
| **Mot de passe perdu, machine détruite** | Clé de secours en 3 parts dont 2 suffisent : carte QR imprimée, station, amis qui hébergent la sauvegarde. | Réinitialisation par email ou SMS. | L'administrateur. |

## Photos, fichiers, agenda

| | astroportd | Propriétaire (Google Photos…) | Libre (Immich, Nextcloud) |
|---|---|---|---|
| **Visages** | Calculés sur la station (ONNX) ou sur une machine que vous choisissez ; empreintes cloisonnées par membre. | Sur les serveurs du fournisseur. | Locaux, souvent gourmands. |
| **Partage** | Envoi chiffré à la station de l'ami reconnu ; lien secret dont la clé ne quitte pas le navigateur. | Lien vers le serveur du fournisseur. | Lien ou fédération, rarement de bout en bout. |
| **Contacts, agenda** | Fichiers chiffrés, servis en CardDAV/CalDAV au téléphone. | Lisibles par le fournisseur. | CardDAV/CalDAV, en clair en base. |
| **Recherche** | Texte lu dans les images et PDF, visages, villes, dates, recherche par l'image : tout calculé localement. | Sur les serveurs. | Selon l'outil. |

## Communication

| | astroportd | Propriétaire (WhatsApp, Zoom) | Libre (Matrix, Jitsi, Mastodon) |
|---|---|---|---|
| **Messages privés** | NIP-17 (gift wrap) : contenu et expéditeur chiffrés ; la station sait seulement à quel compte hébergé elle remet l'enveloppe. | Chiffrés chez Signal/WhatsApp, mais métadonnées conservées. | Chiffrés (Matrix) ; métadonnées visibles des serveurs. |
| **Visio** | Navigateur à navigateur, STUN et relai TURN intégrés au binaire. | Serveurs du fournisseur. | Serveurs Jitsi et coturn à installer. |
| **Graphe social** | Liste de contacts signée (Nostr) qui vous suit d'une station à l'autre ; le relai filtre par cercle N¹/N². | Propriété de la plateforme. | Fédéré ; migration possible mais partielle. |

## Assistant et vie privée

| | astroportd | Propriétaire (ChatGPT, Gemini) | Libre (Ollama seul) |
|---|---|---|---|
| **Mémoire** | BRO : mémoire vectorielle locale (`sqlite-vec`, `bge-m3`), filtrage des secrets. | Sur les serveurs du fournisseur. | À assembler soi-même. |
| **Modèle** | Le vôtre : Ollama, ou une clé déclarée et chiffrée. | Imposé. | Le vôtre. |
| **Vigie** | Chaque soir, signale une photo géolocalisée ou signée, ou un fichier sensible déposé dans `/Public`. Ne retire rien. | — | — |

## Économie

| | astroportd | Propriétaire (SaaS) | Libre |
|---|---|---|---|
| **Financement** | Loyer hebdomadaire en Ẑen, unité de compte de la coopérative adossée à la monnaie libre Ğ1. | Abonnement et exploitation des données. | Dons, bénévolat. |
| **Répartition** | Automatique : 1×PAF à la machine, 2×PAF au Capitaine, puis le surplus en 3 tiers (trésorerie, R&D, actifs). | Aux actionnaires. | Comptabilité manuelle. |
| **Défaut de paiement** | Suspension, jamais suppression : tout revient au paiement. | Fermeture du compte. | Selon l'hébergeur. |
| **Hors ligne** | Billets papier imprimables, retour automatique à l'émetteur à l'échéance. | — | — |

## En une image

- **Les services propriétaires** sont des métropoles : un confort sans limite, mais vous y êtes locataire.
- **Le libre auto-hébergé** rend le code, mais garde le même plan client-serveur : quelqu'un détient la base.
- **astroportd** est une flotte de navires : chaque station est à son Capitaine, les stations se parlent directement,
  la cryptographie fait la règle et la coopérative paie les frais.

Ce qu'astroportd n'est pas : un réseau social grand public, ni un service hébergé, ni un placement. Le Ẑen n'est pas
une monnaie publique.
