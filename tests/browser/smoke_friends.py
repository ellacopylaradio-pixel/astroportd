"""Test de fumée navigateur des « Services ouverts par la constellation » (Ma station › Réseau › Essaim).

Liste lisible (station, service en mots humains), « Ouvrir » chez une station précise ou chez la plus puissante, adresse locale
à utiliser avec « Copier », « Fermer », erreur compréhensible, station injoignable dite, état vide. Ordinateur et 400 px.

    ~/.astro/bin/python tests/browser/smoke_friends.py [dossier-des-captures]
"""
import copy
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
INITIAL = copy.deepcopy(ADMIN['route'])

with sync_playwright() as pw:
    br = pw.chromium.launch()
    for width, tag in [(1280, 'desktop'), (400, 'mobile')]:
        ADMIN['route'] = copy.deepcopy(INITIAL)
        ctx = br.new_context(viewport=dict(width=width, height=900), service_workers='block'); page = ctx.new_page()
        ctx.grant_permissions(['clipboard-read', 'clipboard-write'])
        page.on('console', lambda m: errors.append(m.text) if m.type == 'error' and 'simulé' not in m.text and '400' not in m.text and '404' not in m.text else None); page.on('pageerror', lambda e: errors.append(str(e)))
        ctx.add_init_script("localStorage.setItem('basic', btoa('alice@example.org:jeton'))")
        noscroll = lambda what: check(not page.evaluate('document.documentElement.scrollWidth > document.documentElement.clientWidth'), f'{tag} : pas de défilement horizontal ({what})')

        page.goto(f'http://127.0.0.1:{port}/#/mesh'); page.wait_for_selector('h2:has-text("Services ouverts par la constellation")')
        sec = page.locator('section', has=page.locator('h2', has_text='Services ouverts par la constellation'))
        txt = sec.inner_text()
        check('Station de Marie' in txt and 'Console à distance (SSH)' in txt and 'IA de discussion (Ollama)' in txt and 'Images par IA (ComfyUI)' in txt, f'{tag} : stations et services en mots humains')
        check('La Ferme' not in txt and 'Injoignables : Station éteinte' in txt, f'{tag} : station sans service masquée, station injoignable dite')
        check(sec.locator('ul button', has_text='Ouvrir').count() == 4, f'{tag} : un « Ouvrir » par service')
        noscroll('liste'); page.screenshot(path=str(OUT / f'friends-list-{tag}.png'), full_page=True)

        # Ouvrir le SSH de Marie : la commande à utiliser apparaît, à copier, avec « Fermer ».
        sec.locator('li', has_text='Console à distance').locator('button', has_text='Ouvrir').click()
        page.wait_for_selector('li.ms-svc:has-text("Console à distance") code')
        li = sec.locator('li', has_text='Console à distance')
        check('ssh -p 24003 <utilisateur>@127.0.0.1' in li.inner_text(), f'{tag} : commande SSH locale affichée')
        check('port' not in STATE.get('last_connect', {}), f'{tag} : la page laisse la station choisir le port (stable)')
        li.locator('button', has_text='Copier').click(); page.wait_for_timeout(200)
        check(page.evaluate('navigator.clipboard.readText()') == 'ssh -p 24003 <utilisateur>@127.0.0.1', f'{tag} : « Copier » copie la commande')
        check(li.locator('button', has_text='Fermer').bounding_box()['height'] >= 34, f'{tag} : bouton « Fermer » de la taille des autres boutons de l’écran')
        noscroll('ouvert'); page.screenshot(path=str(OUT / f'friends-open-{tag}.png'), full_page=True)

        # Au plus puissant : Ollama s'ouvre chez Nantes (80) sur le port habituel.
        sec.locator('p', has_text='Au plus puissant').locator('button', has_text='IA de discussion').click()
        page.wait_for_selector('li.ms-svc:has-text("Atelier Nantes") code')
        check('http://127.0.0.1:11434' in sec.locator('li', has_text='Atelier Nantes').filter(has_text='Ollama').inner_text(), f'{tag} : Ollama ouvert chez la plus puissante, port 11434')

        # Erreur compréhensible : la station refuse ComfyUI.
        sec.locator('li', has_text='Images par IA').locator('button', has_text='Ouvrir').click(); page.wait_for_timeout(400)
        check('Ouverture impossible : la station amie refuse ce flux' in page.inner_text('body'), f'{tag} : refus dit en clair')

        # Fermer.
        sec.locator('li', has_text='Console à distance').locator('button', has_text='Fermer').click()
        page.wait_for_selector('li.ms-svc:has-text("Console à distance") button:has-text("Ouvrir")')
        check(sec.locator('li', has_text='Console à distance').locator('code').count() == 0, f'{tag} : accès fermé, plus d’adresse')

        # État vide.
        ADMIN['route'] = dict(mesh=True, stations=[])
        page.goto(f'http://127.0.0.1:{port}/#/'); page.goto(f'http://127.0.0.1:{port}/#/mesh'); page.wait_for_selector('h2:has-text("Services ouverts par la constellation")')
        check('Aucune station de la constellation ne vous ouvre de service' in page.inner_text('body'), f'{tag} : état vide clair')
        page.screenshot(path=str(OUT / f'friends-empty-{tag}.png'), full_page=True)
        ctx.close()
    br.close()
srv.shutdown()
check(not errors, 'aucune erreur de console : ' + '; '.join(errors[:5]))
sys.exit(1 if failures else 0)
