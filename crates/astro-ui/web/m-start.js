'use strict';
// Deux cartes d'après l'empreinte du membre (/api/v1/me/footprint), chargées avec leur page : « Mon espace » (Moi › Compte : la
// part de chaque nature de fichier, en privé et en public, en deux anneaux) et « Où sont vos données » (Moi › Sauvegarde : par
// zone, ce qui est à l'abri chez les amis, ce qu'on garde pour eux). « Bien démarrer » n'existe qu'à l'accueil (m-home.js).
function draw(K, el, which, fp) {
  const { h, t } = K, head = K.head;
  // ── Mon espace : la part de chaque nature de fichier, en privé (uCloud) et en public, en deux anneaux (le détail est dans Fichiers).
  const KINDS = [['photo', '📷', t('Photos')], ['video', '🎬', t('Vidéos')], ['music', '🎵', t('Musique')], ['doc', '📄', t('Documents')], ['other', '📦', t('Autres')]];
  const NS = 'http://www.w3.org/2000/svg', sv = (tag, at) => { const e = document.createElementNS(NS, tag); for (const k in at) e.setAttribute(k, at[k]); return e; };
  const ring = (label, parts, kinds = KINDS, cls = 'ac-k-') => {
    const sum = kinds.reduce((n, [k]) => n + (parts[k] || 0), 0), svg = sv('svg', { viewBox: '0 0 42 42', class: 'ac-ring', role: 'img', 'aria-label': `${label} : ${K.bytes(sum)}` });
    svg.append(sv('circle', { cx: 21, cy: 21, r: 15.9155, class: 'ac-ring-bg' }));
    let at = 25; // départ à midi : le cercle fait 100 de périmètre (r = 100 / 2π)
    for (const [k] of kinds) {
      const pc = sum ? 100 * (parts[k] || 0) / sum : 0;
      if (pc > 0) svg.append(sv('circle', { cx: 21, cy: 21, r: 15.9155, class: cls + k, 'stroke-dasharray': `${pc} ${100 - pc}`, 'stroke-dashoffset': at }));
      at = (at - pc + 100) % 100;
    }
    return h('figure', { class: 'ac-donut' }, svg, h('figcaption', {}, h('b', {}, label), h('br'), h('span', { class: 'muted' }, sum ? K.bytes(sum) : t('vide'))));
  };
  // ── Où sont vos données, ce qui est à l'abri chez vos amis, ce que vous gardez pour eux (jauges). ──
  const ZONES = [['prive', '🔒', t('Privé')], ['partage', '🔗', t('Partagé')], ['public', '🌍', t('Public')], ['coffre', '🗝️', t('Coffre')],
    ['souvenirs', '🫂', t('Visages, messages et mémoire')], ['recherche', '🔍', t('Index de recherche')], ['carnet', '📇', t('Carnet et agenda')]];
  const gauge = (label, value, max, note) => h('div', { class: 'ac-gauge' }, h('span', {}, label, ' ', h('small', { class: 'muted' }, note)),
    h('meter', { min: 0, max: Math.max(max, 1), value, low: max * .5, high: max * .9, optimum: max, 'aria-label': label, class: 'ac-meter' }));
  const where = sp => {
    if (!sp || !sp.total) return [];
    const parts = Object.fromEntries(Object.entries(sp.zones).map(([k, v]) => [k, v.bytes])), pc = Math.round(100 * sp.safe / sp.total);
    return [
      h('h4', {}, t('Où sont vos données')),
      h('div', { class: 'ac-donuts' }, ring(t('Mes données'), parts, ZONES, 'ac-z-'),
        h('ul', { class: 'ac-legend' }, ZONES.filter(([k]) => parts[k]).map(([k, ico, name]) => h('li', {}, h('i', { class: 'ac-z-' + k, 'aria-hidden': 'true' }), `${ico} ${name} `, h('span', { class: 'muted' }, K.bytes(parts[k])))))),
      h('h4', {}, t('À l’abri chez vos amis')),
      gauge(t('{0} % de vos données', pc), sp.safe, sp.total, t('{0} sur {1}', K.bytes(sp.safe), K.bytes(sp.total))),
      ...(sp.guardians.length ? sp.guardians.map(g => h('p', { class: 'muted ac-who' }, t('{0} garde {1} pour vous', g.name, K.bytes(g.bytes))))
        : [h('p', { class: 'warn' }, t('Aucun ami ne garde encore vos données : si la station tombe, elles sont perdues.'))]),
      ...(sp.hosted.length ? [h('h4', {}, t('Ce que vous gardez pour vos amis'), ' ', h('small', { class: 'muted' }, K.bytes(sp.hosted_total))),
        ...sp.hosted.map(f => f.quota ? gauge(f.name, f.bytes, f.quota, t('{0} sur {1} accordés', K.bytes(f.bytes), K.bytes(f.quota))) : h('p', { class: 'muted ac-who' }, `${f.name} · ${K.bytes(f.bytes)}`))] : []),
    ];
  };
  (fp || K.api('/api/v1/me/footprint')).then(fp => {
    if (!fp?.disk) return;
    const d = fp.disk, bk = d.by_kind || { ucloud: {}, public: {} }, all = k => (bk.ucloud[k] || 0) + (bk.public[k] || 0);
    el.hidden = false;
    if (which === 'spread') return d.spread?.total ? el.append(head(t('Où sont vos données'), t('Vos fichiers par zone, la part copiée chez vos gardiens et ce que vous gardez pour vos amis.')), ...where(d.spread).slice(1)) : (el.hidden = true);
    el.append(head(t('Mon espace'), t('Ce que vos fichiers occupent, par nature. Le détail est dans Fichiers ; le dossier Public est lisible par tous.')),
      h('p', {}, d.unlimited ? t('{0} utilisés', K.bytes(d.used_bytes)) : t('{0} utilisés sur {1}', K.bytes(d.used_bytes), K.bytes(d.quota_bytes))),
      d.unlimited || !d.quota_bytes ? null : h('meter', { min: 0, max: d.quota_bytes, value: d.used_bytes, high: d.quota_bytes * .9, 'aria-label': t('Espace utilisé'), class: 'ac-meter' }),
      h('div', { class: 'ac-donuts' }, ring(t('Privé'), bk.ucloud), ring(t('Public'), bk.public)),
      h('ul', { class: 'ac-legend' }, KINDS.filter(([k]) => all(k)).map(([k, ico, name]) => h('li', {}, h('i', { class: 'ac-k-' + k, 'aria-hidden': 'true' }), `${ico} ${name} `, h('span', { class: 'muted' }, K.bytes(all(k)))))),
      h('p', {}, h('a', { href: '#/cloud' }, t('Ouvrir mes fichiers') + '\u00a0→'), ' · ', h('a', { href: '#/backup' }, t('Qui les garde à l’abri') + '\u00a0→')));
  }, () => {});

}

export const space = (K, el) => draw(K, el, 'space');
export const spread = (K, el, fp) => draw(K, el, 'spread', fp);
