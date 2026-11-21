'use strict';
// Voyages repérés par la station (photos loin de chez soi plusieurs jours de suite) : une carte par voyage, partagée par
// Photos › Voyages et la page d'une personne. Le nom vient du dossier où le membre a rangé ses photos, sinon des dates.
const ymd = s => new Date(s * 1000).toISOString().slice(0, 10);
export const span = (K, a, b) => {
  const o = { day: 'numeric', month: 'long', year: 'numeric', timeZone: 'UTC' }, A = new Date(a * 1000), B = new Date(b * 1000);
  return ymd(a) === ymd(b) ? A.toLocaleDateString(K.lang, o) : K.t('du {0} au {1}', A.toLocaleDateString(K.lang, A.getUTCFullYear() === B.getUTCFullYear() ? { day: 'numeric', month: 'long', timeZone: 'UTC' } : o), B.toLocaleDateString(K.lang, o));
};
export const personHref = n => '#/person?n=' + encodeURIComponent(n);
export function tripCard(K, tr, grid) {
  const { h, t } = K, when = span(K, tr.from, tr.to);
  return h('section', { class: 'card wide trip' },
    h('h3', {}, tr.name || t('Voyage {0}', when)),
    h('p', { class: 'muted' }, [tr.name ? when : '', t('{0} jour(s) · {1} photo(s) · à {2} km de chez vous', tr.days, tr.photos, tr.km)].filter(Boolean).join(' · ')),
    tr.people.length ? h('p', { class: 'fd-chips' }, t('Avec'), ' ', tr.people.map(p => h('a', { class: 'pill', href: personHref(p.name) }, p.name))) : null,
    grid(tr.covers),
    h('p', {}, h('a', { class: 'btn ghost', href: `#/find?from=${ymd(tr.from)}&to=${ymd(tr.to)}` }, t('Toutes les photos de ce voyage'))));
}
export async function tripsView(K, grid) {
  const { h, t } = K, box = h('div', {}, h('p', { class: 'muted' }, t('Chargement…')));
  K.api('/api/v1/ucloud/trips').then(r => {
    box.replaceChildren(...(r.trips.length ? r.trips.map(tr => tripCard(K, tr, grid))
      : [h('p', { class: 'muted' }, t('Aucun voyage repéré pour l’instant : la station les reconnaît aux photos datées et situées, prises loin de chez vous plusieurs jours de suite.'))]));
  }, e => box.replaceChildren(h('p', { class: 'err', role: 'alert' }, e.message)));
  return box;
}
