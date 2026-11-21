"""Test de fumée navigateur des souvenirs : « Il y a un an » sur l'accueil, Photos › Voyages, page d'une personne.
Ordinateur et 400 px, aucune erreur de console, aucun « null ».

    ~/.astro/bin/python tests/browser/smoke_souvenirs.py [dossier-des-captures]
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

T = 1720000000
ph = lambda i, t=T: dict(path=f'/Photos/Bretagne 2024/b{i}.jpg', mime='image/jpeg', size=10, mtime=t, taken_at=t, t=t, thumb=None, lat=48.39, lon=-4.49)
TRIP = dict(name='Bretagne 2024', **{'from': T}, to=T + 4 * 86400, days=4, photos=8, km=680, lat=48.39, lon=-4.49, covers=[ph(i) for i in range(6)], people=[dict(name='Léa', photos=2), dict(name='Paul', photos=1)], bbox=[48, -5, 49, -4])
TRIP2 = dict(TRIP, name=None, **{'from': T - 90 * 86400}, to=T - 88 * 86400, days=3, photos=12, km=1200, people=[])

with sync_playwright() as pw:
    br = pw.chromium.launch()
    for width, tag in [(1280, 'desktop'), (400, 'mobile')]:
        ctx = br.new_context(viewport=dict(width=width, height=900), service_workers='block'); page = ctx.new_page()
        page.on('console', lambda m: errors.append(m.text) if m.type == 'error' and 'simulé' not in m.text and '404' not in m.text else None); page.on('pageerror', lambda e: errors.append(str(e)))
        ctx.add_init_script("localStorage.setItem('basic', btoa('alice@example.org:jeton'))")
        noscroll = lambda what: check(not page.evaluate('document.documentElement.scrollWidth > document.documentElement.clientWidth'), f'{tag} : pas de défilement horizontal ({what})')
        clean = lambda what: check('null' not in page.inner_text('main') and 'undefined' not in page.inner_text('main') and 'NaN' not in page.inner_text('main'), f'{tag} : aucun null/undefined/NaN ({what})')

        # Accueil : « Il y a 1 an », 4 vignettes au plus, liens vers les souvenirs et les voyages.
        STATE['memories'] = dict(memories=[dict(year=2025, ago=1, items=[ph(i) for i in range(7)])])
        page.goto(f'http://127.0.0.1:{port}/#/'); page.wait_for_selector('.hm-mem')
        card = page.locator('.hm-mem')
        check('Il y a 1 an' in card.inner_text() and '7 photo(s) ce jour-là' in card.inner_text(), f'{tag} : carte « Il y a 1 an »')
        check(card.locator('.tile').count() == 4, f'{tag} : 4 vignettes ({card.locator(".tile").count()})')
        check(card.locator('a[href="#/library?tab=trips"]').count() == 1, f'{tag} : lien vers les voyages')
        clean('accueil'); noscroll('accueil'); page.screenshot(path=str(OUT / f'souvenirs-home-{tag}.png'), full_page=True)
        # Sans souvenir ce jour-là : pas de carte vide.
        STATE['memories'] = dict(memories=[]); page.reload(); page.wait_for_selector('.hm-tiles')
        check(page.locator('.hm-mem').count() == 0, f'{tag} : rien ce jour-là, pas de carte')

        # Photos › Voyages.
        STATE['trips'] = dict(trips=[TRIP, TRIP2], home=[43.75, 1.25])
        page.goto(f'http://127.0.0.1:{port}/#/library?tab=trips'); page.wait_for_selector('.trip')
        txt = page.locator('main').inner_text()
        check('Bretagne 2024' in txt and '4 jour(s) · 8 photo(s) · à 680 km de chez vous' in txt, f'{tag} : voyage nommé par son dossier')
        check('Voyage du' in txt, f'{tag} : voyage sans dossier parlant : nommé par ses dates')
        check(page.locator('a[href="#/person?n=L%C3%A9a"]').count() == 1, f'{tag} : les personnes mènent à leur page')
        check(page.locator('a[href^="#/find?from=2024-07-03&to=2024-07-07"]').count() == 1, f'{tag} : « toutes les photos » ouvre la recherche sur ces dates')
        clean('voyages'); noscroll('voyages'); page.screenshot(path=str(OUT / f'souvenirs-trips-{tag}.png'), full_page=True)

        # Page d'une personne.
        STATE['person'] = dict(name='Léa', photos=42, first=T - 3 * 365 * 86400, last=T, items=[ph(i) for i in range(8)], years=[dict(year=2022, photos=5), dict(year=2023, photos=12), dict(year=2024, photos=25)], **{'with': [dict(name='Paul', photos=9)]}, trips=[TRIP])
        page.click('a[href="#/person?n=L%C3%A9a"]'); page.wait_for_selector('text=Au fil des années')
        txt = page.locator('main').inner_text()
        check('42 photo(s)' in txt and 'Paul · 9' in txt and 'Voyages ensemble' in txt, f'{tag} : page de Léa')
        check(page.locator('.years meter').count() == 3, f'{tag} : une jauge par année')
        check(page.locator('a[href="#/find?q=L%C3%A9a"]').count() == 1, f'{tag} : toutes ses photos par la recherche')
        clean('personne'); noscroll('personne'); page.screenshot(path=str(OUT / f'souvenirs-person-{tag}.png'), full_page=True)
        STATE['person'] = dict(name='Zoé', photos=0, items=[], years=[], trips=[])
        page.goto(f'http://127.0.0.1:{port}/#/person?n=Zo%C3%A9'); page.wait_for_selector('text=Aucune photo de Zoé')
        check(page.locator('a[href="#/people/tri"]').count() >= 1, f'{tag} : personne sans photo : on dit comment faire')
        for k in ('memories', 'trips', 'person'): STATE.pop(k, None)
        ctx.close()
    br.close()
srv.shutdown()
check(not errors, f'aucune erreur de console ({errors[:3]})')
print('ÉCHEC' if failures else 'tout est vert'); sys.exit(1 if failures else 0)
