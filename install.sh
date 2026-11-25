#!/usr/bin/env bash
# Installeur d'astroportd — préversion 0.2.0-beta
#
#   curl -fsSL https://raw.githubusercontent.com/papiche/astroportd/main/install.sh | bash
#   curl -fsSL https://raw.githubusercontent.com/papiche/astroportd/main/install.sh | bash -s -- --yes --mode origin
#
# Ce script : détecte votre machine, télécharge l'archive de la release GitHub, VÉRIFIE son empreinte SHA-256
# (il s'arrête si elle ne correspond pas : aucun repli silencieux), installe le binaire, prépare la reconnaissance
# de visages et d'embeddings locaux (libonnxruntime + modèles, chacun vérifié par SHA-256), signale les ports déjà pris,
# puis propose de lancer `astroportd init`. Relancé sur une station déjà installée, il montre ce qui est en place et
# fait une MISE À JOUR (binaire, modèles manquants, redémarrage du service) sans reproposer `init`.
# Il ne demande jamais les droits administrateur.
#
# Avant d'installer les modèles, il examine la machine (architecture, cœurs, mémoire, place et vitesse du disque, GPU,
# Ollama) et recommande un profil — léger, standard ou confort — que vous pouvez accepter ou changer.
# `install.sh --check-hardware` affiche ce diagnostic sans rien installer.

set -euo pipefail

REPO="papiche/astroportd"
BIN="astroportd"
PREFIX="${HOME}/.local"
VERSION="latest"
STATIC=0
YES=0
RUN_INIT=1
NO_FACE=0
EMBED=auto       # auto : selon la machine ; yes : impose ; no : refuse
PROFILE=auto     # auto | leger | standard | confort | cerveau
SEARCH=auto      # recherche multimédia locale : auto (selon le profil), yes, no
AI_INDEX=off     # cerveau partagé (service ai-index) : friends seulement pour le profil « cerveau », après confirmation
CHECK_ONLY=0
FORCE=0
MODE=""
EMAIL=""
DOMAIN=""
LAT=""
LON=""
LOCATE=0
DRY=0
LOG=""
UPDATE=0
STEP=0

usage() {
  cat <<'USAGE'
Installeur d'astroportd (préversion 0.2.0-beta)

Usage : install.sh [options]

  --version vX.Y.Z   version à installer (défaut : la dernière release)
  --prefix DIR       dossier d'installation ; le binaire va dans DIR/bin (défaut : ~/.local)
  --static           binaire statique musl (sans dépendance à la glibc) — x86_64 et aarch64 ; choisi tout seul sous Android (Termux)
  --profile P        auto (défaut), leger, standard, confort ou cerveau : ce qui tourne sur cette machine (voir ci-dessous)
  --no-face          n'installe pas les modèles de visages
  --embed / --no-embed   impose ou refuse les embeddings locaux de BRO (bge-m3, ≈ 560 Mo), quelle que soit la machine
  --search / --no-search impose ou refuse la recherche multimédia locale (texte des images, recherche d'images, parole)
  --check-hardware   affiche le diagnostic de la machine et le profil recommandé, sans rien installer
  --no-init          n'appelle pas `astroportd init` à la fin
  --force            réinstalle même si la version en place est déjà la dernière
                     (sans --force, une station à jour est quand même vérifiée : modèles de recherche mis à jour,
                     nouveaux modèles et nouveaux plugins proposés, plugins installés mis à jour)
  --yes, -y          mode non interactif : accepte les réponses par défaut
  --mode origin|zen  constellation à rejoindre (transmis à `astroportd init`)
  --email EMAIL      email du Capitaine (transmis à `astroportd init`)
  --domain NOM       domaine public : HTTPS Let's Encrypt géré par astroportd si les ports 80/443 sont libres
                     (s'ils sont pris par Nginx Proxy Manager, astroportd reste local sur 127.0.0.1:8090)
  --lat X --lon Y    position de la station (20h12 solaire vrai), transmise à `astroportd init`
  --locate           devine la position d'après l'adresse IP publique (interroge ip-api.com, à la ville près) ;
                     sans --lat/--lon, l'assistant interactif la propose de lui-même
  --dry-run          montre ce qui serait fait (machine, version, fichiers, profil) sans rien installer ni écrire
  --help, -h         cette aide

Profils (recommandés d'après votre machine, que l'assistant détecte et vous montre) :
  leger      rien en local : visages et mémoire passent par une autre machine (Brain GPU, Ollama) — Pi Zero, vieux Pi, NAS
  standard   visages locaux (≈ 190 Mo) et, si la machine a assez de mémoire et de disque, embeddings locaux ; fils limités
  confort    visages et embeddings locaux, tous les cœurs — PC ou serveur avec 8 Go de mémoire et 6 cœurs ou plus
  cerveau    tout le confort, et la station prête sa puissance de calcul à vos Capitaines amis (« Cerveau de la
             constellation ») : elle calcule, en mémoire, l'index multimédia de leurs membres qui l'acceptent (confirmé)

Recherche multimédia (≈ 660 Mo de modèles, vérifiés par SHA-256, depuis IPFS puis Hugging Face) : texte des images et PDF
(PP-OCRv6), recherche d'images par description (SigLIP 2), sens des textes (E5), parole des vidéos (Whisper, confort et
cerveau). En dessous de 8 Go de mémoire, rien n'est installé : le membre confiera ce calcul à un Capitaine ami.

Architectures : x86_64 (PC, serveur), aarch64 (Raspberry Pi 4/5, Pi Zero 2 W en 64 bits),
armv7l (Raspberry Pi 2/3, Pi OS 32 bits). Linux uniquement.
USAGE
}

# Journal : les messages de l'installeur sont aussi consignés dans ~/.zen/astroportd/install.log (dernière exécution ;
# jamais de secret : les codes affichés par « init » n'y figurent pas).
_log() { [ -z "$LOG" ] || printf '%s\n' "$*" >> "$LOG" 2>/dev/null || true; }
# Couleurs : seulement sur un terminal, et jamais si NO_COLOR est défini (no-color.org). Le journal reste sans couleur.
C_OUT_G=''; C_OUT_R=''; C_OUT_OFF=''; C_ERR_Y=''; C_ERR_R=''; C_ERR_OFF=''
if [ -z "${NO_COLOR:-}" ]; then
  if [ -t 1 ]; then C_OUT_G=$'\033[32m'; C_OUT_R=$'\033[31m'; C_OUT_OFF=$'\033[0m'; fi
  if [ -t 2 ]; then C_ERR_Y=$'\033[33m'; C_ERR_R=$'\033[31m'; C_ERR_OFF=$'\033[0m'; fi
fi
say()  { local m="$*"; _log "$m"; m="${m//✔/${C_OUT_G}✔${C_OUT_OFF}}"; m="${m//✘/${C_OUT_R}✘${C_OUT_OFF}}"; printf '%s\n' "$m"; }
warn() { printf '%s⚠  %s%s\n' "$C_ERR_Y" "$*" "$C_ERR_OFF" >&2; _log "⚠  $*"; }
die()  { printf '%s✘ %s%s\n' "$C_ERR_R" "$*" "$C_ERR_OFF" >&2; _log "✘ $*"; exit 1; }
# Échec expliqué : constat, cause probable, remède (les deux derniers facultatifs).
fail() {
  printf '%s✘ %s%s\n' "$C_ERR_R" "$1" "$C_ERR_OFF" >&2; _log "✘ $1"
  [ -z "${2:-}" ] || { printf '  Cause  : %s\n' "$2" >&2; _log "  Cause  : $2"; }
  [ -z "${3:-}" ] || { printf '  Remède : %s\n' "$3" >&2; _log "  Remède : $3"; }
  [ -z "$LOG" ] || printf '  Détails: %s\n' "$LOG" >&2
  exit 1
}
# Étape numérotée d'une installation neuve (« [2/5] ») ; sans numéro pour une mise à jour ou un simple diagnostic.
step() {
  STEP=$((STEP + 1)); say ""
  if [ "$CHECK_ONLY" = 0 ] && [ "$UPDATE" = 0 ]; then say "▶ [${STEP}/5] $1"; else say "▶ $1"; fi
}

while [ $# -gt 0 ]; do
  case "$1" in
    --version) shift; VERSION="${1:?--version attend une valeur (ex. v0.2.0-beta)}" ;;
    --prefix)  shift; PREFIX="${1:?--prefix attend un dossier}" ;;
    --static)  STATIC=1 ;;
    --no-init) RUN_INIT=0 ;;
    --force)   FORCE=1 ;;
    --no-face) NO_FACE=1 ;;
    --embed)    EMBED=yes ;;
    --no-embed) EMBED=no ;;
    --search)    SEARCH=yes ;;
    --no-search) SEARCH=no ;;
    --profile)  shift; PROFILE="${1:?--profile attend auto, leger, standard, confort ou cerveau}" ;;
    --check-hardware) CHECK_ONLY=1 ;;
    --yes|-y)  YES=1 ;;
    --mode)    shift; MODE="${1:?--mode attend origin ou zen}" ;;
    --domain)  shift; DOMAIN="${1:?--domain attend un nom de domaine}" ;;
    --email)   shift; EMAIL="${1:?--email attend une adresse}" ;;
    --lat)     shift; LAT="${1:?--lat attend une latitude (ex. 43.60)}" ;;
    --lon)     shift; LON="${1:?--lon attend une longitude (ex. 1.44)}" ;;
    --locate)  LOCATE=1 ;;
    --dry-run) DRY=1 ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; die "option inconnue : $1" ;;
  esac
  shift
done

[ -z "$LAT" ] && [ -z "$LON" ] || { [ -n "$LAT" ] && [ -n "$LON" ]; } || die "--lat et --lon vont ensemble."
for _c in "$LAT" "$LON"; do
  [ -z "$_c" ] || printf '%s' "$_c" | grep -Eq '^-?[0-9]+([.][0-9]+)?$' || die "coordonnée invalide : $_c (attendu : un nombre décimal, ex. 43.60)"
