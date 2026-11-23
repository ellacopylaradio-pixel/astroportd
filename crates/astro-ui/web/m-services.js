'use strict';
// Partages (Capitaine) : UN écran pour les services entre stations, rangé par tâche et non par mécanisme.
//  1. « Chez mes amis » : ce que les stations de la constellation et les Capitaines amis vous ouvrent → Utiliser / Fermer.
//  2. « Sur cette machine » : ce que vous partagez → qui peut l'utiliser, amis ajoutés ; et l'ajout d'un service choisi
//     parmi les ports TCP qui écoutent ici (lus par la station, sans les siens) ou saisi à la main.
//  3. « S'entraider » : demander de l'aide à un ami, ou en donner — l'accès s'ouvre d'un bouton, sans ligne de commande.
// Les API sont celles d'avant (tunnels, route, captains, assist) : le rangement change, pas les règles.
const NAMES = { ssh: 'Console à distance (SSH)', ollama: 'IA de discussion (Ollama)', comfyui: 'Images par IA (ComfyUI)', 'ai-index': 'Lecture de documents (IA)', 'bureau-vnc': 'Bureau à distance (VNC)', 'bureau-rdp': 'Bureau à distance (RDP)', jellyfin: 'Films et musique (Jellyfin)', 'open-webui': 'IA en discussion (Open WebUI)', 'home-assistant': 'Maison (Home Assistant)', imprimante: 'Imprimantes (CUPS)' };
const MODES = { off: 'Personne', listed: 'Seulement les amis ajoutés', friends: 'Tous mes Capitaines amis', swarm: 'Toute la constellation' };
const WHY = { off: 'Personne ne peut l\'utiliser.', listed: 'Seuls les amis ajoutés ci-dessous, pour la durée choisie.', friends: 'Vos Capitaines amis (suivi réciproque), plus les amis ajoutés ci-dessous.', swarm: 'Toute station de la constellation peut l\'utiliser.' };
const HOURS = [[2, '2 heures'], [24, '24 heures'], [168, '7 jours'], [0, 'Sans limite']];
const STATUS = { pending: 'en attente', accepted: 'accepté', declined: 'refusé', closed: 'clos', expired: 'expiré' };
const WEB = /^(ollama|comfyui|jellyfin|open-webui|home-assistant|imprimante|web)/;

