"""Romans-photos du guide : de vraies captures de l'interface (station simulée de `tests/browser/mock_station.py`), mises
en cases avec une légende et une bulle, comme une bande dessinée.

    ~/.astro/bin/python docs/roman/roman_photo.py        → docs/img/roman-*.webp

Playwright, Chromium et Pillow sont requis. À relancer quand un écran raconté ici change.
"""
import base64, html, io, json, sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'tests' / 'browser'))
import mock_station as M  # noqa: E402
from PIL import Image  # noqa: E402
from playwright.sync_api import sync_playwright  # noqa: E402

OUT = ROOT / 'docs' / 'img'
SCENE = dict(who='alice', ask='none')
ALICE = dict(hex='aa' * 32, email='alice@exemple.org', captain=False, ai=False)
LEA = dict(hex='ab' * 32, email='lea@exemple.org', captain=False, ai=False)

# Ce que la station simulée répond en plus (ou autrement) pour les besoins du récit.
def extra_get(p):
    who = SCENE['who']
    if p == '/health': return dict(status='ok')
    if p == '/api/v1/me': return ALICE if who == 'alice' else LEA
    if p == '/api/v1/dashboard': return dict(sovereignty=dict(encrypted_files=SCENE.get('files', 128)))
    if p == '/api/v1/me/footprint': return dict(disk=dict(used_bytes=3 << 30, quota_bytes=20 << 30), privacy=dict(gps=0, sensitive=0))
    if p == '/api/v1/faces': return dict(enabled=True, unknown=0 if SCENE.get('calm') else 6, suggestions=[] if SCENE.get('calm') else [dict(name='Léa', score=0.74)], people=[dict(name='Léa')])
    if p == '/api/v1/friends': return dict(friends=[dict(hex='ab' * 32, label='Léa', hosted=False)])
    if p == '/api/v1/devices': return dict(devices=[], max=16)
    if p == '/api/v1/faces/pending-shares': return dict(pending=[] if SCENE.get('calm') else [dict(name='Léa', count=2)], auto_share=True)
    if p == '/api/v1/backup':
        if who == 'lea': return dict(mine=None, friends=[], share=None, hosting=[dict(owner='aa' * 32, label='Alice', status='accepted', used_bytes=2 << 30, quota_bytes=5 << 30, rescue=dict(station='lyon.coop'))])
        return dict(mine=dict(enabled=True, status='accepted', label='Léa', host='ab' * 32, files=128, backed_up_bytes=2 << 30, quota_bytes=5 << 30), friends=[dict(hex='ab' * 32, label='Léa')], hosting=[], share=None, rescue_friends=1)
    return None


class H(M.H):
    def do_GET(self):
        r = extra_get(self.path.split('?')[0])
        return self.send(200, r) if r is not None else super().do_GET()

    def do_POST(self):
        p = self.path
        if p == '/api/v1/register':
            n = int(self.headers.get('Content-Length', 0)); self.rfile.read(n)
            return self.send(200, dict(email='alice@exemple.org', token='JT', hex='aa' * 32, g1pub='G1', npub='npub1alice', nsec='nsec1…', nostrns='/ipns/k51', constellation='ORIGIN', lang='fr', card=M.CARD, card_svg=M.QR, **{'pass': '482913'}))
        if p == '/api/v1/restore/ask':
            n = int(self.headers.get('Content-Length', 0)); self.rfile.read(n)
            return self.send(200, dict(status=SCENE['ask'], sent=2, by='Léa'))
        return super().do_POST()


TERM = """<!doctype html><meta charset="utf-8"><style>body{margin:0;background:#11141c;color:#d8dee9;font:15px/1.5 ui-monospace,Menlo,monospace;padding:18px}
.p{color:#7fdca6}.d{color:#8790ad}.ok{color:#7fdca6}.h{color:#9bb0ff;font-weight:700}</style><body>%s</body>"""


