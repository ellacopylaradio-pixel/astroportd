"""Station simulée pour les essais navigateur : API factice, fichiers statiques RÉELS de la coque (`crates/astro-ui/web/`).

Utilisée par `smoke_ui.py` (test de fumée) et `docs/roman/roman_photo.py` (captures des romans-photos)."""
import base64, io, json, math, random, sys, threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from PIL import Image, ImageDraw
from playwright.sync_api import sync_playwright

WEB = Path(__file__).resolve().parents[2] / 'crates' / 'astro-ui' / 'web'
random.seed(7)
MIME = {'.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.svg': 'image/svg+xml', '.webmanifest': 'application/manifest+json'}

def make_faces():
    faces, fid = [], 1
    def add(name, pub, group, gsize, x, y, named, sug=None):
        nonlocal fid
        faces.append(dict(id=fid, name=name, pubkey=pub, named=named, path=f'/Photos/{fid}.jpg', bbox=[10, 10, 100, 100], x=x, y=y, group_id=group, group_size=gsize, suggest=sug))
        fid += 1
    for pi, name in enumerate(['Marie', 'Paul', 'Léa']):
        for _ in range(14 + pi * 6): add(name, f'{pi:064x}', None, None, random.uniform(-1, 1), random.uniform(-1, 1), True)
    for gi in range(10):
        n = random.randint(2, 9)
        for _ in range(n): add(f'Inconnu_{fid:08d}', None, f'g{gi:011d}', n, random.uniform(-1, 1), random.uniform(-1, 1), False)
    for _ in range(80): add(f'Inconnu_{fid:08d}', None, None, None, random.uniform(-1, 1), random.uniform(-1, 1), False)
    faces[-1]['suggest'] = dict(name='Marie', score=0.71)
    return faces

COOP = dict(exists=True, updated_at=1790000000, did='did:nostr:' + 'ab' * 32, unmanaged=['ZZ_LEGACY'], sections=[dict(title='Règle 3×1/3', fields=[dict(key=k, value=v, secret=False) for k, v in [('TREASURY_PERCENT', '33.33'), ('RND_PERCENT', '33.33'), ('ASSETS_PERCENT', '33.33'), ('CAPTAIN_BONUS_PERCENT', '1')]]), dict(title='OpenCollective', fields=[dict(key='OCSLUG', value='uplanet', secret=False), dict(key='OCAPIKEY', secret=True, set=True, readable=True)]),
    dict(id='ovh', title='DNSLink OVH', level='essential', fields=[dict(key=k, secret=True, set=True, readable=True) for k in ('OVH_APP_KEY', 'OVH_APP_SECRET', 'OVH_CONSUMER_KEY')] + [dict(key='OVH_ZONE', value='coop.example', secret=False)])])
ROUTE = dict(mesh=True, stations=[
    dict(peer='12D3KooWAmiLyon', name='Station de Marie', power_score=40, distance_km=3, services=[dict(name='ssh', local=None), dict(name='ollama', local=None)]),
    dict(peer='12D3KooWAmiNantes', name='Atelier Nantes', power_score=80, distance_km=520, services=[dict(name='ollama', local=None), dict(name='comfyui', local=None)]),
    dict(peer='12D3KooWAmiFerme', name='La Ferme', power_score=10, distance_km=40, services=[]),
    dict(peer='12D3KooWAmiLoin', name='Station éteinte', power_score=5, distance_km=None, services=dict(error='délai dépassé'))])
ADMIN = dict(face=dict(mode='off', label=None, locked=None, onnx_compiled=True, models='/m', library=None, pending=3, failed=0),
             kinds=dict(kinds=[dict(kind=1, label='Note publique', read='public', write='circle', project=True, default='read=public write=circle project=on', source='défaut'), dict(kind=7, label='Réaction (+N = Ẑen)', read='circle', write='circle', project=True, default='read=circle write=circle project=on', source='défaut')],
                        choices=dict(read=['public', 'addressed', 'friends', 'circle', 'deny'], write=['circle', 'open', 'deny']), fixed={'1059': 'x', '22242': 'y'}))
ADMIN.update(route=ROUTE, tunnels=None, tasks=None, assist=dict(services=['ssh'], friends=[], incoming=[], outgoing=[]))  # réglages facultatifs de m-options.js : absents, l'écran s'en passe sans erreur
ADMIN['locks'] = dict(mode='village', plane=dict(origin=True, kubo=True, pool_peers=1, p2p=True), gates=dict(register=True, register_cli=True, crawl=True, n2=True, mutual_only=False, photos_auto=True, open_writes=[]), people=dict(hosted=4, n1=9, n1_mutual=5, n2=37, n2_known=37), faces=dict(auto_off_members=1), public=dict(files=12, unserved=0, sensitive=3, members=1))
ACCOUNT = dict(hex='aa' * 32, npub='npub1test', email='alice@example.org', role='multipass', g1pub='G1TEST', home_station='Qm:ab', lang='fr', langs=['fr', 'en', 'es'], profile=dict(name='Alice', about='Bonjour', picture=None),
               dav=dict(url='/dav', login='alice@example.org'), token_generation=0, keys_exportable=True, has_g1_key=True, relays=[dict(url='wss://s.example', read=True, write=True)])
