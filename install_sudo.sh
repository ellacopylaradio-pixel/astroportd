#!/usr/bin/env bash
# install_sudo.sh — les outils du système dont astroportd et ses plugins ont besoin. Demande les droits administrateur.
#
# install.sh (qui, lui, n'utilise jamais sudo) le propose quand il manque quelque chose ; il se lance aussi seul :
#
#   sudo bash install_sudo.sh [--user NOM] [--kubo | --no-kubo] [--no-linger] [--yes] [--dry-run]
#
# Ce qu'il fait, seulement pour ce qui manque :
#   1. paquets : ffmpeg (images des vidéos pour la recherche, Médiathèque), poppler (lecture des PDF), python3-pip et venv
#      (ONNX Runtime : visages et recherche ; plugins), git (plugins), curl ;
#   2. kubo (ORIGIN : publication des fichiers, en miroir du disque, sur le **réseau IPFS des stations ORIGIN** — swarm.key à
#      zéros, jamais l'IPFS mondial) : archive officielle de GitHub vérifiée par une empreinte SHA-512 figée ici, dépôt créé pour
#      l'utilisateur avec la swarm.key d'ORIGIN et l'amorçage sur les stations ORIGIN, *filestore* activé (Kubo publie par
#      référence aux fichiers du disque : aucune place en plus), service systemd sous l'utilisateur. Un kubo déjà installé (par
#      Astroport.ONE, par exemple) est gardé tel quel : seul le filestore est activé. Jamais en ẐEN (--no-kubo) ;
#   3. « linger » : le service utilisateur d'astroportd démarre avec la machine, sans session ouverte.
#
# Il ne touche jamais au pare-feu, aux ports, ni à astroportd lui-même.

set -euo pipefail

KUBO_VERSION="v0.40.0"
# Empreintes publiées avec la release officielle (github.com/ipfs/kubo/releases/tag/v0.40.0, fichiers *.sha512).
KUBO_SHA512_amd64="8dcd6ed48bdaaf224ecd0c4c1f77b555e645dccfc4669db227ea2b1a02cebc97081662884b64a7c75959922ee5d36e4a5ba277acc0e012c2933429c8e6bacfbe"
KUBO_SHA512_arm64="717a1fbe34312068e3c63e96f2e95404ee3f49b07a16a4ddcce23e0d08a5590d9fcc5960d0bf768d4220864c5f7ff3af23da25defb1bad1c2c4efbb47ed83681"
# Réseau IPFS d'ORIGIN : la clé à zéros, et ses stations d'amorçage (A_boostrap_nodes.txt d'Astroport.ONE ; si ce fichier est
# présent sur la machine, c'est lui qui est lu).
ORIGIN_SWARM_KEY=$'/key/swarm/psk/1.0.0/\n/base16/\n0000000000000000000000000000000000000000000000000000000000000000'
ORIGIN_BOOTSTRAP=(
  /ip4/149.102.158.67/tcp/4001/p2p/12D3KooWM6jEPqDEgnjmTjnB4vCBkGoQp7rtS5m9mpikSHoDD581
  /dnsaddr/ipfs.sagittarius.copylaradio.com/p2p/12D3KooWAvWWWtscBjFwybk8WSr2tmiwDtvNzJEh8vwyFntbpxPX
  /dnsaddr/ipfs.guenoel.fr/p2p/12D3KooWJRBjm6RHfse7oTMkSsvBk7XNKTTWQozoDZAFLSPRKPXt
)

TARGET="${SUDO_USER:-}"
KUBO=ask
LINGER=1
YES=0
DRY=0

say()  { printf '%s\n' "$*"; }
warn() { printf '⚠  %s\n' "$*" >&2; }
die()  { printf '✘ %s\n' "$*" >&2; exit 1; }
run()  { if [ "$DRY" = 1 ]; then say "  \$ $*"; else "$@"; fi; }

usage() { sed -n '2,18p' "$0" | sed 's/^# \{0,1\}//'; }

while [ $# -gt 0 ]; do
  case "$1" in
    --user)      shift; TARGET="${1:?--user attend un nom (celui qui fait tourner astroportd)}" ;;
    --kubo)      KUBO=yes ;;
    --no-kubo)   KUBO=no ;;
    --no-linger) LINGER=0 ;;
    --yes|-y)    YES=1 ;;
    --dry-run)   DRY=1 ;;
    -h|--help)   usage; exit 0 ;;
    *) usage >&2; die "option inconnue : $1" ;;
  esac
  shift