done
case "$MODE" in ""|origin|zen) ;; *) die "--mode doit valoir origin ou zen (reçu : $MODE)" ;; esac
case "$PROFILE" in
  light|léger) PROFILE=leger ;;
  comfort) PROFILE=confort ;;
esac
case "$PROFILE" in auto|leger|standard|confort|cerveau) ;; *) die "--profile doit valoir auto, leger, standard, confort ou cerveau (reçu : $PROFILE)" ;; esac

# --- Journal et présentation ------------------------------------------------------------------------------------
if [ "$CHECK_ONLY" = 0 ] && [ "$DRY" = 0 ]; then
  LOG="${HOME}/.zen/astroportd/install.log"
  if mkdir -p "$(dirname "$LOG")" 2>/dev/null && : > "$LOG" 2>/dev/null; then chmod 600 "$LOG" 2>/dev/null || true; else LOG=""; fi
  _log "install.sh d'astroportd — $(date '+%Y-%m-%d %H:%M:%S') — $(uname -sm)"
fi
if [ "$CHECK_ONLY" = 0 ]; then
  say ""
  say "════════════════════════════════════════════════════════════════════"
  say "  astroportd — $([ "$DRY" = 1 ] && printf 'simulation (--dry-run), rien ne sera écrit' || printf 'installation')"
  say "════════════════════════════════════════════════════════════════════"
  say "  Parcours : [1/5] examen de la machine   [2/5] téléchargement et empreinte"
  say "             [3/5] modèles locaux         [4/5] configuration de la station"
  say "             [5/5] plugins (facultatif)       durée ≈ 2 à 3 minutes"
  say "  Écrit dans : ${PREFIX}/bin et ~/.zen — aucun sudo. Les outils du système qui manquent (ffmpeg, kubo…)"
  say "               vous sont proposés à part, avec install_sudo.sh."
  say "  Désinstaller : arrêter le service, supprimer ${PREFIX}/bin/${BIN} et ~/.zen/astroportd."
  say "                 Vos clés (~/.zen/game) ne sont jamais supprimées par ce script."
  say "  Relançable sans risque : une station existante est conservée ; binaire, modèles de recherche et"
  say "                          plugins sont mis à jour, les nouveaux modèles et plugins vous sont proposés."
  say "════════════════════════════════════════════════════════════════════"
fi

# --- Système et architecture ------------------------------------------------------------------------------------
[ "$(uname -s)" = "Linux" ] || die "astroportd ne tourne que sous Linux (système détecté : $(uname -s))."

for tool in curl tar sha256sum; do
  command -v "$tool" >/dev/null 2>&1 || fail "outil manquant : $tool" "l'installeur s'appuie sur curl, tar et sha256sum." "sudo apt install curl tar coreutils   (puis relancez install.sh)"
done

machine="$(uname -m)"
case "$machine" in
  x86_64|amd64)  TARGET="x86_64-unknown-linux-gnu" ;;
  aarch64|arm64) TARGET="aarch64-unknown-linux-gnu" ;;
  armv7l|armv6l) TARGET="armv7-unknown-linux-gnueabihf" ;;
  *) die "architecture non supportée : $machine (supportées : x86_64, aarch64, armv7l)" ;;
esac

# Android (Termux) n'a pas la glibc : seul le binaire statique y tourne.
if [ -n "${TERMUX_VERSION:-}" ] || [ "$(uname -o 2>/dev/null)" = "Android" ]; then
  [ "$TARGET" = "aarch64-unknown-linux-gnu" ] || die "Android : seul l'ARM 64 bits (aarch64) est supporté (architecture détectée : $machine)."
  say "Android (Termux) détecté : utilisation du binaire statique ARM 64 bits."
  STATIC=1
fi

if [ "$STATIC" = 1 ]; then
  case "$TARGET" in
    x86_64-unknown-linux-gnu) TARGET="x86_64-unknown-linux-musl" ;;
    aarch64-unknown-linux-gnu) TARGET="aarch64-unknown-linux-musl" ;;
    *) die "--static n'existe que pour x86_64 et aarch64 (architecture détectée : $machine)." ;;
  esac
elif [ "$TARGET" = "x86_64-unknown-linux-gnu" ] && command -v ldd >/dev/null 2>&1 && ldd --version 2>&1 | grep -qi musl; then
  say "Système musl (Alpine ?) détecté : utilisation du binaire statique."
  TARGET="x86_64-unknown-linux-musl"
fi

# --- glibc trop ancienne : les binaires gnu demandent la 2.35 (Debian 12, Ubuntu 22.04) ; en dessous, binaire statique -------
case "$TARGET" in
  *-linux-gnu*)
    glibc="$(getconf GNU_LIBC_VERSION 2>/dev/null | awk '{print $2}')"
    if [ -n "$glibc" ] && [ "$(printf '%s\n2.35\n' "$glibc" | sort -V | head -n 1)" != "2.35" ]; then
      case "$TARGET" in
        x86_64-*) say "  ✔ glibc ${glibc} (< 2.35) → binaire statique choisi automatiquement (sans visages ni embeddings locaux)."; TARGET="x86_64-unknown-linux-musl" ;;
        aarch64-*) say "  ✔ glibc ${glibc} (< 2.35) → binaire statique choisi automatiquement (sans visages ni embeddings locaux)."; TARGET="aarch64-unknown-linux-musl" ;;
        *) fail "glibc ${glibc} trop ancienne pour ce binaire (2.35 requise)." "pas de binaire statique pour ${machine}." "mettez le système à jour (Raspberry Pi OS / Debian récents)." ;;
      esac
    fi ;;
esac

# --- Horloge : un Raspberry Pi sans pile RTC démarre en 1970 -----------------------------------------------------
now="$(date +%s)"
if [ "$now" -lt 1767225600 ]; then
  warn "L'horloge de cette machine est antérieure à 2026 ($(date -u '+%Y-%m-%d')). astroportd refuse de calculer ses"
  warn "échéances avec une heure fausse. Réglez l'heure (sudo timedatectl set-ntp true) avant de le démarrer."
fi

# --- Votre machine : détection et recommandation -----------------------------------------------------------------
# Le calcul local coûte de la mémoire (≈ 0,5 Go pour les visages, ≈ 1,3 Go au pic pour bge-m3), du disque (≈ 190 Mo et
# ≈ 560 Mo) et du processeur. L'assistant mesure ce que la machine offre et recommande ce qui tient confortablement.
DATA="${HOME}/.zen/astroportd"
MODELS="${DATA}/models"

# --- Installation déjà en place ? -------------------------------------------------------------------------------
# Binaire présent ET station initialisée (station.conf) : on met à jour, on ne refait jamais « init » (il redériverait
# l'identité de la station). Le profil est déduit des modèles déjà là : ce qui avait été refusé n'est pas installé.
UNIT="${HOME}/.config/systemd/user/${BIN}.service"
INSTALLED=""
if [ -x "${PREFIX}/bin/${BIN}" ]; then
  INSTALLED="$("${PREFIX}/bin/${BIN}" --version 2>/dev/null | awk '{print $2}' || true)"
fi
conf_get() { sed -n "s/^${1}[[:space:]]*=[[:space:]]*//p" "${DATA}/station.conf" 2>/dev/null | head -n 1; }
service_state() {
  if command -v systemctl >/dev/null 2>&1 && [ -f "$UNIT" ]; then systemctl --user is-active "$BIN" 2>/dev/null || true
  else printf 'absent'; fi
}
if [ -n "$INSTALLED" ] && [ -f "${DATA}/station.conf" ]; then
  UPDATE=1
  RUN_INIT=0
  mode_txt="ORIGIN (ouverte)"; [ "$(conf_get mode)" = zen ] && mode_txt="ẐEN (privée)"
  dom="$(conf_get domain)"; cap="$(conf_get captain_email)"
  say ""
  say "▶ Installation en place"
  say "  Binaire      : ${PREFIX}/bin/${BIN} (${INSTALLED})"
  say "  Station      : ${DATA} · constellation ${mode_txt}"
  say "  Adresse      : $([ -n "$dom" ] && printf 'https://%s/' "$dom" || printf 'HTTP local (sans domaine)')"
  [ -n "$cap" ] || cap="pas encore créé (page /welcome, code d'installation dans le journal)"
  say "  Capitaine    : ${cap}"
  case "$(service_state)" in
    active) say "  Service      : ${BIN} (systemd utilisateur) en marche" ;;
    absent) if pgrep -x "$BIN" >/dev/null 2>&1; then say "  Service      : en marche hors systemd (PID $(pgrep -x "$BIN" | head -n 1))"; else say "  Service      : aucun (lancé à la main)"; fi ;;
    *) say "  Service      : ${BIN} (systemd utilisateur) arrêté" ;;
  esac
  say "  Mise à jour  : la station, ses comptes et ses données sont conservés ; pas de nouvel « init »."
elif [ -n "$INSTALLED" ]; then
  say ""; say "▶ Binaire déjà installé (${INSTALLED}) mais station pas encore initialisée : installation, puis « init »."
fi

meminfo_mb() { awk -v k="$1:" '$1 == k { printf "%d", $2 / 1024 }' /proc/meminfo 2>/dev/null || true; }

nearest_dir() { local d="$1"; while [ ! -d "$d" ] && [ "$d" != "/" ]; do d="$(dirname "$d")"; done; printf '%s' "$d"; }

