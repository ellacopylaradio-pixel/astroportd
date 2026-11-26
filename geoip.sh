#!/usr/bin/env bash
# Données : IP Geolocation by DB-IP (https://db-ip.com), CC BY 4.0.
set -euo pipefail
export LC_ALL=C

die() { echo "$*" >&2; exit 1; }
(( $# <= 1 )) || die "Usage : $0 [IPv4|IPv6]"
command -v mmdblookup >/dev/null || die "Installer le paquet mmdb-bin."

if (( $# )); then
    ip=$1
else
    ip=$(curl -fsS --max-time 10 https://api64.ipify.org)
fi
[[ -n $ip ]] || die "Adresse IP manquante."

cache=${XDG_CACHE_HOME:-$HOME/.cache}/geoip-dbip
mkdir -p -- "$cache"
month=$(date -u +%Y-%m)
db="$cache/dbip-city-lite-$month.mmdb"

if [[ ! -s $db ]]; then
    tmp=$(mktemp "$cache/.download.XXXXXXXX")
    trap 'rm -f -- "$tmp"' EXIT
    url="https://download.db-ip.com/free/dbip-city-lite-$month.mmdb.gz"
    if curl -fsSL --connect-timeout 10 --max-time 300 "$url" | gzip -dc > "$tmp" && [[ -s $tmp ]]; then
        mv -f -- "$tmp" "$db"
    else
        shopt -s nullglob
        previous=("$cache"/dbip-city-lite-????-??.mmdb)
        (( ${#previous[@]} )) || die "Impossible de télécharger la base DB-IP."
        db=${previous[-1]}
        echo "Mise à jour indisponible. Utilisation de ${db##*/}." >&2
    fi
fi

coordinate() {
    mmdblookup --file "$db" --ip "$ip" location "$1" |
        awk '$2 == "<double>" && $1 ~ /^-?[0-9]+([.][0-9]+)?$/ {print $1; found=1}
             END {if (!found) exit 1}'
}

lat=$(coordinate latitude) || die "Latitude indisponible pour $ip."
lon=$(coordinate longitude) || die "Longitude indisponible pour $ip."
printf '%.2f %.2f\n' "$lat" "$lon"
