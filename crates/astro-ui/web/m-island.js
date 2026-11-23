'use strict';
// Île « Ğ1 en différé » (docs/design/ISLAND.md). Deux vues, un seul écran :
//  - membre (#/island) : ses promesses envoyées et reçues, et ce qu'il peut encore promettre. Envoyer ne se fait PAS ici :
//    le bouton habituel « Envoyer des Ẑen » bascule seul en promesse quand l'île est ouverte ;
//  - Capitaine (#/ile) : état du réseau Ğ1, ouverture et fermeture de l'île, ouverture automatique, stations voisines en île
//    (une coupure en touche souvent plusieurs), promesses et règlements.
const ago = s => (s ? new Date(s * 1000).toLocaleString(undefined, { dateStyle: 'medium', timeStyle: 'short' }) : '—');
const short = x => (x ? `${x.slice(0, 8)}…` : '—');
const SETTLE = { paid: ['ok', 'Réglée en Ğ1'], short: ['degraded', 'En attente de fonds'], unknown: ['degraded', 'À vérifier par le Capitaine'], sending: ['muted', 'Règlement en cours'] };

export default async function island(root, K) {
  const { h, t, api } = K;
  if (location.hash.startsWith('#/ile')) return captain(root, K);
  const names = new Map();
  await api('/api/v1/friends').then(r => r.friends.forEach(f => names.set(f.hex, f.label)), () => {});
  const who = hex => names.get(hex) || short(hex);
  const draw = async () => {
    let v;
    try { v = await api('/api/v1/island'); } catch (e) { root.replaceChildren(h('h2', {}, t('Ğ1 en différé')), h('p', { class: 'muted' }, e.message)); return; }
    const head = v.open
      ? h('section', { class: 'card' },
          h('p', { class: 'degraded', role: 'status' }, '🏝️ ', t('Réseau Ğ1 coupé depuis le {0}.', ago(v.opened_at))),
          v.frozen ? h('p', { class: 'err', role: 'alert' }, t('Deux promesses contradictoires ont été vues : vos promesses sont bloquées jusqu\'au retour du réseau.'))
            : v.credit ? [h('p', { class: 'big' }, v.available_zen, h('small', { class: 'muted' }, ' Ẑen')), h('p', { class: 'muted' }, t('Vous pouvez encore promettre {0} Ğ1 : votre dernier solde connu, plus vos dividendes du jour.', v.available))]
            : h('p', { class: 'muted' }, t('Votre solde n\'était pas connu de la station avant la coupure : vous pouvez recevoir des promesses, pas en faire.')),
          h('p', { class: 'muted' }, t('Vos envois à vos proches deviennent des promesses. Votre station les règle en Ğ1 depuis votre portefeuille dès le retour du réseau.')),
          v.credit && !v.frozen ? h('p', {}, h('button', { type: 'button', onclick: async () => (await K.mod('m-credit.js')).default(K, { mode: 'member', done: () => draw() }) }, t('Envoyer des Ẑen'))) : null)
      : h('section', { class: 'card' }, h('p', { class: 'ok', role: 'status' }, '✅ ', t('Le réseau Ğ1 répond : vos envois partent directement.')));
    const rows = v.promises || [];
    root.replaceChildren(h('h2', {}, t('Ğ1 en différé')), head,
      h('section', { class: 'card' }, h('h2', {}, t('Promesses')),
        rows.length ? h('ul', { class: 'files' }, rows.map(p => {
          const [cls, label] = p.settled ? SETTLE[p.settled] || ['muted', p.settled] : p.state === 'accepted' ? ['muted', 'À régler au retour du réseau'] : p.state === 'refused' ? ['err', 'Refusée'] : ['muted', 'En vérification'];
          return h('li', {},
            h('strong', {}, p.sent ? t('À {0}', who(p.dest)) : t('De {0}', who(p.author))),
            h('span', {}, ` ${p.amount_zen} Ẑen (${p.amount} Ğ1) · ${ago(p.at)} · `),
            h('span', { class: cls }, t(label)), p.state === 'refused' && p.reason ? h('small', { class: 'muted' }, ` (${p.reason})`) : null,
            !p.sent && !p.known_station ? h('small', { class: 'degraded' }, ' ⚠️ ', t('garantie par une station inconnue d\'ici')) : null);
        })) : h('p', { class: 'muted' }, t('Aucune promesse.'))));
  };
  await draw();
  K.every(30000, () => draw().catch(() => {}));
}

