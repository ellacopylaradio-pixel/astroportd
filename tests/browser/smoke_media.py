"""Test de fumée navigateur de la Médiathèque : API simulée, fichiers statiques RÉELS de l'interface.

Grille par type, recherche sans accents, fiche d'un film, lecture vidéo dans sa fenêtre, album d'un mix, file d'écoute qui
continue quand on change d'écran, correction d'une fiche, ordinateur et téléphone (400 px). Aucune erreur de console tolérée.

    ~/.astro/bin/python tests/browser/smoke_media.py [dossier-des-captures]
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
    br = pw.chromium.launch(args=['--autoplay-policy=no-user-gesture-required'])
    ctx = br.new_context(viewport=dict(width=1280, height=820), service_workers='block'); page = ctx.new_page()
    page.on('console', lambda m: errors.append(m.text) if m.type == 'error' and 'simulé' not in m.text else None); page.on('pageerror', lambda e: errors.append(str(e))); page.on('response', lambda r: errors.append(f'{r.status} {r.url}') if r.status >= 400 else None)
    ctx.add_init_script("localStorage.setItem('basic', btoa('alice@example.org:jeton'))")
    page.goto(f'http://127.0.0.1:{port}/#/media'); page.wait_for_selector('.mm-card'); page.wait_for_timeout(500)
    check(page.locator('.mm-body section').count() == 3, 'trois rayons : films et vidéos, séries, musique')
    check(page.locator('.mm-card').count() == 4, f'quatre cartes (2 films, 1 série, 1 album) : {page.locator(".mm-card").count()}')
    check(page.locator('a[href="#/media"]').count() >= 1, 'la Médiathèque est un sous-onglet de Fichiers')
    page.screenshot(path=str(OUT / 'media-grid.png'))

    page.fill('input[type=search]', 'miyazaki'); page.wait_for_timeout(200)
    check(page.locator('.mm-card').count() == 1 and 'Chihiro' in page.locator('.mm-card').inner_text(), 'la recherche trouve un film par son réalisateur')
    page.fill('input[type=search]', 'esprits fantastique'); page.wait_for_timeout(200)
    check(page.locator('.mm-card').count() == 1, 'plusieurs mots : mot-clé ET genre')
    page.fill('input[type=search]', 'tartes'); page.wait_for_timeout(200)
    check(page.locator('.mm-card').count() == 1 and 'Kaamelott' in page.locator('.mm-card').inner_text(), 'un épisode retrouvé par son titre mène à sa série')
    page.fill('input[type=search]', ''); page.click('button[data-k=music]'); page.wait_for_timeout(200)
    check(page.locator('.mm-card').count() == 1 and page.locator('button[data-k=music]').get_attribute('aria-pressed') == 'true', 'filtre Musique')
    page.click('button[data-k=""]')

    page.locator('.mm-card', has_text='Chihiro').click(); page.wait_for_selector('.mm-detail')
    check('Hayao Miyazaki' in page.inner_text('.mm-detail') and 'esprits' in page.inner_text('.mm-detail'), 'la fiche montre réalisation et mots-clés')
    page.screenshot(path=str(OUT / 'media-detail.png'))
    page.locator('.mm-detail button', has_text='Lire').click(); page.wait_for_selector('dialog.mm-video[open] video')
    check(page.locator('dialog.mm-video video').get_attribute('src').startswith('/play/'), 'la vidéo est lue par une adresse /play (flux, pas de blob)')
    page.locator('dialog.mm-video button', has_text='Fermer').click(); page.wait_for_timeout(200)
    check(page.locator('dialog.mm-video').count() == 0, 'la fenêtre vidéo se ferme et disparaît')

    page.wait_for_selector('.mm-share p')
    check('Copié par : Léa' in page.inner_text('.mm-share') and 'Repartagé par : Léa' in page.inner_text('.mm-share'), 'la trace dit qui l\'a copié et qui le repartage')
    page.check('#mm-sh'); page.wait_for_timeout(300)
    shares = [b for p_, b in STATE['posts'] if p_.endswith('/media/share')]
    check(shares and shares[-1]['on'] is True and shares[-1]['paths'] == ['/Vidéos/Films/Le Voyage de Chihiro (2001).media.json'] and 'list' not in shares[-1], 'l\'interrupteur partage le film seul')
    page.screenshot(path=str(OUT / 'media-share.png'))
    page.locator('.mm-detail button', has_text='Corriger').click(); page.wait_for_selector('form.mm-form')
    labels = page.locator('form.mm-form label').all_inner_texts()
    check(any('Réalisation' in l for l in labels) and not any('Artiste' in l for l in labels), 'formulaire d\'un film : réalisation, pas d\'artiste')
    page.fill('#mm-f0', 'Le Voyage de Chihiro (version longue)'); page.fill('#mm-f5', 'esprits, bains, dragon')
    page.click('form.mm-form button[type=submit]'); page.wait_for_selector('.mm-detail')
    edits = [b for p_, b in STATE['posts'] if p_.endswith('/media/edit')]
    check(edits and edits[-1]['set']['keywords'] == ['esprits', 'bains', 'dragon'] and edits[-1]['set']['title'].endswith('(version longue)'), 'la correction envoie les champs du formulaire, listes découpées')
    check('version longue' in page.inner_text('.mm-detail h2'), 'la fiche affichée suit la correction')

    page.locator('button', has_text='Médiathèque').first.click(); page.locator('.mm-card', has_text='Mon Mix').click(); page.wait_for_selector('.mm-tracks li')
    check('Mix de DJ Test' in page.inner_text('.mm-body') and page.locator('.mm-tracks li').count() == 3, 'un mix : « Mix de DJ Test », trois morceaux dans l\'ordre')
    page.locator('button', has_text='Tout lire').click(); page.wait_for_selector('.mm-player:not([hidden])'); page.wait_for_timeout(600)
    check('Titre Un' in page.inner_text('.mm-player'), 'le lecteur joue le premier morceau')
    page.screenshot(path=str(OUT / 'media-album.png'))
    page.locator('.mm-player button[aria-label=Suivant]').click(); page.wait_for_timeout(400)
    check('Titre Deux' in page.inner_text('.mm-player'), 'Suivant passe au morceau 2')
    page.goto(f'http://127.0.0.1:{port}/#/replication'); page.wait_for_timeout(700)
    check(page.locator('.mm-player').is_visible() and 'Titre Deux' in page.inner_text('.mm-player'), 'la musique continue sur un autre écran (Sauvegarde)')

    # Chez mes amis : deux sources pour un même film, une station hors ligne signalée, copie demandée puis arrivée.
    page.goto(f'http://127.0.0.1:{port}/#/media'); page.wait_for_selector('.mm-card'); page.click('button[data-k=friends]'); page.locator('.mm-card', has_text='Château').wait_for()
    check('Paul' in page.inner_text('.mm-body') and 'injoignable' in page.inner_text('.mm-body'), 'la station hors ligne de Paul est signalée')
    check('Léa, Paul' in page.locator('.mm-card', has_text='Château').inner_text(), 'deux sources pour le même film : « chez Léa, Paul »')
    page.locator('.mm-card', has_text='Château').click(); page.wait_for_selector('article.card')
    check('2 copies chez vos amis' in page.inner_text('article.card'), 'la fiche dit le nombre de copies')
    page.screenshot(path=str(OUT / 'media-friends-detail.png'))
    page.locator('article.card button', has_text='Lire').click(); page.wait_for_timeout(400)
    check('Copie demandée' in page.inner_text('article.card'), 'au clic « Lire », la copie est demandée')
    reqs = [b for p_, b in STATE['posts'] if p_.endswith('/friends/request')]
    check(reqs and reqs[-1]['event_id'] == 'e1' * 32, 'la demande désigne le film choisi')
    page.wait_for_timeout(10800)
    check(page.locator('#toasts', has_text='est arrivé').count() >= 1 or 'arrivé' in page.inner_text('#toasts'), 'l\'arrivée est annoncée (relecture toutes les 10 s)')
    page.locator('button', has_text='Médiathèque').first.click(); page.click('button[data-k=friends]'); page.locator('.mm-card', has_text='Château').wait_for()
    check(page.locator('.mm-card', has_text='Château').inner_text().startswith('✓') or '✓' in page.locator('.mm-card', has_text='Château').inner_text(), 'le film copié est marqué ✓')
    page.screenshot(path=str(OUT / 'media-friends.png'))

    page.set_viewport_size(dict(width=400, height=860)); page.goto(f'http://127.0.0.1:{port}/#/media'); page.wait_for_selector('.mm-card'); page.click('button[data-k=""]'); page.wait_for_timeout(500)
    over = page.evaluate("document.documentElement.scrollWidth - document.documentElement.clientWidth")
    check(over <= 0, f'aucun débordement horizontal à 400 px ({over} px)')
    nav = page.locator('header nav').bounding_box(); pl = page.locator('.mm-player').bounding_box()
    check(pl and nav and pl['y'] + pl['height'] <= nav['y'] + 1, 'à 400 px le lecteur se pose au-dessus de la barre du bas')
    small = page.evaluate("[...document.querySelectorAll('.mm-player button, .mm-card')].filter(b => b.getBoundingClientRect().height < 44).length")
    check(small == 0, f'zones de pouce d\'au moins 44 px ({small} trop petite(s))')
    page.screenshot(path=str(OUT / 'media-400.png'), full_page=True)
    page.locator('.mm-card', has_text='Kaamelott').click(); page.wait_for_selector('.mm-tracks li'); page.screenshot(path=str(OUT / 'media-serie-400.png'))
    check(page.locator('.mm-tracks li').first.inner_text().startswith('▶') and 'S01E01' in page.locator('.mm-tracks li').first.inner_text(), 'les épisodes sont dans l\'ordre')
    page.check('#mm-sh'); page.wait_for_timeout(300)
    shares = [b for p_, b in STATE['posts'] if p_.endswith('/media/share')]
    check(shares[-1].get('list') == dict(kind='series', title='Kaamelott') and len(shares[-1]['paths']) == 2, 'une série se partage comme une liste de ses épisodes')
    br.close()

srv.shutdown()
if errors: print('Erreurs de console :', *errors, sep='\n  ')
sys.exit(1 if failures or errors else 0)
