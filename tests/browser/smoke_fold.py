"""Pli des sections (m-fold.js) sur la station simulée : une carte se plie d'un geste sur son titre (souris ou clavier), reste
pliée après un rechargement et après le re-rendu périodique de l'écran ; un <details> garde l'état choisi par le membre ;
les cartes sans titre et les dialogues ne sont pas touchés. Aucune erreur de console tolérée.

    ~/.astro/bin/python tests/browser/smoke_fold.py [dossier-des-captures]
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

FOLDED = "[...document.querySelectorAll('main section.card.folded')].map(s => s.querySelector('.fold-t').textContent.trim())"
with sync_playwright() as pw:
    br = pw.chromium.launch()
    for w in (1280, 400):
        ctx = br.new_context(viewport=dict(width=w, height=900), service_workers='block'); page = ctx.new_page()
        page.on('console', lambda m: errors.append(m.text) if m.type == 'error' else None); page.on('pageerror', lambda e: errors.append(str(e)))
        ctx.add_init_script("localStorage.setItem('basic', btoa('alice@example.org:jeton'))")
        page.clock.install()
        page.goto(f'http://127.0.0.1:{port}/#/money'); page.wait_for_selector('main .fold-t'); page.wait_for_timeout(300)
        btn = page.locator('main section.card .fold-t', has_text='Durée couverte')
        check(btn.get_attribute('aria-expanded') == 'true', f'{w} : la carte est dépliée par défaut, son titre est un bouton')
        box = btn.bounding_box(); check(box['height'] >= 48, f'{w} : zone de toucher du titre ≥ 48 px ({box["height"]:.0f})')
        btn.click(); page.wait_for_timeout(200)
        check(page.evaluate(FOLDED) == ['Durée couverte'], f'{w} : un clic plie la carte : {page.evaluate(FOLDED)}')
        check(page.locator('main section.card.folded meter').is_hidden(), f'{w} : le contenu plié est caché')
        page.screenshot(path=str(OUT / f'fold-{w}.png'), full_page=True)
        # Re-rendu périodique de Mes Ẑen (toutes les 30 s : replaceChildren de tout l'écran).
        page.clock.fast_forward(31000); page.wait_for_timeout(500)
        check(page.evaluate(FOLDED) == ['Durée couverte'], f'{w} : toujours pliée après le re-rendu de l’écran')
        page.reload(); page.wait_for_selector('main .fold-t'); page.wait_for_timeout(300)
        check(page.evaluate(FOLDED) == ['Durée couverte'], f'{w} : toujours pliée après rechargement')
        # Clavier : Entrée sur le titre déplie.
        page.locator('main section.card .fold-t', has_text='Durée couverte').focus(); page.keyboard.press('Enter'); page.wait_for_timeout(200)
        check(page.evaluate(FOLDED) == [], f'{w} : Entrée déplie la carte')
        # Un autre écran ne partage pas l'état (clé = écran + titre).
        page.goto(f'http://127.0.0.1:{port}/#/options'); page.wait_for_selector('main details.fold'); page.wait_for_timeout(300)
        d = page.locator('main details.fold', has_text='Reconnaissance des visages')
        check(not d.evaluate('d => d.open'), f'{w} : un <details> replié par l’écran le reste par défaut')
        d.locator('summary').click(); page.wait_for_timeout(200)
        page.reload(); page.wait_for_selector('main details.fold'); page.wait_for_timeout(300)
        check(page.locator('main details.fold', has_text='Reconnaissance des visages').evaluate('d => d.open'), f'{w} : le <details> ouvert par le membre l’est encore après rechargement')
        check(page.locator('dialog .fold-t').count() == 0, f'{w} : aucun dialogue n’est plié')
        ctx.close()
    br.close()
check(not errors, f'aucune erreur de console : {errors[:3]}')
sys.exit(1 if failures else 0)