export default async function services(root, K) {
  const { h, t } = K, get = p => K.api('/api/v1/station/admin/' + p).then(v => v || { error: '—' }, e => ({ error: e.message })), post = (p, body) => K.api('/api/v1/station/admin/' + p, { method: 'POST', body });
  if (!document.querySelector('link[href="/ui/m-services.css"]')) { const css = h('link', { rel: 'stylesheet', href: '/ui/m-services.css' }); document.head.append(css); K.cleanup(() => css.remove()); }
  if (!root.firstChild) root.append(h('p', { class: 'muted' }, t('Chargement…')));
  const [tun, rt, caps, asst] = await Promise.all([get('tunnels'), get('route'), get('captains'), get('assist')]);
  const again = () => services(root, K), open = root._open ||= new Set(), reached = root._reached ||= {};
  const nm = n => t(NAMES[n] || n);
  const { tip } = K;
  const head = (title, ...x) => h('div', { class: 'sv-head' }, h('h2', {}, title), x.length ? tip(...x) : null);
  // Un bouton occupé pendant l'action ; l'écran se redessine après.
  const busy = (run, msg) => async ev => { const b = ev.currentTarget; b.disabled = true; try { await run(); if (msg) K.toast(msg, 1); again(); } catch (e) { K.toast(e.message); b.disabled = false; } };
  // Section repliable dont l'état survit au redessin.
  const fold = (key, summary, ...body) => { const d = h('details', { class: 'sv-fold', open: open.has(key) }, h('summary', {}, summary), ...body); d.addEventListener('toggle', () => (d.open ? open.add(key) : open.delete(key))); return d; };
  const copy = text => h('button', { type: 'button', class: 'ghost', onclick: () => navigator.clipboard?.writeText(text).then(() => K.toast(t('Copié : {0}', text), 1), e => K.toast(e.message)) }, t('Copier'));
  const how = (s, a) => s === 'ssh' ? `ssh -p ${a.split(':').pop()} <utilisateur>@127.0.0.1` : s.startsWith('bureau') ? t('client de bureau à distance vers {0}', a) : WEB.test(s) ? `http://${a}` : a;
  const reach = (s, a) => h('span', { class: 'sv-reach' }, h('code', {}, how(s, a)), copy(how(s, a)));
  const connect = async (service, peer, hours) => {
    try { return (await post('route', { connect: { service, peer, mode: 'power', ...(hours ? { hours } : {}) } })).addr; }
    catch (e) { throw new Error(t('Ouverture impossible : {0}', e.message)); }
  };

  // ── 1. Chez mes amis ──
  const rows = [], seen = new Set();
  for (const x of rt.stations || []) if (Array.isArray(x.services)) for (const s of x.services) { rows.push({ peer: x.peer, who: x.name, s }); seen.add(x.peer + '/' + s.name); }
  for (const c of caps.captains || []) if (c.friend && c.peer && Array.isArray(c.services)) for (const s of c.services) if (!seen.has(c.peer + '/' + s.name)) rows.push({ peer: c.peer, who: c.label, s });
  rows.sort((a, b) => !!b.s.local - !!a.s.local);
  const theirs = ({ peer, who, s }) => h('li', { class: 'sv-row' + (s.local ? ' on' : '') }, h('div', { class: 'sv-main' }, h('strong', {}, nm(s.name)), h('small', { class: 'muted' }, who)),
    h('div', { class: 'sv-act' }, s.local
      ? [reach(s.name, s.local.addr), h('button', { class: 'ghost', onclick: busy(() => post('route', { disconnect: { service: s.name, peer } }), t('Accès fermé')) }, t('Fermer'))]
      : h('button', { onclick: busy(async () => K.toast(t('Ouvert : {0}', how(s.name, await connect(s.name, peer))), 1)) }, t('Utiliser'))));
  const offered = (rt.stations || []).filter(x => Array.isArray(x.services) && x.services.length), down = (rt.stations || []).filter(x => x.services?.error);
  const several = [...new Set(offered.flatMap(x => x.services.map(s => s.name)))].filter(n => offered.filter(x => x.services.some(s => s.name === n)).length > 1);
  const usedN = rows.filter(r => r.s.local).length;
  const inbound = h('section', { class: 'card wide' }, head(t('Chez mes amis'), t('Les services que d\'autres stations vous ouvrent : une IA, un bureau à distance, des films… « Utiliser » crée une adresse sur cette station (127.0.0.1), fermée seule après 8 heures.'), t('C\'est la station amie qui décide de ce qu\'elle vous ouvre.')),
    rt.error ? h('p', { class: 'degraded', role: 'alert' }, rt.error)
      : !rt.mesh ? h('p', { class: 'degraded' }, t('Maillage coupé : relancez la station sans --no-p2p pour rejoindre les autres stations.'), ' ', h('a', { href: '#/mesh' }, t('Essaim')))
      : !rows.length ? h('p', { class: 'muted', role: 'status' }, t('Aucun ami ne vous ouvre de service pour l\'instant. Un Capitaine ami le fait depuis son écran Réseau › Partages.'))
      : [h('ul', { class: 'sv-list' }, rows.map(theirs)),
        several.length ? h('p', { class: 'sv-chips' }, h('span', { class: 'muted' }, t('Au plus puissant :')), ' ', several.map(n => h('button', { class: 'ghost', onclick: busy(async () => K.toast(t('Ouvert : {0}', how(n, await connect(n))), 1)) }, nm(n)))) : null],
    down.length ? h('p', { class: 'muted' }, t('Injoignables : {0}', down.map(x => x.name).join(', '))) : null);

  // ── 2. Sur cette machine ──
  let outbound;
  if (tun.error) outbound = h('section', { class: 'card wide' }, head(t('Sur cette machine')), h('p', { class: 'degraded' }, tun.error));
  else {
    const set = (body, msg) => busy(() => post('tunnels', body), msg);
    const apply = (body, msg) => post('tunnels', body).then(() => { K.toast(msg, 1); again(); }, e => K.toast(e.message));
    const when = u => u ? t('jusqu\'à {0}', new Date(u * 1000).toLocaleString(undefined, { dateStyle: 'short', timeStyle: 'short' })) : t('sans limite');
    const idle = sv => sv.up === false && sv.name !== 'ssh' && !sv.shared && !sv.customized && !sv.grants.length;
    const absent = tun.services.filter(idle);
    const mine = sv => {
      const mode = h('select', { 'aria-label': t('Qui peut utiliser {0}', nm(sv.name)) }, tun.modes.map(m => h('option', { value: m, selected: m === sv.mode }, t(MODES[m]) + (m === sv.default_mode ? ' ' + t('(défaut)') : ''))));
      mode.onchange = () => apply({ service: sv.name, mode: mode.value }, t('{0} : accès modifié', nm(sv.name)));
      const named = sv.mode === 'listed' || sv.mode === 'friends';
      const who = h('select', { 'aria-label': t('Capitaine ami') }, h('option', { value: '' }, t('Choisir un ami…')), tun.friends.map(f => h('option', { value: f.peer }, f.label)), h('option', { value: '*' }, t('Autre station (PeerID)…')));
      const peer = h('input', { placeholder: 'PeerID 12D3Koo…', autocomplete: 'off', 'aria-label': 'PeerID', hidden: true });
      who.onchange = () => { peer.hidden = who.value !== '*'; if (!peer.hidden) peer.focus(); };
      const hours = h('select', { 'aria-label': t('Durée') }, HOURS.map(([v, l]) => h('option', { value: v }, t(l))));
      const grant = () => { const p = who.value === '*' ? peer.value.trim() : who.value; if (!p) return K.toast(t('Choisissez un ami ou collez un PeerID')); apply({ service: sv.name, grant: { peer: p, label: who.value === '*' ? '' : who.options[who.selectedIndex].text, hours: Number(hours.value) } }, t('Accès accordé')); };
      return h('li', { class: 'sv-card' + (sv.mode === 'off' ? '' : ' on') },
        h('div', { class: 'sv-row' }, h('div', { class: 'sv-main' }, h('strong', {}, nm(sv.name)), h('small', { class: 'muted' }, sv.target || '', ' ', sv.up === false ? h('span', { class: 'pill disabled' }, t('ne répond pas')) : null)),
          h('label', { class: 'sv-act' }, h('span', { class: 'muted' }, t('Qui :')), mode)),
        h('p', { class: sv.name === 'ssh' && sv.mode !== 'off' ? 'degraded' : 'muted', role: 'note' }, t(WHY[sv.mode] || ''), sv.name === 'ssh' && sv.mode !== 'off' ? ' ' + t('Clé jumelle de leur identité, chaque session est journalisée.') : ''),
        named && sv.grants.length ? h('p', { class: 'sv-chips' }, sv.grants.map(g => h('span', { class: 'sv-chip' + (g.expired ? ' off' : '') }, h('span', {}, h('strong', {}, g.label || g.peer.slice(0, 12)), ' ', g.expired ? t('expiré') : when(g.until)),
          h('button', { class: 'ghost sv-x', title: t('Retirer'), 'aria-label': t('Retirer {0}', g.label || g.peer), onclick: set({ service: sv.name, revoke: g.peer }, t('Accès retiré')) }, '×')))) : null,
        named ? fold('add:' + sv.name, t('+ Ajouter un ami'), h('div', { class: 'sv-form' }, who, peer, hours, h('button', { onclick: grant }, t('Autoriser')))) : null,
        sv.customized || sv.shared ? h('p', { class: 'sv-chips' }, sv.customized ? h('button', { class: 'ghost', onclick: set({ service: sv.name, reset: true }, t('Réglage par défaut rétabli')) }, t('Rétablir le défaut')) : null,
          sv.shared ? K.sure(t('Ne plus partager'), () => apply({ unshare: sv.name }, t('Partage retiré'))) : null) : null);
    };
    // Ajouter : les ports qui écoutent ici (sinon, hors Linux, les services courants détectés), ou une saisie à la main.
    const share = (name, target) => apply({ share: { name, target, mode: 'listed' } }, t('{0} partagé : ajoutez qui peut l\'utiliser.', name));
    const cands = (tun.listening || []).length ? tun.listening : (tun.suggest || []).map(s => ({ ...s, port: s.target.split(':').pop() }));
    const cand = c => h('li', { class: 'sv-row' }, h('div', { class: 'sv-main' }, h('strong', {}, c.label ? t(c.label) : c.program || t('Port {0}', c.port)),
      h('small', { class: 'muted' }, [t('port {0}', c.port), c.label && c.program, c.local_only === false ? t('aussi sur le réseau local') : null].filter(Boolean).join(' · '))),
      h('div', { class: 'sv-act' }, h('button', { class: 'ghost', onclick: () => share(c.name, c.target) }, t('Partager'))));
    const where = h('input', { placeholder: t('5900 ou 192.168.1.20:8123'), autocomplete: 'off', inputmode: 'url', 'aria-label': t('Port ou adresse') });
    const name = h('input', { placeholder: t('nom (facultatif)'), autocomplete: 'off', maxlength: 32, 'aria-label': t('Nom du service') });
    const manual = () => {
      const w = where.value.trim(), target = /^\d{1,5}$/.test(w) ? '127.0.0.1:' + w : w.replace(/^localhost:/, '127.0.0.1:');
      if (!w) return K.toast(t('Port ou adresse') + ' ?');
      const port = target.split(':').pop(), known = cands.find(c => String(c.port) === port && c.target === target);
      share((name.value.trim() || known?.name || 'port-' + port).toLowerCase().replace(/[^a-z0-9_-]+/g, '-').slice(0, 32), target);
    };
    const shared = tun.services.filter(s => s.shared).length, full = tun.max_shared && shared >= tun.max_shared;
    const adder = fold('add', h('strong', {}, t('+ Partager un service de cette machine')),
      full ? h('p', { class: 'degraded' }, t('Limite atteinte ({0} services partagés) : retirez-en un d\'abord.', tun.max_shared)) : null,
      cands.length ? [h('p', { class: 'muted' }, t('Ces programmes écoutent sur cette machine :')), h('ul', { class: 'sv-list' }, cands.map(cand))] : h('p', { class: 'muted' }, t('Aucun autre port n\'écoute sur cette machine.')),
      h('p', { class: 'muted' }, t('Autre port, ou un appareil du réseau local :')), h('div', { class: 'sv-form' }, where, name, h('button', { onclick: manual }, t('Partager'))),
      h('p', { class: 'sv-chips' }, h('button', { class: 'ghost', onclick: busy(async () => {}) }, t('Actualiser la liste'))),
      h('p', { class: 'muted' }, t('Bureau à distance : démarrez d\'abord un serveur VNC (wayvnc, x11vnc) ou RDP. Un service ajouté n\'est ouvert à personne tant que vous n\'avez pas choisi qui.')));
    const svs = tun.services.filter(sv => !idle(sv));
    outbound = h('section', { class: 'card wide' }, head(t('Sur cette machine'), t('Ce que cette station prête aux autres. Pour chaque service, choisissez qui peut l\'utiliser ; un ami ajouté peut l\'être pour une durée limitée.'), tun.origin ? t('Constellation ORIGIN : le SSH est ouvert par défaut aux Capitaines amis (suivi réciproque).') : t('Constellation ẐEN : le SSH est fermé par défaut. Vous le rouvrez pour un ami le temps qu\'il vous aide, ou pour tous vos Capitaines amis.'), t('Le réglage s\'applique tout de suite, y compris aux sessions déjà ouvertes (vérifié chaque minute).')),
      svs.length ? h('ul', { class: 'sv-list' }, svs.map(mine)) : h('p', { class: 'muted' }, t('Rien n\'est partagé pour l\'instant.')),
      absent.length ? h('p', { class: 'muted', role: 'note' }, t('Non détectés : {0}.', absent.map(sv => `${nm(sv.name)} (${sv.target})`).join(', '))) : null, adder);
  }

  // ── 3. S'entraider ──
  let help = null, pending = 0;
  if (!asst.error) {
    const act = (body, msg) => busy(async () => { const r = await post('assist', body); if (r.sent !== undefined) K.toast(`${msg} (${t('{0} ami(s) sollicité(s)', r.sent)})`, 1); else K.toast(msg, 1); });
    const service = h('select', { 'aria-label': t('Service à ouvrir') }, asst.services.map(sv => h('option', { value: sv }, nm(sv))));
    const hours = h('select', { 'aria-label': t('Durée') }, [[2, '2 heures'], [24, '24 heures'], [72, '3 jours'], [168, '7 jours']].map(([v, l]) => h('option', { value: v }, t(l))));
    const to = h('select', { 'aria-label': t('Qui solliciter') }, h('option', { value: '' }, t('Tous mes Capitaines amis')), asst.friends.map(f => h('option', { value: f.hex }, f.label)));
    const first = h('input', { type: 'checkbox', id: 'sv-first' }), note = h('input', { maxlength: 280, placeholder: t('Ce qui ne va pas (facultatif)'), autocomplete: 'off', 'aria-label': t('Message') });
    pending = asst.incoming.filter(r => r.status === 'pending').length;
    // Accepté : un bouton ouvre l'accès vers la station de l'ami (l'adresse reste affichée tant que l'écran est ouvert).
    const helping = r => reached[r.id]
      ? h('p', { class: 'sv-chips' }, reach(r.service, reached[r.id]), h('button', { class: 'ghost', onclick: busy(async () => { await post('route', { disconnect: { service: r.service, peer: r.station } }); delete reached[r.id]; }, t('Accès fermé')) }, t('Fermer')))
      : h('p', {}, h('button', { onclick: busy(async () => { reached[r.id] = await connect(r.service, r.station, r.hours); }) }, t('Ouvrir l\'accès')));
    const incoming = asst.incoming.map(r => h('li', { class: 'sv-card' + (r.status === 'pending' ? ' on' : '') }, h('p', {}, h('strong', {}, r.from), ' ' + t('demande de l\'aide : {0}, {1} h', nm(r.service), r.hours), r.note ? ` — « ${r.note} »` : '', ' ', h('span', { class: 'pill' }, t(STATUS[r.status]))),
      r.status === 'pending' ? h('p', { class: 'sv-chips' }, h('button', { onclick: act({ action: 'respond', id: r.id, other: r.other, accept: true }, t('Vous aidez {0}', r.from)) }, t('Accepter d\'aider')), h('button', { class: 'ghost', onclick: act({ action: 'respond', id: r.id, other: r.other, accept: false }, t('Demande refusée')) }, t('Refuser'))) : null,
      r.status === 'accepted' && r.station ? helping(r) : null));
    const outgoing = asst.outgoing.map(r => h('li', {}, `${nm(r.service)}, ${r.hours} h${r.exclusive ? ' ' + t('(le premier qui accepte)') : ''} : `, r.recipients.map(x => `${x.label} ${t(STATUS[x.status])}`).join(', '), ' ',
      r.recipients.some(x => x.status === 'pending') ? h('button', { class: 'ghost', onclick: act({ action: 'cancel', id: r.id }, t('Demande retirée')) }, t('Retirer')) : null));
    help = h('section', { class: 'card wide' }, head(t('S\'entraider'), t('Demandez de l\'aide : rien n\'est ouvert tant qu\'un ami n\'a pas accepté. L\'accès s\'ouvre alors pour lui seul, pour la durée choisie, puis se ferme tout seul.')),
      incoming.length ? [h('h3', {}, t('Demandes reçues')), h('ul', { class: 'sv-list' }, incoming)] : null,
      asst.friends.length ? fold('ask', h('strong', {}, t('Demander de l\'aide')), h('div', { class: 'sv-form' }, service, hours, to, note), h('label', { for: 'sv-first' }, first, ' ', t('Seul le premier qui accepte reçoit l\'accès')),
        h('p', {}, h('button', { onclick: ev => act({ action: 'request', service: service.value, hours: Number(hours.value), note: note.value.trim(), to: to.value, exclusive: first.checked }, t('Demande envoyée'))(ev) }, t('Envoyer la demande'))))
        : h('p', { class: 'muted' }, t('Aucun Capitaine ami réciproque pour le moment (suivez-vous mutuellement).'), ' ', h('a', { href: '#/mesh' }, t('Essaim'))),
      outgoing.length ? [h('h3', {}, t('Mes demandes')), h('ul', {}, outgoing)] : null);
  }

  // En tête : l'essentiel d'un coup d'œil.
  const mineN = tun.error ? 0 : tun.services.filter(s => s.mode !== 'off').length;
  const sum = h('p', { class: 'sv-sum' }, h('span', { class: 'sv-chip' }, t('{0} utilisé(s) chez mes amis', usedN)), h('span', { class: 'sv-chip' }, t('{0} ouvert(s) aux autres', mineN)),
    pending ? h('span', { class: 'sv-chip warn' }, t('{0} demande(s) d\'aide en attente', pending)) : null);
  root.replaceChildren(sum, ...[pending ? help : null, inbound, outbound, pending ? null : help].filter(Boolean));

  // Les demandes d'aide arrivent d'une autre station : redessin seul, sans écraser une saisie en cours.
  root._asstKey = JSON.stringify([asst.incoming, asst.outgoing]);
  if (!root._asstPoll) {
    root._asstPoll = true;
    K.every(15000, async () => {
      const typing = ['INPUT', 'SELECT'].includes(document.activeElement?.tagName) && root.contains(document.activeElement);
      const now = await get('assist');
      if (!now.error && JSON.stringify([now.incoming, now.outgoing]) !== root._asstKey && !typing) again();
    });
  }
}
