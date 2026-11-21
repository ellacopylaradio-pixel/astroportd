'use strict';
// Page d'une personne reconnue (#/person?n=Léa) : ses photos, depuis quand, combien par année, avec qui elle apparaît le plus,
// les voyages où elle était. Tout vient des visages que le membre a nommés ; rien ne quitte la station.
export default async function person(root, K, qs) {
  const { h, t } = K, name = qs.get('n') || '', uid = qs.get('uid');
  const css = h('link', { rel: 'stylesheet', href: '/ui/m-photos.css' }); document.head.append(css); K.cleanup(() => css.remove());
  const { tripCard, personHref } = await K.mod('m-trips.js');
  const grid = (items, list = []) => { const g = h('div', { class: 'photos' }); K.tiles(g, list, items); return g; };
  const day = s => new Date(s * 1000).toLocaleDateString(K.lang, { day: 'numeric', month: 'long', year: 'numeric', timeZone: 'UTC' });
  root.replaceChildren(h('h2', {}, name), h('p', { class: 'muted' }, t('Chargement…')));
  let r;
  try { r = await K.api('/api/v1/ucloud/person?name=' + encodeURIComponent(name) + (uid ? '&uid=' + encodeURIComponent(uid) : '')); } catch (e) { root.lastChild.replaceWith(h('p', { class: 'err', role: 'alert' }, e.message)); return; }
  if (!r.photos) {
    root.lastChild.replaceWith(h('p', {}, t('Aucune photo de {0} pour l’instant : nommez son visage dans Proches › Trier les visages.', name), ' ', h('a', { href: '#/people/tri' }, t('Trier les visages'))));
    return;
  }
  const max = Math.max(...r.years.map(y => y.photos));
  root.replaceChildren(...[
    h('h2', {}, name),
    h('p', { class: 'muted' }, t('{0} photo(s), du {1} au {2}', r.photos, day(r.first), day(r.last))),
    h('section', { class: 'card wide' }, h('h3', {}, t('Au fil des années')),
      h('ul', { class: 'years' }, r.years.map(y => h('li', {}, h('span', {}, String(y.year)), h('meter', { min: 0, max, value: y.photos, 'aria-label': t('{0} photo(s)', y.photos) }), h('small', {}, String(y.photos)))))),
    r.with.length ? h('section', { class: 'card wide' }, h('h3', {}, t('Souvent avec')), h('p', { class: 'fd-chips' }, r.with.map(p => h('a', { class: 'pill', href: personHref(p.name) }, `${p.name} · ${p.photos}`)))) : null,
    h('section', { class: 'card wide' }, h('h3', {}, t('Dernières photos')), grid(r.items),
      h('p', {}, h('a', { class: 'btn ghost', href: '#/find?q=' + encodeURIComponent(name) }, t('Toutes les photos de {0}', name)))),
    ...(r.trips.length ? [h('h3', {}, t('Voyages ensemble')), ...r.trips.map(tr => tripCard(K, tr, grid))] : []),
  ].filter(Boolean));
}
