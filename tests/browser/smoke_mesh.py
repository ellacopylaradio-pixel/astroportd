"""Test navigateur de l'écran Essaim (Ma station › Réseau › Essaim, m-mesh.js + m-swarm.js) sur la station simulée.

Un seul encart pour les stations et leurs Capitaines : bilan (stations, en ligne, Capitaines amis, stockage libre annoncé),
fiche d'une station au clic (balise : matériel, stockage, places…), suivi de son Capitaine depuis la fiche, Capitaines dont
la station n'est pas vue rangés à part. Ordinateur et téléphone (400 px), clair et sombre ; aucune erreur de console tolérée.

    ~/.astro/bin/python tests/browser/smoke_mesh.py [dossier-des-captures]
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
    for name, vp, scheme in (('desktop', dict(width=1280, height=900), 'light'), ('mobile', dict(width=400, height=860), 'dark')):
        ctx = br.new_context(viewport=vp, color_scheme=scheme, service_workers='block'); page = ctx.new_page()
        page.on('console', lambda m: errors.append(m.text) if m.type == 'error' else None); page.on('pageerror', lambda e: errors.append(str(e)))
        ctx.add_init_script("localStorage.setItem('basic', btoa('alice@example.org:jeton')); localStorage.setItem('space', JSON.stringify('station'))")
        page.goto(f'http://127.0.0.1:{port}/#/mesh'); page.wait_for_selector('.ms-stats'); page.wait_for_timeout(300)
        main = page.inner_text('main')
        check('Aucun autre Capitaine' not in main and 'Ce que « Suivre » ouvre' not in main and "c'est sa station qui décide" not in main, f'{name} : le bla-bla est retiré')
        check(page.locator('h2', has_text='Capitaines').count() == 0, f'{name} : plus d\'encart « Capitaines » séparé')
        stats = page.inner_text('.ms-stats')
        check('4 To' in stats and 'libres sur 5 To (2 station(s))' in stats, f'{name} : stockage libre de l\'essaim = 812 Go + 3,2 To ; total 5 To : {stats!r}')
        cards = page.locator('.ms-node')
        check(cards.count() == 3, f'{name} : trois stations cliquables ({cards.count()})')
        check('Marie' in cards.nth(1).inner_text(), f'{name} : la station montre son Capitaine')
        check(page.locator('.ms-caps .ms-cap').count() == 2, f'{name} : Paul et Inès (station non vue) rangés sous « Autres Capitaines »')
        wide = page.evaluate('document.documentElement.scrollWidth > innerWidth + 1')
        check(not wide, f'{name} : pas de défilement horizontal')
        small = page.evaluate("[...document.querySelectorAll('.ms-node, .ms-cap button')].filter(b => b.getBoundingClientRect().height < 44).length")
        check(small == 0, f'{name} : zones de clic d\'au moins 44 px ({small} trop petites)')
        page.screenshot(path=str(OUT / f'mesh-{name}.png'), full_page=True)

        # Fiche de la station de Marie.
        cards.nth(1).click(); page.wait_for_selector('dialog.ms-sheet[open]')
        sheet = page.inner_text('dialog.ms-sheet')
        for want in ('Station de Marie', '16 cœurs', '64 Go de mémoire', '3,2 To sur 4 To', '12 / 50', 'astroportd/2.0.0', '12D3KooWAmiLyon', 'Marie'):
            check(want in sheet, f'{name} : la fiche dit « {want} »')
        page.screenshot(path=str(OUT / f'mesh-fiche-{name}.png'))
        b = page.locator('dialog.ms-sheet .ms-cap button'); b.click()
        check(b.inner_text() == 'Confirmer ?' and STATE.get('followed') is None, f'{name} : ne plus suivre se confirme d\'un second appui')
        b.click(); page.wait_for_selector('dialog.ms-sheet', state='hidden'); page.wait_for_timeout(400)
        check(STATE.get('followed') == {'unfollow': 'dd' * 32}, f'{name} : ne plus suivre Marie part au serveur ({STATE.get("followed")})')
        page.locator('.ms-node').nth(1).click(); page.wait_for_selector('dialog.ms-sheet[open]')
        page.locator('dialog.ms-sheet button', has_text='Suivre').click(); page.wait_for_timeout(400)
        check(STATE.get('followed') == {'follow': 'dd' * 32}, f'{name} : suivre à nouveau depuis la fiche ({STATE.get("followed")})')
        STATE.pop('followed', None)
        page.locator('.ms-node').nth(2).click(); page.wait_for_selector('dialog.ms-sheet[open]')
        check('pas encore connu ici' in page.inner_text('dialog.ms-sheet'), f'{name} : station sans Capitaine connu : on le dit')
        page.keyboard.press('Escape'); page.wait_for_timeout(200)
        check(page.locator('dialog.ms-sheet[open]').count() == 0, f'{name} : Échap ferme la fiche')
        ctx.close()
    br.close()

check(not errors, f'console sans erreur {errors[:3]}')
srv.shutdown()
sys.exit(1 if failures else 0)
