'use strict';
// Maillage et Capitaines amis (Capitaine) : l'essaim (stations vues et leurs Capitaines, fiche et suivi : m-swarm.js), les
// stations jointes (ajout à chaud, gardées comme bootstrap), sa station dans le maillage. Les services échangés avec les amis
// sont dans Partages (m-services.js).
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

  // ── Essaim : stations vues et leurs Capitaines, un seul encart ; chaque station s'ouvre en fiche (m-swarm.js) ──
  const swarm = (await K.mod('m-swarm.js')).default(K, cs, c, () => mesh(root, K));

  root.replaceChildren(...[first, swarm, stations, h('section', { class: 'card wide' }, h('h2', {}, t('Ma station')), me)].filter(Boolean));
}
