'use strict';
// Astroport est en bêta : le dire simplement, et donner à chacun le moyen d'y participer. Le retour part par la station
// (POST /api/v1/feedback) : ticket dans le dépôt de la coopérative si le Capitaine l'a configuré, sinon gardé sur la station.
const CODE = 'https://github.com/papiche/astroportd';

// Feuille du module, chargée une fois (la carte de l'accueil et le formulaire, qui peut s'ouvrir depuis n'importe quel écran).
const style = K => { if (!document.querySelector('link[href="/ui/m-beta.css"]')) document.head.append(K.h('link', { rel: 'stylesheet', href: '/ui/m-beta.css' })); };

// Formulaire de retour (souci ou idée) : un titre, une description, et, si on le veut, sa clé publique pour être recontacté.
export function ask(K, { category = 'bug', title = '' } = {}) {
  const { h, t } = K;
  style(K);
  const name = h('input', { required: true, minLength: 3, maxLength: 120, value: title, 'aria-label': t('Titre'), placeholder: category === 'bug' ? t('Ce qui ne va pas, en quelques mots') : t('Votre idée, en quelques mots') });
  const text = h('textarea', { required: true, minLength: 3, rows: 6, maxLength: 8000, 'aria-label': t('Description'), placeholder: category === 'bug' ? t('Ce que vous faisiez, ce que vous attendiez, ce qui s’est passé.') : t('À quoi cela servirait, pour qui, comment vous l’imaginez.') });
  const me = h('input', { type: 'checkbox', id: 'bt-me' });
  const send = h('button', { value: 'ok' }, t('Envoyer'));
  const d = h('dialog', { class: 'bt-dialog', 'aria-labelledby': 'bt-h' }, h('form', { method: 'dialog' },
    h('h2', { id: 'bt-h' }, category === 'bug' ? '🐞 ' + t('Signaler un souci') : '💡 ' + t('Proposer une idée')),
    h('label', {}, t('Titre')), name, h('label', {}, t('Description')), text,
    h('p', {}, h('label', { for: 'bt-me' }, me, ' ', t('Joindre ma clé publique, pour qu’on puisse me répondre'))),
    h('p', { class: 'muted' }, t('Rien d’autre n’est envoyé : ni vos fichiers, ni votre adresse.')),
    h('menu', {}, h('button', { class: 'ghost', value: 'no', formNoValidate: true }, t('Annuler')), send)));
  d.onsubmit = async ev => {
    if (ev.submitter?.value !== 'ok') return;
    ev.preventDefault(); send.disabled = true;
    try {
      const r = await K.api('/api/v1/feedback', { method: 'POST', body: { title: name.value, description: text.value, source: 'astroportd', category: category === 'bug' ? 'bug' : 'feedback', platform: (navigator.userAgentData?.platform || navigator.platform || '').slice(0, 40), share_identity: me.checked } });
      d.close(); K.toast(r.issue_url ? t('Merci ! Votre retour est publié : {0}', r.issue_url) : t('Merci ! Votre retour est bien arrivé.'), 1);
    } catch (e) {
      send.disabled = false;
      K.toast(/non activés/.test(e.message) ? t('Les retours ne sont pas activés sur cette station : parlez-en à votre Capitaine, ou écrivez sur {0}', CODE + '/issues') : e.message);
    }
  };
  d.onclose = () => d.remove();
  document.body.append(d); d.showModal(); name.focus();
}

// Carte « en bêta » : ce que c'est, et deux gestes pour participer.
export function card(K) {
  const { h, t } = K;
  style(K);
  return [h('h2', {}, '🧪 ', t('Astroport est en bêta')),
    h('p', {}, t('Il s’améliore chaque semaine, avec ceux qui l’utilisent. Un souci, une idée ? Chacun peut participer à rendre Astroport meilleur.')),
    h('div', { class: 'bt-acts' },
      h('button', { type: 'button', onclick: () => ask(K, { category: 'bug' }) }, '🐞 ', t('Signaler un souci')),
      h('button', { type: 'button', class: 'ghost', onclick: () => ask(K, { category: 'idea' }) }, '💡 ', t('Proposer une idée')),
      h('a', { class: 'btn ghost', href: CODE, target: '_blank', rel: 'noopener' }, t('Le code est libre (AGPL)'), ' ↗'))];
}
