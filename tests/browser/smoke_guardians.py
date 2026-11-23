"""Test de fumée navigateur des gardiens (Mon compte › Ma sécurité, « Pour vous ») et de la page publique de secours /restore.

Niveau (0 = alerte, 1 = fragile, 2 = bien, 3+ = solide), chances estimées ou « pas encore assez de recul », suggestion d'un
gardien de plus, suivi du retour des fichiers ; sur /restore : contacts nommés par leur nom public avec leur état, et le cas
« personne à prévenir ». Ordinateur et 400 px.

    ~/.astro/bin/python tests/browser/smoke_guardians.py [dossier-des-captures]
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

# Rangés par la station, le conseillé d'abord : un autre Astroport, puis le cercle le plus proche, puis rencontré en personne.
FRIENDS = [dict(hex='aa' * 32, label='Léa', same_station=False, level=5, met_verified=True), dict(hex='bb' * 32, label='Paul', same_station=False, level=3, met_verified=False),
           dict(hex='cc' * 32, label='Marie', same_station=True, level=5, met_verified=False)]
def backup(guardians, safety=None, restore=None):
    return dict(mine=None, friends=FRIENDS, hosting=[], rescue_friends=len(guardians), guardians=guardians, safety=safety or dict(permille=None, next_permille=None, files_permille=None), restore=restore)
G = lambda label, same=False, files=True, p=None: dict(label=label, same_station=same, files=files, availability=p)

with sync_playwright() as pw:
    br = pw.chromium.launch()
    for width, tag in [(1280, 'desktop'), (400, 'mobile')]:
        ctx = br.new_context(viewport=dict(width=width, height=900), service_workers='block'); page = ctx.new_page()
        page.on('console', lambda m: errors.append(m.text) if m.type == 'error' and 'simulé' not in m.text and '400' not in m.text and '404' not in m.text else None); page.on('pageerror', lambda e: errors.append(str(e)))
        ctx.add_init_script("localStorage.setItem('basic', btoa('alice@example.org:jeton'))")
        noscroll = lambda what: check(not page.evaluate('document.documentElement.scrollWidth > document.documentElement.clientWidth'), f'{tag} : pas de défilement horizontal ({what})')
        guard = lambda: page.locator('#ac-guard').inner_text()

        # Aucun gardien (Bob est sur la même station) : alerte rouge dans Ma sécurité et dans « Pour vous ».
        STATE['backup'] = backup([G('Bob', same=True)])
        page.goto(f'http://127.0.0.1:{port}/#/'); page.wait_for_selector('.hm-news')
        check('si votre station tombe, votre compte est perdu' in page.locator('.hm-news').inner_text(), f'{tag} : alerte « aucun gardien » dans Pour vous')
        page.goto(f'http://127.0.0.1:{port}/#/backup'); page.wait_for_selector('#ac-guard')
        txt = guard()
        check('Aucun gardien' in txt and 'ne vous protège pas d’une panne'.replace('’', "'") in txt, f'{tag} : niveau 0, Bob sur la même station ne compte pas')
        check(page.locator('#ac-guard p.err[role=alert]').count() == 1, f'{tag} : alerte rouge annoncée')
        check('une part seule' in txt.lower() and page.locator('#gd-friend option').count() == 3, f'{tag} : la phrase sur la carte + un gardien, et le choix d’un proche')
        noscroll('aucun gardien'); page.screenshot(path=str(OUT / f'guardians-0-{tag}.png'), full_page=True)
        opts = page.locator('#gd-friend option').all_inner_texts()
        check(opts[0] == '🏠 Léa ✓' and opts[2] == '🏠 Marie (même station)' and page.locator('#gd-friend').input_value() == 'aa' * 32, f'{tag} : le gardien conseillé en tête et choisi d’avance, la même station signalée')
        check('En tête : vos proches sur un autre Astroport' in txt, f'{tag} : la règle du conseil est dite')
        # Tous les proches sur cette station : le dire, et proposer de rencontrer quelqu'un d'ailleurs.
        STATE['backup'] = dict(backup([G('Bob', same=True)]), friends=[FRIENDS[2]])
        page.reload(); page.wait_for_selector('#ac-guard')
        check('doit surtout être sur un autre Astroport' in guard() and page.locator('#ac-guard a[href="#/rencontre"]').count() == 1, f'{tag} : proches tous ici, invitation à se rencontrer')
        noscroll('proches tous ici'); page.screenshot(path=str(OUT / f'guardians-here-{tag}.png'), full_page=True)

        # Un gardien sans recul : pas de chiffre inventé.
        STATE['backup'] = backup([G('Léa')])
        page.reload(); page.wait_for_selector('#ac-guard')
        txt = guard()
        check('fragile' in txt and 'Pas encore assez de recul' in txt and '%' not in txt, f'{tag} : niveau 1, pas encore de chiffre')
        # Deux gardiens, avec chiffre et suggestion ; puis trois.
        STATE['backup'] = backup([G('Léa', p=900), G('Paul', files=False, p=700)], dict(permille=970, next_permille=997, files_permille=900))
        page.reload(); page.wait_for_selector('#ac-guard')
        txt = guard()
        check('c\'est bien' in txt and '97 % de chances' in txt and 'Vos fichiers : 90 %' in txt and 'passer à 99 %' in txt, f'{tag} : niveau 2, chances, fichiers et suggestion')
        check(page.locator('#gd-friend option').count() == 1, f'{tag} : seuls les proches qui ne sont pas encore gardiens sont proposés')
        noscroll('deux gardiens'); page.screenshot(path=str(OUT / f'guardians-2-{tag}.png'), full_page=True)
        STATE['backup'] = backup([G('Léa', p=900), G('Paul', p=900), G('Marie', p=900)], dict(permille=999, next_permille=None, files_permille=999))
        page.reload(); page.wait_for_selector('#ac-guard')
        check('3 gardiens : c\'est solide' in guard() and 'passer à' not in guard(), f'{tag} : niveau 3, plus de suggestion')

        # Après un rétablissement : le suivi du retour des fichiers (Ma sécurité et Pour vous).
        STATE['backup'] = backup([G('Léa', p=900)], dict(permille=900, next_permille=990, files_permille=900), dict(state='late', since=1, returned=['Léa'], waiting=[dict(label='Paul', seen_days=4), dict(label='Marie', seen_days=None)]))
        page.reload(); page.wait_for_selector('#ac-guard')
        body = page.locator('main').inner_text()
        check('Vos fichiers reviennent' in body and 'Paul est hors ligne depuis 4 jour(s)' in body and 'Marie n\'a pas encore répondu' in body and 'sans rien écraser' in body, f'{tag} : suivi nominatif du retour des fichiers')
        noscroll('retour des fichiers'); page.screenshot(path=str(OUT / f'guardians-restore-{tag}.png'), full_page=True)
        page.goto(f'http://127.0.0.1:{port}/#/'); page.wait_for_selector('.hm-news')
        check('Vos fichiers reviennent : 1 ami(s) ont renvoyé, 2 attendu(s)' in page.locator('.hm-news').inner_text(), f'{tag} : suivi dans Pour vous')

        # Page publique de secours : contacts nommés par leur nom public et leur état ; puis « personne à prévenir ».
        STATE['rescue'] = dict(status='waiting', sent=3, denied=[], asks_left=2, contacts=[dict(name='Léa', state='denied', since=None), dict(name='Paul', state='asked', since=None), dict(name=None, state='asked', since=None), dict(name='Marie', state='offline', since=1790000000)])
        page.goto(f'http://127.0.0.1:{port}/restore'); page.fill('#card', 'astro-share:v1|x|y|z|w|1|q')
        page.wait_for_selector('#who li')
        who = page.locator('#who').inner_text()
        check('Léa — n’a pas reconnu la demande' in who and 'Paul — prévenu' in who and 'Un contact sans nom public — prévenu' in who and 'Marie — station injoignable depuis' in who, f'{tag} : contacts nommés avec leur état')
        check(not page.locator('#note').is_hidden() and '@' not in who, f'{tag} : rappel « seuls les gardiens répondent », aucune adresse')
        noscroll('secours'); page.screenshot(path=str(OUT / f'guardians-rescue-{tag}.png'), full_page=True)
        STATE['rescue'] = dict(status='nobody', sent=0, denied=[], asks_left=2, contacts=[])
        page.goto(f'http://127.0.0.1:{port}/restore'); page.fill('#card', 'astro-share:v1|x|y|z|w|1|q')
        page.wait_for_function("document.querySelector('#rs').textContent.includes('Personne')")
        check('ancienne station' in page.locator('#rs').inner_text(), f'{tag} : personne à prévenir, l’autre voie est dite')
        STATE.pop('rescue'); STATE.pop('backup')
        ctx.close()
    br.close()
srv.shutdown()
print('erreurs console :', errors or 'aucune')
sys.exit(1 if failures or errors else 0)
