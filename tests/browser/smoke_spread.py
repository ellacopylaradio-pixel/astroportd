"""Test de fumée navigateur de la répartition des données (Moi › Sauvegarde › Où sont vos données) : anneau par zone (privé, partagé,
Public, Coffre, visages/messages/mémoire, index, carnet), jauge « à l'abri chez vos amis », jauges de ce qu'on garde pour
eux. Sans gardien : on le dit et on mène au choix d'un gardien. Ordinateur et 400 px.

    ~/.astro/bin/python tests/browser/smoke_spread.py [dossier-des-captures]
"""
import sys
from pathlib import Path
from playwright.sync_api import sync_playwright
from mock_station import *  # noqa: F403

OUT = Path(sys.argv[1] if len(sys.argv) > 1 else '.')
srv, port = start()
errors, failures = [], []
def check(cond, msg):
    print(('OK   ' if cond else 'FAIL ') + msg)
    if not cond: failures.append(msg)

Z = lambda b: dict(bytes=b, files=1)
SPREAD = dict(total=2_000_000_000, safe=1_500_000_000, zones=dict(prive=Z(1_400_000_000), partage=Z(300_000_000), public=Z(120_000_000), coffre=Z(80_000_000), souvenirs=Z(60_000_000), recherche=Z(40_000_000)),
              guardians=[dict(hex='bb' * 32, name='Léa Martin', bytes=900_000_000), dict(hex='cc' * 32, name='Paul', bytes=700_000_000)],
              hosted=[dict(hex='bb' * 32, name='Léa Martin', bytes=3_000_000_000, quota=5_000_000_000), dict(hex='dd' * 32, name='Zoé', bytes=10_000_000, quota=None)], hosted_total=3_010_000_000)

with sync_playwright() as pw:
    br = pw.chromium.launch()
    for width, tag in [(1280, 'desktop'), (400, 'mobile')]:
        ctx = br.new_context(viewport=dict(width=width, height=900), service_workers='block'); page = ctx.new_page()
        page.on('console', lambda m: errors.append(m.text) if m.type == 'error' and 'simulé' not in m.text and '404' not in m.text else None); page.on('pageerror', lambda e: errors.append(str(e)))
        ctx.add_init_script("localStorage.setItem('basic', btoa('alice@example.org:jeton'))")
        noscroll = lambda what: check(not page.evaluate('document.documentElement.scrollWidth > document.documentElement.clientWidth'), f'{tag} : pas de défilement horizontal ({what})')
        STATE['spread'] = SPREAD
        page.goto(f'http://127.0.0.1:{port}/#/backup'); page.wait_for_selector('text=Où sont vos données')
        txt = page.locator('main').inner_text()
        check('🗝️ Coffre' in txt and '🫂 Visages, messages et mémoire' in txt and '🔗 Partagé' in txt, f'{tag} : légende des zones')
        check(page.locator('.ac-z-coffre').count() >= 2, f'{tag} : la zone Coffre a sa part d’anneau et sa pastille')
        check('75 % de vos données' in txt and 'Léa Martin garde' in txt, f'{tag} : jauge « à l’abri » et gardiens')
        check('Ce que vous gardez pour vos amis' in txt and 'accordés' in txt and 'Zoé' in txt, f'{tag} : jauges de ce qu’on garde pour les amis')
        check(page.locator('.ac-gauge meter').count() == 2, f'{tag} : une jauge « à l’abri » + une par volume accordé ({page.locator(".ac-gauge meter").count()})')
        check(not any(j in txt for j in ['CID', 'fragment', 'k-sur-n', 'null', 'undefined', 'NaN']), f'{tag} : ni jargon ni null')
        noscroll('répartition'); page.locator('text=Où sont vos données').scroll_into_view_if_needed(); page.screenshot(path=str(OUT / f'spread-{tag}.png'), full_page=True)
        STATE['spread'] = dict(SPREAD, safe=0, guardians=[], hosted=[], hosted_total=0)
        page.reload(); page.wait_for_selector('text=Où sont vos données')
        check(page.locator('.warn', has_text='Aucun ami ne garde encore vos données').count() == 1 and page.locator('#ac-guard, #bk-friend').count() >= 1, f'{tag} : sans gardien, l’alerte est à côté de son choix')
        check('0 % à l’abri' in page.inner_text('main'), f'{tag} : le bilan dit la part des fichiers à l’abri')
        STATE.pop('spread')
        ctx.close()
    br.close()
srv.shutdown()
check(not errors, f'aucune erreur de console ({errors[:3]})')
print('ÉCHEC' if failures else 'tout est vert'); sys.exit(1 if failures else 0)
