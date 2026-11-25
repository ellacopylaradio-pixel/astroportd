"""Test de fumée navigateur de « Lecture de vos documents » (Mon compte) et de son pendant Capitaine (Membres).

Membre : état inactif et bénéfice dit en clair, choix d'une station amie, consentement explicite (bouton grisé sans lui), état
actif avec l'avancement, arrêt en un geste, compte désactivé par le Capitaine, incitation depuis la recherche. Capitaine :
interrupteurs de la station, du défaut et de chaque MULTIPASS. Ordinateur et téléphone (400 px), aucune erreur de console.

    ~/.astro/bin/python tests/browser/smoke_reading.py [dossier-des-captures]
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
posts = lambda path: [b for p_, b in STATE['posts'] if p_ == path]

with sync_playwright() as pw:
    br = pw.chromium.launch()
    for width, tag in [(1280, 'desktop'), (400, 'mobile')]:
        READING.update(state='off', blocked=None, stations=[], local=[], photos=None, progress=dict(read=0, by_friend=0, failed=0, waiting=37, more=False))
        ctx = br.new_context(viewport=dict(width=width, height=900), service_workers='block'); page = ctx.new_page()
        page.on('console', lambda m: errors.append(m.text) if m.type == 'error' and 'simulé' not in m.text else None); page.on('pageerror', lambda e: errors.append(str(e))); page.on('response', lambda r: errors.append(f'{r.status} {r.url}') if r.status >= 400 else None)
        ctx.add_init_script("localStorage.setItem('basic', btoa('alice@example.org:jeton'))")
        noscroll = lambda what: check(not page.evaluate('document.documentElement.scrollWidth > document.documentElement.clientWidth'), f'{tag} : pas de défilement horizontal ({what})')

        # Incitation depuis la recherche : personne ne lit encore.
        page.goto(f'http://127.0.0.1:{port}/#/find'); page.wait_for_selector('a[href="#/devices?card=reading"]')
        link = page.locator('a[href="#/devices?card=reading"]', has_text='Documents et photos cherchables')
        check(link.is_visible(), f'{tag} : la recherche signale que le texte des documents n’est pas encore cherchable, avec le chemin')
        link.click(); page.wait_for_selector('#reading .pill')
        page.wait_for_timeout(200)
        top, bottom, vh = page.evaluate("(r => [r.top, r.bottom, innerHeight])(document.getElementById('reading').getBoundingClientRect())")
        check(0 <= top and (bottom <= vh or top < 200), f'{tag} : le lien amène directement à la carte, entièrement sous les yeux ({top:.0f}–{bottom:.0f} px / {vh})')
        card = page.locator('#reading')
        check('Inactive' in card.inner_text() and 'facture' in card.inner_text() and '37 document(s) attendent' in card.inner_text() and 'photo(s)' not in card.inner_text(), f'{tag} : état inactif, bénéfice concret et ce qui attend (aucune promesse sur les photos)')
        page.screenshot(path=str(OUT / f'reading-off-{tag}.png'), full_page=True)

        # Choisir et consentir.
        card.locator('button', has_text='Voir les stations amies disponibles').click(); page.wait_for_selector('#reading form')
        check(card.locator('input[name=rd-station]').count() == 2 and card.locator('input[name=rd-station]:checked').count() == 1, f'{tag} : deux stations, la première présélectionnée')
        check('se partagent le travail' in card.inner_text(), f'{tag} : l’intérêt d’en choisir plusieurs est dit')
        check('à 12 km' in card.inner_text() and '1 station(s) n’ont pas répondu' in card.inner_text(), f'{tag} : distance et stations silencieuses dites')
        go = card.locator('button[type=submit]')
        check(go.is_disabled(), f'{tag} : « Confier la lecture » grisé tant que l’accord n’est pas coché')
        check(card.locator('input[name=rd-task]').count() == 0 and 'ne renvoie que le texte lu' in card.inner_text(), f'{tag} : seul le texte des documents se confie, et c’est dit')
        check('vos recherches se font ici' in card.inner_text(), f'{tag} : il est dit que la recherche reste chez soi')
        check(all(card.locator('.rd-choice').nth(i).bounding_box()['height'] >= 47 for i in range(card.locator('.rd-choice').count())), f'{tag} : chaque choix fait au moins 48 px de haut')
        noscroll('formulaire')
        page.screenshot(path=str(OUT / f'reading-choose-{tag}.png'), full_page=True)
        card.locator('label[for=rd-st-1]').click(); card.locator('label[for=rd-ok]').click()
        card.locator('label[for=rd-st-0]').click(); card.locator('label[for=rd-st-1]').click()
        check(go.is_disabled(), f'{tag} : aucune station cochée : bouton grisé même avec l’accord')
        card.locator('label[for=rd-st-0]').click(); card.locator('label[for=rd-st-1]').click()
        check(go.is_enabled(), f'{tag} : accord coché, le bouton s’active')
        go.click(); page.wait_for_selector('#reading progress')
        sent = posts('/api/v1/me/reading')[-1]
        check(sent == {'accept': {'peers': ['12D3KooWMarie', '12D3KooWAtelier'], 'tasks': ['ocr']}}, f'{tag} : les deux stations et la lecture du texte sont envoyées : {sent}')
        check(card.locator('.rd-lender').count() == 2 and 'se partagent le travail' in card.inner_text() and '6 document(s) lu(s)' in card.inner_text() and '34 en attente' in card.inner_text(), f'{tag} : état actif, les deux stations et l’avancement')
        page.screenshot(path=str(OUT / f'reading-friend-{tag}.png'), full_page=True)
        card.locator('.rd-lender', has_text='Atelier du Lez').locator('button', has_text='Retirer').click(); page.wait_for_timeout(300)
        check(posts('/api/v1/me/reading')[-1] == {'revoke': '12D3KooWAtelier'} and card.locator('.rd-lender').count() == 1, f'{tag} : retirer une seule station')
        check(card.locator('button', has_text='Ajouter une station').count() == 1, f'{tag} : on peut en rajouter')
        card.locator('button', has_text='Arrêter').click(); page.wait_for_selector('#reading button:has-text("Voir les stations amies disponibles")')
        check(posts('/api/v1/me/reading')[-1] == {'revoke': True}, f'{tag} : « Arrêter » en un geste')

        # Station équipée : elle lit et regarde elle-même ; la recherche par ce que montrent les photos est annoncée avec un exemple.
        READING.update(state='local', local=['ocr', 'vision'], stations=[], photos=dict(seen=120, waiting=30, more=False, videos=True), progress=dict(read=40, by_friend=0, failed=0, waiting=0, more=False))
        page.reload(); page.wait_for_selector('#reading progress')
        check('vélo rouge' in card.inner_text() and 'vos vidéos' in card.inner_text() and '120 photo(s) ou vidéo(s) analysée(s)' in card.inner_text() and '30 en attente' in card.inner_text() and card.locator('progress').count() == 1, f'{tag} : station équipée : photos analysées ici, exemple de recherche, avancement')
        noscroll('station équipée')
        page.screenshot(path=str(OUT / f'reading-local-{tag}.png'), full_page=True)

        # Le membre garde la main : pause, reprise, effacement confirmé dans la page ; la place de son index est dite.
        READING.update(pack=3_400_000); page.reload(); page.wait_for_selector('#reading progress')
        check('Votre index occupe' in card.inner_text(), f'{tag} : la place prise par l’index est dite')
        card.locator('button', has_text='Mettre en pause l’analyse de mes fichiers').click(); page.wait_for_selector('#reading .pill:has-text("En pause")')
        check(posts('/api/v1/me/reading')[-1] == {'pause': True} and card.locator('button', has_text='Reprendre l’analyse').count() == 1, f'{tag} : en pause, et le bouton propose de reprendre')
        erase = card.locator('button', has_text='Effacer ce qui a été calculé')
        erase.click(); page.wait_for_timeout(200)
        check(posts('/api/v1/me/reading')[-1] == {'pause': True} and 'seront oubliés' in card.inner_text(), f'{tag} : un premier clic demande confirmation et dit ce qui sera oublié')
        card.locator('button', has_text='Confirmer ?').click(); page.wait_for_timeout(400)
        check(posts('/api/v1/me/reading')[-1] == {'erase': True}, f'{tag} : effacé après confirmation')
        noscroll('pause et effacement')
        page.screenshot(path=str(OUT / f'reading-paused-{tag}.png'), full_page=True)
        card.locator('button', has_text='Reprendre l’analyse').click(); page.wait_for_timeout(300)
        READING.update(paused=False, pack=None)

        # Le Capitaine a désactivé ce compte : le membre sait pourquoi et à qui s'adresser.
        READING.update(state='blocked', blocked='member', progress=dict(read=12, by_friend=0, failed=0, waiting=0, more=False))
        page.reload(); page.wait_for_selector('#reading .pill')
        check('Capitaine a désactivé' in card.inner_text() and 'Parlez-lui-en' in card.inner_text() and 'reste dans votre recherche' in card.inner_text() and card.locator('button').all_inner_texts() == ['Effacer ce qui a été calculé'], f'{tag} : compte désactivé, la raison et la suite ; seul l’effacement reste possible')

        # Capitaine : Membres.
        page.goto(f'http://127.0.0.1:{port}/#/members'); page.wait_for_selector('table.mb-list')
        page.locator('details.fold summary', has_text='Analyse des documents').click(); page.wait_for_selector('#ix-on')  # réglages généraux repliés
        check(page.locator('#ix-on').is_checked() and page.locator('#ix-def').is_checked(), f'{tag} : station et défaut activés')
        bob, chloe = page.locator('#ix-m-' + 'bb' * 6), page.locator('#ix-m-' + 'cc' * 6)
        check(bob.is_checked() and not chloe.is_checked(), f'{tag} : Bob activé (par défaut), Chloé désactivée (réglée)')
        check('via La station de Marie' in page.inner_text('table') and '120 lu(s)' in page.inner_text('table'), f'{tag} : ce qui a été lu et par qui')
        page.screenshot(path=str(OUT / f'reading-captain-{tag}.png'), full_page=True)
        page.locator('label[for=ix-m-' + 'bb' * 6 + ']').click(); page.wait_for_timeout(300)
        check(posts('/api/v1/station/admin/index')[-1] == {'member': {'hex': 'bb' * 32, 'enabled': False}} and not page.locator('#ix-m-' + 'bb' * 6).is_checked(), f'{tag} : désactiver Bob')
        page.locator('tr', has_text='bob@example.org').locator('button', has_text='Par défaut').click(); page.wait_for_timeout(300)
        check(posts('/api/v1/station/admin/index')[-1] == {'member': {'hex': 'bb' * 32, 'enabled': None}} and page.locator('#ix-m-' + 'bb' * 6).is_checked(), f'{tag} : Bob revient au défaut')
        page.locator('label[for=ix-on]').click(); page.wait_for_timeout(300)
        check(page.locator('#ix-def').is_disabled() and page.locator('#ix-m-' + 'bb' * 6).is_disabled() and 'Coupée' in page.inner_text('table'), f'{tag} : station coupée : tout le reste grisé et dit')
        page.locator('label[for=ix-on]').click(); page.wait_for_timeout(300)
        noscroll('Membres')
        ctx.close()
    br.close()
srv.shutdown()
check(not errors, 'aucune erreur de console : ' + '; '.join(errors[:5]))
print(f'\n{len(failures)} échec(s)'); sys.exit(1 if failures else 0)