async function captain(root, K) {
  const { h, t, api, toast } = K;
  const draw = async () => {
    let s;
    try { s = await api('/api/v1/station/island'); } catch (e) { root.replaceChildren(h('h2', {}, t('Île Ğ1')), h('p', { class: 'err', role: 'alert' }, e.message)); return; }
    const act = (path, body) => api(`/api/v1/station/island/${path}`, { method: 'POST', body: body || {} }).then(() => draw(), e => toast(e.message, 0));
    const net = s.down_since ? h('p', { class: 'degraded', role: 'status' }, '📡 ', t('Duniter injoignable depuis le {0}', ago(s.down_since))) : h('p', { class: 'ok', role: 'status' }, '📡 ', t('Duniter répond'));
    const state = s.open
      ? [h('p', { class: 'big' }, '🏝️ ', t('Île ouverte')), h('p', { class: 'muted' }, t('Depuis le {0} · {1}', ago(s.opened_at), s.island)),
         K.sure(t('Fermer l\'île'), () => act('close'), t('Plus de nouvelles promesses ; celles faites seront réglées dès que Duniter répond.'))]
      : [h('p', { class: 'big' }, t('Île fermée')), h('p', { class: 'muted' }, t('Les envois passent directement par la Ğ1.')),
         K.sure(t('Ouvrir l\'île maintenant'), () => act('open'), t('Vos membres pourront promettre leur dernier solde connu plus leurs dividendes ; la station referme l\'île dès que Duniter répond.'))];
    const opts = [[null, 'Jamais seule'], [15, '15 min'], [30, '30 min'], [60, '1 h'], [120, '2 h'], [240, '4 h']];
    const cur = s.auto_after_secs == null ? null : Math.round(s.auto_after_secs / 60);
    const sel = h('select', { id: 'isl-auto', onchange: () => act('auto', { minutes: sel.value === '' ? null : Number(sel.value) }) },
      opts.map(([m, l]) => h('option', { value: m ?? '', selected: m === cur }, t(l))),
      cur != null && !opts.some(([m]) => m === cur) ? h('option', { value: cur, selected: true }, `${cur} min`) : null);
    const count = (o, k) => (o && o[k]) || 0;
    const p = s.promises || {}, st = s.settlements || {};
    root.replaceChildren(h('h2', {}, t('Île Ğ1')),
      h('div', { class: 'grid' },
        h('section', { class: 'card' }, h('h2', {}, t('État')), net, ...state),
        h('section', { class: 'card' }, h('h2', {}, t('Ouverture automatique')),
          h('label', { for: 'isl-auto' }, t('Ouvrir l\'île seule après une coupure de'), ' ', sel),
          h('p', { class: 'muted' }, t('Instantané des soldes : {0} · {1} membre(s)', ago(s.snapshot_at), s.snapshot_members)),
          h('p', { class: 'muted' }, t('Sans instantané récent, l\'île ne peut rien promettre : il est pris toutes les 6 h tant que Duniter répond.'))),
        h('section', { class: 'card' }, h('h2', {}, t('Promesses')),
          h('p', {}, t('{0} acceptée(s) · {1} refusée(s) · {2} en vérification', count(p, 'accepted'), count(p, 'refused'), count(p, 'pending'))),
          h('p', {}, t('{0} réglée(s) en Ğ1 · {1} en attente de fonds · {2} à vérifier', count(st, 'paid'), count(st, 'short'), count(st, 'unknown') + count(st, 'sending'))),
          count(st, 'unknown') + count(st, 'sending') ? h('p', { class: 'err', role: 'alert' }, t('Un règlement a une issue inconnue : vérifiez le portefeuille du membre sur la Ğ1. Il ne sera jamais renvoyé seul.')) : null),
        h('section', { class: 'card' }, h('h2', {}, t('Stations voisines en île')),
          s.neighbors?.length ? h('ul', { class: 'files' }, s.neighbors.map(n => h('li', {},
            h('strong', {}, short(n.node)), ' ', h('span', { class: n.known_station ? 'ok' : 'degraded' }, n.known_station ? t('connue') : t('inconnue')),
            h('span', { class: 'muted' }, ` · ${t('{0} membre(s), {1} promesse(s)', n.members, n.promises)} · ${ago(n.at)}`))))
            : h('p', { class: 'muted' }, t('Aucune autre station vue en île.')),
          h('p', { class: 'muted' }, t('Une coupure touche souvent plusieurs stations : leurs promesses arrivent avec le crédit signé de leur station. Une station inconnue ne vaut que sa parole.')))));
  };
  await draw();
  K.every(30000, () => draw().catch(() => {}));
}