# nvme | ssd | hdd | sd (carte SD/eMMC) | inconnu
disk_kind() {
  local src base rota
  src="$(df -P "$1" 2>/dev/null | awk 'NR==2 {print $1}')"
  case "$src" in /dev/*) ;; *) printf 'inconnu'; return 0 ;; esac
  base="$(lsblk -no pkname "$src" 2>/dev/null | head -n 1 || true)"
  [ -n "$base" ] || base="${src#/dev/}"
  case "$base" in mmcblk*) printf 'sd'; return 0 ;; nvme*) printf 'nvme'; return 0 ;; esac
  rota="$(cat "/sys/block/${base}/queue/rotational" 2>/dev/null || true)"
  case "$rota" in 1) printf 'hdd' ;; 0) printf 'ssd' ;; *) printf 'inconnu' ;; esac
}

# Mo/s en écriture synchronisée de 256 Mo dans le dossier donné ; vide si la mesure est impossible.
disk_write_mbps() {
  local f out num unit
  f="$(mktemp -p "$1" .astroportd-disktest.XXXXXX 2>/dev/null)" || return 0
  out="$(LC_ALL=C dd if=/dev/zero of="$f" bs=1M count=256 conv=fdatasync 2>&1 | tail -n 1 || true)"
  rm -f "$f"
  num="$(printf '%s' "$out" | awk '{print $(NF-1)}')"; unit="$(printf '%s' "$out" | awk '{print $NF}')"
  case "$unit" in
    GB/s) awk -v n="$num" 'BEGIN { printf "%d", n * 1000 }' ;;
    MB/s) awk -v n="$num" 'BEGIN { printf "%d", n }' ;;
    kB/s) awk -v n="$num" 'BEGIN { printf "%d", n / 1000 }' ;;
  esac
}

detect_hardware() {
  HW_CORES="$(nproc 2>/dev/null || getconf _NPROCESSORS_ONLN 2>/dev/null || echo 1)"
  HW_RAM_MB="$(meminfo_mb MemTotal)"; HW_RAM_MB="${HW_RAM_MB:-0}"
  HW_SWAP_MB="$(meminfo_mb SwapTotal)"; HW_SWAP_MB="${HW_SWAP_MB:-0}"
  HW_CPU="$(sed -n 's/^model name[[:space:]]*:[[:space:]]*//p' /proc/cpuinfo 2>/dev/null | head -n 1 || true)"
  [ -n "$HW_CPU" ] || HW_CPU="$(tr -d '\0' < /proc/device-tree/model 2>/dev/null || true)"
  [ -n "$HW_CPU" ] || HW_CPU="$machine"
  # int8 rapide : AVX2 (x86) ou produit scalaire NEON « dotprod » (ARM 64 bits). Sans, bge-m3 reste utilisable mais plusieurs fois plus lent.
  HW_INT8_FAST=0
  case "$machine" in
    x86_64|amd64) grep -qw avx2 /proc/cpuinfo 2>/dev/null && HW_INT8_FAST=1 ;;
    aarch64|arm64) grep -qw asimddp /proc/cpuinfo 2>/dev/null && HW_INT8_FAST=1 ;;
  esac
  local dir; dir="$(nearest_dir "$DATA")"
  HW_DISK_DIR="$dir"
  HW_DISK_FREE_MB="$(df -Pm "$dir" 2>/dev/null | awk 'NR==2 {print $4}')"; HW_DISK_FREE_MB="${HW_DISK_FREE_MB:-0}"
  HW_DISK_KIND="$(disk_kind "$dir")"
  HW_DISK_MBPS=""
  if [ "${ASTRO_SKIP_DISKTEST:-0}" != 1 ] && [ "$HW_DISK_FREE_MB" -ge 600 ]; then HW_DISK_MBPS="$(disk_write_mbps "$dir")"; fi
  HW_GPU=""
  command -v nvidia-smi >/dev/null 2>&1 && HW_GPU="$(nvidia-smi --query-gpu=name --format=csv,noheader 2>/dev/null | head -n 1 || true)"
  HW_OLLAMA=0
  if curl -fsS -m 2 http://127.0.0.1:11434/api/tags >/dev/null 2>&1; then HW_OLLAMA=1; fi
  # Surcharges pour essayer d'autres machines (tests) : ASTRO_HW_CORES, _RAM_MB, _SWAP_MB, _DISK_FREE_MB, _DISK_KIND, _DISK_MBPS, _INT8_FAST.
  HW_CORES="${ASTRO_HW_CORES:-$HW_CORES}"; HW_RAM_MB="${ASTRO_HW_RAM_MB:-$HW_RAM_MB}"; HW_SWAP_MB="${ASTRO_HW_SWAP_MB:-$HW_SWAP_MB}"
  HW_DISK_FREE_MB="${ASTRO_HW_DISK_FREE_MB:-$HW_DISK_FREE_MB}"; HW_DISK_KIND="${ASTRO_HW_DISK_KIND:-$HW_DISK_KIND}"
  HW_DISK_MBPS="${ASTRO_HW_DISK_MBPS-$HW_DISK_MBPS}"; HW_INT8_FAST="${ASTRO_HW_INT8_FAST:-$HW_INT8_FAST}"
  return 0
}

pname() { case "$1" in leger) printf "léger" ;; confort) printf "confort" ;; cerveau) printf "cerveau de la constellation" ;; *) printf "%s" "$1" ;; esac; }

# Modèles de recherche multimédia d'un profil : Whisper (parole) seulement en confort et cerveau.
# En mise à jour (« actuel »), seulement ceux déjà installés.
search_models() {
  local m out=""
  case "${PROFILE_CHOSEN:-}" in
    actuel) for m in ppocrv6-small:ocr-det siglip2-base:siglip2-vision-int8 e5-small:e5-small-int8 whisper-base:whisper-base-encoder-int8; do
              [ -f "${MODELS}/${m#*:}.onnx" ] && out="${out:+$out }${m%%:*}"
            done; printf '%s' "${out}${NEW_MODELS:+ $NEW_MODELS}" ;;
    confort|cerveau) printf 'ppocrv6-small siglip2-base e5-small whisper-base' ;;
    *) printf 'ppocrv6-small siglip2-base e5-small' ;;
  esac
}

fmt_mb() { awk -v m="$1" 'BEGIN { if (m >= 1024) printf "%.1f Go", m / 1024; else printf "%d Mo", m }' | tr . ,; }

# Remplit REC_FACE, REC_EMBED (1 = la machine tient), REC_PROFILE, REC_THREADS et REC_NOTES (ce qui limite ou mérite attention).
recommend() {
  REC_NOTES=(); REC_FACE=1; REC_EMBED=1; REC_SEARCH=1
  case "$TARGET" in
    *-musl) REC_FACE=0; REC_EMBED=0; REC_SEARCH=0; REC_NOTES+=("binaire statique (musl) : pas d'ONNX Runtime, donc ni visages ni embeddings locaux (--face-engine et --ollama vers une autre machine)") ;;
  esac
  if [ "$HW_RAM_MB" -le 0 ]; then
    REC_FACE=0; REC_EMBED=0; REC_NOTES+=("mémoire non détectée : profil léger par prudence")
  else
    if [ "$HW_RAM_MB" -lt 1800 ]; then REC_FACE=0; REC_NOTES+=("mémoire $(fmt_mb "$HW_RAM_MB") : il en faut ≈ 2 Go pour les visages locaux"); fi
    if [ "$HW_RAM_MB" -lt 3500 ]; then REC_EMBED=0; REC_NOTES+=("mémoire $(fmt_mb "$HW_RAM_MB") : bge-m3 monte à ≈ 1,3 Go, il en faut ≈ 4 Go pour le garder avec le reste"); fi
  fi
  if [ "$HW_DISK_FREE_MB" -lt 1000 ]; then REC_FACE=0; REC_NOTES+=("disque : $(fmt_mb "$HW_DISK_FREE_MB") libres, les modèles de visages demandent ≈ 1 Go de marge"); fi
  if [ "$HW_DISK_FREE_MB" -lt 2500 ]; then REC_EMBED=0; REC_NOTES+=("disque : $(fmt_mb "$HW_DISK_FREE_MB") libres, bge-m3 (≈ 560 Mo) demande ≈ 2,5 Go de marge"); fi
  if [ "$HW_CORES" -lt 2 ]; then REC_EMBED=0; REC_NOTES+=("un seul cœur : les embeddings locaux gêneraient le relai"); fi
  if [ -n "$HW_DISK_MBPS" ] && [ "$HW_DISK_MBPS" -lt 15 ]; then
    REC_EMBED=0; REC_NOTES+=("disque très lent (≈ ${HW_DISK_MBPS} Mo/s en écriture) : charger bge-m3 prendrait plus d'une minute à chaque démarrage")
  elif [ -n "$HW_DISK_MBPS" ] && [ "$HW_DISK_MBPS" -lt 40 ]; then
    REC_NOTES+=("disque lent (≈ ${HW_DISK_MBPS} Mo/s) : le démarrage chargera bge-m3 en ≈ $((560 / HW_DISK_MBPS)) s au moins")
  fi
  case "$HW_DISK_KIND" in
    sd) REC_NOTES+=("carte SD/eMMC : lente et usée par les écritures ; un SSD USB ou NVMe est bien préférable pour la base et les blobs") ;;
    hdd) REC_NOTES+=("disque mécanique : démarrage et recherche plus lents qu'avec un SSD") ;;
  esac
  # Recherche multimédia : SigLIP 2 et Whisper demandent ≈ 1,5 Go de plus ; sous 8 Go ou sans calcul int8 rapide, on délègue.
  if [ "$HW_RAM_MB" -lt 7000 ]; then REC_SEARCH=0; REC_NOTES+=("mémoire $(fmt_mb "$HW_RAM_MB") : la recherche multimédia (≈ 1,5 Go en marche) sera confiée à un Capitaine ami (Mon compte › Intelligence artificielle)")
  elif [ "$HW_INT8_FAST" = 0 ]; then REC_SEARCH=0; REC_NOTES+=("pas d'AVX2 / dotprod : la recherche multimédia locale serait lente, mieux vaut la confier à un Capitaine ami")
  elif [ "$HW_DISK_FREE_MB" -lt 3000 ]; then REC_SEARCH=0; REC_NOTES+=("disque : $(fmt_mb "$HW_DISK_FREE_MB") libres, la recherche multimédia (≈ 660 Mo) demande ≈ 3 Go de marge"); fi
  [ "$REC_EMBED" = 1 ] && [ "$HW_INT8_FAST" = 0 ] && REC_NOTES+=("pas d'AVX2 / dotprod : les calculs int8 de bge-m3 seront plusieurs fois plus lents (quelques secondes par texte long)")
  if [ "$REC_FACE" = 0 ]; then REC_PROFILE=leger
  elif [ "$REC_EMBED" = 0 ]; then REC_PROFILE=standard
  elif [ "$HW_RAM_MB" -ge 7000 ] && [ "$HW_CORES" -ge 6 ]; then REC_PROFILE=confort
  else REC_PROFILE=standard
  fi
  threads_for "$REC_PROFILE"
}

