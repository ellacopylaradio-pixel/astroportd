"""Test de fumée navigateur de la mise à jour depuis l'interface du Capitaine (Ma station › Détails techniques).

Nouvelle version annoncée, modèles proposés et plugins dits en mots simples, confirmation avant de lancer (la station
redémarre), journal qui défile pendant la mise à jour ; le cockpit signale la version dans « À faire ». Ordinateur et 400 px.

    ~/.astro/bin/python tests/browser/smoke_update.py [dossier-des-captures]
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

UPDATE = dict(current='1.7.0', latest='v1.8.0', newer=True, error=None, running=False, log=['Mise à jour terminée.'], can_run=True, command='curl … | bash',
              models=[dict(id='ppocrv6-small', label='PP-OCRv6 small', mb=33, installed=True, tasks=['ocr']),
                      dict(id='whisper-base', label='Whisper base multilingue (int8)', mb=80, installed=False, tasks=['speech'])],
              plugins=[dict(name='media', installed='0.2.0', available='0.3.0', state='maj'), dict(name='energy', installed=None, available='0.1.0', state='nouveau')])

with sync_playwright() as pw:
    br = pw.chromium.launch()
    for width, tag in [(1280, 'desktop'), (400, 'mobile')]:
        ctx = br.new_context(viewport=dict(width=width, height=900), service_workers='block'); page = ctx.new_page()
        page.on('console', lambda m: errors.append(m.text) if m.type == 'error' and 'simulé' not in m.text and '404' not in m.text else None); page.on('pageerror', lambda e: errors.append(str(e)))
        ctx.add_init_script("localStorage.setItem('basic', btoa('alice@example.org:jeton'))")
        noscroll = lambda what: check(not page.evaluate('document.documentElement.scrollWidth > document.documentElement.clientWidth'), f'{tag} : pas de défilement horizontal ({what})')
        STATE['dashboard'] = dict(station=dict(node='abc', peer_id='12D3', domain=None, version='1.7.0', uptime_secs=60, captain=True), relay=dict(events=1, last_event_at=None, db_bytes=1))
        STATE['update'] = dict(UPDATE); STATE['update_posts'] = []

        page.goto(f'http://127.0.0.1:{port}/#/machine'); page.wait_for_selector('text=Nouvelle version : v1.8.0')
        card = page.locator('section', has_text='Modèles proposés')
        txt = card.inner_text()
        check('(vous avez 1.7.0)' in txt, f'{tag} : version en place dite')
        check('Whisper base multilingue (int8) (≈ 80 Mo)' in txt, f'{tag} : modèle proposé avec sa taille')
        check('media 0.2.0 : mise à jour disponible' in txt and 'energy : nouveau' in txt, f'{tag} : plugins en mots simples ({txt})')
        check('null' not in txt and 'undefined' not in txt, f'{tag} : aucun « null »')
        noscroll('mise à jour'); page.screenshot(path=str(OUT / f'update-{tag}.png'), full_page=True)

        # Contrôle explicite : un premier clic demande confirmation et dit la conséquence ; rien n'est lancé.
        page.click('text=Mettre à jour vers v1.8.0')
        check('vos membres la perdent 1 à 3 minutes' in card.inner_text() and not STATE['update_posts'], f'{tag} : confirmation avant de lancer')
        page.click('text=Confirmer ?')
        page.wait_for_selector('text=Mise à jour en cours…')
        check(len(STATE['update_posts']) == 1, f'{tag} : une seule demande envoyée')
        check('empreinte SHA-256 vérifiée' in page.locator('pre.log').inner_text(), f'{tag} : le journal défile')
        noscroll('en cours'); page.screenshot(path=str(OUT / f'update-running-{tag}.png'), full_page=True)

        # À jour : pas de bouton « Mettre à jour vers », mais on peut revoir modèles et plugins.
        STATE['update'] = {**UPDATE, 'newer': False, 'latest': 'v1.7.0', 'models': UPDATE['models'][:1], 'plugins': [dict(name='media', installed='0.3.0', available='0.3.0', state='ajour')]}
        page.reload(); page.wait_for_selector('text=Version 1.7.0 : à jour')
        check(page.locator('text=Vérifier modèles et plugins').count() == 1, f'{tag} : vérification possible quand tout est à jour')

        # Sans systemd : la commande à taper sur la machine, pas de bouton.
        STATE['update'] = {**UPDATE, 'can_run': False}
        page.reload(); page.wait_for_selector('text=Sur la machine :')
        check(page.locator('text=Mettre à jour vers').count() == 0, f'{tag} : pas de bouton sans systemd')
        STATE.pop('dashboard'); STATE.pop('update')
        ctx.close()
    br.close()
srv.shutdown()
check(not errors, f'aucune erreur de console ({errors[:3]})')
print('ÉCHEC' if failures else 'tout est vert'); sys.exit(1 if failures else 0)
