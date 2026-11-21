"""Test de fumée navigateur du déménagement (« Je viens de Google ou d'Apple ») : une vraie archive Takeout en deux parties (zip64,
`.json` d'une photo rangé dans l'autre partie, album et « Photos from » en double, contacts, agenda, Drive) est lue dans Chromium.

    ~/.astro/bin/python tests/browser/smoke_move.py [dossier-des-captures]

Vérifie : la photo d'album et sa copie ne partent qu'une fois, rangées dans l'album ; dates, lieu et description repris (le lieu 0,0
de Google est ignoré par la station) ; contacts importés ; un objet CalDAV par événement, fuseau compris ; le document Drive rangé ;
une relance n'envoie rien de plus. Aucune erreur de console.
"""
import io, json, sys, tempfile, zipfile
from pathlib import Path
from PIL import Image
from mock_station import *  # noqa: F403 (données et gestionnaire simulés)

OUT = Path(sys.argv[1] if len(sys.argv) > 1 else '.')
srv, port = start()
failures, errors = [], []
def check(cond, msg):
    print(('OK   ' if cond else 'FAIL ') + msg)
    if not cond: failures.append(msg)

def jpg(color):
    b = io.BytesIO(); Image.new('RGB', (64, 48), color).save(b, 'JPEG'); return b.getvalue()

tmp = Path(tempfile.mkdtemp())
one, two, three, four = jpg((200, 40, 40)), jpg((40, 40, 200)), jpg((40, 200, 40)), jpg((90, 90, 90))
ics = ("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VTIMEZONE\r\nTZID:Europe/Paris\r\nBEGIN:STANDARD\r\nDTSTART:19701025T030000\r\nTZOFFSETFROM:+0200\r\nTZOFFSETTO:+0100\r\nEND:STANDARD\r\nEND:VTIMEZONE\r\n"
       "BEGIN:VEVENT\r\nUID:evt1@google.com\r\nDTSTART;TZID=Europe/Paris:20261201T100000\r\nSUMMARY:Marché\r\nEND:VEVENT\r\n"
       "BEGIN:VEVENT\r\nUID:evt2@google.com\r\nDTSTART:20261205T180000Z\r\nSUMMARY:Concert replié\r\n  sur deux lignes\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n")
with zipfile.ZipFile(tmp / 'takeout-001.zip', 'w', zipfile.ZIP_DEFLATED) as z:
    for name, data in [('Takeout/Google Photos/Vacances 2023/IMG_1.jpg', one), ('Takeout/Google Photos/Photos from 2023/IMG_1.jpg', one), ('Takeout/Google Photos/Photos from 2023/IMG_2.jpg', two),
                       ('Takeout/Google Photos/Anniv Lea/IMG_1(1).jpg', three), ('Takeout/Google Photos/Anniv Lea/metadata.json', json.dumps({"title": "Anniversaire de Léa"}).encode()),
                       ('Takeout/Google Photos/Trash/IMG_9.jpg', four),
                       ('Takeout/Contacts/Tous les contacts/Tous les contacts.vcf', b'BEGIN:VCARD\r\nVERSION:3.0\r\nFN:L\xc3\xa9a\r\nEND:VCARD\r\n'), ('Takeout/Agenda/moi.ics', ics.encode()), ('Takeout/archive_browser.html', b'<html>')]:
        with z.open(zipfile.ZipInfo(name), 'w', force_zip64=True) as f: f.write(data)
with zipfile.ZipFile(tmp / 'takeout-002.zip', 'w', zipfile.ZIP_DEFLATED) as z:
    z.writestr('Takeout/Google Photos/Vacances 2023/IMG_1.jpg.supplemental-metadata.json', json.dumps({"title": "IMG_1.jpg", "description": "Plage de Collioure", "photoTakenTime": {"timestamp": "1690000000"}, "geoData": {"latitude": 42.52, "longitude": 3.08}}))
    z.writestr('Takeout/Drive/Mes documents/Budget.pdf', b'%PDF-1.4 budget')
    # Doublon nommé à la manière de Google (le rang après l'extension) et fichier d'informations tronqué à 51 caractères.
    z.writestr('Takeout/Google Photos/Anniv Lea/IMG_1.jpg(1).json', json.dumps({"title": "IMG_1.jpg", "photoTakenTime": {"timestamp": "1700000000"}, "people": [{"name": "Léa"}, {"name": "Paul"}]}))
    z.writestr('Takeout/Google Photos/Photos from 2023/IMG_2.jpg.supplemental-metad.json', json.dumps({"title": "IMG_2.jpg", "photoTakenTime": {"timestamp": "1691000000"}}))
# Export Apple (privacy.apple.com) : Photo Details.csv, album en .csv, une image d'iCloud Drive, la corbeille.
heic = b'\x00\x00\x00\x18ftypheic' + b'\x00' * 40
with zipfile.ZipFile(tmp / 'icloud.zip', 'w', zipfile.ZIP_DEFLATED) as z:
    z.writestr('iCloud Photos/Photos/IMG_0001.HEIC', heic)
    z.writestr('iCloud Photos/Photo Details.csv', 'imgName,fileChecksum,favorite,hidden,originalCreationDate,viewCount,importDate\nIMG_0001.HEIC,abc,no,no,"Monday September 20,2021 12:00 PM GMT",1,"Monday September 20,2021 12:00 PM GMT"\n')
    z.writestr('iCloud Photos/Albums/Été.csv', 'Images\nIMG_0001.HEIC\n')
    z.writestr('iCloud Photos/Recently Deleted/IMG_0002.HEIC', heic + b'x')
    z.writestr('iCloud Drive/Docs/scan.jpg', four)