CONTACTS = dict(contacts=[dict(path='/Contacts/Lea.vcf', name='Léa Martin', emails=['lea@example.org'], pubkey='ab' * 32, via='vcard', g1pub=None, warning=None, hosted_here=None, friends=True, faces=12, faces_linked=12),
                          dict(path='/Contacts/Paul.vcf', name='Paul', emails=[], pubkey=None, via=None, g1pub=None, warning=None, hosted_here=None, friends=False, faces=3, faces_linked=0)],
                people_without_contact=[dict(name='Nina', faces=4)], carddav=dict(url='https://s.example/dav/addressbooks/alice%40example.org/contacts/', server='https://s.example/dav/', login='alice@example.org'))
INVITES = dict(invites=[dict(token='1' * 32, label='Marc', email=None, contact_path=None, count=5, state='claimed', created_at=1790000000, expires_at=1791000000, opened_at=1790000100, claimed_at=1790000200, url='https://s.example/i/' + '1' * 32),
                        dict(token='2' * 32, label='Zoé', email=None, contact_path=None, count=3, state='opened', created_at=1790000000, expires_at=1791000000, opened_at=1790000100, claimed_at=None, url='https://s.example/i/' + '2' * 32)])
TOKEN = 'a1' * 16
QR = '<?xml version="1.0"?><svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10"><rect width="10" height="10"/></svg>'
CARD = 'astro-share:v1|alice@example.org|' + 'ab' * 32 + '|isolated|0123456789abcdef|1|c2VjcmV0c2VjcmV0|abcd'
STATE = dict(faces=make_faces(), posts=[], deleted=[], chat=[])
# Lecture des documents : vue du membre (état modifiable par le test) et réglages du Capitaine.
READING = dict(state='off', blocked=None, stations=[], local=[], progress=dict(read=0, by_friend=0, failed=0, waiting=37, more=False), photos=None, tasks=[dict(id='ocr', ready=True)], paused=False, pack=None)
OFFERS = dict(stations=[dict(peer='12D3KooWMarie', name='La station de Marie', distance_km=12.4, power=31.0), dict(peer='12D3KooWAtelier', name='Atelier du Lez', distance_km=None, power=12.0)], silent=1)
ACCOUNTS = [dict(hex='bb' * 32, email='bob@example.org', status='cooperative_member', quota_bytes=10 ** 10, used_bytes=2 * 10 ** 9, unlimited=False, weekly_zen=1, files=420),
            dict(hex='cc' * 32, email='chloe@example.org', status='active_rental', quota_bytes=10 ** 10, used_bytes=10 ** 8, unlimited=False, weekly_zen=1, files=12)]
ADMIN['index'] = dict(enabled=True, default=True, engine=[], lend=None, members=[dict(hex='bb' * 32, email='bob@example.org', enabled=True, own=False, read=120, friend='La station de Marie'), dict(hex='cc' * 32, email='chloe@example.org', enabled=False, own=True, read=0, friend=None)])

def people():
    by = {}
    for f in STATE['faces']:
        if f['named']:
            p = by.setdefault(f['pubkey'] or f['name'], dict(name=f['name'], pubkey=f['pubkey'], faces=0, photos=set()))
            p['faces'] += 1; p['photos'].add(f['path'])
    return sorted([dict(name=p['name'], pubkey=p['pubkey'], faces=p['faces'], photos=len(p['photos'])) for p in by.values()], key=lambda p: -p['photos'])

def thumb(seed):
    im = Image.new('RGB', (96, 96), tuple(random.Random(seed).randrange(60, 220) for _ in range(3)))
    d = ImageDraw.Draw(im); d.ellipse((24, 20, 72, 80), fill=(240, 210, 190)); d.ellipse((38, 40, 46, 48), fill=(40, 40, 40)); d.ellipse((54, 40, 62, 48), fill=(40, 40, 40))
    b = io.BytesIO(); im.save(b, 'JPEG'); return 'data:image/jpeg;base64,' + base64.b64encode(b.getvalue()).decode()

REPL = dict(mine=dict(at=1, events=30, replicated=27, unreachable=1, peers=[
    dict(node_id='12D3A', name='poppy', captain='ami@example.org', level=1, have=27, error=None),
    dict(node_id='12D3B', name='sagittarius', captain='fred@example.org', level=2, have=12, error=None),
    dict(node_id='12D3C', name='perdue', captain=None, level=None, have=None, error='tunnel : connexion fermée')],
    media=dict(files=8, replicated=5, lookup_failed=1, peers=[dict(node_id='12D3A', name='poppy', have=5)])),
    crawl=dict(at=1, ms=4200, relays=3, keys=40, fetched=120, inserted=12, errors=[dict(relay='p2p://12D3C', key='ab', reason='connexion fermée')]),
    routes=[dict(target='p2p://12D3A|wss://relay.poppy.example', via='p2p', at=1, error=None), dict(target='p2p://12D3C', via='down', at=1, error='tunnel : connexion fermée')])

# Médiathèque : deux films, une série de deux épisodes, un mix de trois morceaux (fiches résumées comme les rend la station).
def _wav(seconds=2, rate=8000):
    import math, struct
    n = seconds * rate
    data = b''.join(struct.pack('<h', int(8000 * math.sin(2 * math.pi * 440 * i / rate))) for i in range(n))
    return b'RIFF' + struct.pack('<I', 36 + len(data)) + b'WAVEfmt ' + struct.pack('<IHHIIHH', 16, 1, 1, rate, rate * 2, 2, 16) + b'data' + struct.pack('<I', len(data)) + data