# Fils de calcul par modèle : « confort » laisse ONNX Runtime prendre tous les cœurs (0) ; sinon on en garde la moitié pour le relai.
threads_for() {
  REC_THREADS=0
  case "$1" in confort|cerveau) return 0 ;; esac
  if [ "$HW_CORES" -le 2 ]; then REC_THREADS=1; elif [ "$HW_CORES" -le 4 ]; then REC_THREADS=2; else REC_THREADS=$((HW_CORES / 2)); fi
}

show_hardware() {
  step "Examen de la machine"
  say "  Architecture : ${machine} (binaire ${TARGET})"
  say "  Processeur   : ${HW_CORES} cœur(s) — ${HW_CPU}$([ "$HW_INT8_FAST" = 1 ] && printf ' (calcul int8 rapide)')"
  say "  Mémoire      : $(fmt_mb "$HW_RAM_MB") (swap $(fmt_mb "$HW_SWAP_MB"))"
  say "  Disque       : $(fmt_mb "$HW_DISK_FREE_MB") libres dans ${HW_DISK_DIR} — ${HW_DISK_KIND}$([ -n "$HW_DISK_MBPS" ] && printf ', écriture ≈ %s Mo/s' "$HW_DISK_MBPS")"
  say "  GPU          : ${HW_GPU:-aucun NVIDIA détecté}$([ -n "$HW_GPU" ] && printf ' (inutile pour visages et embeddings : le CPU suffit)')"
  say "  Ollama       : $([ "$HW_OLLAMA" = 1 ] && printf 'joint sur 127.0.0.1:11434' || printf 'absent en local (résumés RÊVE : --ollama vers une machine qui l'\''héberge)')"
}

show_recommendation() {
  say ""; say "▶ Recommandation : profil « $(pname "$REC_PROFILE") »"
  say "  $([ "$REC_FACE" = 1 ] && printf '✔' || printf '✘') visages locaux (≈ 190 Mo de modèles)"
  say "  $([ "$REC_EMBED" = 1 ] && printf '✔' || printf '✘') embeddings locaux de BRO : bge-m3 int8 (≈ 560 Mo), recherche et mémoire sans Ollama pour les vecteurs"
  say "  $([ "$REC_SEARCH" = 1 ] && printf '✔' || printf '✘') recherche multimédia locale (≈ 660 Mo) : texte des images, recherche d'images par description, parole"
  [ "$REC_PROFILE" = leger ] || [ "$REC_THREADS" = 0 ] || say "  ↳ ${REC_THREADS} fil(s) de calcul par modèle, pour laisser de la place au relai et à l'interface"
  local i; for i in "${!REC_NOTES[@]}"; do say "  ⚠ ${REC_NOTES[$i]}"; done
}

# Profil retenu : --profile, sinon la recommandation, que l'on propose de changer au terminal. Remplit WANT_FACE, WANT_EMBED, WANT_THREADS.
choose_profile() {
  local chosen="$PROFILE"
  if [ "$chosen" = auto ]; then
    chosen="$REC_PROFILE"
    if [ "$YES" = 0 ] && [ -r /dev/tty ]; then
      {
        say ""
        say "  1) léger     rien en local : visages et mémoire passent par une autre machine (Brain GPU, Ollama)"
        say "  2) standard  visages locaux, et embeddings locaux si la machine les porte ; fils de calcul limités"
        say "  3) confort   visages, embeddings et recherche multimédia locaux, tous les cœurs"
        say "  4) cerveau   tout le confort, et prêter sa puissance de calcul à vos Capitaines amis"
        local def=2; [ "$REC_PROFILE" = leger ] && def=1; [ "$REC_PROFILE" = confort ] && def=3
        printf '  Votre choix [%s, entrée = recommandé] : ' "$def"
      } > /dev/tty
      local a; read -r a < /dev/tty || a=""
      case "${a:-$def}" in 1) chosen=leger ;; 2) chosen=standard ;; 3) chosen=confort ;; 4) chosen=cerveau ;; *) warn "réponse inconnue (« $a ») : profil recommandé conservé" ;; esac
    fi
  fi
  WANT_FACE=0; WANT_EMBED=0; WANT_SEARCH=0
  case "$chosen" in
    standard) WANT_FACE=1; WANT_EMBED="$REC_EMBED"; WANT_SEARCH="$REC_SEARCH" ;;
    confort|cerveau) WANT_FACE=1; WANT_EMBED=1; WANT_SEARCH=1 ;;
  esac
  case "$chosen" in
    standard|confort|cerveau)
      [ "$REC_FACE" = 1 ] || warn "profil « $(pname "$chosen") » demandé alors que la machine est juste pour les visages locaux : à vos risques."
      [ "$chosen" != standard ] && [ "$REC_EMBED" = 0 ] && warn "profil « $(pname "$chosen") » demandé alors que la machine est juste pour bge-m3 : à vos risques."
      [ "$chosen" != standard ] && [ "$REC_SEARCH" = 0 ] && warn "profil « $(pname "$chosen") » demandé alors que la machine est juste pour la recherche multimédia : à vos risques." ;;
  esac
  case "$SEARCH" in yes) WANT_SEARCH=1 ;; no) WANT_SEARCH=0 ;; esac
  # Cerveau de la constellation : les documents des membres de vos amis passeront (en mémoire) par cette machine. Jamais sans un oui.
  AI_INDEX=off
  if [ "$chosen" = cerveau ]; then
    say ""
    say "  Cerveau de la constellation : cette station lira le texte des documents (scans, PDF, photos de tickets)"
    say "  pour les membres de vos Capitaines amis qui l'acceptent. Leurs fichiers passent ici EN MÉMOIRE seulement,"
    say "  rien n'est écrit sur ce disque ; vous pourrez fermer ce service à tout moment (Réglages › Tunnels › ai-index)."
    # Non par défaut : seul un « o » tapé (ou --profile cerveau --yes, choix explicite en ligne de commande) l'ouvre.
    local a="n"
    if [ "$YES" = 1 ]; then a="o"
    elif [ -r /dev/tty ]; then printf '  Ouvrir ce service à vos Capitaines amis ? [o/N] ' > /dev/tty; read -r a < /dev/tty || a="n"; fi
    case "$a" in o|O|oui|Oui) AI_INDEX=friends ;; *) say "  D'accord : service fermé (ouvrable plus tard dans les Réglages)." ;; esac
  fi
  [ "$NO_FACE" = 1 ] && WANT_FACE=0
  case "$EMBED" in yes) WANT_EMBED=1 ;; no) WANT_EMBED=0 ;; esac
  case "$TARGET" in *-musl) WANT_FACE=0; WANT_EMBED=0; WANT_SEARCH=0; AI_INDEX=off ;; esac
  [ "$WANT_SEARCH" = 1 ] || AI_INDEX=off
  threads_for "$chosen"; WANT_THREADS="$REC_THREADS"
  PROFILE_CHOSEN="$chosen"
}

detect_hardware
# Version à installer (« latest » : la dernière release, préversions comprises).
resolve_version() {
  if [ "$VERSION" = "latest" ]; then
    # Une préversion n'est jamais « latest » pour GitHub : on interroge alors la liste des releases.
    local api="https://api.github.com/repos/${REPO}/releases?per_page=1" tag
    tag="$(curl -fsSL "$api" 2>/dev/null | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n 1 || true)"
    [ -n "$tag" ] || fail "impossible de lire la dernière release." "pas de réseau, GitHub injoignable ou limite de requêtes atteinte (${api})." "vérifiez la connexion, ou imposez la version : install.sh --version vX.Y.Z"
    VERSION="$tag"
  fi
  case "$VERSION" in v*) ;; *) VERSION="v${VERSION}" ;; esac
}

# Oui/non au terminal (oui par défaut ; --yes répond oui ; sans terminal : non).
confirm() {
  [ "$YES" = 1 ] && return 0
  [ -r /dev/tty ] || return 1
  printf '%s [O/n] ' "$1" > /dev/tty
  local a; read -r a < /dev/tty || a="n"
  case "${a:-o}" in n|N|non|Non) return 1 ;; *) return 0 ;; esac
}

# Binaire déjà à jour : on ne le retélécharge pas, mais modèles et plugins sont quand même revus (un plugin ou un modèle
# peut avoir été ajouté ou corrigé sans nouvelle version de la station).
SKIP_BIN=0
if [ "$UPDATE" = 1 ] && [ "$CHECK_ONLY" = 0 ]; then
  resolve_version
  say ""
  if [ "${VERSION#v}" = "$INSTALLED" ] && [ "$FORCE" = 0 ]; then
    SKIP_BIN=1
    say "✔ Station déjà à jour (${INSTALLED}) — on vérifie les modèles de recherche et les plugins (--force réinstalle le binaire)."
  elif [ "$DRY" = 0 ]; then
    confirm "Mettre à jour ${INSTALLED} → ${VERSION#v} ?" || { SKIP_BIN=1; say "D'accord, binaire inchangé ; on vérifie seulement modèles et plugins."; }
  fi
fi

recommend
if [ "$UPDATE" = 0 ] || [ "$CHECK_ONLY" = 1 ]; then
  show_hardware
  show_recommendation
fi
if [ "$CHECK_ONLY" = 1 ]; then
  say ""; say "Diagnostic seul (--check-hardware) : rien n'a été installé."
  exit 0
fi
if [ "$UPDATE" = 1 ] && [ "$PROFILE" = auto ]; then
  # Mise à jour : on garde les choix de la première installation, lus sur le disque.
  WANT_FACE=0; WANT_EMBED=0; WANT_SEARCH=0; WANT_THREADS=0; PROFILE_CHOSEN=actuel
  { [ -f "${MODELS}/det_10g.onnx" ] || [ -f "${HOME}/.insightface/models/buffalo_l/det_10g.onnx" ]; } && WANT_FACE=1
  [ -f "${MODELS}/bge-m3-int8.onnx" ] && WANT_EMBED=1
  [ -f "${MODELS}/ocr-det.onnx" ] && WANT_SEARCH=1
  [ "$NO_FACE" = 1 ] && WANT_FACE=0
  case "$EMBED" in yes) WANT_EMBED=1 ;; no) WANT_EMBED=0 ;; esac
