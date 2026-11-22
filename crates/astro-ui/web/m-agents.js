'use strict';
// Mes agents : des assistants (OpenClaw, Claude…) demandent à agir pour le membre sur un site où il est inscrit. Il accepte ou
// refuse. Pour agir sur un site, la station ouvre un navigateur jetable, montré ici : le membre s'y connecte lui-même, puis le
// prête pour le temps choisi, et peut reprendre la main à tout moment. Aucun mot de passe n'est jamais remis à l'agent.
export default async function agents(root, K) {
  const { h, t } = K;
  const css = h('link', { rel: 'stylesheet', href: '/ui/m-agents.css' }); document.head.append(css); K.cleanup(() => css.remove());
  const DUR = [[300, t('5 minutes')], [900, t('15 minutes')], [1800, t('30 minutes')], [3600, t('1 heure')]];
  const durOf = s => (DUR.find(d => d[0] >= s) || DUR[DUR.length - 1])[1];
  const post = (url, body) => K.api(url, { method: 'POST', body });
  const left = until => Math.max(0, Math.ceil((until * 1000 - Date.now()) / 60000));
  const reqs = h('div', { class: 'ag-reqs' }), list = h('ul', { class: 'ag-list' }), fresh = h('div', { class: 'ag-token', hidden: true });
  const viewers = new Map(), watching = new Set();
  let load = async () => {};
  const act = async (btn, run, ok) => {
    btn.disabled = true;
    try { await run(); if (ok) K.toast(ok, 1); } catch (e) { K.toast(e.message); }
    btn.disabled = false; await load();
  };
  const durSel = (val, label) => h('select', { 'aria-label': label }, DUR.map(([s, l]) => h('option', { value: s, selected: s === val }, l)));

  // ── L'écran du navigateur prêté (module commun m-screen.js) ──
  const S = await K.mod('m-screen.js');
  const screen = id => S.viewer(K, { stream: '/api/v1/agents/screen?id=' + encodeURIComponent(id), send: g => post('/api/v1/agents/input', { id, ...g }), mode: 'drive' });
  const viewer = id => { if (!viewers.has(id)) viewers.set(id, screen(id)); return viewers.get(id).el; };
  K.cleanup(() => viewers.forEach(v => v.stop()));

  // ── Les demandes en cours ──
  const card = r => {
    const browse = r.mode === 'navigateur', stop = (label, ok) => { const b = h('button', { type: 'button', class: 'ghost' }, label); b.onclick = () => act(b, () => post('/api/v1/agents/stop', { id: r.id }), ok); return b; };
    const why = r.reason ? h('p', { class: 'muted' }, '« ', r.reason, ' »') : null;
    if (r.state === 'pending') {
      const sel = durSel(900, t('Durée'));
      const more = h('input', { class: 'ag-type', placeholder: 'accounts.google.com', autocapitalize: 'off', spellcheck: false, 'aria-label': t('Sites liés permis') });
      const linked = browse ? h('details', { class: 'fold' }, h('summary', {}, t('Connexion par un autre site ?')),
        h('p', { class: 'muted' }, t('Si vous entrez sur {0} avec un autre compte (Google, FranceConnect…), indiquez ce site : la page pourra y passer aussi.', r.site)), more) : null;
      const yes = h('button', { type: 'button' }, browse ? t('Me connecter pour lui') : t('Accorder'));
      yes.onclick = () => act(yes, () => post('/api/v1/agents/decide', { id: r.id, approve: true, secs: +sel.value, hosts: more.value.split(/[\s,]+/).filter(Boolean) }), browse ? null : t('Accord donné'));
      const no = h('button', { type: 'button', class: 'ghost' }, t('Refuser'));
      no.onclick = () => act(no, () => post('/api/v1/agents/decide', { id: r.id, approve: false }), t('Demande refusée'));
      return h('li', { class: 'ag-req' }, h('strong', {}, browse ? t('{0} voudrait agir sur {1} pour vous', r.agent, r.site) : t('{0} demande l\'accès à {1}', r.agent, r.site)), why, linked,
        h('div', { class: 'ag-row' }, h('label', {}, t('Lui laisser'), ' ', sel), yes, no));
    }
    if (r.state === 'login') {
      const ready = h('button', { type: 'button' }, t('C\'est prêt, lui confier'));
      ready.onclick = () => act(ready, () => post('/api/v1/agents/ready', { id: r.id }), t('{0} peut agir maintenant', r.agent));
      return h('li', { class: 'ag-req' }, h('strong', {}, t('Connectez-vous à {0} ci-dessous, comme d\'habitude', r.site)),
        h('p', { class: 'muted' }, t('{0} ne connaîtra pas votre mot de passe. Pendant le temps choisi, il pourra faire sur ce site ce que vous pourriez y faire vous-même.', r.agent)),
        viewer(r.id), h('div', { class: 'ag-row' }, ready, stop(t('Annuler'), t('Demande annulée'))));
    }
    const see = watching.has(r.id), eyeBtn = h('button', { type: 'button', class: 'ghost', onclick: () => { if (see) watching.delete(r.id); else watching.add(r.id); load(); } }, see ? t('Ne plus regarder') : t('Regarder'));
    return h('li', { class: 'ag-req ag-live' }, h('strong', {}, browse ? t('{0} agit sur {1} pour vous', r.agent, r.site) : t('{0} a accès à {1}', r.agent, r.site)), ' ',
      h('span', { class: 'pill' }, t('reste {0} min', left(r.until))), why,
      browse && see ? viewer(r.id) : null,
      h('div', { class: 'ag-row' }, browse ? eyeBtn : null, stop(browse ? t('Reprendre la main') : t('Retirer l\'accès'), t('Vous avez repris la main'))));
  };
  const showReqs = all => {
    for (const [id, v] of viewers) if (!all.some(r => r.id === id && (r.state === 'login' || (r.state === 'granted' && watching.has(id))))) { v.stop(); viewers.delete(id); }
    reqs.replaceChildren(all.length ? h('ul', { class: 'ag-list' }, all.map(card)) : h('p', { class: 'muted' }, t('Aucune demande en cours.')));
  };

  // ── Les agents déclarés ──
  const showAgents = all => list.replaceChildren(...(all.length ? all.map(a => h('li', {},
    h('div', {}, h('strong', {}, a.name), h('br'), h('small', { class: 'muted' }, t('{0} au plus par accès', durOf(a.max_secs)), a.station ? ' · ' + t('Sur {0}', stationName.get(a.station) || '…' + a.station.slice(-8)) : '')),
    K.sure(t('Retirer'), () => post('/api/v1/agents/revoke', { id: a.id }).then(() => { K.toast(t('Agent retiré'), 1); return load(); }, e => K.toast(e.message)), t('Son jeton cessera de fonctionner.')))) :
    [h('li', { class: 'muted' }, t('Aucun agent déclaré.'))]));
  const name = h('input', { name: 'name', required: true, maxlength: 40, placeholder: t('Nom de l\'agent (ex. OpenClaw)'), 'aria-label': t('Nom de l\'agent') }), max = durSel(3600, t('Durée maximale d\'un accès'));
  // Où tourne l'agent : cette station, ou une autre station de la constellation (annuaire des balises).
  const where = h('select', { name: 'station', 'aria-label': t('Où tourne l\'agent') }, h('option', { value: '' }, t('Sur cette station')));
  const stationName = new Map();
  K.api('/api/v1/stations').then(r => (r.stations || []).filter(s => !s.self && s.station).forEach(s => { stationName.set(s.station, s.name); where.append(h('option', { value: s.station }, t('Sur {0}', s.name))); }), () => {});
  const form = h('form', { class: 'ag-row' }, name, h('label', {}, t('Au plus'), ' ', max), where, h('button', { type: 'submit' }, t('Déclarer')));
  form.onsubmit = async ev => {
    ev.preventDefault();
    try {
      const r = await post('/api/v1/agents', { name: name.value.trim(), group: 'Agents', max_secs: +max.value, ...(where.value && { station: where.value }) });
      const copy = h('button', { type: 'button', class: 'ghost', onclick: () => navigator.clipboard.writeText(r.token).then(() => K.toast(t('Jeton copié'), 1), () => {}) }, t('Copier'));
      fresh.hidden = false; form.reset();
      fresh.replaceChildren(h('p', {}, h('strong', {}, t('Copiez ce jeton dans la configuration de {0} : il ne sera plus jamais affiché.', r.name))), h('div', { class: 'ag-row' }, h('code', { class: 'ag-code' }, r.token), copy));
      await load();
    } catch (e) { K.toast(e.message); }
  };
  const mcp = location.origin + '/mcp', code = s => h('code', { class: 'ag-code' }, s), remoteHow = h('p', { hidden: true });
  const howto = h('details', { class: 'fold' }, h('summary', {}, t('Brancher un agent')),
    h('p', {}, t('L\'agent parle à votre station par le protocole MCP, avec son jeton :')),
    h('dl', { class: 'stack' }, h('dt', {}, t('Adresse')), h('dd', {}, code(mcp)), h('dt', {}, t('En-tête')), h('dd', {}, code('Authorization: Bearer <jeton>'))),
    h('p', {}, t('Par exemple, pour Claude Code :')), code(`claude mcp add --transport http astroport ${mcp} --header "Authorization: Bearer <jeton>"`),
    h('p', { class: 'muted' }, t('Pour agir sur un site, l\'agent demande le mode navigateur ; une fois prêté, il pilote la page avec Playwright ou Puppeteer. Il doit tourner sur cette station ou sur une station de votre constellation.')), remoteHow);

  load = async () => {
    const [r, a] = await Promise.all([K.api('/api/v1/agents/requests'), K.api('/api/v1/agents')]);
    showReqs(r.requests); showAgents(a.agents);
    // Agent sur une autre station : il parle à SA station, qui relaie vers celle-ci par le maillage.
    if (a.here) { remoteHow.hidden = false; remoteHow.replaceChildren(t('Agent sur une autre station : il s\'adresse à elle, avec cette adresse :'), ' ', code(`https://<${t('sa station')}>/mcp?station=${a.here}`)); }
  };
  K.listen(ev => { if (ev.kind === 'agent') load().catch(() => {}); });
  const tick = setInterval(() => load().catch(() => {}), 30000); K.cleanup(() => clearInterval(tick));
  root.replaceChildren(
    h('section', { class: 'card wide' }, h('h2', {}, '🤖 ', t('Demandes en cours')), reqs),
    h('section', { class: 'card wide' }, h('h2', {}, t('Mes agents')),
      h('p', { class: 'muted' }, t('Un agent est un assistant (une IA, un robot) qui peut demander à agir pour vous. Il ne reçoit jamais votre mot de passe : vous décidez à chaque fois.')),
      list, form, fresh, howto));
  try { await load(); } catch (e) { reqs.replaceChildren(h('p', { class: 'err', role: 'alert' }, e.message)); }
}
