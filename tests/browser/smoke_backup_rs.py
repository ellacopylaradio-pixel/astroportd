"""Test de fumée navigateur de la sauvegarde répartie (lot 7, docs/design/SAUVEGARDE_RS.md).

Ma sécurité › Mes gardiens : sécurité estimée face à l'objectif de 99,9 %, place économisée, « quelques-uns suffisent »,
réparation et rangement dits en mots simples, sans jargon. Médiathèque : pastille discrète dans la liste, phrase détaillée
dans la fiche (copies chez les amis, date de vérification), une seule lecture pour toute la médiathèque. Ma station ›
Détails techniques : le résumé du Capitaine. Ordinateur et 400 px.

    ~/.astro/bin/python tests/browser/smoke_backup_rs.py [dossier-des-captures]
"""
import sys
import time
from pathlib import Path
from playwright.sync_api import sync_playwright
from mock_station import *  # noqa: F403

OUT = Path(sys.argv[1] if len(sys.argv) > 1 else '.')
srv, port = start()
errors, failures, safety_calls = [], [], []
def check(cond, msg):
    print(('OK   ' if cond else 'FAIL ') + msg)
    if not cond: failures.append(msg)

JARGON = ['CID', 'fragment', 'paquet', 'k-sur-n', 'IPNS', 'NIP', 'Reed', 'ARSF']
FRIENDS = [dict(hex='aa' * 32, label='Léa'), dict(hex='bb' * 32, label='Paul'), dict(hex='cc' * 32, label='Marie')]
G = lambda label: dict(label=label, same_station=False, files=True, availability=900)
def backup(coded):
    return dict(mine=None, friends=FRIENDS, hosting=[], rescue_friends=5, guardians=[G(n) for n in ['Léa', 'Paul', 'Marie', 'Jo', 'Ana']],
                safety=dict(permille=999, next_permille=None, files_permille=999), coded=coded, restore=None)
CODED = dict(packets=4, holders=5, need=3, safety_permille=999, target_permille=999, ratio_pct=170, repairing=0, thin=0, compacting=False)
now = int(time.time())
FILM, SONG = '/Vidéos/Autres/Atelier vélo (2024).mp4', '/Musique/DJ Test/Mon Mix/01 - Artiste A - Titre Un.mp3'