else
  choose_profile
fi
say ""; say "▶ Profil retenu : $(pname "$PROFILE_CHOSEN") (visages locaux : $([ "$WANT_FACE" = 1 ] && printf oui || printf non), embeddings locaux : $([ "$WANT_EMBED" = 1 ] && printf oui || printf non), recherche multimédia : $([ "$WANT_SEARCH" = 1 ] && printf locale || printf 'confiée à un ami')$([ "$AI_INDEX" = friends ] && printf ', cerveau partagé ouvert aux amis'))"

# --- Simulation : ce qui serait fait, sans rien écrire ----------------------------------------------------------
if [ "$DRY" = 1 ]; then
  resolve_version
  say ""; say "▶ Ce qui serait fait"
  say "  • télécharger ${BIN}-${VERSION#v}-${TARGET}.tar.gz (github.com/${REPO}/releases) et vérifier son SHA-256"
  say "  • installer ${PREFIX}/bin/${BIN}"
  [ "$WANT_FACE" = 1 ] && say "  • modèles de visages (≈ 190 Mo) dans ${MODELS}"
  [ "$WANT_EMBED" = 1 ] && say "  • modèles d'embeddings bge-m3 (≈ 560 Mo) dans ${MODELS}"
  [ "$WANT_SEARCH" = 1 ] && say "  • modèles de recherche multimédia ($(search_models)) dans ${MODELS} (astroportd models add)"
  [ "$AI_INDEX" = friends ] && say "  • ouvrir le cerveau partagé (ai-index) aux Capitaines amis (init --ai-index friends)"
  { [ "$WANT_FACE" = 1 ] || [ "$WANT_EMBED" = 1 ] || [ "$WANT_SEARCH" = 1 ]; } || say "  • aucun modèle local (profil sans calcul local)"
  if [ "$UPDATE" = 1 ]; then say "  • mettre à jour les modèles installés, proposer ceux que le catalogue a ajoutés, puis les plugins (install-plugins.sh --update)"; say "  • proposer de redémarrer le service (station et données conservées)"
  elif [ "$RUN_INIT" = 1 ]; then say "  • lancer « ${BIN} init » (clés Y-Level, ORIGIN/ẐEN, réseau), puis proposer les plugins"
  else say "  • s'arrêter avant « init » (--no-init)"; fi
  say ""; say "Simulation terminée : rien n'a été installé ni écrit."
  exit 0
fi

# --- Téléchargement ---------------------------------------------------------------------------------------------
resolve_version
BASE="https://github.com/${REPO}/releases/download/${VERSION}"
NUM="${VERSION#v}"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# Télécharge l'archive de ${TARGET}, vérifie son empreinte, l'installe, et dit si le binaire démarre (code retour).
fetch_and_install() {
  local name="${BIN}-${NUM}-${TARGET}" archive="${BIN}-${NUM}-${TARGET}.tar.gz" expected actual prog=(-sS)
  say "  astroportd ${VERSION} pour ${TARGET}"
  say "  Téléchargement de ${archive}…"
  [ -t 2 ] && prog=(-#)
  curl -fL "${prog[@]}" --proto '=https' --tlsv1.2 -o "${tmp}/${archive}" "${BASE}/${archive}" \
    || fail "téléchargement impossible : ${archive}" "réseau coupé, ou cette cible n'existe pas dans la release ${VERSION} (${BASE})." "vérifiez la connexion, puis relancez ; autre version : install.sh --version vX.Y.Z"
  curl -fsSL --proto '=https' --tlsv1.2 -o "${tmp}/SHA256SUMS" "${BASE}/SHA256SUMS" \
    || fail "téléchargement impossible : SHA256SUMS" "la release ${VERSION} n'a pas de fichier d'empreintes." "relancez plus tard ou choisissez une autre version (--version)."

  # Vérification de l'empreinte
  expected="$(awk -v f="$archive" '{ n=$2; sub(/^\*/, "", n); if (n == f) { print $1; exit } }' "${tmp}/SHA256SUMS")"
  [ -n "$expected" ] || die "${archive} ne figure pas dans SHA256SUMS : installation refusée."
  actual="$(sha256sum "${tmp}/${archive}" | awk '{print $1}')"
  if [ "$expected" != "$actual" ]; then
    say "  attendue : ${expected}"; say "  calculée : ${actual}"
    fail "empreinte SHA-256 incorrecte pour ${archive} : installation refusée." "fichier corrompu pendant le transfert, ou modifié." "relancez install.sh ; si cela persiste, ne l'utilisez pas et signalez-le : https://github.com/${REPO}/issues"
  fi
  say "  ✔ empreinte SHA-256 vérifiée"

  # Installation
  tar -C "$tmp" -xzf "${tmp}/${archive}" || die "archive illisible : ${archive}"
  [ -f "${tmp}/${name}/${BIN}" ] || die "l'archive ne contient pas ${name}/${BIN}"
  mkdir -p "${PREFIX}/bin"
  install -m 0755 "${tmp}/${name}/${BIN}" "${PREFIX}/bin/${BIN}"
  say "  ✔ installé : ${PREFIX}/bin/${BIN}"
  "${PREFIX}/bin/${BIN}" --version 2>/dev/null
}

step "Téléchargement et vérification"
if [ "$SKIP_BIN" = 1 ]; then
  say "  binaire en place conservé : ${PREFIX}/bin/${BIN} (${INSTALLED})"
elif ! fetch_and_install; then
  "${PREFIX}/bin/${BIN}" --version 2>&1 | head -n 3 >&2 || true
  # Auto-guérison : un binaire gnu qui ne démarre pas (glibc ou bibliothèque absente) est remplacé par le statique.
  case "$TARGET" in
    x86_64-unknown-linux-gnu|aarch64-unknown-linux-gnu)
      warn "Le binaire standard ne démarre pas sur cette machine (glibc ou bibliothèque incompatible)."
      say "  → Récupération automatique : binaire statique autonome (sans visages ni embeddings locaux)."
      TARGET="${TARGET%-gnu}-musl"; STATIC=1
      WANT_FACE=0; WANT_EMBED=0
      fetch_and_install || fail "le binaire statique ne démarre pas non plus." "architecture non prise en charge ou système très inhabituel." "signalez-le avec le détail ci-dessus : https://github.com/${REPO}/issues" ;;
    *) fail "le binaire installé ne démarre pas." "architecture incompatible avec cette machine (${TARGET})." "aucune alternative statique pour cette architecture : signalez-le https://github.com/${REPO}/issues" ;;
  esac
fi

# --- Modèles locaux : visages (SCRFD + ArcFace) et embeddings de BRO (bge-m3) --------------------------------------
# Binaire glibc = compilé avec `onnx`. Il lui faut libonnxruntime (extraite d'une roue pip) et les modèles. Rien n'est
# jamais accepté sans SHA-256 correct. Visages : IPFS (UPlanet) puis web ; bge-m3 : Hugging Face, révision figée.
# Les deux modèles du paquet InsightFace buffalo_l : nom | SHA-256 | CID IPFS (v1)
MODEL_FILES=(
  "det_10g.onnx|5838f7fe053675b1c7a08b633df49e7af5495cee0493c7dcf6697200b85b5b91|bafybeid76moctrw6jgyiv7uvcllidn6ihig22fo4mgu65iv7nlgzzf7zzm"
  "w600k_r50.onnx|4c06341c33c2ca1f86781dab0e829f88ad5b64be9fba56e56bc9ebdefc619e43|bafybeiejw6zzuiwk36afumfy4hwvdpdnt5f6xrb5pkvkobewrf2hwtmvja"
)
IPFS_GATEWAY="${ASTRO_IPFS_GATEWAY:-https://ipfs.copylaradio.com}"
BUFFALO_URL="https://github.com/deepinsight/insightface/releases/download/v0.7/buffalo_l.zip"

have_ort() {
  [ -n "${ORT_DYLIB_PATH:-}" ] && [ -e "$ORT_DYLIB_PATH" ] && return 0
  compgen -G "${DATA}/lib/libonnxruntime.so*" >/dev/null && return 0
  compgen -G "${HOME}/.astro/lib/python3*/site-packages/onnxruntime/capi/libonnxruntime.so*" >/dev/null && return 0
  compgen -G "/usr/local/lib/libonnxruntime.so*" >/dev/null && return 0
  compgen -G "/usr/lib/libonnxruntime.so*" >/dev/null && return 0
  compgen -G "/usr/lib/*-linux-gnu/libonnxruntime.so*" >/dev/null
}

install_ort() {
  have_ort && { say "  ✔ libonnxruntime déjà présente"; return 0; }
  if ! { command -v python3 >/dev/null 2>&1 && python3 -m pip --version >/dev/null 2>&1; }; then
    warn "libonnxruntime : python3/pip absents (sudo apt install python3-pip), ou définissez ORT_DYLIB_PATH"; return 1
  fi
  say "  Installation de libonnxruntime (roue pip, extraite dans ${DATA}/lib)…"
  local whl; whl="$(mktemp -d)"
  if python3 -m pip download onnxruntime --no-deps -d "$whl" -q 2>/dev/null && python3 - "$whl" "${DATA}/lib" <<'PY'
import glob, os, sys, zipfile
whl = glob.glob(os.path.join(sys.argv[1], "onnxruntime-*.whl"))[0]
os.makedirs(sys.argv[2], exist_ok=True)
with zipfile.ZipFile(whl) as z:
    for n in z.namelist():
        if n.startswith("onnxruntime/capi/libonnxruntime") and ".so" in n:
            open(os.path.join(sys.argv[2], os.path.basename(n)), "wb").write(z.read(n))
PY
  then say "  ✔ libonnxruntime installée"; rm -rf "$whl"
  else warn "libonnxruntime non installée (pip a échoué)"; rm -rf "$whl"; return 1
  fi
}