def shots(port):
    """Captures « téléphone » (390 × 640, rendu ×2) de chaque case, par nom."""
    base, got = f'http://127.0.0.1:{port}', {}
    with sync_playwright() as pw:
        br = pw.chromium.launch()

        def phone(name, url, prep=None, who='alice', login=True, hold=500):
            SCENE['who'] = who
            ctx = br.new_context(viewport=dict(width=390, height=640), device_scale_factor=2, locale='fr-FR', service_workers='block')
            if login:
                ctx.add_init_script(f"localStorage.setItem('basic', btoa('{(ALICE if who == 'alice' else LEA)['email']}:jeton'))")
            pg = ctx.new_page(); pg.goto(base + url) if url else None
            if prep: prep(pg)
            pg.wait_for_timeout(hold)
            got[name] = pg.screenshot(); ctx.close()

        def term(name, body):
            ctx = br.new_context(viewport=dict(width=390, height=640), device_scale_factor=2); pg = ctx.new_page()
            pg.set_content(TERM.replace('%s', body)); got[name] = pg.screenshot(); ctx.close()

        # Fred, Capitaine.
        term('install', '<span class="p">fred@pi:~$</span> curl -fsSL …/install.sh | bash<br><br><span class="h">▸ Examen de la machine</span><br>'
             '  4 cœurs · 8 Go · SSD<br>  profil recommandé : <b>standard</b><br><br><span class="h">▸ Téléchargement et vérification</span><br>'
             '<span class="ok">  ✔ empreinte SHA-256 vérifiée</span><br><span class="ok">  ✔ installé : ~/.local/bin/astroportd</span><br><br>'
             '<span class="h">▸ Modèles locaux</span><br><span class="ok">  ✔ visages : SCRFD + ArcFace</span><br><br>'
             '<span class="h">▸ Plugins (facultatif)</span><br>  films, énergie, DNSLink : plus tard<br><br>'
             '<span class="h">▸ Configuration de la station</span><br>  → astroportd init')
        term('init', '<span class="p">fred@pi:~$</span> astroportd init<br><br><span class="h">① Rituel Y-Level</span><br><span class="ok">  ✔ SSH = IPFS = Ğ1 = Nostr</span><br><br>'
             '<span class="h">② Position de la station</span><br>  43.60, 1.44 · 20h12 solaire<br><br><span class="h">Constellation</span> : ORIGIN<br>'
             'Email du Capitaine : fred@exemple.org<br><br><span class="ok">✔ MULTIPASS du Capitaine créé</span><br>  PASS : <b>739104</b>  <span class="d">(notez-le)</span><br><br>'
             '<span class="h">③ Visio</span> : STUN + relais<br><span class="h">④ Réseau</span> : ouvrir 443 et 4101 sur la box<br><br>'
             '<span class="p">fred@pi:~$</span> astroportd serve<br><span class="ok">✔ station prête</span> → https://fred.exemple.org')
        # Alice rejoint la station, Léa reçoit ses photos.
        phone('stations', '/stations#nouveau', login=False)
        phone('register', '/register', lambda pg: (pg.wait_for_selector('#f:not([hidden])'), pg.fill('#email', 'alice@exemple.org'), pg.click('#go'), pg.wait_for_selector('#done:not([hidden])')), login=False)
        phone('home', '/#/')
        phone('tri', '/#/people/tri', lambda pg: pg.wait_for_selector('canvas, .tri, main section'), hold=900)
        phone('invite', f'/i/{M.TOKEN}', lambda pg: pg.wait_for_selector('#f:not([hidden])'), login=False)
        # Secours : Alice a perdu sa station, Léa confirme.
        def ask(pg):
            SCENE['ask'] = 'waiting'; pg.wait_for_selector('#f:not([hidden])'); pg.fill('#card', M.CARD); pg.wait_for_timeout(300)
        phone('restore', '/restore', ask, login=False)
        phone('lea', '/#/account?tab=securite', lambda pg: (pg.wait_for_selector('text=dit avoir perdu sa station'), pg.locator('label:has-text("je l\'ai reconnu")').click(),
                                                           pg.evaluate("document.querySelector('[role=alert]').scrollIntoView({block: 'start'})"), pg.evaluate('scrollBy(0, -12)')), who='lea')
        def done(pg):
            SCENE['ask'] = 'ready'; pg.wait_for_selector('#f:not([hidden])'); pg.fill('#card', M.CARD); pg.wait_for_selector('#done:not([hidden])')
        phone('reborn', '/restore', done, login=False)
        SCENE['calm'] = True
        phone('back', '/#/')
        br.close()
    return got


