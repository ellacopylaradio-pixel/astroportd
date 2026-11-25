'use strict';
// Mes Ẑen › historique des virements du membre, le plus récent d'abord, par pages (« Voir plus »). Montants en centimes
// entiers, mis en forme sans flottant ; la contrepartie est nommée quand la station la connaît, sinon son adresse courte.
export const zc = c => `${Math.trunc(c / 100)},${String(c % 100).padStart(2, '0')}`;
// Genre d'un virement fait par la station (référence technique traduite par le serveur).
const KIND = { credit: 'Crédit de Ẑen par la coopérative', share: 'Part de sociétaire', rnd: 'Part recherche et développement', assets: 'Part matériel', captain: 'Part du Capitaine', cash: 'Apport à la coopérative', transfer: 'Virement entre membres', station: 'Opération de la station' };
export const short = a => (a ? `${a.slice(0, 6)}…${a.slice(-4)}` : '');

export default function history(K) {
  const { h, t, api } = K;
  const list = h('ul', { class: 'zh-list' }), note = h('p', { class: 'muted', role: 'status' });
  const more = h('button', { type: 'button', class: 'ghost', hidden: true }, t('Voir plus'));
  const box = h('section', { class: 'card wide zh', id: 'historique', 'aria-labelledby': 'zh-t' }, h('h2', { id: 'zh-t' }, t('Historique des virements')), note, list, h('p', {}, more));
  const who = x => (x.coop ? t('La coopérative') : x.name || short(x.other) || t('adresse inconnue'));
  const row = x => h('li', { class: 'zh-' + (x.dir === 'out' ? 'out' : 'in') },
    h('span', { class: 'zh-ic', 'aria-hidden': 'true' }, x.dir === 'out' ? '↗' : '↙'),
    h('span', { class: 'zh-who' }, h('strong', {}, t(x.dir === 'out' ? 'Envoyé à {0}' : 'Reçu de {0}', who(x))),
      x.memo || x.kind ? h('small', { class: 'muted' }, x.memo || t(KIND[x.kind] || KIND.station)) : null,
      h('small', { class: 'muted' }, new Date(x.at * 1000).toLocaleString(undefined, { dateStyle: 'medium', timeStyle: 'short' }))),
    h('span', { class: 'zh-amt' }, `${x.dir === 'out' ? '−' : '+'}${zc(x.zen_cents)} Ẑ`));
  let next = null, busy = false;
  const load = async () => {
    if (busy) return;
    busy = true; more.disabled = true;
    try {
      const r = await api('/api/v1/me/zen/history' + (next ? '?after=' + encodeURIComponent(next) : ''));
      note.className = 'muted';
      note.textContent = !r.indexer ? t('Cette station ne lit pas encore le détail des virements : votre solde reste juste, seul l’historique manque.')
        : !list.childElementCount && !r.items.length ? t('Aucun virement pour l’instant. Ce que vous recevez et envoyez s’affichera ici.') : '';
      list.append(...r.items.map(row));
      next = r.next; more.hidden = !next;
    } catch (e) { note.className = 'degraded'; note.textContent = e.message; } finally { busy = false; more.disabled = false; }
  };
  more.onclick = load;
  load();
  return box;
}