done

[ "$(uname -s)" = Linux ] || die "Linux seulement."
[ "$DRY" = 1 ] || [ "$(id -u)" = 0 ] || die "à lancer avec sudo : sudo bash $0 --user $(id -un)"
[ -n "$TARGET" ] || die "pour quel utilisateur ? --user NOM (celui qui fait tourner astroportd)"
[ "$TARGET" != root ] || die "astroportd ne tourne pas sous root : --user NOM"
id "$TARGET" >/dev/null 2>&1 || die "utilisateur inconnu : $TARGET"
UHOME="$(getent passwd "$TARGET" | cut -d: -f6)"
{ [ -n "$UHOME" ] && [ -d "$UHOME" ]; } || die "dossier personnel de $TARGET introuvable"

# Commande lancée sous l'utilisateur (son dépôt IPFS, ses fichiers).
as_user() {
  if command -v runuser >/dev/null 2>&1; then run runuser -u "$TARGET" -- env HOME="$UHOME" IPFS_PATH="$UHOME/.ipfs" "$@"
  else run sudo -u "$TARGET" env HOME="$UHOME" IPFS_PATH="$UHOME/.ipfs" "$@"; fi
}

confirm() { # question [défaut o|n]
  local def="${2:-n}" a
  [ "$YES" = 1 ] && { [ "$def" = o ]; return; }
  [ -r /dev/tty ] || { [ "$def" = o ]; return; }
  printf '%s [%s] ' "$1" "$([ "$def" = o ] && echo O/n || echo o/N)" > /dev/tty
  read -r a < /dev/tty || a=""
  case "${a:-$def}" in o|O|oui|Oui|y|Y) return 0 ;; *) return 1 ;; esac
}

say "════════════════════════════════════════════════════════════════════"
say "  Outils du système pour astroportd (utilisateur : $TARGET)"
say "════════════════════════════════════════════════════════════════════"

# --- 1. Paquets -------------------------------------------------------------------------------------------------
if command -v apt-get >/dev/null 2>&1; then PM=apt
elif command -v dnf >/dev/null 2>&1; then PM=dnf
elif command -v pacman >/dev/null 2>&1; then PM=pacman
else PM=""; fi

pkg() { # besoin → nom du paquet pour ce système
  case "$PM:$1" in
    *:ffmpeg) echo ffmpeg ;;
    pacman:poppler) echo poppler ;;
    *:poppler) echo poppler-utils ;;
    apt:pip) echo "python3-pip python3-venv" ;;
    pacman:pip) echo python-pip ;;
    *:pip) echo python3-pip ;;
    *:git) echo git ;;
    *:curl) echo curl ;;
  esac
}

missing=()
{ command -v ffmpeg >/dev/null 2>&1 && command -v ffprobe >/dev/null 2>&1; } || missing+=(ffmpeg)
command -v pdftoppm >/dev/null 2>&1 || missing+=(poppler)
{ command -v python3 >/dev/null 2>&1 && python3 -m pip --version >/dev/null 2>&1; } || missing+=(pip)
command -v git >/dev/null 2>&1 || missing+=(git)
command -v curl >/dev/null 2>&1 || missing+=(curl)

say ""
say "▶ Paquets"
if [ ${#missing[@]} = 0 ]; then
  say "  ✔ ffmpeg, poppler, python3-pip, git et curl sont là"
elif [ -z "$PM" ]; then
  warn "gestionnaire de paquets inconnu : installez vous-même ${missing[*]}"
else
  names=(); for m in "${missing[@]}"; do read -r -a n <<< "$(pkg "$m")"; names+=("${n[@]}"); done
  say "  à installer : ${names[*]}"
  case "$PM" in
    apt)    run apt-get update -qq; run env DEBIAN_FRONTEND=noninteractive apt-get install -y "${names[@]}" || warn "installation incomplète : ${names[*]}" ;;
    dnf)    run dnf install -y "${names[@]}" || warn "installation incomplète : ${names[*]} (ffmpeg demande le dépôt RPM Fusion sous Fedora)" ;;
    pacman) run pacman -S --noconfirm --needed "${names[@]}" || warn "installation incomplète : ${names[*]}" ;;
  esac
