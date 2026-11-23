# Brancher mes appareils

Trois gestes, une seule fois, et votre station travaille pour vous : vos **contacts** nourrissent la reconnaissance des visages, vos **photos** arrivent toutes seules, vos **fichiers** s'ouvrent comme un disque.

Tout se trouve dans **Moi › Appareils** :

| Ce qu'on vous demande | Où le trouver |
|---|---|
| **Adresse** du serveur | `https://<votre station>/dav` (bouton « Copier ») |
| **Identifiant** | votre adresse (ex. `lea@qo-op.com`) |
| **Mot de passe** | le **jeton principal** (bouton « Afficher ») ou un **jeton d'appareil** |

Le sens des échanges est montré par trois signes :

| Signe | Sens | Pour quoi faire |
|---|---|---|
| ⬆ **Envoi** | l'appareil envoie à la station | sauvegarde automatique des photos d'un téléphone |
| ⬇ **Lecture** | la station donne à l'appareil | une tablette ou un cadre photo qui affiche un album |
| ⇅ **Les deux** | synchronisation | contacts, agenda, dossier de travail sur l'ordinateur |

> **Jeton principal ou jeton d'appareil ?** Le jeton principal ouvre tout votre uCloud, contacts et agenda compris : gardez-le pour vos propres appareils. Un jeton d'appareil ne voit qu'un dossier et se révoque d'un geste : idéal pour l'envoi des photos d'un téléphone, ou pour un appareil prêté.

---

## Avant tout · Je viens de Google ou d'Apple 🚚

**Fichiers › Déménager** (ou « Bien démarrer » dans **Moi**) : choisissez toutes les parties de votre archive Google Takeout (takeout.google.com : Google Photos, Contacts, Agenda, Drive, en « .zip ») ou de votre export iCloud (privacy.apple.com). Photos avec leurs dates, lieux et albums, contacts, agenda et documents arrivent chiffrés ; ce que vous avez déjà n'est pas renvoyé. Faites-le de préférence sur un ordinateur, page ouverte : vous pouvez interrompre et reprendre.

## Étape 1 · Mes contacts ⇅

Vos fiches de contact servent de base à la reconnaissance : quand la station reconnaît un visage, elle le range dans la fiche de la personne.

### Le plus simple : importer un fichier une fois

1. **Exportez** vos contacts en un fichier `.vcf` :
   - **Android** : application *Contacts* › menu ☰ › *Gérer les contacts* › *Exporter* › *Exporter vers un fichier .vcf*.
   - **iPhone** (iOS 16 ou plus) : application *Contacts* › *Listes* › appui long sur *Tous les contacts* › *Exporter*.
   - **Google** : contacts.google.com › *Exporter* › *vCard (pour les contacts iOS)*.
   - **Ordinateur** : Thunderbird, Outlook ou Contacts (macOS) savent tous exporter en vCard.
2. Dans la station : **Proches › Importer un .vcf**. Les doublons sont ignorés.

### Mieux : synchroniser en continu

- **Android** : installez **DAVx⁵** (F-Droid ou Play Store) › *＋* › *Connexion avec une URL et un nom d'utilisateur* › adresse, identifiant, **jeton principal**. Cochez le carnet *contacts* (et l'agenda). Vos contacts vont et viennent entre le téléphone et la station.
- **iPhone** : *Réglages › Contacts › Comptes › Ajouter un compte › Autre › Ajouter un compte CardDAV*. Serveur : le nom de la station seul (ex. `qo-op.com`) ; utilisateur : votre identifiant ; mot de passe : le jeton principal. Pour l'agenda, même chemin avec *Ajouter un compte CalDAV*.
- **Ordinateur** : Thunderbird › *Nouveau carnet d'adresses CardDAV* › adresse de la station.

## Étape 2 · Mes photos ⬆

Créez d'abord un **jeton d'appareil** dans **Moi › Appareils** : nom « Téléphone de Léa », dossier `/Photos/Téléphone de Léa`, droits **⬆ Envoi seul**. Copiez-le : il n'est montré qu'une fois.

- **Android** : **FolderSync** ou **PhotoSync** › nouveau compte *WebDAV* (adresse, identifiant, jeton d'appareil) › synchronisation *vers le serveur* du dossier `DCIM/Camera`, programmée chaque nuit ou à chaque photo.
- **iPhone** : **PhotoSync** › *Réglages › Configurer › WebDAV* (mêmes informations) › *Transfert automatique* : chaque nouvelle photo part vers la station.
- Les photos arrivent chiffrées. La station y cherche les visages et, si la recherche est activée, le texte et le contenu de l'image.

## Étape 3 · Mes fichiers comme un disque ⇅

- **Windows** : *Explorateur › Ce PC › Ajouter un emplacement réseau* › adresse `https://<votre station>/dav` › identifiant et jeton principal.
- **macOS** : *Finder › Aller › Se connecter au serveur* (⌘K) › `https://<votre station>/dav`. Voir aussi [le guide macOS](MACOS.md).
- **Linux** : gestionnaire de fichiers › *Autres emplacements* › `davs://<votre station>/dav`.
- **Android** : DAVx⁵ ajoute aussi le uCloud dans l'application *Fichiers* (onglet *Montages WebDAV*).
- **iPhone** : l'application *Fichiers* ne sait pas monter WebDAV ; utilisez **Documents** (Readdle) ou **FE File Explorer**, qui s'y ajoutent ensuite comme emplacement.

Le dossier **Public** est votre site : ce que vous y déposez est lisible par tous.

## Bonus · Publier depuis « Partager » ✍️

Ajoutez la station à l'écran d'accueil (menu du navigateur › *Ajouter à l'écran d'accueil* ou *Installer l'application*). Ensuite, depuis la galerie, le navigateur ou n'importe quelle application : **Partager › Astroport**. Vous choisissez alors : **🔒 Ranger en privé** (un lien devient un favori de **Fichiers › Favoris**, les photos et textes vont dans le dossier de votre choix, chiffrés) ou **📰 Publier sur mon blog** (une note). Rien ne part sans votre geste.

- **Android** (Chrome, Edge, Samsung Internet) : fonctionne dès l'installation.
- **iPhone** : Safari ne propose pas encore les applications web dans « Partager » ; copiez le lien ou ajoutez la photo depuis **Mon blog › 📷 Images**.

## Si ça ne marche pas

- **« Identifiant ou mot de passe refusé »** : recopiez le jeton avec le bouton « Copier » ; s'il a été remplacé, l'ancien ne marche plus.
- **Le jeton d'appareil ne voit pas mes contacts** : normal, il ne voit que son dossier. Les contacts et l'agenda demandent le jeton principal.
- **Rien n'arrive** : regardez dans **Moi › Appareils** la date du « dernier envoi » de l'appareil.
- Toujours bloqué ? Votre Capitaine est là pour ça.
