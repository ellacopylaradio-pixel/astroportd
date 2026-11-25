"""Renvoi : le corpus d'essai de la recherche vit désormais dans le plugin facultatif `corpus` d'astroportd-plugins
(`corpus/bin/fabriquer_corpus.py`), que le Capitaine installe avec `install-plugins.sh corpus`. Une seule copie, pour qu'elle
ne diverge pas. Ce renvoi lance cette copie avec les mêmes arguments :

    ~/.astro/bin/python tests/corpus/fabriquer_corpus.py [dossier] [--envoyer /Essais/Recherche]

Elle est cherchée dans $ASTRO_PLUGINS_DIR, puis ~/.zen/astroportd-plugins (dépôt cloné par install-plugins.sh), puis à côté
de ce dépôt (../astroportd-plugins). Le jeton reste dans ASTRO_TOKEN, jamais en argument.
"""
import os
import sys
from pathlib import Path

HERE = Path(__file__).resolve()
SPOTS = [Path(p) for p in (os.environ.get('ASTRO_PLUGINS_DIR'),) if p] + [Path.home() / '.zen/astroportd-plugins', HERE.parents[3] / 'astroportd-plugins']
for root in SPOTS:
    script = root / 'corpus' / 'bin' / 'fabriquer_corpus.py'
    if script.is_file():
        os.execv(sys.executable, [sys.executable, str(script), *sys.argv[1:]])
sys.exit('le corpus est fourni par le plugin « corpus » : installez-le avec install-plugins.sh corpus '
         '(https://github.com/papiche/astroportd-plugins), ou clonez astroportd-plugins à côté de ce dépôt')
