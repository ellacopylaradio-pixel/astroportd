'use strict';
// Écluses de la station (Capitaine) : un canal animé à six sas, du coffre au public. Chaque porte est ouverte, filtrée ou
// fermée selon le mode (Forteresse, Village, Place publique) et ce que `serve` a fermé. Un clic sur un mode en montre
// l'aperçu sur le canal ; « Appliquer » seul change la station. SVG seul (CSP stricte), animations dans m-locks.css.
const NS = 'http://www.w3.org/2000/svg';
const el = (tag, attrs = {}, ...kids) => { const e = document.createElementNS(NS, tag); for (const [k, v] of Object.entries(attrs)) e.setAttribute(k, v); e.append(...kids); return e; };

export const MODES = [
  { id: 'forteresse', icon: '🏰', name: 'Forteresse', motto: 'Entre amis réciproques seulement', says: ['Les amis d\'amis n\'entrent plus', 'Inscriptions libres fermées : on entre sur invitation', 'Chaque photo attend votre accord'] },
  { id: 'village', icon: '🏡', name: 'Village', motto: 'Amis et amis d\'amis', says: ['Amis et amis d\'amis écrivent sur le relai', 'Inscriptions selon la station', 'Une photo part seule si l\'ami y est seul avec vous'] },
  { id: 'place', icon: '🎪', name: 'Place publique', motto: 'Ouverte à tous pour parler', says: ['Tout le monde peut publier son profil et des notes publiques', 'Le reste comme au Village', 'Le relai devient un lieu de rencontre : surveillez-le'] },
];

// Les six sas et les cinq portes qui les séparent. État d'une porte : open (le flux passe), filter (sur accord, lecture
// seule…), shut (rien ne passe).
export const ROOMS = ['coffre', 'ucloud', 'mutual', 'circle', 'swarm', 'public'];
export function gates(v, mode) {
  const g = v?.gates || {}, fort = mode === 'forteresse';
  return [
    { state: 'shut', why: 'Jamais partagé' },
    fort ? { state: 'filter', why: 'Photos sur accord' } : { state: 'open', why: 'Photos seul à seul' },
    fort ? { state: 'shut', why: 'Réciproques seulement' } : { state: 'open', why: 'Écrire sur le relai' },
    !g.crawl ? { state: 'shut', why: 'Copie coupée' } : fort ? { state: 'filter', why: 'Copie des amis seulement' } : { state: 'open', why: 'Copie des amis d\'amis' },
    mode === 'place' ? { state: 'open', why: 'Tout le monde publie' } : { state: 'filter', why: 'Lecture seule' },
  ];
}
// Qui se trouve dans chaque sas (chiffres de la vue), pour le mode montré.
export function rooms(v, mode) {
  const p = v?.people || {}, pl = v?.plane || {}, pub = v?.public || {}, fort = mode === 'forteresse';
  return [
    { icon: '🔐', name: 'Coffre', who: ['Mots de passe : le membre seul'] },
    { icon: '☁️', name: 'uCloud chiffré', who: ['{0} membre(s) hébergé(s)', p.hosted || 0] },
    { icon: '🤝', name: 'Amis réciproques', who: ['{0} ami(s) qui suivent en retour', p.n1_mutual || 0] },
    { icon: '👥', name: 'Cercle', who: fort ? ['{0} suivi(s) à sens unique laissé(s) dehors', Math.max(0, (p.n1 || 0) - (p.n1_mutual || 0))] : ['{0} suivi(s) + {1} ami(s) d\'amis', p.n1 || 0, p.n2_known || 0] },
    { icon: pl.origin ? '🌐' : '✨', name: pl.origin ? 'ORIGIN' : 'Constellation ẐEN', who: [pl.origin ? (pl.kubo ? 'Réseau ouvert, avec Kubo' : 'Réseau ouvert') : 'Réseau privé (swarm.key)'] },
    { icon: '🌍', name: 'Public', who: ['{0} fichier(s) dans les dossiers Public', pub.files || 0] },
  ];
}

