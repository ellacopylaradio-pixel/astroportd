"""Test de fumée navigateur de la fiche plante (#/plante) : API simulée, fichiers statiques RÉELS de l'interface.

Depuis la visionneuse, « 🌿 Fiche plante » ; la fiche dit toujours qu'une reconnaissance peut se tromper ; sans clé de la
coopérative, on écrit soi-même le nom ; avec clé, « Reconnaître la plante » propose des espèces (la plus probable retenue,
⚠ sur une plante toxique) et préremplit depuis Wikipédia, source citée ; tout reste modifiable et « Enregistrer » envoie ce que
le membre a écrit. Ordinateur et téléphone (400 px), aucun défilement horizontal, aucune erreur de console.

    ~/.astro/bin/python tests/browser/smoke_plant.py [dossier-des-captures]
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
        ctx = br.new_context(viewport=dict(width=width, height=900), service_workers='block'); page = ctx.new_page()
        page.on('console', lambda m: errors.append(m.text) if m.type == 'error' and 'simulé' not in m.text else None); page.on('pageerror', lambda e: errors.append(str(e))); page.on('response', lambda r: errors.append(f'{r.status} {r.url}') if r.status >= 400 else None)
        ctx.add_init_script("localStorage.setItem('basic', btoa('alice@example.org:jeton'))")
        noscroll = lambda what: check(not page.evaluate('document.documentElement.scrollWidth > document.documentElement.clientWidth'), f'{tag} : pas de défilement horizontal ({what})')

        # La visionneuse mène à la fiche.
        READING.update(state='local', local=['ocr', 'vision'], photos=dict(seen=3, waiting=0, more=False))
        page.goto(f'http://127.0.0.1:{port}/#/find?q=electricite+lea'); page.wait_for_selector('.fd-best')
        page.locator('.tile[data-path="/Scans/scan001.jpg"]').first.click(); page.wait_for_selector('#view[open]'); page.wait_for_timeout(300)
        link = page.locator('#view a', has_text='Fiche plante')
        check(link.count() == 1, f'{tag} : la visionneuse propose « 🌿 Fiche plante »')
        link.click(); page.wait_for_selector('.pl-warn')
        check('#/plante?path=' in page.url and not page.locator('#view[open]').count(), f'{tag} : la fiche s’ouvre, la visionneuse se ferme')

        # Sans clé : on écrit le nom soi-même.
        PLANT.update(available=False, fiche=None)
        page.goto(f'http://127.0.0.1:{port}/#/plante?path=%2FPhotos%2Fail.jpg'); page.reload(); page.wait_for_selector('.pl-warn')
        check('ne consommez jamais' in page.inner_text('.pl-warn'), f'{tag} : l’avertissement est toujours là')
        check(page.locator('button', has_text='Reconnaître la plante').count() == 0 and 'n’est pas activée' in page.inner_text('main'), f'{tag} : sans clé, pas de bouton, on écrit le nom')
        page.fill('#pl-sci', 'Allium ursinum'); page.locator('button', has_text='Compléter depuis Wikipédia').click(); page.wait_for_selector('text=Prérempli depuis Wikipédia')
        check('pesto' in page.input_value('#pl-edible') and page.locator('a[href="https://fr.wikipedia.org/wiki/Ail_des_ours"]').count() == 1, f'{tag} : Wikipédia remplit les rubriques vides, source citée')

        # Avec clé : reconnaissance, propositions, choix, notes, enregistrement.
        PLANT.update(available=True, fiche=None)
        page.reload(); page.wait_for_selector('.pl-warn')
        page.locator('button', has_text='Reconnaître la plante').click(); page.wait_for_selector('.pl-prop')
        check(page.locator('.pl-prop').count() == 2 and page.locator('.pl-prop input').first.is_checked(), f'{tag} : propositions, la plus probable retenue')
        check(page.locator('.pl-prop .pl-bad').count() == 1, f'{tag} : ⚠ sur la plante toxique (muguet)')
        check(page.inner_text('h2').strip().endswith('Ail des ours'), f'{tag} : le titre prend le nom commun')
        page.fill('#pl-notes', 'Cueillie au bois de la Garenne'); page.fill('#pl-edible', 'Oui, les feuilles')
        page.locator('button', has_text='Enregistrer la fiche').click(); page.wait_for_timeout(400)
        sent = [b for p, b in STATE['posts'] if p == '/api/v1/ucloud/plant/fiche'][-1]['fiche']
        check(sent['notes'] == 'Cueillie au bois de la Garenne' and sent['edible'] == 'Oui, les feuilles' and sent['species']['scientific'] == 'Allium ursinum' and sent['wiki'] is False, f'{tag} : ce que le membre a écrit est envoyé')
        b = page.locator('.pl-grid textarea').first.bounding_box(); check(b and b['height'] >= 48, f'{tag} : champs au pouce')
        noscroll('fiche')
        page.screenshot(path=str(OUT / f'plant-{tag}.png'), full_page=True)
        ctx.close()
    br.close()
srv.shutdown()
errors = [e for e in errors if 'favicon' not in e]
check(not errors, 'aucune erreur de console ni réponse en échec : ' + '; '.join(errors[:5]))
sys.exit(1 if failures else 0)
