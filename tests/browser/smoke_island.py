"""Test de fumée navigateur de l'île « Ğ1 en différé » (m-island.js, m-credit.js) sur la station simulée, à 400 px et sur ordinateur.

    ~/.astro/bin/python tests/browser/smoke_island.py [dossier-des-captures]

Vérifie : le membre voit que le réseau Ğ1 est coupé, ce qu'il peut encore promettre, ses promesses envoyées et reçues (une station
inconnue est signalée) ; « Envoyer des Ẑen » garde le même geste mais avertit avant, dans le récapitulatif et après ; le Capitaine
voit l'état, la panne, les stations voisines en île, règle l'ouverture automatique, ferme et rouvre l'île. Aucune erreur de console.
"""
import sys
from pathlib import Path
from playwright.sync_api import sync_playwright
from mock_station import *  # noqa: F403 (données et gestionnaire simulés)

OUT = Path(sys.argv[1] if len(sys.argv) > 1 else '.')
srv, port = start()
failures, errors = [], []
def check(cond, msg):
    print(('OK   ' if cond else 'FAIL ') + msg)
    if not cond: failures.append(msg)

with sync_playwright() as pw:
    b = pw.chromium.launch()
    for name, vp in [('mobile', dict(width=400, height=860)), ('desktop', dict(width=1280, height=900))]:
        c = b.new_context(viewport=vp, timezone_id='Europe/Paris')
        c.add_init_script("localStorage.setItem('basic', btoa('alice@example.org:jeton'))")
        p = c.new_page()
        p.on('pageerror', lambda e: errors.append(str(e)))
        p.on('console', lambda m: m.type == 'error' and 'Service Worker' not in m.text and errors.append(m.text))
        STATE.pop('island', None); STATE['posts'].clear(); ISLAND_STATION.update(open=True, auto_after_secs=1800)

        # Membre : état de l'île et promesses.
        p.goto(f'http://127.0.0.1:{port}/#/island'); p.wait_for_selector('text=Promesses')
        txt = p.inner_text('main')
        check('Réseau Ğ1 coupé' in txt, f'{name} : le membre voit que le réseau Ğ1 est coupé')
        check('150.00' in txt and '15.00 Ğ1' in txt, f'{name} : ce qu\'il peut encore promettre, en Ẑen et en Ğ1')
        check('À Léa' in txt and 'De efefefef' in txt, f'{name} : promesse envoyée (nom du proche) et reçue')
        check('station inconnue' in txt, f'{name} : la promesse garantie par une station inconnue est signalée')
        check('null' not in txt, f'{name} : aucun « null » affiché')
        p.screenshot(path=str(OUT / f'island-member-{name}.png'), full_page=True)

        # Même geste « Envoyer des Ẑen », averti à chaque étape.
        p.click('main button:has-text("Envoyer des Ẑen")'); p.wait_for_selector('#credit .cr-form')
        check(p.is_visible('#credit >> text=vos envois deviennent des promesses'), f'{name} : l\'envoi avertit avant (île ouverte)')
        p.select_option('#cr-to', 'ab' * 32); p.fill('#cr-amount', '20'); p.click('#credit button[type=submit]')
        p.wait_for_selector('#credit h3')  # le titre du récapitulatif (pas le bouton « Voir le récapitulatif »)
        check(p.is_visible('#credit >> text=ceci est une promesse'), f'{name} : le récapitulatif dit que c\'est une promesse')
        check(p.is_visible('#credit >> text=Promis, réglé au retour du réseau'), f'{name} : « Promis » au lieu de « Débité »')
        p.screenshot(path=str(OUT / f'island-send-{name}.png'), full_page=True)
        p.click('#credit button:has-text("Envoyer 20 Ẑen")'); p.click('#credit button:has-text("Confirmer")')  # deux clics : armer, confirmer
        p.wait_for_selector('#credit >> text=réglée en Ğ1 au retour du réseau')
        check(any(x[0] == '/api/v1/me/zen/send' and x[1].get('confirm') and x[1].get('credit_id') for x in STATE['posts']), f'{name} : envoi confirmé, avec identifiant rejouable')
        p.keyboard.press('Escape')

        # Capitaine : état, voisins, ouverture automatique, fermeture.
        p.goto(f'http://127.0.0.1:{port}/#/ile'); p.wait_for_selector('text=Ouverture automatique')
        txt = p.inner_text('main')
        check('Île ouverte' in txt and 'Duniter injoignable' in txt, f'{name} : le Capitaine voit l\'île ouverte et la panne')
        check('Stations voisines en île' in txt and '4 membre(s), 3 promesse(s)' in txt, f'{name} : les stations voisines en île sont listées')
        check(p.input_value('#isl-auto') == '30', f'{name} : délai automatique affiché (30 min)')
        p.select_option('#isl-auto', ''); p.wait_for_timeout(300)
        check(('/api/v1/station/island/auto', {'minutes': None}) in STATE['posts'], f'{name} : « Jamais seule » envoyé')
        p.screenshot(path=str(OUT / f'island-captain-{name}.png'), full_page=True)
        p.click('main button:has-text("Fermer l\'île")'); p.click('main button:has-text("Confirmer")')
        p.wait_for_selector('text=Île fermée')
        check(p.is_visible('main button:has-text("Ouvrir l\'île maintenant")'), f'{name} : après fermeture, le Capitaine peut rouvrir')
        ISLAND_STATION['open'] = True
        overflow = p.evaluate('document.documentElement.scrollWidth > window.innerWidth')
        check(not overflow, f'{name} : pas de défilement horizontal')
        c.close()
    b.close()
srv.shutdown()
check(not errors, 'aucune erreur de console : ' + '; '.join(errors[:3]))
print(f'{len(failures)} échec(s)')
sys.exit(1 if failures else 0)
