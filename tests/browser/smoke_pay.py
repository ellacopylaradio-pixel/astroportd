"""Test de fumée navigateur de « Payer / Recevoir » par QR (m-pay.js) sur la station simulée, à 400 px et sur ordinateur.

    ~/.astro/bin/python tests/browser/smoke_pay.py [dossier-des-captures]

Vérifie : Mes Ẑen propose Payer et Recevoir ; Recevoir montre un QR, un montant demandé, et dit quand l'argent arrive ;
Payer lit un VRAI QR filmé par une caméra simulée (code « Recevoir » d'une autre station), ouvre l'envoi habituel avec
destinataire et montant remplis, avertit que la personne n'est pas un proche, montre le nom connu de la station (jamais un nom
venu du QR), envoie après deux clics ; un lien ouvert par l'appareil photo mène droit au paiement et reconnaît un proche ;
les QR étrangers sont refusés. Aucune erreur de console, pas de défilement horizontal.
"""
import io, subprocess, sys, tempfile
from pathlib import Path
import qrcode
from PIL import Image
from playwright.sync_api import sync_playwright
from mock_station import *  # noqa: F403 (données et gestionnaire simulés)

OUT = Path(sys.argv[1] if len(sys.argv) > 1 else '.')
EF, AB, AA, Z = 'ef' * 32, 'ab' * 32, 'aa' * 32, '12' * 32
srv, port = start()
failures, errors = [], []
def check(cond, msg):
    print(('OK   ' if cond else 'FAIL ') + msg)
    if not cond: failures.append(msg)

# Caméra simulée : une vidéo qui filme le code « Recevoir » d'un membre d'une AUTRE station (seuls clé et montant comptent).
tmp = Path(tempfile.mkdtemp())
code = qrcode.make(f'https://autre.station.example/#/payer?to={EF}&a=7', border=4).get_image().convert('RGB').resize((330, 330), Image.NEAREST)
frame = Image.new('RGB', (640, 480), 'white'); frame.paste(code, (155, 75)); frame.save(tmp / 'qr.png')
subprocess.run(['ffmpeg', '-loglevel', 'error', '-y', '-loop', '1', '-i', str(tmp / 'qr.png'), '-t', '1', '-r', '10', '-pix_fmt', 'yuv420p', str(tmp / 'qr.y4m')], check=True)