# Les trois récits : (titre, [(capture, légende, qui parle, bulle)]).
STORIES = {
    'roman-capitaine': ('Fred ouvre sa station', [
        ('install', 'Samedi matin. Fred branche un vieux PC.', '🧑‍✈️', 'Une commande, et le programme est là.'),
        ('init', 'Il lui donne une identité.', '🧑‍✈️', 'Je note mon PASS, puis je démarre.'),
        ('stations', 'Sa station rejoint la porte /stations.', '🧑‍✈️', 'Mes voisins la trouvent tout seuls.'),
        ('home', 'Chaque membre arrive sur son accueil.', '🧑‍✈️', 'Quatre gestes, et une liste pour bien démarrer.'),
    ]),
    'roman-membre': ('Alice rejoint la station, et ses photos trouvent Léa', [
        ('stations', 'Alice cherche une station près de chez elle.', '👩', '« Je suis nouveau » : celle de Fred a de la place.'),
        ('register', 'Un email, et son compte existe.', '👩', 'Je garde mon PASS et j\'imprime ma carte de secours.'),
        ('tri', 'Elle dépose ses photos ; la station propose des noms.', '👩', 'Je nomme une personne une fois : ses photos partent seules.'),
        ('invite', 'Léa n\'a pas encore de compte : elle reçoit un lien.', '👱‍♀️', 'Alice m\'envoie mes photos. Un clic, et j\'ai mon espace.'),
    ]),
    'roman-secours': ('J\'appelle un ami : Alice a perdu sa station', [
        ('restore', 'La station d\'Alice a brûlé. Sur une autre station, elle scanne sa carte imprimée.', '👩', 'J\'ai prévenu mes gardiens. J\'appelle Léa.'),
        ('lea', 'Léa reconnaît sa voix au téléphone.', '👱‍♀️', 'C\'est bien Alice : je coche, et je confirme.'),
        ('reborn', 'La part de Léa arrive d\'elle-même.', '👩', 'Mon compte renaît. Je note mon nouveau PASS.'),
        ('back', 'Amis, contacts et fichiers reviennent au fil des heures.', '👩', 'Tout est là, et personne n\'a rien lu.'),
    ]),
}

PAGE = """<!doctype html><meta charset="utf-8"><style>
body{margin:0;background:#fff8e7;font-family:system-ui,-apple-system,"Segoe UI",Roboto,sans-serif;color:#1b1f2a}
.strip{width:560px;padding:14px 16px 18px;box-sizing:border-box;display:grid;grid-template-columns:1fr 1fr;gap:16px 14px}
h1{grid-column:1/-1;margin:2px 0 0;font-size:21px;font-weight:800;letter-spacing:-.01em}
.case{position:relative;background:#fff;border:3px solid #1b1f2a;border-radius:8px;padding:7px 7px 9px;box-shadow:4px 4px 0 #1b1f2a}
.cap{display:flex;gap:6px;align-items:baseline;background:#ffe066;border:2px solid #1b1f2a;border-radius:3px;padding:4px 7px;font-size:12.5px;font-weight:700;line-height:1.25;min-height:32px;margin-bottom:7px}
.n{flex:none;display:inline-grid;place-items:center;width:20px;height:20px;border-radius:50%;background:#1b1f2a;color:#ffe066;font-size:12px}
.shot{display:block;width:100%;border:2px solid #1b1f2a;border-radius:12px}
.bub{position:relative;margin:10px 4px 0 30px;background:#fff;border:2.5px solid #1b1f2a;border-radius:16px;padding:6px 10px;font-size:13px;font-weight:650;line-height:1.3}
.bub::before{content:"";position:absolute;left:-12px;top:12px;border:7px solid transparent;border-right:11px solid #1b1f2a}
.who{position:absolute;left:-36px;top:-2px;font-size:26px}
</style><body>%s</body>"""


def compose(br, name, title, cases, imgs):
    cells = []
    for i, (shot, cap, who, says) in enumerate(cases, 1):
        uri = 'data:image/png;base64,' + base64.b64encode(imgs[shot]).decode()
        cells.append(f'<div class="case"><div class="cap"><span class="n">{i}</span><span>{html.escape(cap)}</span></div>'
                     f'<img class="shot" src="{uri}" alt=""><div class="bub"><span class="who">{who}</span>{html.escape(says)}</div></div>')
    pg = br.new_page(viewport=dict(width=560, height=400), device_scale_factor=2)
    pg.set_content(PAGE.replace('%s', f'<div class="strip"><h1>{html.escape(title)}</h1>{"".join(cells)}</div>'))
    pg.wait_for_timeout(200)
    png = pg.locator('.strip').screenshot(); pg.close()
    im = Image.open(io.BytesIO(png)).convert('RGB')
    im.save(OUT / f'{name}.webp', 'WEBP', quality=78, method=6)
    print(f'{name}.webp', im.size, (OUT / f'{name}.webp').stat().st_size // 1024, 'Ko')


def main():
    srv = M.ThreadingHTTPServer(('127.0.0.1', 0), H)
    M.threading.Thread(target=srv.serve_forever, daemon=True).start()
    imgs = shots(srv.server_address[1])
    with sync_playwright() as pw:
        br = pw.chromium.launch()
        for name, (title, cases) in STORIES.items():
            compose(br, name, title, cases, imgs)
        br.close()
    srv.shutdown()


if __name__ == '__main__':
    main()
