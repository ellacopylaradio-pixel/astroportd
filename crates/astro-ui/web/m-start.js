'use strict';
// Mon compte, onglet Moi : « Bien démarrer » (contacts, appareils, photos cherchables : la carte disparaît quand tout est fait) et
// « Mon espace » (la part de chaque nature de fichier, en privé et en public, en deux anneaux). Remplit deux cartes déjà placées.
export default function start(K, { start, space, head, nDevices, guide }) {
  const { h, t } = K;
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
        : [h('p', {}, t('Aucun ami ne garde encore vos données : si la station tombe, elles sont perdues.'), ' ', h('a', { href: '#/account?tab=securite' }, t('Choisir un gardien')))]),
      ...(sp.hosted.length ? [h('h4', {}, t('Ce que vous gardez pour vos amis'), ' ', h('small', { class: 'muted' }, K.bytes(sp.hosted_total))),
        ...sp.hosted.map(f => f.quota ? gauge(f.name, f.bytes, f.quota, t('{0} sur {1} accordés', K.bytes(f.bytes), K.bytes(f.quota))) : h('p', { class: 'muted ac-who' }, `${f.name} · ${K.bytes(f.bytes)}`))] : []),
    ];
  };
  K.api('/api/v1/me/footprint').then(fp => {
    if (!fp.disk) return;
    const d = fp.disk, bk = d.by_kind || { ucloud: {}, public: {} }, all = k => (bk.ucloud[k] || 0) + (bk.public[k] || 0);
    space.hidden = false;
    space.append(head(t('Mon espace'), t('Ce que vos fichiers occupent, par nature. Le détail est dans Fichiers ; le dossier Public est lisible par tous.')),
      h('p', {}, d.unlimited ? t('{0} utilisés', K.bytes(d.used_bytes)) : t('{0} utilisés sur {1}', K.bytes(d.used_bytes), K.bytes(d.quota_bytes))),
      d.unlimited || !d.quota_bytes ? null : h('meter', { min: 0, max: d.quota_bytes, value: d.used_bytes, high: d.quota_bytes * .9, 'aria-label': t('Espace utilisé'), class: 'ac-meter' }),
      h('div', { class: 'ac-donuts' }, ring(t('Privé'), bk.ucloud), ring(t('Public'), bk.public)),
      h('ul', { class: 'ac-legend' }, KINDS.filter(([k]) => all(k)).map(([k, ico, name]) => h('li', {}, h('i', { class: 'ac-k-' + k, 'aria-hidden': 'true' }), `${ico} ${name} `, h('span', { class: 'muted' }, K.bytes(all(k)))))),
      ...where(d.spread),
      h('p', {}, h('a', { href: '#/cloud' }, t('Ouvrir mes fichiers') + ' →')));
  }, () => {});

  // ── Bien démarrer : les trois gestes qui donnent toute sa force à la station. La carte disparaît quand tout est fait.
  Promise.all([K.api('/api/v1/book?q=').then(b => b.contacts.length, () => 0), K.api('/api/v1/me/reading').then(v => v.state === 'local' || v.state === 'friend', () => true)]).then(([nContacts, searching]) => {
    const steps = [
      [false, '🚚', t('Je viens de Google ou d’Apple'), t('Photos, albums, contacts, agenda et documents en un geste, sans doublons.'), '#/move', t('Déménager')],
      [nContacts > 0, '📇', t('Mes contacts'), t('Importez ou synchronisez votre carnet : les visages reconnus se rangent dans les fiches.'), '#/people', t('Importer')],
      [nDevices > 0, '⇅', t('Mes appareils'), t('Branchez téléphone et ordinateur : ⬆ vos photos partent seules, ⬇ vos fichiers s’ouvrent comme un disque.'), '#/account?tab=appareils', t('Brancher')],
      [searching, '🔍', t('Photos cherchables'), t('Retrouvez une photo par ce qu’elle montre ou par le texte qu’elle contient.'), '#/account?tab=moi&card=reading', t('Activer')]];
    const left = steps.slice(1).filter(x => !x[0]).length;
    if (!left) return;
    start.hidden = false;
    start.append(head(t('Bien démarrer'), t('Trois gestes, une seule fois. Le guide détaille chaque appareil : Android, iPhone, ordinateur.')),
      h('ol', { class: 'ac-steps' }, steps.map(([done, ico, name, why, href, go]) => h('li', { class: done ? 'done' : '' },
        h('span', { class: 'ac-step-ico', 'aria-hidden': 'true' }, done ? '✓' : ico),
        h('span', { class: 't' }, h('b', {}, name), h('small', { class: 'muted' }, done ? t('Fait') : why)),
        done ? null : h('a', { class: 'button', href }, go)))),
      h('p', {}, h('a', { href: guide, target: '_blank', rel: 'noopener' }, '📖 ' + t('Guide pas à pas : Android, iPhone, ordinateur'))));
  });

}