# Dépose un modèle dans $MODELS s'il a le bon SHA-256 ; essaie le nœud IPFS local, puis la passerelle.
fetch_model() { # nom sha cid
  local name="$1" sha="$2" cid="$3" dest="${MODELS}/$1" part="${MODELS}/$1.part"
  if [ -f "$dest" ] && [ "$(sha256sum "$dest" | cut -d' ' -f1)" = "$sha" ]; then say "  ✔ $name déjà présent"; return 0; fi
  mkdir -p "$MODELS"; rm -f "$part"
  if command -v ipfs >/dev/null 2>&1 && timeout 5 ipfs id >/dev/null 2>&1 \
     && timeout 120 ipfs cat "$cid" > "$part" 2>/dev/null && [ -s "$part" ]; then
    say "  $name : reçu par IPFS (nœud local)"
  elif curl -fsSL --proto '=https' --tlsv1.2 -m 600 -o "$part" "${IPFS_GATEWAY}/ipfs/${cid}" 2>/dev/null && [ -s "$part" ]; then
    say "  $name : reçu par la passerelle ${IPFS_GATEWAY}"
  else
    rm -f "$part"; return 1
  fi
  if [ "$(sha256sum "$part" | cut -d' ' -f1)" = "$sha" ]; then mv "$part" "$dest"; say "  ✔ $name vérifié (SHA-256)"
  else rm -f "$part"; warn "$name : SHA-256 incorrect, fichier refusé"; return 1; fi
}

# Dernier recours : le paquet buffalo_l du web classique (275 Mo), dont on ne garde que les deux modèles.
fetch_models_from_web() {
  say "  Repli : téléchargement de buffalo_l depuis GitHub (275 Mo)…"
  local z; z="$(mktemp -d)"
  curl -fSL --proto '=https' --tlsv1.2 -o "${z}/b.zip" "$BUFFALO_URL" || { rm -rf "$z"; return 1; }
  python3 -c 'import sys,zipfile; z=zipfile.ZipFile(sys.argv[1]); [open(sys.argv[2]+"/"+n,"wb").write(z.read(n)) for n in sys.argv[3:]]' \
    "${z}/b.zip" "$MODELS" det_10g.onnx w600k_r50.onnx || { rm -rf "$z"; return 1; }
  rm -rf "$z"
  local m name sha
  for m in "${MODEL_FILES[@]}"; do
    name="${m%%|*}"; sha="$(echo "$m" | cut -d'|' -f2)"
    [ "$(sha256sum "${MODELS}/${name}" | cut -d' ' -f1)" = "$sha" ] || { rm -f "${MODELS}/${name}"; warn "$name : SHA-256 incorrect"; return 1; }
  done
}

install_models() {
  # Déjà là ailleurs ? astroportd les trouve tout seul.
  if [ -f "${HOME}/.insightface/models/buffalo_l/det_10g.onnx" ] && [ -f "${HOME}/.insightface/models/buffalo_l/w600k_r50.onnx" ]; then
    say "  ✔ modèles déjà présents (~/.insightface/models/buffalo_l)"; return 0
  fi
  warn "Modèles InsightFace buffalo_l : poids publiés pour la RECHERCHE NON COMMERCIALE, vérifiez la licence de votre usage."
  local m missing=0
  for m in "${MODEL_FILES[@]}"; do
    IFS='|' read -r n sha cid <<< "$m"
    fetch_model "$n" "$sha" "$cid" || missing=1
  done
  [ "$missing" = 0 ] || fetch_models_from_web || { warn "modèles non installés : placez det_10g.onnx et w600k_r50.onnx dans ${MODELS}"; return 1; }
}


# bge-m3 int8 (Xenova/bge-m3, licence MIT), révision figée : nom installé | chemin dans le dépôt | SHA-256 | taille en Mo
BGE_REV="4de13258303883538bd53b696b452bf8099f0858"
BGE_BASE="${ASTRO_BGE_BASE_URL:-https://huggingface.co/Xenova/bge-m3/resolve/${BGE_REV}}"
BGE_FILES=(
  "bge-m3-int8.onnx|onnx/model_int8.onnx|a206e10e995aa2a833924bcd725ba5dd6c3425cd34bac3cf2b5677cd2a1c51d6|542"
  "bge-m3-tokenizer.json|tokenizer.json|6710678b12670bc442b99edc952c4d996ae309a7020c1fa0096dd245c2faf790|17"
)

# Télécharge un fichier (reprise possible) et ne le garde que si son SHA-256 est correct.
fetch_checked() { # nom chemin sha taille_mo
  local name="$1" path="$2" sha="$3" size="$4" dest="${MODELS}/$1" part="${MODELS}/$1.part" try
  if [ -f "$dest" ] && [ "$(sha256sum "$dest" | cut -d' ' -f1)" = "$sha" ]; then say "  ✔ $name déjà présent"; return 0; fi
  mkdir -p "$MODELS"
  local free; free="$(df -Pm "$MODELS" 2>/dev/null | awk 'NR==2 {print $4}')"
  if [ "${free:-0}" -lt $((size * 2 + 200)) ]; then warn "$name : place insuffisante dans ${MODELS} ($(fmt_mb "${free:-0}") libres, $((size * 2 + 200)) Mo demandés)"; return 1; fi
  local prog=(-sS); [ -t 2 ] && prog=(-#)
  for try in 1 2; do
    say "  $name : téléchargement (${size} Mo)…"
    curl -fL "${prog[@]}" --proto '=https' --tlsv1.2 -C - -o "$part" "${BGE_BASE}/${path}" || { rm -f "$part"; continue; }
    if [ "$(sha256sum "$part" | cut -d' ' -f1)" = "$sha" ]; then mv "$part" "$dest"; say "  ✔ $name vérifié (SHA-256)"; return 0; fi
    rm -f "$part"; warn "$name : SHA-256 incorrect (essai ${try}/2)"
  done
  return 1
}

install_embed_models() {
  local m n p sha size
  for m in "${BGE_FILES[@]}"; do
    IFS='|' read -r n p sha size <<< "$m"
    fetch_checked "$n" "$p" "$sha" "$size" || { warn "embeddings locaux non installés : placez ${n} dans ${MODELS} (Xenova/bge-m3, ${p})"; return 1; }
  done
}

# Recherche multimédia : le binaire installe lui-même ses modèles (catalogue figé, SHA-256, IPFS puis Hugging Face).
# Un modèle déjà là et intact n'est pas retéléchargé ; un fichier dont l'empreinte a changé avec la nouvelle version l'est.
MODELS_CHANGED=0
install_search_models() {
  local id ok=0 out
  for id in $(search_models); do
    say "  ${id} :"
    if out="$("${PREFIX}/bin/${BIN}" models --dir "$MODELS" add "$id")"; then
      say "$out"
      case "$out" in *"installé depuis"*) MODELS_CHANGED=1 ;; esac
    else
      warn "${id} non installé : relancez « ${BIN} models add ${id} »"; ok=1
    fi
  done
  return "$ok"
}

# Mise à jour : modèles que le catalogue de ce binaire connaît et qui ne sont pas encore là (ajoutés depuis la première
# installation). Proposés seulement si la machine tient la recherche locale ; la parole (Whisper) seulement en confort.
NEW_MODELS=""
offer_new_models() {
  local lines id mb tasks label rec def a
  lines="$("${PREFIX}/bin/${BIN}" models --dir "$MODELS" list --absent 2>/dev/null)" || return 0
  [ -n "$lines" ] || return 0
  [ "$REC_SEARCH" = 1 ] || { say "  Recherche multimédia : de nouveaux modèles existent, mais cette machine la confie à un Capitaine ami."; return 0; }
  say ""; say "▶ Nouveaux modèles de recherche disponibles"
  while IFS='|' read -r id mb tasks label; do
    [ -n "$id" ] || continue
    rec=1; case "$tasks" in *speech*) case "$REC_PROFILE" in confort|cerveau) ;; *) rec=0 ;; esac ;; esac
    def=$([ "$rec" = 1 ] && printf 'O/n' || printf 'o/N')
    a=""
    if [ "$YES" = 1 ]; then a=$([ "$rec" = 1 ] && printf o || printf n)
    elif [ -r /dev/tty ]; then
      printf '  %s — %s (≈ %s Mo)%s. L'\''installer ? [%s] ' "$id" "$label" "$mb" "$([ "$rec" = 1 ] || printf ', machine un peu juste')" "$def" > /dev/tty
      read -r a < /dev/tty || a=""
    fi
    [ -n "$a" ] || a=$([ "$rec" = 1 ] && printf o || printf n)
    case "$a" in o|O|oui|Oui|y|Y) NEW_MODELS="${NEW_MODELS:+$NEW_MODELS }${id}" ;; *) say "  ${id} : laissé de côté (plus tard : ${BIN} models add ${id})" ;; esac
  done <<< "$lines"
  [ -z "$NEW_MODELS" ] || WANT_SEARCH=1
}
[ "$PROFILE_CHOSEN" = actuel ] && offer_new_models

# Seulement ce que le profil retenu demande.
if [ "$WANT_FACE" = 0 ] && [ "$WANT_EMBED" = 0 ] && [ "$WANT_SEARCH" = 0 ]; then
  step "Modèles locaux"; say "  aucun demandé par le profil « $(pname "$PROFILE_CHOSEN") » (rien à télécharger)."
fi
# Rejouable : si pip manque ici, install_sudo.sh l'installe plus bas et l'étape est relancée.
ORT_PENDING=0
setup_local_models() {
  if install_ort; then
    ORT_PENDING=0
    if [ "$WANT_SEARCH" = 1 ]; then
      install_search_models || warn "recherche multimédia locale incomplète : voir « astroportd models list »"
    fi
    if [ "$WANT_FACE" = 1 ]; then install_models || warn "reconnaissance de visages locale incomplète : voir « astroportd face-check » (ou serve --face-engine http://…)"; fi
    if [ "$WANT_EMBED" = 1 ]; then install_embed_models || warn "embeddings locaux incomplets : voir « astroportd embed-check » (ou serve --ollama http://… pour les vecteurs)"; fi
    if [ "$WANT_EMBED" = 1 ] && [ -f "${MODELS}/bge-m3-int8.onnx" ]; then
      "${PREFIX}/bin/${BIN}" embed-check || warn "essai des embeddings locaux en échec : voir le message ci-dessus"
    fi
  elif { command -v python3 >/dev/null 2>&1 && python3 -m pip --version >/dev/null 2>&1; }; then
    warn "ONNX Runtime absent : ni visages ni embeddings locaux (voir « astroportd face-check » / « embed-check »)"
  else
    ORT_PENDING=1
    say "  ONNX Runtime attend python3-pip : il sera proposé avec les outils du système, puis cette étape reprendra."
  fi
}
if [ "$WANT_FACE" = 1 ] || [ "$WANT_EMBED" = 1 ] || [ "$WANT_SEARCH" = 1 ]; then
  step "Modèles locaux"
  setup_local_models
