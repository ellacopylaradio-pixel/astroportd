'use strict';
// Alertes du Capitaine : ce que la station a signalé pendant son absence, gardé jusqu'à ce qu'il le marque traité.
// En tête, les virements à l'issue inconnue (la station s'est arrêtée pendant l'envoi) : rien ne se rejoue seul.
export default async function alerts(root, K) {
  const { h, t } = K;
  const list = h('ul', { class: 'list' }), pend = h('section', { class: 'card', hidden: true });
  const act = async (url, ok, btn) => {
    btn.disabled = true;
    try { await K.api(url, { method: 'POST' }); K.toast(ok, 1); await load(); } catch (e) { K.toast(e.message); btn.disabled = false; }
  };
  const clear = h('button', { type: 'button', class: 'ghost', hidden: true }, t('Tout effacer'));
  clear.onclick = () => { if (confirm(t('Effacer définitivement toutes les alertes ? Les virements en suspens restent affichés.'))) act('/api/v1/station/alerts/clear', t('Alertes effacées'), clear); };
  const load = async () => {
    const r = await K.api('/api/v1/station/alerts');
    pend.hidden = !r.pending.length;
    pend.replaceChildren(h('h2', {}, '⏳ ', t('Virements à l\'issue inconnue')),
      h('p', { class: 'muted' }, t('La station s\'est arrêtée pendant un envoi : vérifiez le solde avant de rejouer quoi que ce soit.')),
      h('ul', {}, ...r.pending.map(p => h('li', {}, h('code', {}, `${p.scope} · ${p.key}`), ' — ', t('depuis le {0}', K.when(p.since))))));
    clear.hidden = !r.alerts.length;
    list.replaceChildren(...(r.alerts.length ? r.alerts.map(a => {
      const done = h('button', { type: 'button', class: 'ghost' }, t('Traité'));
      done.onclick = () => act(`/api/v1/station/alerts/${a.id}/done`, t('Alerte classée'), done);
      const del = h('button', { type: 'button', class: 'ghost', 'aria-label': t('Supprimer') }, '🗑️');
      del.onclick = () => act(`/api/v1/station/alerts/${a.id}/delete`, t('Alerte supprimée'), del);
      return h('li', {}, h('div', {}, h('strong', {}, a.message), h('br'),
        h('small', { class: 'muted' }, a.source, ' · ', a.count > 1 ? t('{0} fois, du {1} au {2}', a.count, K.when(a.first_at), K.when(a.last_at)) : K.when(a.last_at))), h('span', {}, done, ' ', del));
    }) : [h('li', { class: 'muted' }, t('Aucune alerte en attente.'))]));
  };
  root.replaceChildren(pend, h('section', { class: 'card' }, h('h2', {}, '⚠️ ', t('Alertes à traiter')), list, h('p', {}, clear)));
  await load();
}