with sync_playwright() as pw:
    br = pw.chromium.launch()
    for width, tag in [(1280, 'desktop'), (400, 'mobile')]:
        ctx = br.new_context(viewport=dict(width=width, height=900), service_workers='block'); page = ctx.new_page()
        page.on('console', lambda m: errors.append(m.text) if m.type == 'error' and 'simulé' not in m.text and '404' not in m.text else None); page.on('pageerror', lambda e: errors.append(str(e)))
        page.on('request', lambda r: safety_calls.append(r.url) if r.url.endswith('/media/safety') else None)
        ctx.add_init_script("localStorage.setItem('basic', btoa('alice@example.org:jeton'))")
        noscroll = lambda what: check(not page.evaluate('document.documentElement.scrollWidth > document.documentElement.clientWidth'), f'{tag} : pas de défilement horizontal ({what})')
        clean = lambda txt, what: check(not any(j.lower() in txt.lower() for j in JARGON), f'{tag} : aucun jargon ({what})')

        # Mes gardiens : 3 sur 5, 1,7 fois au lieu de 5, objectif atteint.
        STATE['backup'] = backup(CODED)
        page.goto(f'http://127.0.0.1:{port}/#/backup'); page.wait_for_selector('#ac-guard')
        txt = page.locator('#ac-guard').inner_text()
        check('Sécurité estimée : 99,9 % (objectif 99,9 %)' in txt, f'{tag} : sécurité estimée et objectif')
        check('3 de vos 5 gardiens suffisent pour tout retrouver' in txt, f'{tag} : quelques-uns suffisent')
        check('Vos amis gardent 1,7 fois vos fichiers au lieu de 5' in txt, f'{tag} : place économisée')
        check('null' not in txt, f'{tag} : aucun « null » affiché')
        clean(txt, 'Mes gardiens'); noscroll('gardiens'); page.screenshot(path=str(OUT / f'backup-rs-guard-{tag}.png'), full_page=True)
        # Réparation en cours, puis plus de marge.
        STATE['backup'] = backup({**CODED, 'repairing': 1, 'safety_permille': 990})
        page.reload(); page.wait_for_selector('#ac-guard')
        txt = page.locator('#ac-guard').inner_text()
        check('Réparation en cours' in txt and '99 % (objectif 99,9 %)' in txt, f'{tag} : réparation dite simplement')
        clean(txt, 'réparation')
        STATE['backup'] = backup({**CODED, 'thin': 1})
        page.reload(); page.wait_for_selector('#ac-guard')
        check('n\'a plus de marge' in page.locator('#ac-guard').inner_text(), f'{tag} : plus de marge')
        page.screenshot(path=str(OUT / f'backup-rs-thin-{tag}.png'), full_page=True)

        # Médiathèque : pastilles dans la liste, phrase dans la fiche ; une seule lecture.
        STATE['safety'] = dict(files={FILM: dict(copies=3, at=['Léa', 'Paul', 'Marie'], verified_at=now - 3 * 3600, packed=False, safe=True),
                                      SONG: dict(copies=1, at=['Paul'], verified_at=now - 3600, packed=True, safe=False)})
        safety_calls.clear()
        page.goto(f'http://127.0.0.1:{port}/#/media'); page.wait_for_selector('.mm-card')
        check(len(safety_calls) == 1, f'{tag} : une seule lecture groupée ({len(safety_calls)})')
        film = page.locator('.mm-card', has_text='Atelier vélo')
        check('🟢' in film.inner_text(), f'{tag} : pastille verte discrète dans la liste')
        check('🟢' not in page.locator('.mm-card', has_text='Chihiro').inner_text(), f'{tag} : rien sur un fichier non partagé')
        film.click(); page.wait_for_selector('.mm-safe')
        txt = page.locator('.mm-safe').inner_text()
        check('En sécurité : 3 copies chez vos amis' in txt and 'vérifié il y a 3 h' in txt, f'{tag} : fiche détaillée ({txt})')
        clean(page.locator('.mm-detail').inner_text(), 'fiche'); noscroll('fiche'); page.screenshot(path=str(OUT / f'backup-rs-media-{tag}.png'), full_page=True)
        page.goto(f'http://127.0.0.1:{port}/#/media?k=music'); page.wait_for_selector('.mm-card')
        page.locator('.mm-card').first.click(); page.wait_for_selector('.mm-tracks')
        check('🟡' in page.locator('.mm-tracks li').first.inner_text(), f'{tag} : pastille jaune sur le morceau partagé')

        # Ma station › Détails techniques : résumé du Capitaine.
        STATE['dashboard'] = dict(station=dict(node='abc', peer_id='12D3', domain=None, version='0', uptime_secs=60, captain=True), relay=dict(events=1, last_event_at=None, db_bytes=1),
                                  backup_rs=dict(packets=12, staged=1, frags_acked=50, frags_pending=2, frags_lost=1, repairing=1, drops_queued=3, faulty_guardians=1, suspects=1, held_for_friends=7))
        page.goto(f'http://127.0.0.1:{port}/#/machine'); page.wait_for_selector('text=Sauvegarde k-sur-n')
        txt = page.locator('main').inner_text()
        check('Gardiens fautifs' in txt and '50 acquittés' in txt and '1 perdus' in txt and 'Fragments gardés pour des amis' in txt, f'{tag} : résumé Capitaine')
        noscroll('Capitaine'); page.screenshot(path=str(OUT / f'backup-rs-captain-{tag}.png'), full_page=True)
        STATE.pop('dashboard'); STATE.pop('safety'); STATE.pop('backup')
        ctx.close()
    br.close()
srv.shutdown()
check(not errors, f'aucune erreur de console ({errors[:3]})')
print('ÉCHEC' if failures else 'tout est vert'); sys.exit(1 if failures else 0)
