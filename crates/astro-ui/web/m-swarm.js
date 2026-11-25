'use strict';
// Essaim (Capitaine, écran Réseau) : un seul encart pour les stations vues et leurs Capitaines. Chaque station s'ouvre en
// fiche (ce que sa balise annonce : nom, matériel, stockage, places, services) avec le geste utile : suivre son Capitaine.
// Le stockage libre de l'essaim est la somme de ce que les balises annoncent (`storage:free_gb`).
const TIER = { Light: '🌿', Standard: '⚡', Brain: '🔥' };
const gb = n => (n >= 1000 ? `${(n / 1000).toLocaleString(document.documentElement.lang || 'fr', { maximumFractionDigits: 1 })} To` : `${n} Go`);

export default function swarm(K, cs, c, reload) {
  const { h, t } = K, nodes = cs?.nodes || [], caps = c?.captains || [];
  const post = body => K.api('/api/v1/station/admin/captains', { method: 'POST', body });
  const capOf = n => caps.find(x => x.peer && x.peer === n.node_id);
  const state = cap => (cap.friend ? t('ami') : cap.follows_me ? t('vous suit') : cap.i_follow ? t('suivi') : t('inconnu'));
  const pill = cap => h('span', { class: cap.friend ? 'pill ok' : 'pill disabled' }, state(cap));
  // Suivre / ne plus suivre : ne plus suivre se confirme d'un second appui (pas de boîte de dialogue).
  const follow = cap => {
    const b = h('button', { class: cap.i_follow ? 'ghost' : '', onclick: async () => {
      if (cap.i_follow && !b.dataset.armed) { b.dataset.armed = 1; b.textContent = t('Confirmer ?'); return; }
      b.disabled = true;
      try { await post(cap.i_follow ? { unfollow: cap.hex } : { follow: cap.hex }); K.toast(cap.i_follow ? t('Vous ne suivez plus {0}', cap.label) : t('Vous suivez {0}', cap.label), 1); if (dlg.open) dlg.close(); reload(); } catch (e) { K.toast(e.message); b.disabled = false; }
    } }, cap.i_follow ? t('Ne plus suivre') : cap.follows_me ? t('Suivre en retour') : t('Suivre'));
    return b;
  };
  const shared = cap => (!cap.friend ? (cap.i_follow ? t('En attente : il doit vous suivre aussi pour partager.') : null)
    : cap.services?.error ? t('Station injoignable : {0}', cap.services.error)
    : cap.services?.length ? [t('{0} service(s) partagé(s) avec vous', cap.services.length), ' · ', h('a', { href: '#/services' }, t('Utiliser'))] : t('Rien de partagé avec vous pour l\'instant.'));

  // ── Fiche d'une station ──
  const dlg = h('dialog', { class: 'ms-sheet', 'aria-labelledby': 'ms-sheet-t' }), head = h('h2', { id: 'ms-sheet-t' }), body = h('div');
  dlg.append(h('button', { class: 'x', type: 'button', 'aria-label': t('Fermer'), onclick: () => dlg.close() }, '×'), head, body);
  dlg.onclick = ev => { if (ev.target === dlg) dlg.close(); };
  const row = (k, ...v) => (v.some(x => x != null && x !== '') ? [h('dt', {}, k), h('dd', {}, ...v)] : []);
  const open = n => {
    const cap = capOf(n), when = n.at ? new Date(n.at * 1000).toLocaleString(document.documentElement.lang || 'fr', { dateStyle: 'medium', timeStyle: 'short' }) : null;
    head.replaceChildren(n.name || '…' + (n.node_id || '').slice(-8), n.self ? h('small', { class: 'muted' }, ' ' + t('(vous)')) : '');
    const hw = [n.cpu_cores && t('{0} cœurs', n.cpu_cores), n.ram_gb && t('{0} Go de mémoire', n.ram_gb), n.gpu_gb > 0 && t('{0} Go de carte graphique', n.gpu_gb)].filter(Boolean).join(' · ');
    body.replaceChildren(
      h('section', { class: 'ms-capbox' }, h('h3', {}, t('Capitaine')),
        cap ? h('div', { class: 'ms-cap' }, h('div', { class: 'ms-who' }, h('span', {}, h('strong', {}, cap.label), ' ', pill(cap)), h('small', { class: 'muted' }, shared(cap))), follow(cap))
          : h('p', { class: 'muted' }, n.self ? t('C\'est votre station.') : t('Son Capitaine n\'est pas encore connu ici : il apparaît dès que sa station a publié son profil sur un relai commun.'))),
      h('dl', { class: 'ms-facts' },
        ...row(t('État'), n.self ? t('cette station') : n.status === 'known' ? t('vue') : t('en ligne'), when ? ` · ${t('dernière balise le {0}', when)}` : ''),
        ...row(t('Puissance'), `${TIER[n.tier] || ''} ${n.tier} · ${n.power_score}`),
        ...row(t('Matériel'), hw),
        ...row(t('Stockage libre'), n.storage_free_gb != null ? t('{0} sur {1}', gb(n.storage_free_gb), gb(n.storage_total_gb || 0)) : ''),
        ...row(t('Distance'), n.distance_km != null ? `${n.distance_km} km` : ''),
        ...row(t('Membres'), n.used != null ? `${n.used}${n.total ? ' / ' + n.total : ''}${n.open === false ? ' · ' + t('inscriptions fermées') : ''}` : ''),
        ...row(t('Mode'), n.mode || ''),
        ...row('UPlanet', n.uplanet || ''),
        ...row(t('Adresse'), n.url ? h('a', { href: n.url, target: '_blank', rel: 'noopener' }, n.url.replace(/^https?:\/\//, '')) : ''),
        ...row(t('Services'), ...(n.services?.filter(x => x !== 'ssh').map(x => h('span', { class: 'ck-chip' }, x)) || [])),
        ...row(t('Logiciel'), n.software || ''),
        ...row('PeerID', h('code', { class: 'ms-id' }, n.node_id || '—'))));
    if (!dlg.open) dlg.showModal();
  };

  // ── Grille des stations ──
  const node = n => {
    const cap = capOf(n), net = n.services?.filter(x => x !== 'ssh') || [];
    return h('button', { type: 'button', class: 'card ck-node ms-node' + (n.self ? ' self' : ''), 'aria-haspopup': 'dialog', onclick: () => open(n) },
      h('span', { class: 'ms-top' }, h('strong', {}, n.name || '…' + (n.node_id || '').slice(-8)), n.self ? h('small', { class: 'muted' }, t('(vous)')) : null,
        h('span', { class: 'ck-st ' + (n.status === 'known' ? 'off' : 'on') }, n.self ? '' : n.status === 'known' ? t('vue') : t('en ligne'))),
      cap ? h('span', { class: 'ms-capl' }, '👤 ', cap.label, ' ', pill(cap)) : null,
      h('span', {}, `${TIER[n.tier] || ''} ${n.tier} · ${n.power_score}`, n.distance_km == null ? '' : ` · ${n.distance_km} km`, n.storage_free_gb == null ? '' : ` · 💾 ${gb(n.storage_free_gb)}`),
      net.length ? h('span', {}, net.map(x => h('span', { class: 'ck-chip' }, x))) : h('span', { class: 'muted' }, t('aucun service partagé')));
  };
  const grid = h('div', { class: 'grid ms-grid' }), only = h('input', { type: 'checkbox', id: 'ms-ia' });
  const paint = () => grid.replaceChildren(...nodes.filter(n => !only.checked || n.services?.includes('ollama')).map(node));
  only.onchange = paint; paint();

  // Capitaines connus dont la station n'est pas (encore) dans l'essaim vu d'ici : ceux qui attendent un geste d'abord.
  const rank = x => (x.follows_me && !x.i_follow ? 0 : x.friend ? 1 : 2);
  const loose = caps.filter(x => !nodes.some(n => x.peer && n.node_id === x.peer)).sort((x, y) => rank(x) - rank(y));
  const list = h('ul', { class: 'ms-caps' }), find = h('input', { type: 'search', placeholder: t('Chercher un Capitaine'), 'aria-label': t('Chercher un Capitaine') });
  const show = () => list.replaceChildren(...loose.filter(x => !find.value || x.label.toLowerCase().includes(find.value.toLowerCase())).map(cap =>
    h('li', { class: 'ms-cap' }, h('div', { class: 'ms-who' }, h('span', {}, h('strong', {}, cap.label), ' ', pill(cap)), h('small', { class: 'muted' }, shared(cap) || t('Station pas encore vue dans l\'essaim.'))), follow(cap))));
  find.oninput = show; show();

  // Bilan : stations, en ligne, Capitaines suivis, stockage libre annoncé.
  const withDisk = nodes.filter(n => n.storage_free_gb != null), free = withDisk.reduce((s, n) => s + n.storage_free_gb, 0), total = withDisk.reduce((s, n) => s + (n.storage_total_gb || 0), 0);
  const online = nodes.filter(n => n.status !== 'known').length, friends = caps.filter(x => x.friend).length;
  const stat = (v, l) => h('div', { class: 'ms-stat' }, h('strong', {}, v), h('small', { class: 'muted' }, l));
  return h('section', { class: 'card wide' }, h('h2', {}, t('L\'essaim')),
    h('div', { class: 'ms-stats' }, stat(String(nodes.length), t('stations')), stat(String(online), t('en ligne')), stat(`${friends} / ${caps.length}`, t('Capitaines amis')),
      stat(withDisk.length ? gb(free) : '—', withDisk.length ? t('libres sur {0} ({1} station(s))', gb(total), withDisk.length) : t('stockage libre : pas encore annoncé'))),
    h('p', {}, h('label', { for: 'ms-ia' }, only, ' ', t('Seulement celles qui offrent l\'IA (Ollama)'))), grid,
    c?.error ? h('p', { class: 'degraded' }, c.error) : loose.length ? [h('h3', {}, t('Autres Capitaines')), loose.length > 8 ? find : null, list] : null,
    h('p', {}, h('a', { class: 'ck-btn', href: '#/planes' }, t('Constellations'))), dlg);
}