with sync_playwright() as pw:
    b = pw.chromium.launch(args=['--use-fake-ui-for-media-stream', '--use-fake-device-for-media-stream', f'--use-file-for-fake-video-capture={tmp / "qr.y4m"}'])
    for name, vp in [('mobile', dict(width=400, height=860)), ('desktop', dict(width=1280, height=900))]:
        c = b.new_context(viewport=vp, timezone_id='Europe/Paris', permissions=['camera'])
        c.add_init_script("localStorage.setItem('basic', btoa('alice@example.org:jeton'))")
        p = c.new_page()
        p.on('pageerror', lambda e: errors.append(str(e)))
        p.on('console', lambda m: m.type == 'error' and 'Service Worker' not in m.text and '400 (Bad Request)' not in m.text and errors.append(m.text))  # 400 : le code expiré, refusé exprès
        STATE['island'] = dict(open=False, promises=[]); STATE['posts'].clear()  # réseau Ğ1 joignable (smoke_island.py couvre l'île)
        STATE['dashboard'] = dict(balance=dict(zen_cents=10000, g1_cents=1000), energy=dict(weeks=3, max_weeks=8, weekly_rent_cents=100), sovereignty=dict(encrypted_files=12))

        # Mes Ẑen : les deux gestes, en évidence.
        p.goto(f'http://127.0.0.1:{port}/#/money'); p.wait_for_selector('main a[href="#/payer"]')
        check(p.is_visible('main a[href="#/recevoir"]'), f'{name} : Mes Ẑen propose Payer et Recevoir')

        # Recevoir : mon QR, un montant, et l'arrivée de l'argent.
        p.click('main a[href="#/recevoir"]'); p.wait_for_selector('.pay-qr img')
        check(p.is_visible('text=Montant au choix de la personne'), f'{name} : sans montant, la personne choisit')
        check('alice@example.org' in p.inner_text('.pay-qr'), f'{name} : mon nom sous le QR')
        p.fill('#pay-amount', '12'); p.click('.pay-form button'); p.wait_for_selector('text=12 Ẑen demandés')
        src = p.get_attribute('.pay-qr img', 'src')
        check(src.startswith('data:image/svg+xml;base64,'), f'{name} : QR affiché en image (aucun script dans le SVG)')
        box = p.locator('.pay-qr img').bounding_box()
        check(box['width'] >= 240, f'{name} : QR assez grand pour être visé ({box["width"]:.0f} px)')
        p.screenshot(path=str(OUT / f'pay-receive-{name}.png'), full_page=True)
        STATE['dashboard']['balance'] = dict(zen_cents=10500, g1_cents=1050)
        p.wait_for_selector('text=Paiement reçu : +5 Ẑen', timeout=12000)
        check(True, f'{name} : « Paiement reçu : +5 Ẑen » quand le solde monte')

        # QR reconnus et refusés (règles de lecture, dans le navigateur).
        got = p.evaluate("""async () => { const { parse } = await import('/ui/m-pay.js'); return [
            parse('https://x.example/#/payer?to=' + 'EF'.repeat(32) + '&a=7'), parse('https://x.example/#/payer?to=' + 'ef'.repeat(32) + '&a=-2'),
            parse('https://x.example/#/cloud?to=' + 'ef'.repeat(32)), parse('https://x.example/#/payer?to=abc'), parse('g1LsRGbH5st3WZfdF2SpknxM12'),
            parse('nostr:npub180cvv07tjdrrgpa0j7j7tmnyl2yr6yr7l8j4s3evf6u64th6gkwsyjh6w6'), parse('npub1abc')]; }""")
        check(got[0] == dict(to=EF, a=7, m=None) and got[1] == dict(to=EF, a=None, m=None), f'{name} : code « Recevoir » lu (clé en minuscules, montant invalide ignoré)')
        check(got[2] is None and got[3] is None and got[4] is None and got[6] is None, f'{name} : autre page, clé tronquée, adresse Ğ1 seule, npub abîmé : refusés')
        check(got[5] == dict(to='3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d', a=None, m=None), f'{name} : un npub d\'une autre application est accepté')

        # Payer en filmant le QR (une seule fois : la caméra simulée est la même pour tout le navigateur).
        if name == 'mobile':
            p.goto(f'http://127.0.0.1:{port}/#/payer'); p.wait_for_selector('.pay-name', timeout=15000)
            check(p.inner_text('.pay-name') == 'efefefef' and p.is_visible('text=Pas encore dans vos proches'), f'{name} : QR filmé → fiche de la personne, nom connu de la station, pas encore proche')
            check(p.evaluate('!document.querySelector(".pay-cam video")?.srcObject'), f'{name} : la caméra s\'éteint dès le code lu')
            p.click('main button:has-text("Payer 7 Ẑen")'); p.wait_for_selector('#credit .cr-form')
            check(p.input_value('#cr-to') == EF and p.input_value('#cr-amount') == '7', f'{name} : QR filmé → destinataire et montant remplis')
            check(p.is_visible('#credit >> text=n\'est pas dans vos proches'), f'{name} : avertit que la personne n\'est pas un proche')
            p.click('#credit button[type=submit]'); p.wait_for_selector('#credit h3')
            check('efefefef' in p.inner_text('#credit .cr-sum'), f'{name} : le récapitulatif montre le nom connu de la station')
            p.screenshot(path=str(OUT / f'pay-scan-{name}.png'), full_page=True)
            p.click('#credit button:has-text("Envoyer 7 Ẑen")'); p.click('#credit button:has-text("Confirmer")')
            p.wait_for_selector('#credit >> text=7 Ẑen envoyés')
            check(any(x[0] == '/api/v1/me/zen/send' and x[1].get('to') == EF and x[1].get('amount_zen') == 7 and x[1].get('confirm') and x[1].get('credit_id') for x in STATE['posts']), f'{name} : envoyé après deux clics, avec identifiant rejouable')
            p.keyboard.press('Escape')
            check(p.is_visible('main button:has-text("Scanner un autre code")'), f'{name} : « Scanner un autre code » pour payer quelqu\'un d\'autre')
            # /scan (Ğ1Billets) partage le lecteur : il filme, lit, et ne prend pas un code « Recevoir » pour un billet.
            p.goto(f'http://127.0.0.1:{port}/scan'); p.click('#cam-on'); p.wait_for_selector('#cam:not([hidden])')
            p.wait_for_selector('#hint:has-text("n’est pas celui d’un billet")', timeout=15000)
            check(True, f'{name} : /scan filme avec le lecteur partagé et refuse un code qui n\'est pas un billet')

        # Lien ouvert par l'appareil photo du téléphone : droit au paiement ; un proche est reconnu.
        p.goto(f'http://127.0.0.1:{port}/#/payer?to={AB}&a=3'); p.wait_for_selector('.pay-name')
        check(p.is_visible('main >> text=Votre proche') and not p.is_visible('main button:has-text("Devenir proches")'), f'{name} : lien direct, fiche d\'un proche (rien à demander)')
        p.click('main button:has-text("Payer 3 Ẑen")'); p.wait_for_selector('#credit .cr-form')
        check(p.is_visible('#credit >> text=Léa, votre proche') and p.input_value('#cr-amount') == '3', f'{name} : lien direct, proche reconnu, montant rempli')
        p.keyboard.press('Escape')

        # Se rencontrer : depuis Proches ; mon code ; une demande reçue ; chercher dans la constellation ; code scanné.
        MEET.update(mine={AB, 'cd' * 32}, back={AB, 'cd' * 32}, met={}, asks=['34' * 32]); STATE['posts'].clear()
        p.goto(f'http://127.0.0.1:{port}/#/people'); p.wait_for_selector('main a[href="#/rencontre"]')
        p.click('main a[href="#/rencontre"]'); p.wait_for_selector('.pay-qr img')
        check(p.get_attribute('.pay-tabs button[data-v=code]', 'aria-pressed') == 'true', f'{name} : Se rencontrer s\'ouvre sur mon code')
        check(p.is_visible('text=une photo envoyée au loin ne vaut pas rencontre'), f'{name} : le code dit qu\'il change (anti-photo)')
        p.wait_for_selector('text=Hugo vient de vous ajouter')
        p.screenshot(path=str(OUT / f'meet-code-{name}.png'), full_page=True)
        p.click('main li:has-text("Hugo") button:has-text("Accepter")'); p.wait_for_selector('text=Hugo vient de vous ajouter', state='detached')
        check((f'/api/v1/me/meet/{"34" * 32}', {}) in STATE['posts'], f'{name} : demande acceptée d\'un geste (nomination, sans preuve)')
        p.click('.pay-tabs button[data-v=find]'); p.fill('#meet-q', 'zo'); p.wait_for_selector('.pay-pick')
        check('Zoé' in p.inner_text('.pay-found') and 'autre station' in p.inner_text('.pay-found'), f'{name} : recherche dans la constellation (autre station signalée)')
        check(p.is_visible('text=sans rencontre vérifiée'), f'{name} : la liste dit qu\'elle ne vaut pas rencontre')
        p.click('.pay-pick'); p.wait_for_selector('.pay-name:has-text("Zoé")')
        p.click('main button:has-text("Devenir proches")'); p.wait_for_selector('text=Demande faite'); p.wait_for_selector('.pay-qr img')
        check(MEET['met'][Z] == dict(how='named', verified=False) and p.is_visible('.pay-qr img'), f'{name} : choisie dans la liste → nomination ; mon code s\'affiche pour qu\'elle le scanne')
        p.screenshot(path=str(OUT / f'meet-wait-{name}.png'), full_page=True)
        MEET['back'].add(Z); p.wait_for_selector('main >> text=Votre proche', timeout=10000)
        check(True, f'{name} : « Votre proche » dès que Zoé accepte, sans recharger')
        slot = int(__import__('time').time()) // 120
        p.goto(f'http://127.0.0.1:{port}/#/rencontre?to={EF}&m={slot}.{"cd" * 64}'); p.wait_for_selector('.pay-name')
        check(p.is_visible('text=votre rencontre sera vérifiée'), f'{name} : code « rencontre » scanné : la rencontre sera vérifiée')
        p.click('main button:has-text("Devenir proches")'); p.wait_for_selector('text=Demande faite')
        check((f'/api/v1/me/meet/{EF}', {'proof': f'{slot}.' + 'cd' * 64}) in STATE['posts'] and MEET['met'][EF]['how'] == 'qr', f'{name} : la preuve de présence part avec l\'ajout')
        p.goto(f'http://127.0.0.1:{port}/#/rencontre?to={"56" * 32}&m=1.{"cd" * 64}'); p.wait_for_selector('.pay-name')
        p.click('main button:has-text("Devenir proches")'); p.wait_for_selector('text=ce code a expiré')
        check('56' * 32 not in MEET['mine'], f'{name} : code expiré : dit clairement, rien n\'est ajouté')
        # Mes cercles : défauts (ami réciproque = Ami, suivi sans retour = Contact), ranger d'un geste, effet sur le paiement.
        MEET.update(mine={AB, 'cd' * 32, Z}, back={AB, 'cd' * 32}, ranks={}, met={AB: dict(how='qr', verified=True)})
        p.goto(f'http://127.0.0.1:{port}/#/people'); p.click('main a[href="#/cercles"]'); p.wait_for_selector('.ci-list')
        rings = p.locator('.ci-ring')
        check('Léa' in rings.nth(2).inner_text() and 'Marc' in rings.nth(2).inner_text() and 'Zoé' in rings.nth(4).inner_text(), f'{name} : amis réciproques en « Ami », suivi sans retour en « Contact »')
        check('rencontré en personne' in rings.nth(2).inner_text() and 'pas encore réciproque' in rings.nth(4).inner_text(), f'{name} : rencontre vérifiée et lien non réciproque signalés')
        check(p.is_visible('text=pas même eux'), f'{name} : l\'écran dit que c\'est privé')
        wot = p.locator('#ci-wot').inner_text()
        check('3 sur 5' in wot and 'Montrez votre code' in wot and 'Léa, Marc, Ana' in wot and 'pas encore actif' in wot, f'{name} : toile de confiance : où j\'en suis, qui me tient, dividende pas encore actif')
        p.select_option(f'.ci-ring >> nth=2 >> li:has-text("Léa") select', '5'); p.wait_for_function("document.querySelectorAll('.ci-ring')[0].innerText.includes('Léa')")
        check(MEET['ranks'] == {AB: 5}, f'{name} : Léa rangée en Famille d\'un geste')
        p.screenshot(path=str(OUT / f'circles-{name}.png'), full_page=True)
        STATE['wot'] = dict(member=True, root=False, certs=6, needed=5, reached_pct=90, mutual=4, certifiers=['Léa'], web=dict(members=43, referees=30, roots=4))
        p.reload(); p.wait_for_selector('#ci-wot')
        check('Vous êtes dans la toile : 6 rencontres' in p.locator('#ci-wot').inner_text(), f'{name} : membre de la toile')
        p.locator('#ci-wot').screenshot(path=str(OUT / f'wot-{name}.png')); STATE.pop('wot')
        p.goto(f'http://127.0.0.1:{port}/#/payer?to={AB}&a=2'); p.wait_for_selector('.pay-level select')
        check(p.input_value('.pay-level select') == '5', f'{name} : la fiche montre son cercle (Famille)')
        p.click('main button:has-text("Payer 2 Ẑen")'); p.wait_for_selector('#credit .cr-form')
        check(p.is_visible('#credit >> text=Léa, votre proche (Famille)'), f'{name} : au paiement, le cercle rassure')
        p.keyboard.press('Escape')
        p.goto(f'http://127.0.0.1:{port}/#/payer?to={"cd" * 32}'); p.wait_for_selector('.pay-level select')
        p.select_option('.pay-level select', '2'); p.wait_for_function("document.querySelector('.pay-level select')?.value === '2'")
        p.click('main button:has-text("Payer")'); p.wait_for_selector('#credit .cr-form')
        check(p.is_visible('#credit >> text=une connaissance'), f'{name} : une connaissance : on prévient de vérifier')
        p.keyboard.press('Escape')
        overflow = p.evaluate('document.documentElement.scrollWidth > window.innerWidth')
        check(not overflow, f'{name} : pas de défilement horizontal')
        c.close()
    b.close()
srv.shutdown()
check(not errors, 'aucune erreur de console : ' + '; '.join(errors[:3]))
print(f'{len(failures)} échec(s)')
sys.exit(1 if failures else 0)