fi

# --- Outils du système : ce script n'utilise jamais sudo ; ce qui manque est proposé à part (install_sudo.sh) ---------
SUDO_SCRIPT="${ASTRO_SUDO_SCRIPT:-https://raw.githubusercontent.com/${REPO}/main/install_sudo.sh}"
SYS_NEEDS=()
SUDO_ARGS=(--user "$(id -un)")
{ command -v ffmpeg >/dev/null 2>&1 && command -v ffprobe >/dev/null 2>&1; } || SYS_NEEDS+=("ffmpeg : images des vidéos pour la recherche, Médiathèque")
[ "$WANT_SEARCH" = 1 ] && ! command -v pdftoppm >/dev/null 2>&1 && SYS_NEEDS+=("poppler : lecture des PDF")
if { [ "$WANT_FACE" = 1 ] || [ "$WANT_EMBED" = 1 ] || [ "$WANT_SEARCH" = 1 ]; } && ! have_ort && ! { command -v python3 >/dev/null 2>&1 && python3 -m pip --version >/dev/null 2>&1; }; then
  SYS_NEEDS+=("python3-pip : ONNX Runtime (visages, recherche)")
fi
command -v git >/dev/null 2>&1 || SYS_NEEDS+=("git : plugins (films, énergie, DNS)")
# Navigateur prêté aux agents (Mes agents) : facultatif ; ASTRO_CHROMIUM peut désigner un autre binaire.
if [ -z "${ASTRO_CHROMIUM:-}" ] && ! command -v chromium >/dev/null 2>&1 && ! command -v chromium-browser >/dev/null 2>&1 && ! command -v google-chrome >/dev/null 2>&1; then
  SYS_NEEDS+=("chromium : navigateur jetable prêté aux agents, dans Mes agents (facultatif)"); SUDO_ARGS+=(--chromium)
fi
# ORIGIN publie ses fichiers par Kubo, en miroir du disque, sur le réseau IPFS des stations ORIGIN (swarm.key à zéros) ; une station ẐEN n'a jamais de Kubo.
if [ "$MODE" = zen ] || conf_get mode 2>/dev/null | grep -qx zen; then
  SUDO_ARGS+=(--no-kubo)
elif ! command -v ipfs >/dev/null 2>&1; then
  SYS_NEEDS+=("kubo : publication des fichiers entre stations ORIGIN (miroir du disque ; facultatif)"); SUDO_ARGS+=(--kubo)
elif [ "$(ipfs config --json Experimental.FilestoreEnabled 2>/dev/null)" != true ]; then
  SYS_NEEDS+=("kubo : activer le filestore (publication sans doubler la place sur le disque)"); SUDO_ARGS+=(--kubo)
else
  SUDO_ARGS+=(--kubo)
fi
if command -v loginctl >/dev/null 2>&1 && loginctl show-user "$(id -un)" -p Linger 2>/dev/null | grep -q '=no'; then
  SYS_NEEDS+=("démarrage avec la machine (linger) : sans lui, astroportd s'arrête quand vous fermez votre session")
else
  SUDO_ARGS+=(--no-linger)
fi
say ""
say "▶ Outils du système"
if [ ${#SYS_NEEDS[@]} = 0 ]; then
  say "  ✔ tout est en place (ffmpeg, poppler, git, kubo et démarrage avec la machine, selon la station)"
else
  for n in "${SYS_NEEDS[@]}"; do say "  • manque ${n}"; done
  sudo_cmd="curl -fsSL ${SUDO_SCRIPT} -o install_sudo.sh && sudo bash install_sudo.sh ${SUDO_ARGS[*]}"
  if [ "$DRY" = 1 ] || [ "$YES" = 1 ] || [ ! -r /dev/tty ]; then
    say "  Pour les installer (droits administrateur) :"
    say "    \$ ${sudo_cmd}"
  else
    printf '  Les installer maintenant avec install_sudo.sh ? Il demande votre mot de passe (sudo) et ne fait que cela. [o/N] ' > /dev/tty
    read -r _sys < /dev/tty || _sys="n"
    case "${_sys:-n}" in
      o|O|oui|Oui|y|Y)
        # Lancé depuis un dépôt cloné : son install_sudo.sh ; sinon celui du dépôt, téléchargé (et gardé pour le relire).
        _here=""; if _d="$(dirname "${BASH_SOURCE[0]:-$0}")" && [ -d "$_d" ]; then _here="$(cd "$_d" && pwd)"; fi
        if [ -n "$_here" ] && [ -f "${_here}/install_sudo.sh" ]; then _sudo_sh="${_here}/install_sudo.sh"
        else
          _sudo_sh="${DATA}/install_sudo.sh"; mkdir -p "$DATA"
          curl -fsSL --proto '=https' --tlsv1.2 -o "$_sudo_sh" "$SUDO_SCRIPT" || { warn "install_sudo.sh introuvable : ${SUDO_SCRIPT}"; _sudo_sh=""; }
        fi
        if [ -n "$_sudo_sh" ]; then
          say "  ${_sudo_sh} (lisible avant de l'accepter)"
          # Le terminal sert aux questions du script (et au mot de passe de sudo) ; la redirection est voulue côté utilisateur.
          # shellcheck disable=SC2024
          sudo bash "$_sudo_sh" "${SUDO_ARGS[@]}" < /dev/tty || warn "install_sudo.sh interrompu : relancez « sudo bash ${_sudo_sh} ${SUDO_ARGS[*]} »"
        fi ;;
      *) say "  D'accord. Plus tard : ${sudo_cmd}" ;;
    esac
  fi
fi

# pip vient peut-être d'arriver avec install_sudo.sh : reprendre ONNX Runtime et les modèles (visages, embeddings, recherche).
if [ "$ORT_PENDING" = 1 ]; then
  say ""
  say "▶ Modèles locaux (reprise, maintenant que python3-pip est là)"
  if command -v python3 >/dev/null 2>&1 && python3 -m pip --version >/dev/null 2>&1; then
    setup_local_models
  fi
  [ "$ORT_PENDING" = 0 ] || warn "ONNX Runtime toujours absent : sudo apt install python3-pip, puis relancez ce script (la station est conservée)."
fi

# --- Ports déjà occupés (cause fréquente de « Address already in use ») ------------------------------------------
if [ "$UPDATE" = 0 ] && command -v ss >/dev/null 2>&1; then   # une station en place a déjà réglé ses ports
  busy=""; for p in 8090 80 443; do ss -ltn 2>/dev/null | awk '{print $4}' | grep -qE "[:.]${p}$" && busy="${busy} ${p}"; done
  if [ -n "$busy" ]; then
    warn "Ports déjà utilisés sur cette machine :${busy} (IPFS, Nginx Proxy Manager…)."
    warn "  « astroportd init » en tient compte : si 80/443 sont pris, astroportd écoute sur le port 8090 derrière votre proxy ; sinon il gère lui-même HTTPS."
    warn "  Proxy sur une autre machine : « astroportd init --proxy »."
  fi
fi

case ":${PATH}:" in
  *":${PREFIX}/bin:"*) ;;
  *) warn "${PREFIX}/bin n'est pas dans votre PATH. Ajoutez à ~/.bashrc :  export PATH=\"${PREFIX}/bin:\$PATH\"" ;;
esac

# --- Plugins : présentation et « git clone » par install-plugins.sh (media, energy, dnslink…) --------------------------
PLUGINS_SCRIPT="${ASTRO_PLUGINS_SCRIPT:-https://raw.githubusercontent.com/papiche/astroportd-plugins/main/install-plugins.sh}"
PLUGINS_DIR="${ASTRO_PLUGINS_DIR:-${HOME}/.zen/astroportd-plugins}"
offer_plugins() {
  [ "$YES" = 1 ] && return 0
  step "Plugins (facultatif)"
  [ -r /dev/tty ] || { say "Plugins (films, énergie, DNS…) : curl -fsSL ${PLUGINS_SCRIPT} | bash"; return 0; }
  printf '  Des outils en plus : films et musique, énergie, DNSLink. Vous pourrez les ajouter plus tard.\n  Les voir et en installer ? [o/N] ' > /dev/tty
  read -r answer < /dev/tty || answer="n"
  case "${answer:-n}" in
    o|O|oui|Oui|y|Y) curl -fsSL --proto '=https' --tlsv1.2 "$PLUGINS_SCRIPT" | bash || warn "installation des plugins interrompue : relancez-la quand vous voulez (${PLUGINS_SCRIPT})." ;;
    *) say "D'accord. Quand vous voulez : curl -fsSL ${PLUGINS_SCRIPT} | bash" ;;
  esac
}
# Mise à jour : les plugins déjà clonés sont mis à jour (git pull, puis réinstallation de ceux dont la version a changé) et
# les nouveaux sont proposés. Le script le plus récent est pris en ligne (il sait lire l'état laissé par les anciens).
# Sans plugins clonés, une ligne seulement : on ne repose pas à chaque mise à jour une question déjà refusée.
update_plugins() {
  if [ ! -d "${PLUGINS_DIR}/.git" ]; then
    say ""; say "▶ Plugins : aucun installé. Quand vous voulez : curl -fsSL ${PLUGINS_SCRIPT} | bash"; return 0
  fi
  step "Plugins : mises à jour et nouveautés"
  local args=(--update); [ "$YES" = 1 ] && args+=(--yes)
  if [ ! -r /dev/tty ] && [ "$YES" = 0 ]; then args+=(--list); fi
  curl -fsSL --proto '=https' --tlsv1.2 "$PLUGINS_SCRIPT" | bash -s -- "${args[@]}" \
    || warn "mise à jour des plugins interrompue : ${PLUGINS_DIR}/install-plugins.sh --update"
}