fi

# --- 2. Kubo (ORIGIN) -------------------------------------------------------------------------------------------
kubo_unit_user() { systemctl cat ipfs.service 2>/dev/null | sed -n 's/^User=//p' | tail -n 1; }

say ""
say "▶ Kubo (publication des fichiers d'ORIGIN sur le réseau IPFS des stations ORIGIN)"
if [ "$KUBO" = ask ]; then
  if command -v ipfs >/dev/null 2>&1; then KUBO=yes
  elif confirm "  Installer kubo ${KUBO_VERSION} ? (ORIGIN : vos fichiers publiés entre stations ORIGIN ; inutile pour une station ẐEN seule)" n; then KUBO=yes
  else KUBO=no; fi
fi
if [ "$KUBO" = no ]; then
  say "  non demandé (ẐEN, ou refusé) : astroportd range tout sur le disque, sans publication IPFS."
else
  if command -v ipfs >/dev/null 2>&1; then
    say "  ✔ kubo déjà installé : $(ipfs --version 2>/dev/null) ($(command -v ipfs)) — gardé tel quel"
  else
    case "$(uname -m)" in
      x86_64) arch=amd64; sha="$KUBO_SHA512_amd64" ;;
      aarch64|arm64) arch=arm64; sha="$KUBO_SHA512_arm64" ;;
      *) arch=""; sha="" ;;
    esac
    if [ -z "$arch" ]; then
      warn "kubo ${KUBO_VERSION} n'est pas publié pour $(uname -m) : ORIGIN fonctionne sans (tout reste sur le disque)."
      KUBO=no
    else
      tgz="kubo_${KUBO_VERSION}_linux-${arch}.tar.gz"
      url="https://github.com/ipfs/kubo/releases/download/${KUBO_VERSION}/${tgz}"
      tmp="$(mktemp -d)"
      say "  téléchargement : $url"
      run curl -fsSL --proto '=https' --tlsv1.2 -o "${tmp}/${tgz}" "$url" || die "téléchargement de kubo impossible"
      if [ "$DRY" = 0 ]; then
        [ "$(sha512sum "${tmp}/${tgz}" | cut -d' ' -f1)" = "$sha" ] || { rm -rf "$tmp"; die "empreinte SHA-512 de kubo incorrecte : archive refusée"; }
        say "  ✔ empreinte SHA-512 vérifiée"
      fi
      run tar -xzf "${tmp}/${tgz}" -C "$tmp"
      run install -m 755 "${tmp}/kubo/ipfs" /usr/local/bin/ipfs
      rm -rf "$tmp"
      say "  ✔ kubo installé : /usr/local/bin/ipfs"
    fi
  fi
fi

