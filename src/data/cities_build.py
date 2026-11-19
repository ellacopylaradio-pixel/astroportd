#!/usr/bin/env python3
"""Fabrique `cities.tsv.gz` (lieux de la recherche, `src/search_query.rs`) depuis GeoNames `cities15000.txt`.

Données : GeoNames (https://www.geonames.org), licence CC BY 4.0. Une ligne par nom : `nom sans accents<TAB>lat<TAB>lon`,
de la ville la plus peuplée à la moins peuplée ; les grandes villes (100 000 habitants et plus) ont aussi leurs autres noms
(« londres », « bruxelles »…).

    curl -O https://download.geonames.org/export/dump/cities15000.zip && unzip cities15000.zip
    python3 src/data/cities_build.py cities15000.txt src/data/cities.tsv.gz
"""
import gzip
import re
import sys
import unicodedata

src, dst = sys.argv[1], sys.argv[2]


def fold(s):
    s = unicodedata.normalize('NFKD', s.lower())
    s = ''.join(c for c in s if not unicodedata.combining(c))
    return re.sub(r"[-'’.]", ' ', s).strip()


ok = re.compile(r'^[a-z0-9 ]{2,40}$')
rows = []
for line in open(src, encoding='utf-8'):
    f = line.rstrip('\n').split('\t')
    pop = int(f[14] or 0)
    names = {fold(f[1]), fold(f[2])} | ({fold(a) for a in f[3].split(',') if a} if pop >= 100000 else set())
    for n in names:
        n = re.sub(r'\s+', ' ', n)
        if ok.match(n):
            rows.append((pop, n, round(float(f[4]), 2), round(float(f[5]), 2)))
rows = list({(n, la, lo): (p, n, la, lo) for p, n, la, lo in rows}.values())
rows.sort(key=lambda r: (-r[0], r[1]))
data = ('\n'.join(f'{n}\t{la}\t{lo}' for _, n, la, lo in rows) + '\n').encode()
open(dst, 'wb').write(gzip.compress(data, 9, mtime=0))
print(len(rows), 'noms', len(data), 'octets')