const W = 960, H = 200, RW = 132, GAP = (W - 6 * RW) / 5;
const rx = i => i * (RW + GAP);
function canal() {
  const svg = el('svg', { class: 'lk-canal', viewBox: `0 0 ${W} ${H}`, role: 'img' });
  svg.append(el('rect', { class: 'lk-bank', x: 0, y: 70, width: W, height: 90, rx: 18 }));
  const parts = { rooms: [], gates: [] };
  ROOMS.forEach((id, i) => {
    const g = el('g', { class: `lk-room lk-r${i}` });
    g.append(el('rect', { class: 'lk-basin', x: rx(i), y: 78, width: RW, height: 74, rx: 12 }),
      el('path', { class: 'lk-wave', d: `M${rx(i) + 6} 100 q 15 -8 30 0 t 30 0 t 30 0 t 30 0` }),
      el('text', { class: 'lk-ico', x: rx(i) + RW / 2, y: 138, 'text-anchor': 'middle' }, ''));
    svg.append(g);
    parts.rooms.push(g);
  });
  for (let i = 0; i < 5; i++) {
    const x = rx(i) + RW, g = el('g', { class: 'lk-gate' });
    g.append(el('line', { class: 'lk-flow', x1: x - 26, y1: 122, x2: x + GAP + 26, y2: 122 }),
      el('rect', { class: 'lk-door', x: x + GAP / 2 - 5, y: 70, width: 10, height: 90, rx: 4 }),
      el('circle', { class: 'lk-light', cx: x + GAP / 2, cy: 52, r: 9 }),
      el('text', { class: 'lk-sign', x: x + GAP / 2, y: 188, 'text-anchor': 'middle' }, ''));
    svg.append(g);
    parts.gates.push(g);
  }
  // Bateaux : partent du uCloud et vont aussi loin que les portes le permettent.
  for (let b = 0; b < 3; b++) svg.append(el('circle', { class: `lk-boat lk-b${b}`, cx: rx(1) + 20, cy: 122, r: 7 }));
  return { svg, parts };
}