if [ "$KUBO" = yes ]; then
  if [ ! -e "$UHOME/.ipfs/config" ]; then
    say "  dépôt IPFS créé pour $TARGET ($UHOME/.ipfs), sur le réseau d'ORIGIN"
    as_user ipfs init >/dev/null
    # Réseau privé d'ORIGIN : sa clé, et ses stations d'amorçage à la place de celles de l'IPFS mondial.
    if [ "$DRY" = 1 ]; then say "  \$ swarm.key d'ORIGIN → $UHOME/.ipfs/swarm.key (0600)"
    else
      printf '%s\n' "$ORIGIN_SWARM_KEY" > "$UHOME/.ipfs/swarm.key"
      chown "$TARGET": "$UHOME/.ipfs/swarm.key"; chmod 600 "$UHOME/.ipfs/swarm.key"
    fi
    boot=("${ORIGIN_BOOTSTRAP[@]}")
    list="$UHOME/.zen/Astroport.ONE/A_boostrap_nodes.txt"
    if [ -r "$list" ]; then mapfile -t boot < <(grep -Ev '^[[:space:]]*(#|$)' "$list"); fi
    as_user ipfs bootstrap rm --all >/dev/null
    for b in "${boot[@]}"; do as_user ipfs bootstrap add "$b" >/dev/null; done
    say "  amorçage : ${#boot[@]} station(s) ORIGIN"
    # Kubo 0.40 refuse de démarrer en réseau privé avec sa configuration automatique (mainnet) ; le reste supprime ses
    # avertissements (relevé sur Kubo 0.40.0 : « Swarm is limited to private network », démarrage propre).
    as_user ipfs config --json AutoConf.Enabled false
    as_user ipfs config Routing.Type dht
    as_user ipfs config --json Routing.DelegatedRouters '[]'
    as_user ipfs config --json Ipns.DelegatedPublishers '[]'
    as_user ipfs config --json DNS.Resolvers '{}'
    as_user ipfs config --json AutoTLS.Enabled false
    as_user ipfs config --json Swarm.Transports.Network.Websocket false
    say "  (l'identité de ce kubo sera alignée sur celle de la station par « astroportd init », niveau Y)"
  fi
  # Un kubo déjà là : on ne change pas son réseau, mais on dit s'il n'est pas celui d'ORIGIN.
  key="$(tail -n 1 "$UHOME/.ipfs/swarm.key" 2>/dev/null || true)"
  if [ "$DRY" = 0 ] && [ -z "$key" ]; then
    warn "ce kubo n'a pas de swarm.key : il est sur l'IPFS mondial, pas sur le réseau des stations ORIGIN."
    warn "  Pour le rejoindre : la swarm.key d'ORIGIN (64 zéros) dans $UHOME/.ipfs/swarm.key, puis redémarrer ipfs."
  elif [ "$DRY" = 0 ] && [ "$key" != "0000000000000000000000000000000000000000000000000000000000000000" ]; then
    warn "ce kubo est sur un réseau privé (une constellation ẐEN ?) : ORIGIN n'y publie pas. Kubo n'appartient qu'à ORIGIN."
  fi
  restart=0
  if [ "$DRY" = 1 ] || [ "$(as_user ipfs config --json Experimental.FilestoreEnabled 2>/dev/null)" != true ]; then
    say "  filestore activé : Kubo publie par référence aux fichiers du disque (aucune place en plus)"
    as_user ipfs config --json Experimental.FilestoreEnabled true
    restart=1
  else
    say "  ✔ filestore déjà activé"
  fi
  if systemctl cat ipfs.service >/dev/null 2>&1; then
    owner="$(kubo_unit_user)"
    if [ -n "$owner" ] && [ "$owner" != "$TARGET" ]; then
      warn "le service ipfs tourne sous « $owner », pas sous « $TARGET » : Kubo ne pourra pas relire les fichiers d'astroportd ;"
      warn "  astroportd le détecte et publie alors par copie (place doublée). Mieux : User=$TARGET dans le service ipfs."
    fi
    if [ "$restart" = 1 ]; then run systemctl restart ipfs.service && say "  ✔ service ipfs redémarré (filestore pris en compte)"; fi
  else
    unit=/etc/systemd/system/ipfs.service
    say "  service systemd : $unit (sous $TARGET)"
    if [ "$DRY" = 0 ]; then
      cat > "$unit" <<UNIT
[Unit]
Description=IPFS Kubo (miroir de publication d'astroportd)
After=network-online.target
Wants=network-online.target

[Service]
User=${TARGET}
Environment=IPFS_PATH=${UHOME}/.ipfs
# Jamais sur l'IPFS mondial : sans swarm.key, kubo refuse de démarrer.
Environment=LIBP2P_FORCE_PNET=1
ExecStart=/usr/local/bin/ipfs daemon --enable-gc
Restart=on-failure
RestartSec=5
KillSignal=SIGINT

[Install]
WantedBy=multi-user.target
UNIT
    fi
    run systemctl daemon-reload
    run systemctl enable --now ipfs.service && say "  ✔ service ipfs démarré"
  fi
fi

# --- 3. Démarrage avec la machine -------------------------------------------------------------------------------
say ""
say "▶ Démarrage avec la machine"
if [ "$LINGER" = 0 ]; then
  say "  non demandé"
elif ! command -v loginctl >/dev/null 2>&1; then
  warn "loginctl absent (pas de systemd ?) : lancez astroportd vous-même au démarrage."
elif loginctl show-user "$TARGET" -p Linger 2>/dev/null | grep -q '=yes'; then
  say "  ✔ déjà réglé : le service d'astroportd démarre sans session ouverte"
else
  run loginctl enable-linger "$TARGET" && say "  ✔ réglé : le service d'astroportd démarrera avec la machine"
fi

say ""
say "Terminé. Rien d'autre n'a été modifié."
