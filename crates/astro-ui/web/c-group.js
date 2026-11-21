'use strict';
// Participants d'un groupe : la liste, « ➕ Ajouter quelqu'un » (le même choix que « Nouveau message »), « Retirer » pour chacun,
// « Renommer » et « Quitter le groupe ». Chaque changement est envoyé à tous les participants, anciens et nouveaux.
export default function members(K, host, g, id, done) {
  const { h, t, api } = K;
  host.querySelector('.ch-pick')?.remove();
  const change = async (op, ok) => {
    try { await api('/api/v1/chat/group/update', { method: 'POST', body: { id, ...op } }); K.toast(ok, 1); panel.remove(); done(); } catch (e) { K.toast(e.message); }
  };
  const name = h('input', { value: g.name || '', maxlength: 80, placeholder: t('Nom du groupe (facultatif)'), 'aria-label': t('Nom du groupe') });
  const panel = h('div', { class: 'ch-pick', role: 'dialog', 'aria-label': t('Participants'), onkeydown: ev => { if (ev.key === 'Escape') panel.remove(); } },
    h('div', { class: 'ab-search ch-top' }, h('button', { type: 'button', class: 'ghost', 'aria-label': t('Fermer'), onclick: () => panel.remove() }, '←'), h('strong', {}, t('Participants ({0})', g.members.length + 1))),
    h('form', { class: 'ch-gfoot', onsubmit: ev => { ev.preventDefault(); change({ rename: name.value.trim() }, t('Groupe renommé')); } }, name, h('button', {}, t('Renommer'))),
    h('div', { class: 'ab-scroll' }, h('ul', { class: 'ab-list' },
      h('li', {}, h('button', { type: 'button', class: 'ab-item', onclick: () => K.mod('c-pick.js').then(m => m.default(K, host, hex => change({ add: hex }, t('Participant ajouté')))) }, h('span', { class: 'ab-av' }, '➕'), h('span', { class: 'ab-name' }, h('strong', {}, t('Ajouter quelqu\'un'))))),
      h('li', {}, h('span', { class: 'ab-item' }, h('span', { class: 'ab-av' }, '★'), h('span', { class: 'ab-name' }, h('strong', {}, t('Vous'))))),
      ...g.members.map(m => h('li', {}, h('span', { class: 'ab-item' }, h('span', { class: 'ab-av' }, (m.label[0] || '?').toUpperCase()), h('span', { class: 'ab-name' }, h('strong', {}, m.label)),
        h('button', { type: 'button', class: 'ghost', onclick: () => { if (confirm(t('Retirer {0} du groupe ?', m.label))) change({ remove: m.hex }, t('Participant retiré')); } }, t('Retirer'))))))),
    h('p', { class: 'ch-gfoot' }, h('button', { type: 'button', class: 'ghost', onclick: () => { if (confirm(t('Quitter ce groupe ? Vous gardez l\'historique mais ne recevrez plus ses messages.'))) change({ leave: true }, t('Vous avez quitté le groupe')); } }, '🚪 ' + t('Quitter le groupe'))));
  host.append(panel);
}