WAV = _wav()
def _m(path, typ, title, **kw):
    base = dict(path=path, fiche=path.rsplit('.', 1)[0] + '.media.json', type=typ, title=title, genres=[], keywords=[], cast=[], directors=[], art={}, subtitles=[], level='private', duration_s=245, mime='audio/mpeg' if typ == 'music' else 'video/mp4')
    return {**base, **kw}
MEDIA = [
    _m('/Vidéos/Films/Le Voyage de Chihiro (2001).mp4', 'film', 'Le Voyage de Chihiro', year=2001, duration_s=7500, genres=['Animation', 'Fantastique'], directors=['Hayao Miyazaki'], cast=['Rumi Hiiragi', 'Miyu Irino'], keywords=['esprits'], overview="Chihiro, dix ans, découvre un monde peuplé d'esprits.", **{'art_': 1}),
    _m('/Vidéos/Autres/Atelier vélo (2024).mp4', 'video', 'Atelier vélo', year=2024, duration_s=600, level='circle'),
    _m('/Vidéos/Séries/Kaamelott/Saison 01/Kaamelott S01E01.mp4', 'episode', 'Heat', series=dict(name='Kaamelott', season=1, episode=1), duration_s=210),
    _m('/Vidéos/Séries/Kaamelott/Saison 01/Kaamelott S01E02.mp4', 'episode', 'Les Tartes aux myrtilles', series=dict(name='Kaamelott', season=1, episode=2), duration_s=212),
] + [_m(f'/Musique/DJ Test/Mon Mix/0{i} - {a} - {t}.mp3', 'music', t, music=dict(artist=a, album='Mon Mix', album_artist='DJ Test', track=i, mix_start_s=(i - 1) * 200), level='circle' if i == 1 else 'private')
     for i, (a, t) in enumerate([('Artiste A', 'Titre Un'), ('Artiste B', 'Titre Deux'), ('Artiste C', 'Titre Trois')], 1)]
for it in MEDIA:
    if it.pop('art_', None): it['art'] = dict(poster=it['path'].rsplit('.', 1)[0] + '.poster.jpg')
    if it['type'] == 'music': it['art'] = dict(cover=it['path'].rsplit('.', 1)[0] + '.cover.jpg')

# Chez mes amis : Léa et Paul ont le même film (deux sources), Léa partage aussi un morceau ; Paul a une station hors ligne.
FRIENDS = dict(items=[
    dict(event_id='e1' * 32, x='a1' * 32, type='film', title='Le Château dans le ciel (1986)', year=1986, duration_s=7440, genres=['Animation'], author='11' * 32, author_label='Léa',
         seeders=[dict(hex='11' * 32, label='Léa'), dict(hex='22' * 32, label='Paul')], copied=None, asked_at=None, error=None),
    dict(event_id='e2' * 32, x='a2' * 32, type='music', title='Artiste Z — Matin', duration_s=200, genres=[], music=dict(artist='Artiste Z', album='Aube', album_artist=None, track=1),
         author='11' * 32, author_label='Léa', seeders=[dict(hex='11' * 32, label='Léa')], copied=None, asked_at=None, error=None)],
    friends=[dict(hex='11' * 32, label='Léa', at=1, error=None), dict(hex='22' * 32, label='Paul', at=1, error='tunnel : connexion fermée')])

