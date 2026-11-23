'use strict';
// Proches › Mes cercles (#/cercles) : ranger ses proches en cinq cercles, du plus proche au plus lointain (src/proximity.rs).
// Privé : chiffré pour le membre seul, jamais dans sa liste de contacts publique. Par défaut, un ami réciproque est « Ami »,
// une personne suivie sans retour « Contact » : on ne range que les exceptions, d'un geste (un menu par personne).
export const LEVELS = [[5, '🏠', 'Famille'], [4, '💛', 'Proche'], [3, '🤝', 'Ami'], [2, '👋', 'Connaissance'], [1, '📇', 'Contact']];
const SAYS = {
  5: 'Ceux sur qui vous comptez en toutes circonstances.', 4: 'Vos intimes.', 3: 'Vos amis : tout ami réciproque commence ici.',
  2: 'Des personnes que vous connaissez un peu.', 1: 'Vous les suivez, sans plus.',
};

/** Menu du cercle d'une personne ; `onPick(niveau)` à chaque choix. */
export function levelSelect(K, p, onPick) {
  const { h, t } = K;
  return h('select', { 'aria-label': t('Cercle de {0}', p.label), onchange: ev => onPick(Number(ev.target.value)) },
    LEVELS.map(([l, ico, name]) => h('option', { value: l, selected: l === p.level }, `${ico} ${t(name)}`)));
}

// Toile de confiance N² (astro_economy::wot) : seules les rencontres prouvées (code scanné face à face) y font entrer.
// Repliée en bas de page : c'est un horizon (le dividende), pas le geste du jour ; son état tient dans le titre.
function web(K, w) {
  const { h, t } = K, short = w.certs < w.needed;
  const state = w.root || w.member ? '✓' : t('{0} sur {1}', w.certs, w.needed);
  return h('details', { class: 'card wide fold', id: 'ci-wot' }, h('summary', {}, '🕸️ ', t('Toile de confiance'), ' ', h('small', { class: 'muted' }, t('rencontres prouvées : {0}', state))),
    w.root ? h('p', { class: 'ok' }, t('Capitaine : vous en êtes membre d\'office ; vos rencontres y font entrer les autres.'))
      : w.member ? h('p', { class: 'ok' }, h('strong', {}, '✓ ', t('Vous êtes dans la toile : {0} rencontres prouvées avec des membres.', w.certs)))
        : h('p', {}, h('strong', {}, t('Rencontres prouvées avec des membres : {0} sur {1}.', w.certs, w.needed)), ' ',
          short ? t('Montrez votre code « Se rencontrer » à des membres : chaque code scanné face à face compte.')
            : t('C\'est assez, mais dans un même coin de la toile : rencontrez des membres d\'autres groupes.')),
    w.certifiers.length ? h('p', { class: 'muted' }, t('Vous y tiennent : {0}.', w.certifiers.join(', '))) : '',
    h('p', { class: 'muted small' }, t('La toile compte {0} membres. Elle préparera un dividende universel entre nous ; il n\'est pas encore actif.', w.web.members)));
}

export default async function circles(root, K) {
  const { h, t, api } = K;
  const link = h('link', { rel: 'stylesheet', href: '/ui/m-pay.css' }); document.head.append(link); K.cleanup(() => link.remove());
  let w = null;
  const draw = v => {
    const set = (p, level) => api('/api/v1/me/circles', { method: 'POST', body: { hex: p.hex, level } })
      .then(n => { draw(n); K.toast(t('{0} : {1}', p.label, t(LEVELS.find(x => x[0] === level)[2])), 1); }, e => K.toast(e.message, 0));
    // Les cercles où il y a quelqu'un, en cartes ; les cercles vides, en une ligne (pas quatre cartes « Personne pour l'instant »).
    const empty = LEVELS.filter(([l]) => !v.people.some(p => p.level === l));
    const groups = LEVELS.filter(([l]) => v.people.some(p => p.level === l)).map(([l, ico, name]) => {
      const who = v.people.filter(p => p.level === l);
      return h('section', { class: 'card ci-ring' },
        h('h3', {}, `${ico} ${t(name)} `, h('small', { class: 'muted' }, String(who.length))),
        h('p', { class: 'muted' }, t(SAYS[l])),
        who.length ? h('ul', { class: 'files ci-list' }, who.map(p => h('li', {},
          h('span', { class: 'ci-who' }, h('strong', {}, p.label),
            p.met_verified ? h('small', { class: 'ok' }, ' ✓ ', t('rencontré en personne')) : null,
            p.friends ? null : h('small', { class: 'muted' }, ' ⏳ ', t('pas encore réciproque'))),
          levelSelect(K, p, lv => set(p, lv)))))
          : h('p', { class: 'muted' }, t('Personne pour l\'instant.')));
    });
    root.replaceChildren(
      h('p', { class: 'muted' }, '🔒 ', t('Rangez vos proches du plus proche au plus lointain. C\'est privé : personne, pas même eux, ne sait où vous les avez rangés.')),
      v.people.length ? h('div', { class: 'grid' }, groups) : h('p', { class: 'muted' }, t('Vous ne suivez encore personne.')),
      v.people.length && empty.length ? h('p', { class: 'muted ci-empty' }, t('Vides pour l\'instant : {0}. Le menu de chaque personne la range où vous voulez.', empty.map(([, ico, name]) => `${ico} ${t(name)}`).join(' · '))) : '',
      h('p', { class: 'ck-acts' }, h('a', { class: 'ck-btn', href: '#/rencontre' }, '🤝 ' + t('Se rencontrer'))),
      w ? web(K, w) : '');
  };
  try { const [v, x] = await Promise.all([api('/api/v1/me/circles'), api('/api/v1/me/wot').catch(() => null)]); w = x; draw(v); }
  catch (e) { root.replaceChildren(h('p', { class: 'err', role: 'alert' }, e.message)); }
}
