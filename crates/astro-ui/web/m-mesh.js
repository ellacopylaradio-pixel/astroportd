'use strict';
// Maillage et Capitaines amis (Capitaine) : sa station dans le maillage, les stations jointes (ajout à chaud, gardées comme
// bootstrap), les Capitaines connus (suivre / ne plus suivre). Les services échangés avec eux sont dans Partages (m-services.js).
export default async function mesh(root, K) {
  const { h, t } = K, api = p => K.api('/api/v1/station/admin/' + p), post = (p, body) => K.api('/api/v1/station/admin/' + p, { method: 'POST', body });
  root.replaceChildren(h('p', { class: 'muted' }, t('Chargement…')));
  const [m, c, cs] = await Promise.all([api('mesh').catch(e => ({ error: e.message })), api('captains').catch(e => ({ error: e.message })), K.api('/api/v1/station/constellation').catch(() => null)]);
  const act = (p, body, msg) => async ev => { const b = ev.target; b.disabled = true; try { await post(p, body); if (msg) K.toast(msg, 1); mesh(root, K); } catch (e) { K.toast(e.message); b.disabled = false; } };
  // Action à confirmer d'un second appui (pas de boîte de dialogue).
  const sure = (label, run) => { const b = h('button', { class: 'ghost', onclick: ev => { if (b.dataset.armed) return run(ev); b.dataset.armed = 1; b.textContent = t('Confirmer ?'); } }, label); return b; };

  // Devenir station relais : nom public + port + PeerID, ajouté à P2P_RELAYS de la coopérative.
  const relayForm = () => {
    const local = /^(localhost|127\.|10\.|192\.168\.|172\.(1[6-9]|2\d|3[01])\.|\[|.*\.local$)/.test(location.hostname);
    // Port réel de CETTE UPlanet (4101, 4102… avec plusieurs UPlanet), modifiable si la box redirige un autre port.
    const port = ((m.addrs || []).join(' ').match(/\/tcp\/(\d+)\//) || [])[1] || '4101';
    const pub = h('input', { value: port, inputmode: 'numeric', size: 6, 'aria-label': t('Port public sur la box') });
    const host = h('input', { value: local ? '' : location.hostname, placeholder: 'relais.exemple.org', autocomplete: 'off', 'aria-label': t('Nom public ou IP de la box') });
    const addr = () => { const n = host.value.trim(); return n ? `/${/^\d+\.\d+\.\d+\.\d+$/.test(n) ? 'ip4' : 'dns4'}/${n}/tcp/${pub.value.trim() || port}/p2p/${m.peer_id}` : ''; };
    const show = h('code', { hidden: !addr() }, addr()); host.oninput = pub.oninput = () => { show.textContent = addr(); show.hidden = !addr(); };
    const add = sure(t('Ajouter aux stations relais'), async ev => {
      if (!addr()) return K.toast(t('Nom public ou IP de la box') + ' ?');
      ev.target.disabled = true;
      try {
        const c = await K.api('/api/v1/station/coop'), cur = (c.sections.flatMap(x => x.fields).find(f => f.key === 'P2P_RELAYS')?.value || '').trim();
        await K.api('/api/v1/station/coop', { method: 'POST', body: { values: { P2P_RELAYS: (cur ? cur + '\n' : '') + addr() } } });
        K.toast(t('Station ajoutée aux relais de la constellation : redémarrez-la pour qu\'elle devienne relais.'), 1); mesh(root, K);
      } catch (e) { K.toast(e.message); ev.target.disabled = false; }
    });
    return h('details', {}, h('summary', {}, t('Devenir station relais')),
      h('p', { class: 'muted' }, t('Cette UPlanet écoute sur le port TCP {0}. Ouvert sur la box (ou redirigé depuis le port public indiqué), il fait de cette station le relais des stations derrière une box.', port)),
      h('p', {}, host, ' ', h('label', {}, t('port'), ' ', pub), ' ', add), h('p', {}, show),
      h('p', { class: 'muted' }, t('Vérifiez depuis un autre réseau (téléphone en 4G) : nc -zv <nom> {0}', port)));
  };

  // ── Ma station ──
  const me = m.error ? h('p', { class: 'degraded' }, m.error) : !m.active
    ? h('p', { class: 'degraded', role: 'status' }, t('Maillage coupé : relancez la station sans --no-p2p pour rejoindre les autres stations.'))
    : [h('p', {}, t('Constellation : {0} · {1} station(s) connectée(s).', m.plane, m.connected.length)),
      h('p', { class: 'muted' }, t('Adresse à donner aux autres Capitaines :')), m.addrs.length ? h('ul', {}, m.addrs.map(a => h('li', {}, h('code', {}, a)))) : h('p', { class: 'muted' }, m.peer_id),
      // Stations relais (`--p2p-use-relay`, `P2P_RELAYS` de la coopérative) : la place réservée rend la station joignable derrière sa box.
      h('h3', {}, t('Stations relais')),
      m.relay_server ? h('p', {}, K.pill('ok'), ' ', t('Cette station sert de relais aux stations de la constellation qui sont derrière une box.')) : null,
      (m.relays || []).length ? h('ul', {}, m.relays.map(r => h('li', {},
        r.self ? h('span', { class: 'pill' }, t('cette station')) : r.reserved ? K.pill('ok') : h('span', { class: 'pill disabled' }, r.connected ? t('reliée, sans place') : t('injoignable')),
        ' ', h('code', {}, r.addr), ` (${r.source === 'coopérative' ? t('configuration coopérative') : t('démarrage')})`)))
        : h('p', { class: 'muted' }, t('Aucune station relais. Derrière une box, la station ne joint que celles de son réseau local et celles qui sont joignables. Ajoutez-en dans la configuration coopérative (P2P_RELAYS).'), ' ', h('a', { href: '#/coop' }, t('Configurer'))),
      m.relay_server || (m.relays || []).some(r => r.self) ? null : relayForm()];

  // ── Stations ──
  let stations = null, first = null;
  if (m.active) {
    const addr = h('input', { placeholder: t('astro-join:v1|… ou /ip4/…/tcp/4101/p2p/12D3Koo…'), autocomplete: 'off', 'aria-label': t('Jeton de parrainage ou adresse de la station') });
    const keep = h('input', { type: 'checkbox', checked: true, id: 'ms-keep' });
    const join = h('fieldset', { class: 'coop-s' }, h('legend', {}, t('Joindre une station')), addr, ' ', h('label', { for: 'ms-keep' }, keep, ' ', t('Garder (rejointe à chaque démarrage)')), ' ',
      h('button', { onclick: ev => addr.value.trim() ? act('mesh', { dial: addr.value.trim(), keep: keep.checked }, t('Station jointe'))(ev) : K.toast(t('Adresse de la station') + ' ?') }, t('Joindre')));
    // Premier contact : une station jointe suffit.
    if (!m.connected.length) first = h('section', { class: 'card wide' }, h('h2', {}, t('Rejoindre ma constellation')),
      h('p', {}, t('Reliée à aucune station. Un premier lien suffit, le reste suit seul.')),
      h('ul', {}, h('li', {}, t('Même box : rien à faire, elles se trouvent seules.')),
        h('li', {}, t('Ailleurs : collez le jeton de « astroportd parrainer » d\'un Capitaine de la constellation…')),
        h('li', {}, t('… ou l\'adresse de sa station (Réseau › Ma station).'))), join);
    stations = h('section', { class: 'card wide' }, h('h2', {}, t('Stations')),
      m.bootstrap.length ? h('ul', {}, m.bootstrap.map(b => h('li', {}, b.connected ? K.pill('ok') : h('span', { class: 'pill disabled' }, t('absente')), ' ', h('code', {}, b.addr), ` (${b.source === 'gardée' ? t('gardée') : t('démarrage')}) `,
        b.source === 'gardée' ? sure(t('Retirer'), act('mesh', { forget: b.addr }, t('Station retirée'))) : null))) : h('p', { class: 'muted' }, t('Aucune station gardée.')),
      first ? null : join,
      h('p', { class: 'muted' }, t('Seule une station de la même constellation (même swarm.key) peut être jointe.')));
  }

  // ── Capitaines ──
  // Une rangée par Capitaine : qui, où en est le lien, et le seul geste utile (suivre en retour, ne plus suivre).
  const card = cap => h('li', { class: 'ms-cap' }, h('div', { class: 'ms-who' }, h('span', {}, h('strong', {}, cap.label), ' ', h('span', { class: cap.friend ? 'pill ok' : 'pill disabled' }, cap.friend ? t('ami') : cap.follows_me ? t('vous suit') : cap.i_follow ? t('suivi') : t('inconnu'))),
    h('small', { class: 'muted' }, !cap.friend ? (cap.i_follow ? t('En attente : il doit vous suivre aussi pour partager.') : '')
      : !cap.peer ? t('Station inconnue.')
      : cap.services?.error ? t('Station injoignable : {0}', cap.services.error)
      : cap.services?.length ? [t('{0} service(s) partagé(s) avec vous', cap.services.length), ' · ', h('a', { href: '#/services' }, t('Utiliser'))] : t('Rien de partagé avec vous pour l\'instant.'))),
    cap.i_follow ? sure(t('Ne plus suivre'), act('captains', { unfollow: cap.hex }, t('Vous ne suivez plus {0}', cap.label))) : h('button', { onclick: act('captains', { follow: cap.hex }, t('Vous suivez {0}', cap.label)) }, cap.follows_me ? t('Suivre en retour') : t('Suivre')));
  // D'abord ceux qui attendent un geste (ils vous suivent), puis les amis, puis le reste.
  const rank = x => (x.follows_me && !x.i_follow ? 0 : x.friend ? 1 : 2);
  const caps = (c.captains || []).slice().sort((x, y) => rank(x) - rank(y)), list = h('ul', { class: 'ms-caps' }), find = h('input', { type: 'search', placeholder: t('Chercher un Capitaine'), 'aria-label': t('Chercher un Capitaine') });
  const show = () => list.replaceChildren(...caps.filter(x => !find.value || x.label.toLowerCase().includes(find.value.toLowerCase())).map(card));
  find.oninput = show; show();
  const captains = h('section', { class: 'card wide' }, h('h2', {}, t('Capitaines')),
    c.error ? h('p', { class: 'degraded' }, c.error) : !caps.length ? h('p', { class: 'muted' }, t('Aucun autre Capitaine connu pour l\'instant.')) : [caps.length > 8 ? find : null, list],
    h('p', { class: 'muted' }, t('Un ami réciproque vous montre ce qu\'il vous partage ; c\'est sa station qui décide.'), ' ', h('a', { href: '#/services' }, t('Régler mes partages')), ' · ', h('a', { href: '#/locks' }, t('Ce que « Suivre » ouvre'))));

  // ── Essaim : toutes les stations vues, de la plus puissante à la moins puissante ──
  const TIER = { Light: '🌿', Standard: '⚡', Brain: '🔥' }, nodes = cs?.nodes || [];
  const node = n => { const net = n.services?.filter(x => x !== 'ssh') || [];
    return h('div', { class: 'card ck-node' + (n.self ? ' self' : '') }, h('div', {}, h('strong', {}, n.node_id ? '…' + n.node_id.slice(-8) : '—'), n.self ? h('small', { class: 'muted' }, ' ' + t('(vous)')) : null, ' ', h('span', { class: 'ck-st ' + (n.status === 'known' ? 'off' : 'on') }, n.status === 'known' ? t('vue') : t('en ligne'))),
      h('p', {}, `${TIER[n.tier] || ''} ${n.tier} · ${n.power_score}`, n.distance_km == null ? '' : ` · ${n.distance_km} km`), net.length ? h('p', {}, net.map(x => h('span', { class: 'ck-chip' }, x))) : h('p', { class: 'muted' }, t('aucun service partagé'))); };
  const grid = h('div', { class: 'grid' }), only = h('input', { type: 'checkbox', id: 'ms-ia' });
  const paint = () => grid.replaceChildren(...nodes.filter(n => !only.checked || n.services?.includes('ollama')).map(node));
  only.onchange = paint; paint();
  const swarm = h('section', { class: 'card wide' }, h('h2', {}, t('L\'essaim'), h('small', { class: 'muted' }, ` · ${nodes.length}`)),
    h('p', {}, h('label', { for: 'ms-ia' }, only, ' ', t('Seulement celles qui offrent l\'IA (Ollama)'))), grid,
    h('p', {}, h('a', { class: 'ck-btn', href: '#/planes' }, t('Constellations'))));

  root.replaceChildren(...[first, captains, swarm, stations, h('section', { class: 'card wide' }, h('h2', {}, t('Ma station')), me)].filter(Boolean));
}