class H(BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def send(self, code, body, ctype='application/json'):
        if not isinstance(body, bytes): body = json.dumps(body).encode()
        self.send_response(code); self.send_header('Content-Type', ctype); self.send_header('Content-Length', str(len(body))); self.end_headers(); self.wfile.write(body)
    def do_GET(self):
        p = self.path.split('?')[0]
        if p == '/': return self.send(200, (WEB / 'index.html').read_bytes(), 'text/html')
        if p == '/register': return self.send(200, (WEB / 'register.html').read_bytes(), 'text/html')
        if p == '/api/v1/register': return self.send(200, dict(open=True, constellation='ORIGIN', accounts=1, max_accounts=500))
        if p == '/stations': return self.send(200, (WEB / 'stations.html').read_bytes(), 'text/html')
        if p == '/api/v1/stations': return self.send(200, dict(stations=[dict(station='ici', name='Station ici', self=True, open=True, used=1, total=500, at=1790000000, lat=43.6, lon=1.44), dict(station='tls', name='Toulouse', open=True, used=3, total=50, url='https://toulouse.invalid', join='https://toulouse.invalid/register', at=1790000000, lat=43.61, lon=1.45)]))
        if p == '/api/v1/locate':
            q = self.path.split('q=', 1)[1] if 'q=' in self.path else ''; here = f'http://127.0.0.1:{self.server.server_address[1]}'
            STATE.setdefault('locate', []).append(q)
            if q.startswith('alice'): return self.send(200, dict(where='here'))
            if q.startswith('bob'): return self.send(200, dict(where='elsewhere', url=here, station='bob-station.example'))
            if q.startswith('eve'): return self.send(200, dict(where='elsewhere', url='https://eteinte.invalid', station='eteinte.invalid'))
            return self.send(200, dict(where='unknown'))
        if p == '/restore': return self.send(200, (WEB / 'restore.html').read_bytes(), 'text/html')
        if p == '/claim': return self.send(200, (WEB / 'claim.html').read_bytes(), 'text/html')
        if p.startswith('/i/'): return self.send(200, (WEB / 'invite.html').read_bytes(), 'text/html')
        if p == '/api/v1/invites': return self.send(200, INVITES)
        if p == '/api/v1/invite/' + 'b2' * 16: return self.send(410, dict(error='ce lien a expiré : demandez-en un nouveau', code='invite_expired'))
        if p == f'/api/v1/invite/{TOKEN}': return self.send(200, dict(**{'from': 'alice'}, label='Léa', count=4, photos=4, thumbs=[0, 1], expires_at=1791000000, email_hint='l**@example.org'))
        if p.startswith(f'/api/v1/invite/{TOKEN}/thumb/'):
            im = Image.new('RGB', (16, 12), (180, 90, 60)); b = io.BytesIO(); im.save(b, 'PNG'); return self.send(200, b.getvalue(), 'image/png')
        if p.startswith('/ui/'):
            f = WEB / p[4:]
            return self.send(200, f.read_bytes(), MIME.get(f.suffix, 'application/octet-stream')) if f.is_file() else self.send(404, b'x', 'text/plain')
        if p == '/api/v1/me':
            if not (self.headers.get('Authorization') or '').startswith(('Basic ', 'Nostr ')): self.send(401, dict(error='authentification requise'))
            return self.send(200, dict(hex='aa' * 32, email='alice@example.org', captain=True, ai=STATE.get('ai', True)))
        if p == '/api/v1/me/ai': return self.send(200, dict(ollama_url=None, provider=None, has_key=False, model=None, declared=STATE.get('ai', True)))
        if p == '/api/v1/station/coop': return self.send(200, COOP)
        if p == '/api/v1/account/site': return self.send(200, dict(name='alice', files=0, has_index=True, visits=0, host_url=None, path_url='/site/alice/'))
        if p == '/api/v1/presence': return self.send(200, dict(reachable=True, friends=[]))
        if p == '/api/v1/faces': return self.send(200, dict(enabled=False, unknown=0, suggestions=[]))
        if p == '/api/v1/account': return self.send(200, ACCOUNT)
        if p == '/api/v1/account/images': return self.send(200, dict(images=[], used=0, quota=1000000))
        if p == '/api/v1/contacts': return self.send(200, CONTACTS)
        if p == '/api/v1/me/links': return self.send(200, dict(plane='origin', siblings=['zen1'], keys=[dict(plane='origin', hex='aa' * 32, via='fiche'), dict(plane='zen1', hex='ee' * 32, via='station')]))
        if p == '/api/v1/friends': return self.send(200, dict(friends=[dict(hex='ab' * 32, label='Léa', hosted=False), dict(hex='cd' * 32, label='Marc', hosted=True)]))
        if p == '/api/v1/chat':
            c = STATE['chat']; unread = sum(1 for m in c if not m['mine'] and not STATE.get('chat_read'))
            return self.send(200, dict(unread=unread, plane='origin', conversations=[dict(peer='ab' * 32, label='Léa', last=c[-1]['text'], at=c[-1]['at'], mine=c[-1]['mine'], unread=unread, plane='origin', channels=[dict(plane='origin', peer='ab' * 32, unread=unread), dict(plane='zen1', peer='cd' * 32, unread=0)])] if c else []))
        if p == '/api/v1/chat/messages': return self.send(200, dict(peer='ab' * 32, messages=STATE['chat'], more=False))
        if p.startswith('/api/v1/station/admin/'): k = p.rsplit('/', 1)[1]; return self.send(200, ADMIN[k]) if k in ADMIN else self.send(404, dict(error='non simulé'))
        if p == '/api/v1/faces/folders': return self.send(200, dict(folders=[dict(dir='/Images/2026/10', faces=12, todo=9), dict(dir='/Images/2026/09', faces=4, todo=0)]))
        if p == '/api/v1/faces/board':
            import time; time.sleep(STATE.get('delay', 0))
            return self.send(200, dict(enabled=True, dav='/dav', faces=STATE['faces'], people=people(), unknown=sum(1 for f in STATE['faces'] if not f['named']), groups_skipped=False, truncated=False))
        if p.startswith('/dav/'): return self.send(200, base64.b64decode('R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7'), 'image/gif')
        if p == '/api/v1/station/planes': return self.send(200, dict(configured=True, multi_process=True, front=True, planes=[dict(name='origin', origin=True, http='127.0.0.1:8090', db='/x/uplanet.db', db_exists=True, running=True), dict(name='zen1', origin=False, http='127.0.0.1:8091', db='/x/zen1/uplanet.db', db_exists=False, running=False)], restart_needed=True, pending=['zen1'], path='/x/planes.json', service=dict(unit=STATE.get('unit', 'serve'), path='/h/.config/systemd/user/astroportd.service', command='/usr/bin/astroportd planes', mismatch=STATE.get('unit', 'serve') == 'serve', restart=True)))
        if p.startswith('/api/v1/book'):
            STATE.setdefault('gets', []).append(self.path)
            lea = dict(name='Léa Martin', given='Léa', family='Martin', org='Atelier', title='', phones=[dict(kind='mobile', value='+33 6 12 34 56 78')], emails=[dict(kind='home', value='lea@example.org')], addresses=[], birthday='', note='', urls=[], groups=['Famille'], about='', picture='', nip05='', lud16='')
            if p == '/api/v1/book': return self.send(200, dict(total=3, letters=['L', 'P'], groups=[dict(name='Famille', count=1)], contacts=[
                dict(path='/Contacts/Moi.vcf', name='Alice', org='', title='', emails=['alice@example.org'], phones=[], groups=[], letter='A', me=True, pubkey='aa' * 32, via=None, warning=None, faces=0, picture=''),
                dict(path='/Contacts/Lea.vcf', name='Léa Martin', org='Atelier', title='', emails=['lea@example.org'], phones=lea['phones'], groups=['Famille'], letter='L', me=False, pubkey='ab' * 32, via='vcard', warning=None, faces=12, picture=''),
                dict(path='/Contacts/Paul.vcf', name='Paul Durand', org='', title='', emails=[], phones=[], groups=[], letter='P', me=False, pubkey=None, via=None, warning=None, faces=0, picture='')]))
            if p == '/api/v1/book/me': return self.send(200, dict(path='/Contacts/Moi.vcf', me=True, pubkey='aa' * 32, fields=dict(lea, name='Alice', given='Alice', family='', org='', phones=[], emails=[dict(kind='home', value='alice@example.org')], groups=[], about='Capitaine'), faces=0, hosted_here='alice@example.org', profile=None, diff=None, drift=False))
            if p == '/api/v1/book/card': return self.send(200, dict(path='/Contacts/Lea.vcf', me=False, pubkey='ab' * 32, via='vcard', fields=lea, faces=12, hosted_here=None, plane='origin', keys=[dict(plane='origin', hex='ab' * 32, via='station'), dict(plane='zen1', hex='cd' * 32, via='lien')], profile=dict(name='Léa M.'), drift=True, diff=[
                dict(key='name', label='Nom', mine='Léa Martin', theirs='Léa M.', same=False, can_take=True), dict(key='about', label='Présentation', mine='', theirs='Boulangère', same=False, can_take=True), dict(key='nip05', label='NIP-05', mine='', theirs='', same=True, can_take=False)]))
            if p == '/api/v1/book/photo': return self.send(200, dict(uri=None))
            if p == '/api/v1/book/photo-choices': return self.send(200, dict(faces=[7]))
            if p == '/api/v1/book/duplicates': return self.send(200, dict(groups=[dict(reasons=['même téléphone'], cards=[dict(path='/Contacts/Lea.vcf', name='Léa Martin', emails=[], phones=['+33 6 12']), dict(path='/Contacts/Lea2.vcf', name='L. Martin', emails=[], phones=['06 12'])])]))
        if p == '/api/v1/ucloud/memories': return self.send(200, dict(memories=[]))
        if p == '/api/v1/ucloud/list': return self.send(200, dict(dav='/dav', entries=[dict(name='Documents', path='/Documents', type='dir', mime=None, size=None, mtime=1790000000), dict(name='notes.txt', path='/notes.txt', type='file', mime='text/plain', size=120, mtime=1790000000)]))
        if p == '/api/v1/me/reading': return self.send(200, READING)
        if p == '/api/v1/me/reading/offers': return self.send(200, OFFERS)
        if p == '/api/v1/station/accounts': return self.send(200, dict(accounts=ACCOUNTS))
        if p == '/api/v1/station/hosting': return self.send(200, dict(default_quota_bytes=10 ** 10, default_weekly_zen=1))
        if p == '/api/v1/ucloud/search':  # un scan et un PDF retrouvés par le texte lu dedans (OCR), une photo par son nom
            from urllib.parse import parse_qs, urlsplit
            STATE.setdefault('searches', []).append(parse_qs(urlsplit(self.path).query))
            hit = lambda path, mime, x: dict(path=path, mime=mime, size=10, mtime=1790000000, taken_at=None, t=1790000000, thumb=None, lat=None, lon=None, **({'found': 'text', 'excerpt': x} if x else {}))
            items = [hit('/Scans/scan001.jpg', 'image/jpeg', '…FACTURE N° 2026-117 Électricité du hameau des Écluses…'), hit('/Documents/contrat-énergie.pdf', 'application/pdf', '…fourniture d’électricité, échéance trimestrielle…'), hit('/Photos/electricien.jpg', 'image/jpeg', None), dict(hit('/Photos/anniv.jpg', 'image/jpeg', None), found='seen'), dict(hit('/Vidéos/plage.mp4', 'video/mp4', None), found='seen', at_ms=83000), dict(hit('/Vidéos/Films/Le Voyage de Chihiro (2001).mkv', 'video/x-matroska', '…un voyage au pays des esprits…'), found='fiche', media=True)]
            if 'zanzibar' in self.path: return self.send(200, dict(items=[], next=None, understood=[], maybe=[items[3], items[1]]))
            return self.send(200, dict(items=items, next=None, understood=[dict(text='lea', **{'as': ['person']}, people=['Léa Martin'], month=None)], best=[items[1], items[4], items[0]]))
        if p == '/api/v1/ucloud/text': return self.send(200, dict(text='FACTURE N° 2026-117\nÉlectricité du hameau des Écluses' if 'scan001' in self.path else None))
        if p == '/api/v1/ucloud/timeline':
            q = self.path.split('ym=')[1] if 'ym=' in self.path else None
            if q is None: return self.send(200, dict(months=[dict(ym='2026-10', count=3), dict(ym='2026-09', count=2)]))
            base = 1790000000 if q == '2026-10' else 1787000000
            return self.send(200, dict(items=[dict(path=f'/Photos/{q}-{i}.jpg', mime='image/jpeg', size=10, mtime=base, taken_at=base - i * 90000, t=base - i * 90000, thumb=None, lat=None, lon=None) for i in range(3 if q == '2026-10' else 2)]))
        if p == '/api/v1/ucloud/map':
            if 'items=1' in self.path: return self.send(200, dict(items=[dict(path='/Photos/toulouse.jpg', mime='image/jpeg', size=10, mtime=1790000000, taken_at=1790000000, t=1790000000, thumb=None, lat=43.6, lon=1.44)]))
            return self.send(200, dict(points=[dict(lat=43.6, lon=1.44, n=12, t0=1, t1=2), dict(lat=35.68, lon=139.69, n=3, t0=1, t1=2), dict(lat=-33.9, lon=151.2, n=1, t0=1, t1=2)], months=[dict(ym='2026-09', n=6), dict(ym='2026-10', n=10)], geotagged=16, photos=40))
        if p == '/api/v1/ucloud/duplicates': return self.send(200, dict(exact=[], similar=[]))
        if p == '/api/v1/replication': return self.send(200, REPL)
        if p == '/api/v1/devices': return self.send(200, dict(devices=[dict(id='d1', label='Téléphone de Léa', root='/Photos/Léa', mode='write', created_at=1790000000, last_used=None)], max=16))
        if p == '/api/v1/organize': return self.send(200, dict(folders=[], job=None, runs=[dict(run_id='r1', at=1790000000, moved=7, undone=False, options={})]))
        if p == '/api/v1/pull': return self.send(200, dict(sources=[dict(id='s1', label='Mon Nextcloud', url='https://nc.example/remote.php/dav/files/fred', username='fred', remote_path='Photos', dest_prefix='/Imports/Mon Nextcloud', private_ok=False,
            job=dict(state='running', dry_run=False, started=1, finished=None, progress=dict(done=40, total=100, imported=30, skipped=8, errors=2), report=None, error=None), last=None)]))
        if p == '/api/v1/dashboard': return self.send(200, STATE.get('dashboard') or dict(sovereignty=dict(encrypted_files=12)))  # accueil d'un membre (m-home.js) ; Détails techniques : smoke_backup_rs.py
        if p == '/api/v1/ucloud/media/safety': return self.send(200, STATE.get('safety') or dict(files={}))  # copies des fichiers partagés : smoke_backup_rs.py
        if p == '/api/v1/backup': return self.send(200, STATE.get('backup') or dict(mine=None, friends=[], hosting=[], share=None))  # gardiens : smoke_guardians.py
        if p == '/api/v1/faces/pending-shares': return self.send(200, dict(pending=[], auto_share=True))
        if p == '/api/v1/activity': return self.send(200, b': ok\n\n', 'text/event-stream')
        if p == '/api/v1/ucloud/media': return self.send(200, dict(items=MEDIA, pending=0))
        if p == '/api/v1/ucloud/media/friends':
            # Une copie demandée arrive à la relecture suivante : rangée comme chez l'ami, elle entre dans la médiathèque.
            for it in FRIENDS['items']:
                if it['asked_at'] and not it['copied']:
                    it['copied'] = '/Vidéos/Films/Le Château dans le ciel (1986).mp4'
                    MEDIA.append(_m(it['copied'], 'film', 'Le Château dans le ciel', year=1986, duration_s=7440))
            return self.send(200, FRIENDS)
        if p == '/api/v1/ucloud/media/fiche':
            from urllib.parse import unquote
            it = next(i for i in MEDIA if i['fiche'] == unquote(self.path.split('path=', 1)[1]))
            f = dict(type=it['type'], title=it['title'], original_title=None, year=it.get('year'), overview=it.get('overview'), genres=it['genres'], keywords=it['keywords'])
            if it['type'] == 'music': f['music'] = dict(artist=it['music']['artist'], album=it['music']['album'], album_artist='DJ Test', track=it['music']['track'], composer=None)
            else: f.update(cast=it['cast'], directors=it['directors'], **({'series': dict(it['series'])} if it['type'] == 'episode' else {}))
            return self.send(200, f)
        if p.startswith('/play/'): return self.send(200, WAV, 'audio/wav')
        if p == '/api/v1/ucloud/media/trace':
            return self.send(200, dict(shared=True, **{'from': None}, copies=[dict(label='Léa', at=1790000000)], reshared=['Léa']))
        self.send(404, dict(error='simulé : ' + p))
    def do_DELETE(self):
        STATE['deleted'].append(self.path); self.send(200, dict(revoked=True))
    def do_POST(self):
        n = int(self.headers.get('Content-Length', 0)); body = json.loads(self.rfile.read(n) or b'{}'); p = self.path
        STATE['posts'].append((p, body))
        if p == '/api/v1/faces/thumbs': return self.send(200, dict(thumbs={str(i): thumb(i) for i in body['ids']}, failed=[]))
        if p == '/api/v1/station/planes': return self.send(200, dict(name=body['name'], http='127.0.0.1:8092', p2p='/ip4/0.0.0.0/tcp/4102', swarm_key_path='/x/zen2/swarm.key', created_key='/key/swarm/psk/1.0.0/\\n/base16/\\n' + 'ab' * 32))
        if p == '/api/v1/me/ai':
            STATE['ai'] = bool(body.get('ollama_url') or body.get('provider')); return self.send(200, dict(ollama_url=body.get('ollama_url'), provider=body.get('provider'), has_key=False, model=None, declared=STATE['ai']))
        if p == '/api/v1/book/delete-many': return self.send(200, dict(deleted=len(body['paths']), failed=[]))
        if p == '/api/v1/ucloud/rotate': return self.send(200, dict(rotated=len(body['paths']), failed=[]))
        if p == '/api/v1/ucloud/media/share':
            for it in MEDIA:
                if it['fiche'] in body['paths']: it['level'] = 'circle' if body['on'] else 'private'
            return self.send(200, dict(shared=body['on'], events=len(body['paths']), list=(body.get('list') or {}).get('kind')))
        if p == '/api/v1/ucloud/media/friends/request':
            it = next(i for i in FRIENDS['items'] if i['event_id'] == body['event_id'])
            if it['copied']: return self.send(200, dict(state='copied', path=it['copied']))
            it['asked_at'] = 1790000000; return self.send(200, dict(state='asked', at=it['asked_at']))
        if p == '/api/v1/ucloud/media/play': return self.send(200, dict(urls={x: f'/play/{i}' for i, x in enumerate(body['paths']) if not x.endswith('.jpg')}, expires_at=4102444800))
        if p == '/api/v1/ucloud/media/edit':
            it = next(i for i in MEDIA if i['fiche'] == body['path'])
            it.update(title=body['set']['title'], keywords=body['set'].get('keywords', it['keywords']), genres=body['set'].get('genres', it['genres']))
            return self.send(200, dict(item=it))
        if p.startswith('/api/v1/book'): return self.send(200, dict(path='/Contacts/Lea.vcf', taken=body.get('keys'), imported=2, duplicates=1, rejected=[], merged=True, removed=1, changed=1, fields=body.get('fields') or body))
        if p == '/api/v1/me/links': return self.send(200, dict(linked=[dict(plane='zen1', key='ee' * 32)], skipped=[]))
        if p == '/api/v1/chat/send':
            q = next((x for x in STATE['chat'] if x['id'] == body.get('reply')), None)
            m = dict(id=str(len(STATE['chat'])), mine=True, at=1790000000 + len(STATE['chat']) * 60, text=body['text'], state='ok', seen=False, reply=body.get('reply'), quote=q and dict(text=q['text'], mine=q['mine']), files=body.get('files', []))
            STATE['chat'].append(m); return self.send(200, m)
        if p == '/api/v1/chat/read': STATE['chat_read'] = True; return self.send(200, dict(read=1))
        if p == '/api/v1/chat/delete': STATE['chat'] = [m for m in STATE['chat'] if m['id'] != body['id']]; return self.send(200, dict(deleted=True))
        if p == '/api/v1/me/zen/recipients': return self.send(200, dict(recipients=[dict(to='cash', label='cash'), dict(to='ab' * 32, label='lea@example.org')], max_zen=10000))
        if p == '/api/v1/faces/delete-photos': return self.send(200, dict(deleted=len({f['path'] for f in STATE['faces'] if f['id'] in body['ids']}), missing=0, bytes=0))
        if p == '/api/v1/faces/assign':
            for f in STATE['faces']:
                if f['id'] in body['ids']: f.update(name=body['name'], pubkey=body.get('pubkey'), named=True, group_id=None, group_size=None, suggest=None)
            return self.send(200, dict(assigned=len(body['ids']), name=body['name']))
        if p == '/api/v1/station/coop': return self.send(200, COOP)
        if p == '/api/v1/devices': return self.send(200, dict(id='d2', label=body['label'], root=body['root'], mode=body['mode'], token='dv_' + 'ab' * 16, note='Ce jeton ne sera plus jamais affiché : copiez-le maintenant.'))
        if p == '/api/v1/devices/revoke': return self.send(200, dict(revoked=body['id']))
        if p == '/api/v1/organize/preview': return self.send(200, dict(total=7, placed=1, protected=4, unknown=1, capped=False, by_kind=[dict(kind='images', folder='Images', files=2, bytes=3000, sample=[dict(**{'from': '/Imports/A/p.jpg', 'to': '/Images/p.jpg'})]), dict(kind='documents', folder='Documents', files=5, bytes=900, sample=[])]))
        if p == '/api/v1/organize/undo': return self.send(200, dict(restored=7))
        if p.startswith('/api/v1/organize') or p.startswith('/api/v1/pull'): return self.send(200, dict(ok=True))
        if p == '/api/v1/station/admin/dnslink': return self.send(200, dict(ok=False, error='This credential is not valid') if body['values'].get('OVH_APP_SECRET') == 'faux' else dict(ok=True, zone='coop.example'))
        if p == '/api/v1/station/restart': return self.send(200, dict(restarting=True))
        if p == '/api/v1/station/planes/service': STATE['unit'] = 'planes'; return self.send(200, dict(state='switched', unit='/h/u', command='x', backup='/h/u.bak-1', carried=[], carried_to=None, to_carry=[], unreadable=False))
        if p == '/api/v1/station/admin/locks': ADMIN['locks']['mode'] = body['mode']; return self.send(200, ADMIN['locks'])
        if p == '/api/v1/station/admin/route':  # services ouverts par la constellation : ouvrir (station précise ou la plus puissante), fermer
            rows = ADMIN['route']['stations']
            if 'connect' in body:
                c = body['connect']; peer = c.get('peer') or max((r for r in rows if isinstance(r['services'], list) and any(x['name'] == c['service'] for x in r['services'])), key=lambda r: r['power_score'])['peer']
                if c['service'] == 'comfyui': return self.send(400, dict(error='la station amie refuse ce flux'))
                # Comme la station : port habituel (Ollama, ComfyUI), sinon port stable de la plage 24000–24999.
                STATE['last_connect'] = c; port = c.get('port') or {'ollama': 11434, 'comfyui': 8188}.get(c['service']) or 24000 + len(c['service'])
                for r in rows:
                    for x in (r['services'] if r['peer'] == peer and isinstance(r['services'], list) else []):
                        if x['name'] == c['service']: x['local'] = dict(addr=f'127.0.0.1:{port}', until=1791000000)
                return self.send(200, dict(peer=peer, service=c['service'], addr=f'127.0.0.1:{port}', until=1791000000, route=None))
            for r in rows:
                for x in (r['services'] if isinstance(r['services'], list) else []):
                    if x['name'] == body['disconnect']['service'] and r['peer'] == body['disconnect'].get('peer', r['peer']): x['local'] = None
            return self.send(200, dict(closed=1))
        if p == '/api/v1/station/admin/index':
            ix = ADMIN['index']
            for k in ('enabled', 'default'):
                if k in body: ix[k] = body[k]
            if 'member' in body:
                m = next(x for x in ix['members'] if x['hex'] == body['member']['hex'])
                m.update(enabled=ix['default'] if body['member']['enabled'] is None else body['member']['enabled'], own=body['member']['enabled'] is not None)
            return self.send(200, ix)
        if p == '/api/v1/me/reading':
            if 'pause' in body: READING['paused'] = body['pause']; return self.send(200, READING)
            if body.get('erase'): READING.update(paused=True, pack=None, progress=dict(READING['progress'], read=0), photos=READING['photos'] and dict(READING['photos'], seen=0)); return self.send(200, READING)
            if body.get('revoke') is True: READING['stations'] = []
            elif isinstance(body.get('revoke'), str): READING['stations'] = [s for s in READING['stations'] if s['peer'] != body['revoke']]
            else: READING['stations'] += [dict(peer=p, label=next(s['name'] for s in OFFERS['stations'] if s['peer'] == p), tasks=body['accept']['tasks'], since=1790000000, read=3) for p in body['accept']['peers']]
            READING.update(state='friend' if READING['stations'] else 'off', progress=dict(read=3 * len(READING['stations']), by_friend=3, failed=0, waiting=34, more=False))
            return self.send(200, READING)
        if p.startswith('/api/v1/station/admin/'): k = p.rsplit('/', 1)[1]; return self.send(200, ADMIN[k]) if k in ADMIN else self.send(404, dict(error='non simulé'))
        if p == '/api/v1/register': return self.send(200, dict(email='lea-x7k2p9@uplanet.local', token='JT', pass_='123456', salt='S', pepper='P', nsec='nsec1x', npub='npub1x', hex='cd' * 32, g1pub='G', nostrns='/ipns/k', constellation='ORIGIN', lang='fr', guest=True, card=CARD, card_svg=QR, **{'pass': '123456'}))
        if p == '/api/v1/restore/ask': return self.send(200, STATE.get('rescue') or dict(status='none'))  # secours par les gardiens : rien à attendre, sauf scénario (smoke_guardians.py)
        if p == '/api/v1/restore': return self.send(200, dict(email='alice@example.org', token='JT2', hex='ab' * 32, constellation='ORIGIN', restoring=True, **{'pass': '654321'}))
        if p == '/api/v1/invites': return self.send(200, dict(token=TOKEN, count=3, label=body.get('label'), expires_at=1791000000, url='https://s.example/i/' + TOKEN, qr_svg=QR))
        if p == '/api/v1/invites/remote-accept': return self.send(200, dict(**{'from': 'alice'}, origin=body['origin'], sent=4, warnings=[]))
        if p == f'/api/v1/invite/{TOKEN}/accept': return self.send(200, dict(**{'from': 'alice'}, shared=4, warnings=[]))
        if p == f'/api/v1/invite/{TOKEN}/claim':
            if body.get('email') == 'taken@example.org': return self.send(409, dict(error='adresse déjà hébergée', code='email_taken'))
            return self.send(200, dict(email=body['email'], token='JETON-WEB', pass_='1234', salt='SEL', pepper='POIVRE', nsec='nsec1secret', npub='npub1x', hex='cd' * 32, g1pub='G1X', nostrns='/ipns/k51', constellation='ORIGIN', lang=body['lang'], **{'from': 'alice'}, shared=4, warnings=[]))
        if p.startswith('/api/v1/contacts/'): return self.send(200, dict(path='/Contacts/x.vcf', linked=1))
        if p == '/api/v1/account/unlock':
            if body.get('pass') != '123456': return self.send(401, dict(error='code PASS incorrect : 4 essai(s) restant(s) avant verrouillage'))
            return self.send(200, dict(email=body['email'], hex='aa' * 32, token='TOKEN-SESSION', guest=False))
        if p == '/api/v1/account/sign': return self.send(200, dict(id='11' * 32, pubkey='aa' * 32, sig='22' * 64, created_at=1, **{k: body[k] for k in ('kind', 'content', 'tags')}))
        if p == '/api/v1/account/token': return self.send(200, dict(login='alice@example.org', token='NOUVEAU-JETON', generation=1))
        if p == '/api/v1/account/keys': return self.send(200, dict(nsec='nsec1secret', npub='npub1test', dunikey='sec: x', dunikey_error=None))
        if p == '/api/v1/account/profile': return self.send(200, ACCOUNT)
        if p == '/api/v1/faces/forget':
            STATE['faces'] = [f for f in STATE['faces'] if f['id'] not in body['ids']]; return self.send(200, dict(forgotten=len(body['ids'])))
        self.send(404, dict(error='simulé : ' + p))


def start():
    """Démarre la station simulée sur un port libre ; rend `(serveur, port)`."""
    srv = ThreadingHTTPServer(('127.0.0.1', 0), H)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    return srv, srv.server_address[1]
