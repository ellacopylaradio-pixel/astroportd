"""Test de fumée navigateur de Naviguer ensemble : lancement depuis « Partager » du téléphone (ami choisi dans le carnet, niveau
« tout faire pour moi »), invitation envoyée dans le chat, page affichée, pointeur de l'ami, niveaux, fin ; côté ami d'une autre
station : montrer, puis la main donnée (frappe et clic relayés avec la station de la session). Téléphone, sombre, anglais.

    ~/.astro/bin/python tests/browser/smoke_cosurf.py [dossier-des-captures]
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
posts = lambda path: [b for p_, b in STATE['posts'] if p_ == path]

with sync_playwright() as pw:
    br = pw.chromium.launch()
    def open_page(width, height, lang=None, dark=False):
        ctx = br.new_context(viewport=dict(width=width, height=height), service_workers='block', color_scheme='dark' if dark else 'light')
        ctx.add_init_script("localStorage.setItem('basic', btoa('alice@example.org:jeton'))" + (f";localStorage.setItem('lang', '{lang}')" if lang else ''))
        page = ctx.new_page()
        page.on('console', lambda m: errors.append(m.text) if m.type == 'error' else None); page.on('pageerror', lambda e: errors.append(str(e)))
        return page

    # La personne aidée : un lien partagé depuis le téléphone, l'ami choisi dans le carnet.
    page = open_page(1280, 900)
    page.goto(f'http://127.0.0.1:{port}/#/cosurf?url=https%3A%2F%2Fimpots.gouv.fr%2Fmon-espace'); page.wait_for_selector('form.cs-form')
    check(page.input_value('input[name=url]') == 'https://impots.gouv.fr/mon-espace', 'l\'adresse partagée est reprise')
    check(page.locator('fieldset.cs-pick input[type=checkbox]').count() == 1, 'seuls les amis réciproques avec une clé sont proposés')
    check(page.is_checked('input[name=level][value=drive]'), '« tout faire pour moi » est proposé d\'emblée')
    page.screenshot(path=str(OUT / 'cosurf-lancer.png'), full_page=True)
    page.get_by_role('button', name='Ouvrir la page et inviter').click(); page.wait_for_timeout(300)
    check('Choisissez au moins un ami' in page.inner_text('body'), 'sans ami choisi : on le dit')
    page.check('fieldset.cs-pick input[type=checkbox]'); page.get_by_role('button', name='Ouvrir la page et inviter').click()
    page.wait_for_selector('img.ag-screen[src^="blob:"]', timeout=8000)
    check(STATE['cosurf_created'][-1] == dict(url='https://impots.gouv.fr/mon-espace', guests=[dict(hex='ab' * 32, label='Léa Martin')], level='drive'), 'session : ' + str(STATE['cosurf_created'][-1:]))
    sent = posts('/api/v1/chat/send')[-1]
    check(sent['peer'] == 'ab' * 32 and '#/cosurf?id=c1&station=12D3KooWIci' in sent['text'] and 'impots.gouv.fr' in sent['text'], 'l\'invitation part dans le chat : ' + sent['text'])
    page.wait_for_selector('.sc-dot', timeout=5000)
    check('Léa Martin' in page.inner_text('.sc-dots'), 'le pointeur de l\'amie s\'affiche, nommé')
    check('Vous gardez toujours la main' in page.inner_text('main') and page.locator('.ag-type').is_visible(), 'la personne aidée garde la main et peut taper')
    page.select_option('.ag-list select', 'point'); page.wait_for_timeout(300)
    check(posts('/api/v1/cosurf/level')[-1] == dict(id='c1', guest='ab' * 32, level='point'), 'niveau changé en un geste')
    check(page.locator('.ag-list button[aria-label^="Appeler"]').count() == 1, 'la visio est à un bouton')
    page.screenshot(path=str(OUT / 'cosurf-proprietaire.png'), full_page=True)
    page.get_by_role('button', name='⏹ Terminer').click(); page.get_by_role('button', name='Confirmer ?').click(); page.wait_for_timeout(400)
    check(posts('/api/v1/cosurf/stop')[-1] == dict(id='c1'), 'Terminer arrête la navigation pour tous')

    # L'ami, hébergé ailleurs : il montre, puis on lui donne la main (relayé avec la station de la session).
    STATE['give_hand_at'] = 4
    g = open_page(1280, 900)
    g.goto(f'http://127.0.0.1:{port}/#/cosurf?id=c2&station=12D3KooWAmi'); g.wait_for_selector('img.ag-screen[src^="blob:"]', timeout=8000)
    check('Page de Marc' in g.inner_text('main') and not g.locator('.ag-type').is_visible(), 'l\'ami voit la page de Marc, sans case de frappe')
    box = g.locator('img.ag-screen').bounding_box(); g.mouse.click(box['x'] + box['width'] * 0.25, box['y'] + box['height'] * 0.75); g.wait_for_timeout(300)
    inp = STATE['cosurf_inputs'][-1]
    check(inp['type'] == 'point' and inp['station'] == '12D3KooWAmi' and abs(inp['x'] - 0.25) < 0.02, 'toucher montre un endroit, relayé vers la station de la session : ' + str(inp))
    g.wait_for_function("document.querySelector('.ag-type') && document.querySelector('.ag-type').offsetParent !== null", timeout=8000)
    check('Vous avez la main' in g.inner_text('.cs-status'), 'la main donnée est dite (et annoncée aux lecteurs d\'écran)')
    g.fill('.ag-type', 'mot-de-passe-de-Marc'); g.wait_for_timeout(700)
    check(STATE['cosurf_inputs'][-1] == dict(id='c2', station='12D3KooWAmi', type='text', text='mot-de-passe-de-Marc'), 'l\'ami tape à sa place, mot de passe compris')
    g.mouse.click(box['x'] + box['width'] * 0.5, box['y'] + box['height'] * 0.5); g.wait_for_timeout(300)
    check(STATE['cosurf_inputs'][-1]['type'] == 'click', 'puis clique')
    check(g.locator('button', has_text='Appeler Marc en visio').count() == 1 and g.locator('button', has_text='Quitter').count() == 1, 'l\'ami peut appeler en visio et quitter')
    g.screenshot(path=str(OUT / 'cosurf-ami.png'), full_page=True)

    m = open_page(400, 900, lang='en', dark=True)
    m.goto(f'http://127.0.0.1:{port}/#/cosurf?id=c1'); m.wait_for_selector('img.ag-screen[src^="blob:"]', timeout=8000)
    w = m.evaluate('document.scrollingElement.scrollWidth')
    check(w <= 400, f'à 400 px, rien ne déborde ({w} px)')
    check('You always keep control' in m.inner_text('main') and 'Browse together' in m.inner_text('body'), 'écran traduit en anglais')
    m.screenshot(path=str(OUT / 'cosurf-telephone-sombre-en.png'), full_page=True)
    br.close()

srv.shutdown()
check(not errors, 'aucune erreur de console : ' + '; '.join(errors[:5]))
print(f"\n{'ÉCHEC' if failures else 'RÉUSSI'} : {len(failures)} échec(s)")
sys.exit(1 if failures else 0)
