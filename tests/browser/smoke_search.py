"""Test de fumée navigateur de la recherche unique de Fichiers : API simulée, fichiers statiques RÉELS de l'interface.

Une seule case sous Dossiers / Photos / Médiathèque, dont le texte grisé ne promet que ce que la station sait chercher ; ce que
la station a compris (personne, lieu, date) ; les plus proches d'abord ; Documents / Photos et vidéos / Films et musique ;
pastilles « T » et 👁, moment d'une vidéo sur sa vignette ; « Peut-être » quand rien n'est sûr ; Affiner replié ; « Copier le
texte » dans la visionneuse seulement quand un texte a été lu. Ordinateur et téléphone (400 px). Aucune erreur de console.

    ~/.astro/bin/python tests/browser/smoke_search.py [dossier-des-captures]
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

with sync_playwright() as pw:
    br = pw.chromium.launch()
    for width, tag in [(1280, 'desktop'), (400, 'mobile')]:
        ctx = br.new_context(viewport=dict(width=width, height=820), service_workers='block', permissions=['clipboard-read', 'clipboard-write']); page = ctx.new_page()
        page.on('console', lambda m: errors.append(m.text) if m.type == 'error' and 'simulé' not in m.text else None); page.on('pageerror', lambda e: errors.append(str(e))); page.on('response', lambda r: errors.append(f'{r.status} {r.url}') if r.status >= 400 else None)
        ctx.add_init_script("localStorage.setItem('basic', btoa('alice@example.org:jeton'))")
        box = page.locator('form.find input[type=search]')
        ph = lambda: box.get_attribute('placeholder') or ''
        noscroll = lambda what: check(not page.evaluate('document.documentElement.scrollWidth > document.documentElement.clientWidth'), f'{tag} : pas de défilement horizontal ({what})')
        # Petite station qui ne lit rien encore : la case est là dans les trois rubriques, et ne promet que nom, personnes, lieux, dates.
        READING.update(state='off', local=[], photos=None)
        for route in ('cloud', 'library', 'media'):
            page.goto(f'http://127.0.0.1:{port}/#/{route}'); page.wait_for_selector('form.find'); page.wait_for_timeout(250)
            check(box.is_visible(), f'{tag} : la case de recherche est dans {route}')
        check(ph() == 'Nom, description, Léa, Lyon, 2024…', f'{tag} : petite station : rien de promis sur le texte ni l’image ({ph()})')
        page.goto(f'http://127.0.0.1:{port}/#/find'); page.wait_for_selector('.fd-chips button')
        check(page.locator('.fd-chips button', has_text='facture').count() == 0 and page.locator('.fd-chips button', has_text='Lyon').count() == 1, f'{tag} : exemples adaptés (pas de « facture » sans lecture)')
        check(page.locator('a[href="#/account?tab=moi&card=reading"]').is_visible(), f'{tag} : l’aide invite à rendre le texte cherchable')

        # Station équipée : texte des documents et ce que montrent les photos.
        READING.update(state='local', local=['ocr', 'vision'], photos=dict(seen=3, waiting=0, more=False))
        page.goto(f'http://127.0.0.1:{port}/#/cloud'); page.reload(); page.wait_for_selector('form.find'); page.wait_for_timeout(300)
        check('texte' in ph() and 'ce que montre la photo' in ph(), f'{tag} : station équipée : le champ annonce le texte et l’image ({ph()})')
        box.fill('electricite lea'); box.press('Enter'); page.wait_for_selector('.fd-best')
        check(page.url.endswith('#/find?q=electricite+lea') and box.input_value() == 'electricite lea', f'{tag} : la recherche a son adresse et la case garde le texte')
        check('lea · personne reconnue' in page.inner_text('.fd-chips'), f'{tag} : ce que la station a compris est dit')
        best = page.locator('.fd-best')
        check(best.locator('.tile').count() == 2 and 'contrat-énergie.pdf' in best.inner_text(), f'{tag} : les plus proches d’abord, document compris')
        sec = lambda title: page.locator('section.card', has=page.locator('h3', has_text=title))
        check('· 1' in sec('Documents').inner_text() and 'échéance trimestrielle' in sec('Documents').inner_text() and '/Documents' in sec('Documents').inner_text(), f'{tag} : Documents : nom, dossier et extrait')
        check(sec('Photos et vidéos').locator('.tile').count() == 4, f'{tag} : Photos et vidéos en vignettes')
        check('Chihiro' in sec('Films et musique').inner_text() and 'voyage au pays' in sec('Films et musique').inner_text(), f'{tag} : Films et musique, trouvés par leur fiche')
        grid = sec('Photos et vidéos')
        check(grid.locator('.badge.txt', has_text='T').count() == 1 and grid.locator('.badge.txt', has_text='👁').count() == 2, f'{tag} : « T » pour le texte lu, 👁 pour ce que montre la photo')
        check(grid.locator('.badge', has_text='▶ 1:23').count() == 1, f'{tag} : le moment de la vidéo est sur sa vignette')
        check(page.locator('details.fd-refine').get_attribute('open') is None, f'{tag} : Affiner est replié')
        noscroll('résultats')
        page.screenshot(path=str(OUT / f'search-{tag}.png'), full_page=True)
        grid.locator('.tile:has(.badge.txt)').first.click(); page.wait_for_selector('#view[open]'); page.wait_for_timeout(400)
        copy = page.locator('#view-t button', has_text='Copier le texte')
        check(copy.count() == 1 and copy.is_visible(), f'{tag} : « Copier le texte » dans la visionneuse du scan')
        copy.click(); page.wait_for_timeout(200)
        check(page.evaluate('navigator.clipboard.readText()').startswith('FACTURE'), f'{tag} : le texte lu est dans le presse-papiers')
        page.click('#view-n'); page.wait_for_timeout(400)
        check(page.locator('#view-t button', has_text='Copier le texte').count() == 0 or not page.locator('#view-t button', has_text='Copier le texte').is_visible(), f'{tag} : pas de bouton pour une photo sans texte')
        page.click('#view-x')

        # Affiner : type et période, puis la recherche repart avec.
        page.locator('details.fd-refine summary').click(); page.select_option('details.fd-refine select', 'image')
        page.locator('details.fd-refine button', has_text='Appliquer').click(); page.wait_for_selector('.fd-best'); page.wait_for_timeout(200)
        check('kind=image' in page.url and STATE['searches'][-1].get('kind') == ['image'] and page.locator('details.fd-refine').get_attribute('open') is not None, f'{tag} : Affiner envoie le type et reste ouvert')

        # Rien de sûr : des pistes, jamais un « aucun résultat » sec.
        box.fill('zanzibar'); box.press('Enter'); page.wait_for_selector('text=Peut-être :')
        check('Rien de sûr pour « zanzibar »' in page.inner_text('main') and page.locator('section.card', has=page.locator('h3', has_text='Peut-être')).locator('.tile').count() == 1, f'{tag} : « Peut-être » avec les plus proches')
        check(STATE['searches'][-1].get('kind') == ['image'], f'{tag} : une nouvelle recherche garde les filtres choisis')
        noscroll('peut-être')
        page.screenshot(path=str(OUT / f'search-maybe-{tag}.png'), full_page=True)
        ctx.close()
    br.close()
srv.shutdown()
check(not errors, 'aucune erreur de console : ' + '; '.join(errors[:5]))
print(f'\n{len(failures)} échec(s)'); sys.exit(1 if failures else 0)
