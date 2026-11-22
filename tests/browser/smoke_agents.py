"""Test de fumée navigateur de Mes agents : entrée depuis Mon compte, demande en mode navigateur (écran diffusé, clic et
frappe rejoués, prêt, reprise de la main), demande d'accord simple, déclaration d'un agent (jeton montré une fois), retrait.
Ordinateur puis téléphone (400 px), thème sombre, anglais. Aucune erreur de console.

    ~/.astro/bin/python tests/browser/smoke_agents.py [dossier-des-captures]

Hors de la CI (Playwright et Chromium requis) : à lancer avant de publier une modification de m-agents.js.
"""
import sys
from pathlib import Path
from playwright.sync_api import sync_playwright
from mock_station import *  # noqa: F403 (données et gestionnaire simulés)

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

    page = open_page(1280, 860)
    page.goto(f'http://127.0.0.1:{port}/#/account?tab=appareils'); page.wait_for_selector('a[href="#/agents"]')
    check(page.locator('a[href="#/agents"]').is_visible(), 'Mon compte › Mes appareils mène à Mes agents')
    page.click('a[href="#/agents"]'); page.wait_for_selector('.ag-req')
    check(page.locator('.ag-req').count() == 2, 'deux demandes en attente')
    check('voudrait agir sur linkedin.com' in page.inner_text('.ag-reqs') and 'relever ma facture' in page.inner_text('.ag-reqs'), 'la demande dit qui, où et pourquoi')
    page.screenshot(path=str(OUT / 'agents-demandes.png'), full_page=True)

    # Mode navigateur : le membre accepte pour 30 minutes, le navigateur s'affiche, il s'y connecte.
    first = page.locator('.ag-req').nth(0)
    first.locator('summary', has_text='Connexion par un autre site').click(); first.locator('input.ag-type').fill('accounts.google.com')
    first.locator('select').select_option('1800'); first.get_by_role('button', name='Me connecter pour lui').click()
    page.wait_for_selector('img.ag-screen[src^="blob:"]', timeout=8000)
    check(posts('/api/v1/agents/decide')[-1] == dict(id='r1', approve=True, secs=1800, hosts=['accounts.google.com']), 'accord avec la durée et le site lié choisis : ' + str(posts('/api/v1/agents/decide')[-1:]))
    check('ne connaîtra pas votre mot de passe' in page.inner_text('.ag-reqs'), 'la phrase d\'honnêteté est montrée avant le prêt')
    box = page.locator('img.ag-screen').bounding_box()
    page.mouse.click(box['x'] + box['width'] * 0.5, box['y'] + box['height'] * 0.33); page.wait_for_timeout(300)
    clicks = [b for b in posts('/api/v1/agents/input') if b['type'] == 'click']
    check(clicks and abs(clicks[-1]['x'] - 0.5) < 0.02 and abs(clicks[-1]['y'] - 0.33) < 0.02, 'le clic est rejoué au bon endroit : ' + str(clicks[-1:]))
    page.fill('.ag-type', 'alice@example.org'); page.wait_for_timeout(700)
    texts = [b for b in posts('/api/v1/agents/input') if b['type'] == 'text']
    check(texts and texts[-1]['text'] == 'alice@example.org' and page.input_value('.ag-type') == '', 'la frappe part dans la page puis la case se vide')
    check(page.get_attribute('.ag-type', 'type') == 'password', 'ce qui est tapé est masqué par défaut')
    page.press('.ag-type', 'Enter'); page.wait_for_timeout(300)
    check(posts('/api/v1/agents/input')[-1] == dict(id='r1', type='key', key='Enter'), 'Entrée est rejouée')
    page.screenshot(path=str(OUT / 'agents-connexion.png'), full_page=True)

    # « C'est prêt » : l'agent agit, le membre voit le temps restant et peut reprendre la main.
    page.get_by_role('button', name="C'est prêt, lui confier").click(); page.wait_for_selector('.ag-live')
    live = page.inner_text('.ag-live')
    check('agit sur linkedin.com' in live and 'reste' in live and page.locator('img.ag-screen').count() == 0, 'prêté : bandeau « agit pour vous », temps restant, écran replié')
    page.get_by_role('button', name='Regarder').click(); page.wait_for_selector('img.ag-screen[src^="blob:"]', timeout=8000)
    check(True, 'Regarder rouvre l\'écran pendant que l\'agent travaille')
    # Demande d'accord simple.
    page.locator('.ag-req', has_text='impots.gouv.fr').get_by_role('button', name='Accorder').click(); page.wait_for_timeout(500)
    check('a accès à impots.gouv.fr' in page.inner_text('.ag-reqs'), 'accord simple : « a accès à »')
    page.screenshot(path=str(OUT / 'agents-prete.png'), full_page=True)
    page.locator('.ag-live', has_text='linkedin.com').get_by_role('button', name='Reprendre la main').click(); page.wait_for_timeout(500)
    check(posts('/api/v1/agents/stop')[-1] == dict(id='r1') and 'linkedin.com' not in page.inner_text('.ag-reqs'), 'Reprendre la main arrête le prêt')

    # Déclarer un agent : jeton montré une fois ; retirer un agent (confirmation en deux temps).
    page.fill('input[name=name]', 'Claude'); page.select_option('select[name=station]', 'tls'); page.get_by_role('button', name='Déclarer').click(); page.wait_for_selector('.ag-token code')
    check(page.inner_text('.ag-token code').startswith('ag1_') and 'plus jamais affiché' in page.inner_text('.ag-token'), 'le jeton est montré, avec l\'avertissement')
    check(posts('/api/v1/agents')[-1] == dict(name='Claude', group='Agents', max_secs=3600, station='tls'), 'déclaration sur une autre station : ' + str(posts('/api/v1/agents')[-1:]))
    page.locator('details.fold summary', has_text='Brancher un agent').click()
    check('/mcp' in page.inner_text('details.fold') and 'claude mcp add' in page.inner_text('details.fold') and 'mcp?station=12D3KooWIci' in page.inner_text('details.fold'), 'la façon de brancher un agent est expliquée, d\'ici et d\'une autre station')
    row = page.locator('ul.ag-list li', has_text='OpenClaw').last
    row.get_by_role('button', name='Retirer').click(); row.get_by_role('button', name='Confirmer ?').click(); page.wait_for_timeout(400)
    check(posts('/api/v1/agents/revoke')[-1] == dict(id='a1b2c3d4'), 'retrait après confirmation')
    page.screenshot(path=str(OUT / 'agents-declares.png'), full_page=True)

    # Téléphone, sombre, anglais : rien ne déborde, tout est traduit.
    AGENTS['requests'][:] = [dict(id='r3', agent='OpenClaw', site='linkedin.com', reason='', mode='navigateur', state='login', asked_at=1790000000, until=4102444800)]
    m = open_page(400, 860, lang='en', dark=True)
    m.goto(f'http://127.0.0.1:{port}/#/agents'); m.wait_for_selector('img.ag-screen[src^="blob:"]', timeout=8000)
    width = m.evaluate('document.scrollingElement.scrollWidth')
    check(width <= 400, f'à 400 px, aucune largeur ne déborde ({width} px)')
    txt = m.inner_text('main')
    check('Sign in to linkedin.com below' in txt and 'Ready, hand it over' in txt and 'My agents' in txt, 'écran traduit en anglais')
    small = m.evaluate("[...document.querySelectorAll('.ag-row button')].filter(b => b.offsetParent && (b.offsetHeight < 44 || b.offsetWidth < 44)).length")
    check(small == 0, f'boutons d\'au moins 44 px sous le pouce ({small} trop petits)')
    m.screenshot(path=str(OUT / 'agents-telephone-sombre-en.png'), full_page=True)
    br.close()

srv.shutdown()
check(not errors, 'aucune erreur de console : ' + '; '.join(errors[:5]))
print(f"\n{'ÉCHEC' if failures else 'RÉUSSI'} : {len(failures)} échec(s)")
sys.exit(1 if failures else 0)
