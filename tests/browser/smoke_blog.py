"""Test de fumée navigateur de Mon blog : note et article publiés, partage du téléphone rangé en favori privé (fichier .url exact),
adresse Nostr (« Répondre » d'un blog ami) qui ouvre l'éditeur en mode réponse, lecture « Chez mes amis ». Aucune erreur de console.

    ~/.astro/bin/python tests/browser/smoke_blog.py [dossier-des-captures]

Hors de la CI (Playwright et Chromium requis) : à lancer avant de publier une modification de m-write.js ou m-favs.js.
"""
import sys
from pathlib import Path
from mock_station import *  # noqa: F403 (données et gestionnaire simulés)

OUT = Path(sys.argv[1] if len(sys.argv) > 1 else '.')
srv, port = start()
errors, failures = [], []
def check(cond, msg):
    print(('OK   ' if cond else 'FAIL ') + msg)
    if not cond: failures.append(msg)

SEED = """async () => { const c = await caches.open('astro-share'); await c.put('/ui/share/meta', new Response(JSON.stringify({ title: 'Recette du pain', text: '', url: 'https://exemple.org/pain', files: 0 }))); }"""
# nevent (NIP-19) d'une note : identifiant b9f5…, auteur 7e7e…, relai wss://r.example.
NEVENT = 'nevent1qqstna2yrezu5wghjvswqqculvvwxsrcvu7uc0f78gan4xqhvz49d9sppamhxue69uhhytn90psk6urvv5pzqln7n3p2jxl77x06j209lksmwtswhsdycy2pvulz09prfkr2mh6wpxk3mj'
with sync_playwright() as pw:
    b = pw.chromium.launch()
    for name, vp, scheme in [('desk', dict(width=1280, height=900), 'light'), ('m400', dict(width=400, height=860), 'dark')]:
        STATE['posts'].clear(); STATE['dav'] = {}
        c = b.new_context(viewport=vp, color_scheme=scheme); c.add_init_script("localStorage.setItem('basic', btoa('alice@example.org:jeton'))")
        p = c.new_page()
        p.on('pageerror', lambda e: errors.append(str(e)))
        p.on('console', lambda m: m.type == 'error' and 'Service Worker' not in m.text and errors.append(m.text))
        # Note et article.
        p.goto(f'http://127.0.0.1:{port}/#/write'); p.wait_for_selector('textarea')
        p.fill('textarea', 'Four allumé.'); p.click('button:text-is("Publier la note")'); p.wait_for_timeout(300)
        p.goto(f'http://127.0.0.1:{port}/#/write?tab=article'); p.wait_for_selector('.wr-title')
        p.fill('.wr-title', 'Pourquoi une coopérative ?'); p.fill('.wr-md', '## Trois tiers\nTexte.'); p.wait_for_timeout(700)
        check(p.inner_text('.wr-preview').startswith('Trois tiers'), f'{name} : aperçu en direct')
        p.select_option('select[aria-label="Qui peut lire"]', 'friends')
        p.click('button:text-is("Publier")'); p.wait_for_timeout(500)
        sent = [x for u, x in STATE['posts'] if u == '/api/v1/me/blog/publish']
        check(sent[0] == dict(kind=1, content='Four allumé.', tags=[]), f'{name} : note publiée')
        art = next((x for x in sent if x['kind'] == 30023), {})
        check(['d', 'pourquoi-une-cooperative'] in art.get('tags', []) and art.get('audience') == 'friends', f'{name} : article réservé aux amis, identifiant tiré du titre')
        # Partage du téléphone → favori privé, fichier .url exact (pas du JSON).
        p.goto(f'http://127.0.0.1:{port}/#/'); p.wait_for_timeout(300); p.evaluate(SEED)
        p.goto(f'http://127.0.0.1:{port}/#/write?shared=1'); p.wait_for_selector('.wr-choice')
        p.screenshot(path=str(OUT / f'blog-{name}-share.png'))
        p.click('button:has-text("Ranger") >> nth=-1'); p.wait_for_selector('.wr-favs li a')
        check(STATE['dav'].get('/Favoris/Recette du pain.url') == b'[InternetShortcut]\r\nURL=https://exemple.org/pain\r\n', f'{name} : favori rangé en raccourci .url')
        check('exemple.org' in p.inner_text('.wr-favs'), f'{name} : le favori est listé')
        # « Répondre » d'un blog ami : l'adresse Nostr ouvre l'éditeur de SA station en mode réponse.
        p.goto(f'http://127.0.0.1:{port}/#/write?nostr=web%2Bastro%3A{NEVENT}'); p.wait_for_timeout(600)
        check('Vous répondez à une note' in p.inner_text('main'), f'{name} : adresse Nostr → mode réponse')
        # Chez mes amis : un article réservé se lit ici.
        p.goto(f'http://127.0.0.1:{port}/#/write?tab=friends'); p.wait_for_selector('.wr-friend')
        p.click('.wr-friend >> nth=1'); p.wait_for_selector('.wr-reader h1')
        check('Réservé pour vous' in p.inner_text('.wr-reader'), f'{name} : article réservé lu chez mes amis')
        p.screenshot(path=str(OUT / f'blog-{name}-friends.png'), full_page=True)
        # Bêta : la carte de l'accueil, « Annuler » ferme même vide, un retour part avec les bons champs.
        p.goto(f'http://127.0.0.1:{port}/#/'); p.wait_for_selector('.bt-card button')
        check('Astroport est en bêta' in p.inner_text('.bt-card'), f'{name} : la bêta est annoncée')
        p.click('.bt-card button >> nth=0'); p.wait_for_selector('dialog.bt-dialog[open]')
        p.click('dialog.bt-dialog button:text-is("Annuler")'); p.wait_for_timeout(200)
        check(p.locator('dialog.bt-dialog').count() == 0, f'{name} : « Annuler » ferme le formulaire vide')
        p.click('.bt-card button >> nth=0'); p.fill('dialog.bt-dialog input:not([type])', 'Photo à l’envers'); p.fill('dialog.bt-dialog textarea', 'Après le déménagement, une photo est tournée.')
        p.click('dialog.bt-dialog button:text-is("Envoyer")'); p.wait_for_timeout(400)
        fb = [x for u, x in STATE['posts'] if u == '/api/v1/feedback']
        check(fb and fb[-1]['source'] == 'astroportd' and fb[-1]['category'] == 'bug' and fb[-1]['share_identity'] is False, f'{name} : retour envoyé : {fb[-1:]}')
        p.screenshot(path=str(OUT / f'blog-{name}-beta.png'), full_page=True)
        c.close()

check(not errors, 'aucune erreur de console : ' + '; '.join(errors[:3]))
print('RESULT', 'FAIL' if failures else 'OK')
sys.exit(1 if failures else 0)