with sync_playwright() as pw:
    b = pw.chromium.launch()
    c = b.new_context(viewport=dict(width=400, height=860), color_scheme='dark', timezone_id='Europe/Paris')
    c.add_init_script("localStorage.setItem('basic', btoa('alice@example.org:jeton'))")
    p = c.new_page()
    p.on('pageerror', lambda e: errors.append(str(e)))
    p.on('console', lambda m: m.type == 'error' and 'Service Worker' not in m.text and errors.append(m.text))
    STATE['dav'] = {}; STATE['meta'] = []; STATE['posts'].clear()
    p.goto(f'http://127.0.0.1:{port}/#/move'); p.wait_for_selector('.mv-drop')
    p.set_input_files('input[type=file]', [str(tmp / 'takeout-001.zip'), str(tmp / 'takeout-002.zip')])
    p.wait_for_selector('.mv-plan'); p.screenshot(path=str(OUT / 'move-plan.png'), full_page=True)
    plan = p.inner_text('.mv-plan')
    check('4 fichiers' in plan and 'dates et lieux retrouvés' in plan and '1 carnet' in plan and '1 agenda' in plan, 'plan : ' + plan.replace('\n', ' | '))
    p.click('button:has-text("Déménager")'); p.wait_for_selector('text=Déménagement terminé', timeout=20000)
    p.screenshot(path=str(OUT / 'move-done.png'), full_page=True)
    dav = STATE['dav']
    photos = sorted(k for k in dav if k.startswith('/Photos/'))
    check(photos == ['/Photos/Google/2023/IMG_2.jpg', '/Photos/Google/Anniversaire de Léa/IMG_1(1).jpg', '/Photos/Google/Vacances 2023/IMG_1.jpg'], f'une seule copie, rangée dans son album (vrai nom), corbeille ignorée : {photos}')
    md = {m['path'].rsplit('/', 1)[1]: m for m in STATE['meta']}
    check(md.get('IMG_1(1).jpg', {}).get('desc') == 'Avec Léa, Paul' and md['IMG_1(1).jpg'].get('taken_at') == 1700000000 + 3600, f'doublon « (1) » relié à son .json, personnes reprises : {md.get("IMG_1(1).jpg")}')
    check(md.get('IMG_2.jpg', {}).get('taken_at') == 1691000000 + 7200, 'fichier d’informations tronqué reconnu')
    m1 = next((m for m in STATE['meta'] if m['path'].endswith('IMG_1.jpg')), {})
    check(m1.get('lat') == 42.52 and m1.get('desc') == 'Plage de Collioure' and m1.get('taken_at') == 1690000000 + 7200, f'date (heure de Paris), lieu, description : {m1}')
    cal = sorted(k for k in dav if k.startswith('/calendars/'))
    check(cal == ['/calendars/alice@example.org/default/evt1@google.com.ics', '/calendars/alice@example.org/default/evt2@google.com.ics'] and all(b'BEGIN:VTIMEZONE' in dav[k] and dav[k].count(b'BEGIN:VEVENT') == 1 for k in cal), f'un objet CalDAV par événement, fuseau compris : {cal}')
    check(b'Concert repli\xc3\xa9\r\n  sur deux lignes' in b''.join(dav[k] for k in cal), 'ligne repliée de l’agenda conservée')
    check(any(u == '/api/v1/book/import' for u, _ in STATE['posts']), 'contacts envoyés au carnet')
    check(dav.get('/Drive Google/Mes documents/Budget.pdf') == b'%PDF-1.4 budget', 'document Drive rangé')
    # Relance : rien de plus n'est envoyé.
    before = dict(dav)
    p.goto(f'http://127.0.0.1:{port}/#/write'); p.goto(f'http://127.0.0.1:{port}/#/move'); p.wait_for_selector('.mv-drop')
    p.set_input_files('input[type=file]', [str(tmp / 'takeout-001.zip'), str(tmp / 'takeout-002.zip')])
    p.wait_for_selector('.mv-plan'); p.click('button:has-text("Déménager")'); p.wait_for_selector('text=Déménagement terminé', timeout=20000)
    check(sorted(STATE['dav']) == sorted(before), 'une relance n’envoie rien de plus')
    # Export Apple.
    STATE['meta'] = []
    p.goto(f'http://127.0.0.1:{port}/#/write'); p.goto(f'http://127.0.0.1:{port}/#/move'); p.wait_for_selector('.mv-drop')
    p.set_input_files('input[type=file]', [str(tmp / 'icloud.zip')]); p.wait_for_selector('.mv-plan')
    check('Archive iCloud prête' in p.inner_text('main'), 'export Apple reconnu')
    p.click('button:has-text("Déménager")'); p.wait_for_selector('text=Déménagement terminé', timeout=20000)
    dav = STATE['dav']
    check('/Photos/iCloud/Été/IMG_0001.HEIC' in dav and not any('IMG_0002' in k for k in dav), 'photo iCloud dans son album, corbeille ignorée')
    check(dav.get('/iCloud Drive/Docs/scan.jpg') == four, 'image d’iCloud Drive rangée en document')
    ic = next((m for m in STATE['meta'] if m['path'].endswith('IMG_0001.HEIC')), {})
    check(ic.get('taken_at') == 1632139200 + 7200, f'date de Photo Details.csv (heure de Paris) : {ic}')
    c.close()

check(not errors, 'aucune erreur de console : ' + '; '.join(errors[:3]))
print('RESULT', 'FAIL' if failures else 'OK')
sys.exit(1 if failures else 0)
