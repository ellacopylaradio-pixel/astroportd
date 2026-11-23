"""Test navigateur de l'écran Partages (Ma station › Réseau › Partages, m-services.js) sur la station simulée.

Ce qu'on m'ouvre (Utiliser / Fermer), ce que je prête (qui peut l'utiliser, amis ajoutés), l'ajout d'un service choisi parmi
les ports TCP qui écoutent sur la machine ou saisi à la main, et l'entraide (accepter puis ouvrir l'accès d'un bouton, sans
ligne de commande). Ordinateur et téléphone (400 px), clair et sombre ; aucune erreur de console tolérée.

    ~/.astro/bin/python tests/browser/smoke_services.py [dossier-des-captures]
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
    br = pw.chromium.launch(); ctx = br.new_context(viewport=dict(width=1280, height=900), service_workers='block'); page = ctx.new_page()
    page.on('console', lambda m: errors.append(m.text) if m.type == 'error' else None); page.on('pageerror', lambda e: errors.append(str(e)))
    ctx.add_init_script("localStorage.setItem('basic', btoa('alice@example.org:jeton')); localStorage.setItem('space', JSON.stringify('station'))")
    page.goto(f'http://127.0.0.1:{port}/#/services'); page.wait_for_selector('.sv-sum'); page.wait_for_timeout(300)
    segs = page.locator('.strip nav.segs a').all_inner_texts()
    check(segs == ['Essaim', 'Partages', 'Constellations', 'Écluses'], f'Réseau : sous-onglets {segs}')
    heads = page.locator('.sv-head h2').all_inner_texts()
    check(heads == ["S'entraider", 'Chez mes amis', 'Sur cette machine'], f'une demande d\'aide en attente passe en tête : {heads}')
    page.screenshot(path=str(OUT / 'services-desktop.png'), full_page=True)
    mine = page.locator('.sv-row', has_text='Station de Marie').filter(has_text='Ollama').count() + page.locator('.sv-row', has_text='Marie').filter(has_text='Ollama').count()
    check(page.locator('.sv-row', has_text='Paul').filter(has_text='Jellyfin').count() == 1, 'un ami hors de l\'annuaire montre quand même ce qu\'il partage')
    check(page.locator('.sv-row', has_text='12D3') .count() == 0 and mine == 2, 'pas de doublon pour Marie (annuaire + Capitaines)')

    # Entraide : accepter, puis ouvrir l'accès d'un bouton (plus de commande « astroportd tunnel connect » à recopier).
    page.get_by_role('button', name="Accepter d'aider").click(); page.wait_for_selector("text=Ouvrir l'accès")
    check('astroportd tunnel' not in page.inner_text('main'), 'aucune ligne de commande à recopier')
    page.get_by_role('button', name="Ouvrir l'accès").click(); page.wait_for_selector('.sv-reach code')
    check('ssh -p' in page.inner_text('.sv-card .sv-reach'), 'l\'adresse SSH à utiliser est affichée avec « Copier »')
    check(STATE.get('last_connect', {}).get('peer') == '12D3KooWAmiLyon' and STATE['last_connect'].get('hours') == 2, f"accès vers la station de l'ami, pour la durée demandée : {STATE.get('last_connect')}")

    # Chez mes amis : Utiliser → adresse + Fermer.
    row = page.locator('.sv-row', has_text='Atelier Nantes').filter(has_text='Ollama').first
    row.get_by_role('button', name='Utiliser').click(); page.wait_for_timeout(300)
    row = page.locator('.sv-row', has_text='Atelier Nantes').filter(has_text='Ollama').first
    check('http://127.0.0.1:11434' in row.inner_text(), 'Ollama de l\'Atelier : adresse locale affichée')
    check(page.locator('.sv-sum').inner_text().startswith('2 utilisé(s)'), 'le résumé compte les accès ouverts : ' + page.inner_text('.sv-sum'))

    # Sur cette machine : les ports qui écoutent sont listés ; on en partage un d'un clic, ouvert à personne d'abord.
    page.locator('summary', has_text='Partager un service de cette machine').click()
    cands = page.locator('.sv-fold[open] .sv-list .sv-row .sv-main strong').all_inner_texts()
    check(cands == ['Bureau à distance (VNC)', 'Films et musique (Jellyfin)', 'navidrome'], f'ports à l\'écoute proposés : {cands}')
    check('port 7070' in page.inner_text('.sv-fold[open]'), 'le numéro de port est affiché')
    page.locator('.sv-fold[open] .sv-row', has_text='Jellyfin').get_by_role('button', name='Partager').click(); page.wait_for_timeout(300)
    jf = next(s for s in TUNNELS['services'] if s['name'] == 'jellyfin')
    check(jf['target'] == '127.0.0.1:8096' and jf['mode'] == 'listed' and not jf['grants'], 'Jellyfin partagé, ouvert à personne tant qu\'aucun ami n\'est ajouté')
    # Saisie à la main : un simple numéro de port suffit.
    page.fill('input[aria-label="Port ou adresse"]', '9000'); page.fill('input[aria-label="Nom du service"]', 'Mon Wiki')
    page.locator('.sv-fold[open] .sv-form button', has_text='Partager').click(); page.wait_for_timeout(300)
    wk = next((s for s in TUNNELS['services'] if s['name'] == 'mon-wiki'), None)
    check(wk is not None and wk['target'] == '127.0.0.1:9000', f'saisie « 9000 » → 127.0.0.1:9000, nom normalisé : {wk and wk["name"]}')

    # Qui peut l'utiliser : choisir « Tous mes Capitaines amis », puis ajouter un ami pour 24 h.
    card = page.locator('.sv-card', has_text='Films et musique (Jellyfin)')
    card.locator('select').first.select_option('friends'); page.wait_for_timeout(300)
    check(jf['mode'] == 'friends', 'mode changé d\'un choix')
    card = page.locator('.sv-card', has_text='Films et musique (Jellyfin)')
    card.locator('summary', has_text='Ajouter un ami').click()
    card.locator('select[aria-label="Capitaine ami"]').select_option('12D3KooWAmiNantes'); card.locator('select[aria-label="Durée"]').select_option('24')
    card.get_by_role('button', name='Autoriser').click(); page.wait_for_timeout(300)
    check([g['label'] for g in jf['grants']] == ['Atelier Nantes'], 'ami ajouté')
    check('Atelier Nantes' in page.locator('.sv-card', has_text='Films et musique (Jellyfin)').locator('.sv-chip').first.inner_text(), 'l\'ami apparaît en pastille, avec sa durée et ×')

    # Thème sombre et téléphone : rien ne déborde.
    page.emulate_media(color_scheme='dark'); page.screenshot(path=str(OUT / 'services-dark.png'), full_page=True)
    page.set_viewport_size(dict(width=400, height=860)); page.wait_for_timeout(300)
    over = page.evaluate("document.documentElement.scrollWidth - document.documentElement.clientWidth")
    check(over <= 0, f'400 px : pas de défilement horizontal ({over} px)')
    # La bulle « ? » est petite à l'œil mais sa zone de toucher est agrandie à 48 px (::after), comme dans Mon compte.
    small = page.evaluate("[...document.querySelectorAll('main button, main select, main summary')].filter(e => e.offsetParent && !e.closest('.tip') && e.getBoundingClientRect().height < 32).map(e => e.textContent.trim().slice(0, 20))")
    check(not small, f'cibles tactiles ≥ 32 px : {small}')
    page.screenshot(path=str(OUT / 'services-mobile.png'), full_page=True)

    # Réglages a quitté Réseau pour Santé, et ne parle plus de tunnels.
    page.goto(f'http://127.0.0.1:{port}/#/options'); page.wait_for_selector('details.fold', state='attached'); page.wait_for_timeout(200)
    check(page.locator('.strip nav.segs a').all_inner_texts() == ['État', 'Réglages'], 'Santé : État · Réglages')
    check('Tunnels' not in page.inner_text('main') and page.locator('main a[href="#/services"]').count() == 1, 'Réglages renvoie vers Partages')
    page.goto(f'http://127.0.0.1:{port}/#/tunnels'); page.wait_for_selector('.sv-sum')
    check(page.url.endswith('#/services'), 'l\'ancienne adresse #/tunnels mène à Partages')
    br.close()

srv.shutdown()
check(not errors, f'console sans erreur : {errors[:3]}')
sys.exit(1 if failures else 0)