# --- Mise à jour : redémarrer le service, jamais « init » --------------------------------------------------------
if [ "$UPDATE" = 1 ]; then
  update_plugins
  say ""
  if [ "$SKIP_BIN" = 1 ] && [ "$MODELS_CHANGED" = 0 ]; then
    say "Vérification terminée : binaire, modèles et plugins à jour. Vérifier : ${BIN} doctor"
    exit 0
  fi
  why="passer à la nouvelle version"; [ "$SKIP_BIN" = 1 ] && why="charger les modèles mis à jour"
  case "$(service_state)" in
    active)
      if confirm "Redémarrer le service ${BIN} pour ${why} ?"; then
        systemctl --user restart "$BIN" || warn "redémarrage refusé par systemd : journalctl --user -u ${BIN}"
        sleep 3
        if [ "$(service_state)" = active ]; then say "  ✔ ${BIN} redémarré : $("${PREFIX}/bin/${BIN}" --version 2>/dev/null)"
        else warn "le service ne tourne pas après le redémarrage : journalctl --user -u ${BIN} -n 50"; fi
      else
        say "  Le service tourne encore l'ancienne version : systemctl --user restart ${BIN}"
      fi ;;
    absent)
      if pgrep -x "$BIN" >/dev/null 2>&1; then warn "${BIN} tourne hors systemd : arrêtez-le et relancez votre commande « ${BIN} serve … » pour passer à la nouvelle version."
      else
        # Pas d'unité : « systemctl --user start » échouerait. Linger seul ne suffit pas à démarrer avec la machine.
        say "  Aucun service installé : relancez votre commande « ${BIN} serve … »,"
        say "  ou faites-en un service qui démarre avec la machine : ${BIN} init --systemd (la station existante est reprise)"
      fi ;;
    *) say "  Le service est arrêté : systemctl --user start ${BIN}" ;;
  esac
  say ""
  say "Mise à jour terminée. Station, comptes et données conservés. Vérifier : ${BIN} doctor"
  exit 0
fi

# Carte finale : où aller maintenant (l'adresse est lue dans station.conf, écrit par « init »).
show_final() {
  local dom url
  dom="$(conf_get domain)"
  if [ -n "$dom" ]; then url="https://${dom}/"; else url="http://127.0.0.1:8090/"; fi
  say ""
  say "════════════════════════════════════════════════════════════════════"
  say "  ✔ Installation terminée"
  say "════════════════════════════════════════════════════════════════════"
  if [ -n "$dom" ]; then
    say "  1. Ouvrez l'interface :"
    say "       $ ${url}"
  else
    # Sans domaine, la station n'écoute que sur 127.0.0.1 : une autre machine (le PC d'où vous êtes en SSH) ne l'atteint pas
    # directement. Un tunnel SSH la rend joignable sans rien ouvrir sur le réseau.
    local host; host="$(hostname -I 2>/dev/null | awk '{print $1}')"; host="${host:-$(hostname)}"
    say "  1. Ouvrez l'interface :"
    say "     • sur cette machine :              ${url}"
    if [ -n "${SSH_CONNECTION:-}" ] || [ -z "${DISPLAY:-}${WAYLAND_DISPLAY:-}" ]; then
      say "     • depuis votre PC (tunnel SSH) :   lancez sur votre PC"
      say "       $ ssh -fN -L 8090:127.0.0.1:8090 ${USER:-$(id -un)}@${host}"
      say "       puis ouvrez http://localhost:8090/ dans son navigateur."
    fi
    say "     • accès direct sur le réseau ou HTTPS : ${BIN} init --domain votre.domaine"
  fi
  local cap; cap="$(conf_get captain_email)"
  if [ -n "$cap" ]; then say "  2. Capitaine : ${cap} (MULTIPASS créé par l'assistant)."
  else say "  2. Créez le Capitaine d'abord (l'inscription web n'ouvre qu'après lui) :
       $ ${BIN} multipass create votre@email --captain"; fi
  say "  3. Sauvegardez vos clés : ~/.zen/game"
  # « init » écrit l'unité systemd sans la lancer : on propose de la démarrer ici (et à chaque démarrage de la machine).
  if [ "$(service_state)" != active ] && [ -f "$UNIT" ] && command -v systemctl >/dev/null 2>&1 \
     && confirm "Démarrer la station maintenant (et à chaque démarrage de la machine) ?"; then
    systemctl --user enable --now "$BIN" >/dev/null 2>&1 || warn "démarrage refusé par systemd : journalctl --user -u ${BIN} -n 50"
    sleep 3
    if [ "$(service_state)" = active ]; then say "  ✔ station démarrée : ${url}"
    else warn "la station ne tourne pas : journalctl --user -u ${BIN} -n 50"; fi
  fi
  if [ "$(service_state)" != active ]; then
    say "  4. La station n'est pas encore démarrée : lancez la commande « Démarrer » affichée plus haut,"
    say "     ou installez-la en service : $ ${BIN} init --systemd"
  fi
  say "  ─────────────────────────────────────────────────────────────────"
  say "  État :     $ systemctl --user status ${BIN}      (ou : ${BIN} doctor)"
  say "  Journal :  $ journalctl --user -u ${BIN} -f"
  say "  Arrêter :  $ systemctl --user stop ${BIN}"
  [ -z "$LOG" ] || say "  Compte rendu de cette installation : ${LOG}"
  say "════════════════════════════════════════════════════════════════════"
}

# --- Suite : astroportd init ------------------------------------------------------------------------------------
if [ "$RUN_INIT" = 0 ]; then
  say ""
  say "Terminé. Étape suivante : ${PREFIX}/bin/${BIN} init"
  exit 0
fi

init_args=()
[ -n "$MODE" ] && init_args+=(--mode "$MODE")
[ -n "$EMAIL" ] && init_args+=(--email "$EMAIL")
[ -n "$DOMAIN" ] && init_args+=(--domain "$DOMAIN")

# Interactif et sans position donnée : on propose de la deviner (un seul appel à ip-api.com, refusé par défaut).
if [ -z "$LAT" ] && [ "$LOCATE" != 1 ] && [ "$YES" != 1 ] && [ -r /dev/tty ] && [ ! -f "${DATA}/station.conf" ]; then
  printf '  Deviner la position de la station d'\''après votre IP (une requête à ip-api.com) ? Sinon, l'\''assistant la demandera. [o/N] ' > /dev/tty
  read -r _loc < /dev/tty || _loc="n"
  case "${_loc:-n}" in o|O|oui|Oui) LOCATE=1 ;; esac
fi
# Position : donnée par --lat/--lon, ou devinée d'après l'IP avec --locate (rien n'est envoyé sans ce choix explicite).
if [ -z "$LAT" ] && [ "$LOCATE" = 1 ]; then
  if command -v curl >/dev/null 2>&1; then
    geo="$(curl -fsS --max-time 5 'http://ip-api.com/json/?fields=status,country,city,lat,lon' 2>/dev/null || true)"
    if printf '%s' "$geo" | grep -q '"status":"success"'; then
      LAT="$(printf '%s' "$geo" | sed -n 's/.*"lat":\(-\?[0-9]*\.\?[0-9]\{0,2\}\)[0-9]*.*/\1/p')"
      LON="$(printf '%s' "$geo" | sed -n 's/.*"lon":\(-\?[0-9]*\.\?[0-9]\{0,2\}\)[0-9]*.*/\1/p')"
      place="$(printf '%s' "$geo" | sed -n 's/.*"city":"\([^"]*\)".*/\1/p')"
      if [ -n "$LAT" ] && [ -n "$LON" ]; then
        say "▶ Position devinée d'après l'IP : ${place:-?} (${LAT}, ${LON}) — modifiable dans station.conf"
      else
        LAT=""; LON=""; warn "position non lisible dans la réponse : on la demandera dans l'assistant."
      fi
    else
      warn "position introuvable d'après l'IP : on la demandera dans l'assistant."
    fi
  else
    warn "--locate demande curl : position non devinée."
  fi
fi
[ -n "$LAT" ] && [ -n "$LON" ] && init_args+=(--lat "$LAT" --lon "$LON")
# Fils de calcul choisis d'après la machine (0 = tous les cœurs : on n'écrit rien).
if [ "$WANT_THREADS" -gt 0 ]; then
  [ "$WANT_FACE" = 1 ] && init_args+=(--face-threads "$WANT_THREADS")
  [ "$WANT_EMBED" = 1 ] && init_args+=(--embed-threads "$WANT_THREADS")
fi
[ "$AI_INDEX" = friends ] && init_args+=(--ai-index friends)

if [ "$YES" = 1 ]; then
  init_args+=(--yes)
  step "Configuration de la station"
  say "  ${BIN} init ${init_args[*]}"
  "${PREFIX}/bin/${BIN}" init "${init_args[@]}"
  show_final
  exit 0
fi

# Lancé par « curl | bash », l'entrée standard est le script : on interroge le terminal s'il existe.
if [ -r /dev/tty ]; then
  step "Configuration de la station"
  printf '  L'\''assistant « %s init » crée la clé de la station (Y-Level), vous fait choisir ORIGIN ou ẐEN, et règle le réseau.\n  Le lancer maintenant ? [O/n] ' "$BIN" > /dev/tty
  read -r answer < /dev/tty || answer="n"
  case "${answer:-o}" in
    n|N|non|Non) say "D'accord. Quand vous voulez : ${PREFIX}/bin/${BIN} init" ;;
    *) "${PREFIX}/bin/${BIN}" init ${init_args[@]+"${init_args[@]}"} < /dev/tty ;;
  esac
  offer_plugins
  [ ! -f "${DATA}/station.conf" ] || show_final
else
  say ""
  say "Pas de terminal interactif. Étape suivante : ${PREFIX}/bin/${BIN} init"
fi
