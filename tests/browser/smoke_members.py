"""Test de fumée navigateur de l'écran Membres (Capitaine) : liste réglable sur place.

Tri par activité (les moins actifs d'abord), par disque et par ancienneté ; recherche qui garde le focus ; aide « ? » par
colonne ; quota, prix et Ğ1Billets modifiés dans la ligne (envoi immédiat) ; retrait d'un compte inactif depuis plus de 30
jours en deux temps ; réglages généraux repliés. Ordinateur et téléphone (400 px : une carte par compte), aucune erreur.

    ~/.astro/bin/python tests/browser/smoke_members.py [dossier-des-captures]
"""
import copy, re, sys
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
first = copy.deepcopy(ACCOUNTS)

with sync_playwright() as pw:
    br = pw.chromium.launch()
    for width, tag in [(1280, 'desktop'), (400, 'mobile')]:
        ACCOUNTS[:] = copy.deepcopy(first)
        ctx = br.new_context(viewport=dict(width=width, height=900), service_workers='block'); page = ctx.new_page()
        page.on('console', lambda m: errors.append(m.text) if m.type == 'error' and 'simulé' not in m.text else None); page.on('pageerror', lambda e: errors.append(str(e))); page.on('response', lambda r: errors.append(f'{r.status} {r.url}') if r.status >= 400 else None)
        ctx.add_init_script("localStorage.setItem('basic', btoa('alice@example.org:jeton'))")
        noscroll = lambda what: check(not page.evaluate('document.documentElement.scrollWidth > document.documentElement.clientWidth'), f'{tag} : pas de défilement horizontal ({what})')
        order = lambda: [re.search(r'[\w.]+@example\.org', e).group(0) for e in page.locator('table.mb-list tbody tr td:first-child').all_inner_texts()]

        page.goto(f'http://127.0.0.1:{port}/#/members'); page.wait_for_selector('table.mb-list')
        check(order() == ['parti@example.org', 'dormeur@example.org', 'chloe@example.org', 'bob@example.org'], f'{tag} : les moins actifs d’abord ({order()})')
        gone = page.locator('tr', has_text='parti@example.org')
        check('Retiré le' in gone.inner_text() and gone.locator('button', has_text='Jeton').count() == 0 and gone.locator('button', has_text='Supprimer').count() == 1, f'{tag} : compte retiré : dit, plus de jeton, seule la suppression reste')
        row = page.locator('tr', has_text='dormeur@example.org')
        check('il y a 45 j' in row.inner_text() and '90 %' in row.inner_text() and 'mb-idle-row' in (row.get_attribute('class') or ''), f'{tag} : activité, disque en % et inactivité signalée')
        check(page.locator('details.fold').count() >= 1 and not page.locator('#ix-on').is_visible(), f'{tag} : réglages généraux repliés')
        # Identité tirée du profil public : bandeau, logo, nom, site ; l'initiale quand il n'y a pas de logo.
        who = page.locator('tr', has_text='bob@example.org').locator('td').first
        loaded = lambda sel: who.locator(sel).evaluate('i => i.complete && i.naturalWidth > 0')
        site = who.locator('a.mb-site')
        check('Bob Martin' in who.inner_text() and loaded('img.mb-ban') and loaded('img.mb-av'), f'{tag} : nom, bandeau et logo de Bob (images de la station)')
        check(site.get_attribute('href') == 'https://bob.example.org/atelier/' and 'noopener' in site.get_attribute('rel') and site.get_attribute('target') == '_blank' and 'bob.example.org/atelier' in site.inner_text(), f'{tag} : son site en lien externe, adresse courte')
        check(page.locator('tr', has_text='chloe@example.org').locator('span.mb-av').inner_text() == 'C', f'{tag} : sans logo, l’initiale')
        page.select_option('#mb-sort', 'disk'); page.wait_for_timeout(100)
        check(order()[0] == 'dormeur@example.org' and 's=disk' in page.url, f'{tag} : tri par disque, gardé dans l’adresse')
        page.select_option('#mb-sort', 'since'); page.wait_for_timeout(100)
        check(order() == ['bob@example.org', 'parti@example.org', 'dormeur@example.org', 'chloe@example.org'], f'{tag} : tri par ancienneté')
        page.fill('#mb-find', 'chl'); page.wait_for_timeout(100)
        check(order() == ['chloe@example.org'] and page.evaluate("document.activeElement.id") == 'mb-find', f'{tag} : la recherche filtre et garde le focus')
        page.fill('#mb-find', '')

        # Aide « ? » d'une colonne.
        if width > 800:
            page.locator('table.mb-list th:nth-child(2)').locator('summary').click()
            check(page.locator('table.mb-list th:nth-child(2)').locator('.tip-body').is_visible(), f'{tag} : aide de la colonne Activité')
            page.locator('table.mb-list th:nth-child(2)').locator('summary').click()
        else:
            check(page.locator('td[data-label="Activité"]').first.is_visible(), f'{tag} : chaque cellule annonce sa colonne')

        # Hébergement dans la ligne : quota, puis Ğ1Billets.
        hx = 'bb' * 6
        page.fill(f'#mb-q-{hx}', '20'); page.press(f'#mb-q-{hx}', 'Tab'); page.wait_for_timeout(300)
        check(posts('/api/v1/station/accounts/' + 'bb' * 32 + '/hosting')[-1] == {'quota_bytes': 20 * 2 ** 30, 'weekly_zen': None}, f'{tag} : quota envoyé dès qu’on quitte le champ')
        page.locator(f'label[for=mb-b-{hx}]').click(); page.wait_for_timeout(300)
        check(posts('/api/v1/station/accounts/' + 'bb' * 32 + '/hosting')[-1].get('billet_issuer') is True and page.locator(f'#mb-b-{hx}').is_checked(), f'{tag} : Ğ1Billets autorisés en un geste')
        page.screenshot(path=str(OUT / f'members-{tag}.png'), full_page=True)
        noscroll('Membres')

        # Retrait en deux temps du compte inactif.
        row = page.locator('tr', has_text='dormeur@example.org')
        check(page.locator('tr', has_text='bob@example.org').locator('button', has_text='Retirer le compte').count() == 0, f'{tag} : un compte actif ne se retire pas')
        before = len(posts('/api/v1/station/accounts/' + 'dd' * 32 + '/remove'))
        btn = row.locator('button', has_text='Retirer le compte'); btn.click()
        check('Confirmer' in row.inner_text() and '30 jours' in row.inner_text() and len(posts('/api/v1/station/accounts/' + 'dd' * 32 + '/remove')) == before, f'{tag} : premier clic : il dit ce qui va se passer, rien n’est fait')
        row.locator('button', has_text='Confirmer').click(); page.wait_for_timeout(400)
        check(len(posts('/api/v1/station/accounts/' + 'dd' * 32 + '/remove')) == before + 1 and 'dormeur@example.org' not in page.inner_text('main'), f'{tag} : retiré au second clic, la liste suit')

        # Suppression complète du compte retiré : ce qui sera perdu, sans retour, l'adresse à retaper.
        check(page.locator('tr', has_text='bob@example.org').locator('button', has_text='Supprimer').count() == 0, f'{tag} : un compte actif ne se supprime pas')
        page.locator('tr', has_text='parti@example.org').locator('button', has_text='Supprimer').click(); page.wait_for_selector('#mb-wipe')
        dlg = page.locator('#share')
        go = dlg.locator('button', has_text='Supprimer définitivement')
        check('57 fichier(s)' in dlg.inner_text() and 'revenir en arrière' in dlg.inner_text() and go.is_disabled(), f'{tag} : la boîte dit ce qui sera perdu et que c’est sans retour ; bouton grisé')
        page.fill('#mb-wipe', 'parti@exemple.org')
        check(go.is_disabled(), f'{tag} : une adresse mal retapée ne suffit pas')
        page.screenshot(path=str(OUT / f'members-delete-{tag}.png'))
        page.fill('#mb-wipe', 'parti@example.org'); go.click(); page.wait_for_timeout(400)
        check(posts('/api/v1/station/accounts/' + 'ee' * 32 + '/delete')[-1] == {'confirm': 'parti@example.org'} and not dlg.is_visible() and 'parti@example.org' not in page.inner_text('main'), f'{tag} : supprimé, la liste suit')
        ctx.close()
    br.close()
srv.shutdown()
check(not errors, 'aucune erreur de console : ' + '; '.join(errors[:5]))
print(f'\n{len(failures)} échec(s)'); sys.exit(1 if failures else 0)
