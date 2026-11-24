'use strict';
// BRO : la conversation avec l'IA attribuée au membre (docs/design/IA.md, lot 3) et, repliée, l'activité de la station.
// Chaque réponse dit où elle a été calculée (🏠 chez vous, 🌐 sur Internet), avec quel modèle, et si elle a lu vos données
// (🔒 : la conversation reste alors sur votre station). Une conversation privée ne part sur Internet qu'avec votre accord,
// donné pour une fois. `#commande` (#solde, #aide…) passe toujours par la console de la station. Chaque message se copie, se
// partage (menu du téléphone, sinon copie) ou se supprime : supprimé, il sort aussi de ce que BRO relit de la conversation.
export default async function bro(root, K) {
  const { h, t, api, feed, listeners } = K;
  const css = h('link', { rel: 'stylesheet', href: '/ui/m-bro.css' }); document.head.append(css); K.cleanup(() => css.remove());
  let priv = false, seq = 0;
  const turns = []; // ce que BRO relit : { id, role, content }, dans l'ordre
  const thread = h('ol', { class: 'bro-thread', 'aria-live': 'polite', 'aria-label': t('Conversation avec BRO') });
  const empty = h('p', { class: 'muted' }, t('Demandez par exemple : « Retrouve les photos de Léa à Toulouse », « Résume ma dernière facture », « Quel est mon solde ? »'));
  const lock = h('p', { class: 'bro-lock', hidden: true }, '🔒 ', t('Cette conversation a lu vos données : elle ne part sur Internet qu\'avec votre accord.'));
  const scroll = () => thread.lastElementChild?.scrollIntoView({ block: 'nearest' });

  const where = r => r.where ? h('small', { class: 'muted bro-where' },
    r.where === 'cloud' ? '🌐 ' : '🏠 ', r.via || '', r.model ? ' · ' + r.model : '', r.tools?.length ? ' · ' + t('a cherché dans vos données') : '',
    r.where === 'cloud' && !r.tools_offered ? ' · ' + t('sans accès à vos fichiers') : '') : '';
  // Actions d'un message : copier, partager, supprimer (icônes nommées pour les lecteurs d'écran, 48 px au pouce).
  const act = (icon, label, run) => h('button', { type: 'button', class: 'ghost', title: label, 'aria-label': label, onclick: run }, icon);
  const copy = text => navigator.clipboard?.writeText(text).then(() => K.toast(t('Message copié'), 1), e => K.toast(e.message));
  const acts = (li, text, id) => h('div', { class: 'bro-acts' },
    act('📋', t('Copier ce message'), () => copy(text)),
    act('↗', t('Partager ce message'), () => (navigator.share ? navigator.share({ text }).catch(() => {}) : copy(text))),
    act('🗑', t('Supprimer ce message'), () => { li.remove(); const i = turns.findIndex(x => x.id === id); if (i >= 0) turns.splice(i, 1); if (!thread.children.length) empty.hidden = false; }));
  // `role` : le message entre dans ce que BRO relit (sinon c'est un avis de la station : erreur, refus).
  const bubble = (who, text, extra, role) => {
    const id = role ? ++seq : 0, li = h('li', { class: 'bro-' + who }, h('p', {}, text), extra || '');
    if (role) { turns.push({ id, role, content: text }); li.append(acts(li, text, id)); }
    thread.append(li); empty.hidden = true; scroll();
    return id;
  };

  const send = async (text, allowCloud, mine) => {
    const history = turns.filter(x => x.id !== mine).slice(-20).map(({ role, content }) => ({ role, content }));
    const pending = h('li', { class: 'bro-bro bro-wait' }, h('p', { class: 'muted' }, t('BRO réfléchit…')));
    thread.append(pending); scroll();
    let r;
    try { r = await api('/api/v1/bro/say', { method: 'POST', body: { text, history, private: priv, allow_cloud: !!allowCloud } }); }
    catch (e) { pending.remove(); return bubble('err', e.message); }
    pending.remove();
    if (r.refused === 'private' && r.cloud) {
      // Données privées, pas d'IA chez soi : on demande l'accord, pour cette fois seulement.
      const ok = h('button', { type: 'button' }, t('Envoyer à {0} pour cette fois', r.cloud));
      ok.onclick = () => { ok.closest('li').remove(); send(text, true, mine); };
      return bubble('bro', r.reply, h('p', {}, ok));
    }
    if (r.private) { priv = true; lock.hidden = false; }
    if (r.refused) return bubble('err', r.reply);
    bubble('bro', r.reply, where(r), 'assistant');
  };

  const input = h('textarea', { rows: 2, placeholder: t('Écrire à BRO… (#solde, #aide pour les commandes)'), 'aria-label': t('Message pour BRO'), maxLength: 2000 });
  const form = h('form', { class: 'bro-say' }, input, h('button', {}, t('Poser la question')));
  form.onsubmit = ev => {
    ev.preventDefault();
    const text = input.value.trim(); if (!text) return;
    input.value = ''; send(text, false, bubble('me', text, '', 'user'));
  };
  input.onkeydown = ev => { if (ev.key === 'Enter' && !ev.shiftKey) { ev.preventDefault(); form.requestSubmit(); } };
  const reset = h('button', { type: 'button', class: 'ghost' }, t('Nouvelle conversation'));
  reset.onclick = () => { turns.length = 0; priv = false; lock.hidden = true; thread.replaceChildren(); empty.hidden = false; input.focus(); };

  // Activité de la station, repliée : le fil d'avant.
  const log = h('ol', { id: 'log', 'aria-label': t('Activité de la station'), 'data-empty': t('Rien pour l\'instant.') });
  const add = ev => log.append(h('li', {}, h('span', { class: 'k' }, ev.kind), ev.message));
  feed.forEach(add); listeners.add(add); K.cleanup(() => listeners.delete(add));

  root.replaceChildren(h('section', { class: 'card wide bro-card' },
    h('div', { class: 'bro-head' }, h('h2', {}, 'BRO'), reset), empty, thread, lock, form,
    h('p', {}, h('a', { href: '#/devices?card=ai' }, t('Quelle IA me répond ?'))),
    h('details', { class: 'fold' }, h('summary', {}, t('Activité de la station')), log)));
  input.focus();
}