export default async function locks(root, K) {
  const { h, t } = K;
  const link = h('link', { rel: 'stylesheet', href: '/ui/m-locks.css' }); document.head.append(link); K.cleanup(() => link.remove());
  let v = await K.api('/api/v1/station/admin/locks'), shown = v.mode;
  const { svg, parts } = canal();
  svg.setAttribute('aria-label', t('Canal des écluses : du coffre au public'));
  const cards = h('div', { class: 'lk-rooms' }), facts = h('ul', { class: 'lk-facts' }), apply = h('div', { class: 'lk-apply' });
  const modes = h('div', { class: 'lk-modes', role: 'radiogroup', 'aria-label': t('Mode de la station') });

  const draw = () => {
    const gs = gates(v, shown), rs = rooms(v, shown);
    // Jusqu'où vont les bateaux : la première porte fermée après le uCloud les arrête.
    const reach = gs.findIndex((g, i) => i > 0 && g.state === 'shut');
    svg.dataset.reach = String(reach < 0 ? 5 : reach);
    svg.dataset.mode = shown;
    parts.gates.forEach((g, i) => { g.setAttribute('class', `lk-gate lk-${gs[i].state}`); g.lastChild.textContent = t(gs[i].why); });
    parts.rooms.forEach((g, i) => { g.lastChild.textContent = rs[i].icon; g.classList.toggle('lk-dry', i > (reach < 0 ? 5 : reach) && i > 1); });
    cards.replaceChildren(...rs.map((r, i) => h('div', { class: 'lk-card' + (i > (reach < 0 ? 5 : reach) && i > 1 ? ' lk-far' : '') },
      h('span', { class: 'lk-big', 'aria-hidden': 'true' }, r.icon), h('strong', {}, t(r.name)), h('span', { class: 'muted' }, t(...r.who)),
      i > 0 && h('span', { class: `lk-k lk-${gs[i - 1].state}` }, t(gs[i - 1].why)))));
    modes.querySelectorAll('button').forEach(b => { b.setAttribute('aria-checked', String(b.dataset.mode === shown)); b.classList.toggle('lk-now', b.dataset.mode === v.mode); });
    apply.replaceChildren(...(shown === v.mode ? [h('p', { class: 'muted' }, t('Mode actuel : {0}. Touchez un autre mode pour voir ce qu\'il change sur le canal.', t(MODES.find(m => m.id === v.mode)?.name || v.mode)))] : [
      h('p', {}, t('Aperçu : {0}. Rien n\'est encore changé.', t(MODES.find(m => m.id === shown).name))),
      h('button', { onclick: async ev => {
        ev.target.disabled = true;
        try { v = await K.api('/api/v1/station/admin/locks', { method: 'POST', body: { mode: shown } }); K.toast(t('Mode appliqué : {0}', t(MODES.find(m => m.id === v.mode).name)), 1); draw(); } catch (e) { K.toast(e.message); ev.target.disabled = false; }
      } }, t('Appliquer ce mode')), ' ',
      h('button', { class: 'ghost', onclick: () => { shown = v.mode; draw(); } }, t('Annuler'))]));
    const g = v.gates, pl = v.plane;
    facts.replaceChildren(...[
      pl.origin ? t('ORIGIN : réseau ouvert, bac à sable (1 Ẑen = 0,1 Ğ1).') : t('ẐEN : réseau privé, on n\'y entre qu\'avec la swarm.key.'),
      pl.kubo && t('Kubo branché : le chiffré d\'ORIGIN est aussi sur l\'IPFS public.'),
      pl.pool_peers > 0 && t('Bassin de blobs partagé avec {0} autre(s) plan(s) : chaque plan ne sert que ses propres fichiers.', pl.pool_peers),
      !pl.p2p && t('Maillage P2P coupé : la station ne parle qu\'à ses relais.'),
      !g.register_cli ? t('Inscriptions libres fermées au lancement (--no-register).') : g.register ? t('Inscriptions libres ouvertes.') : t('Inscriptions libres fermées par la Forteresse (les invitations restent possibles).'),
      !g.crawl && t('Copie des amis d\'amis coupée au lancement (--no-crawl).'),
      g.open_writes.length > 0 && t('Tout le monde peut publier ces kinds sur le relai : {0}.', g.open_writes.join(', ')),
      v.faces.auto_off_members > 0 && t('{0} membre(s) ont coupé l\'envoi automatique de leurs photos.', v.faces.auto_off_members),
      v.public.sensitive > 0 ? t('Vigie de 20h12 : {0} fichier(s) de Public en disent trop, chez {1} membre(s) prévenu(s).', v.public.sensitive, v.public.members) : t('Vigie de 20h12 : rien de sensible dans les dossiers Public.'),
    ].filter(Boolean).map(x => h('li', {}, x)));
  };

  modes.append(...MODES.map(m => h('button', { class: 'lk-mode', role: 'radio', 'data-mode': m.id, onclick: () => { shown = m.id; draw(); } },
    h('span', { class: 'lk-big', 'aria-hidden': 'true' }, m.icon), h('strong', {}, t(m.name)), h('em', {}, t(m.motto)),
    h('ul', {}, m.says.map(s => h('li', {}, t(s)))))));

  // « Suivre » : trois liens, trois portées. Les flèches animées montrent le sens.
  const arrow = (cls, two) => el('svg', { class: 'lk-arrow', viewBox: '0 0 120 24', 'aria-hidden': 'true' },
    el('line', { class: `lk-a ${cls}`, x1: 8, y1: 12, x2: 112, y2: 12 }), el('path', { class: `lk-tip ${cls}`, d: 'M104 5 L114 12 L104 19' }), ...(two ? [el('path', { class: `lk-tip ${cls}`, d: 'M16 5 L6 12 L16 19' })] : []));
  const follow = h('section', { class: 'card' }, h('h2', {}, t('Comprendre « Suivre »')),
    h('div', { class: 'lk-follow' },
      h('div', {}, h('div', { class: 'lk-pair' }, '🙂', arrow('lk-one'), '🙂'), h('strong', {}, t('Un membre suit quelqu\'un')), h('p', { class: 'muted' }, t('Cette personne entre dans le cercle : elle peut écrire sur le relai et ses publications sont copiées ici. Rien de privé ne part vers elle.'))),
      h('div', {}, h('div', { class: 'lk-pair' }, '🙂', arrow('lk-two', true), '🙂'), h('strong', {}, t('Ils se suivent l\'un l\'autre')), h('p', { class: 'muted' }, t('Amis réciproques : photos où l\'ami est seul avec le membre, garde de sa sauvegarde et d\'un morceau de sa clé de secours, tunnels « amis ». À réserver aux proches de confiance.'))),
      h('div', {}, h('div', { class: 'lk-pair' }, '🙂', arrow('lk-far2'), '👤', arrow('lk-far2'), '🙂'), h('strong', {}, t('Ami d\'un ami')), h('p', { class: 'muted' }, t('Peut écrire sur le relai et ses notes sont copiées, sauf en Forteresse. Il ne reçoit jamais rien de privé.')))));

  root.replaceChildren(
    h('section', { class: 'card' }, h('h2', {}, t('Écluses de la station')),
      h('p', { class: 'muted' }, t('Vos données vont du coffre (le membre seul) jusqu\'au public. Entre deux bassins, une porte : verte, le flux passe ; orange, il passe sur accord ou en lecture seule ; rouge, rien ne passe.')),
      svg, h('div', { class: 'lk-legend', 'aria-hidden': 'true' }, h('span', { class: 'lk-k lk-open' }, t('ouverte')), h('span', { class: 'lk-k lk-filter' }, t('filtrée')), h('span', { class: 'lk-k lk-shut' }, t('fermée'))), cards),
    h('section', { class: 'card' }, h('h2', {}, t('Mode de la station')), modes, apply),
    follow,
    h('section', { class: 'card' }, h('h2', {}, t('Cette station')), facts,
      h('p', { class: 'muted' }, t('Réglage fin, service par service et kind par kind : '), h('a', { href: '#/services' }, t('Partages')), ' · ', h('a', { href: '#/options' }, t('Réglages')), '.')));
  draw();
}
