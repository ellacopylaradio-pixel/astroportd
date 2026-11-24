'use strict';
// BRO : conversation en flux avec l'IA du membre (docs/design/IA.md) ; activité de la station repliée.
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
  // Copier, partager, supprimer (supprimé : BRO ne le relit plus).
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

  const stream = async (body, on) => {
    const r = await K.req('/api/v1/bro/stream', { method: 'POST', body });
    if (!r.ok || !r.body) { const j = await r.json().catch(() => ({})); throw new Error(j.error || r.statusText); }
    const rd = r.body.getReader(), dec = new TextDecoder();
    let buf = '';
    for (;;) {
      const { value, done } = await rd.read();
      if (done) break;
      buf += dec.decode(value, { stream: true });
      let i;
      while ((i = buf.indexOf('\n\n')) >= 0) {
        const block = buf.slice(0, i); buf = buf.slice(i + 2);
        const data = block.split('\n').filter(l => l.startsWith('data:')).map(l => l.slice(5).trim()).join('');
        if (data) { try { on(JSON.parse(data)); } catch { /* ping, ou ligne illisible : ignorée */ } }
      }
    }
  };

  let busy = false;
  const send = async (text, allowCloud, mine) => {
    busy = true; go.disabled = true;
    try { await answer(text, allowCloud, mine); } finally { busy = false; go.disabled = false; }
  };
  const answer = async (text, allowCloud, mine) => {
    const history = turns.filter(x => x.id !== mine).slice(-20).map(({ role, content }) => ({ role, content }));
    const p = h('p', { class: 'muted' }, t('BRO réfléchit…')), meta = h('small', { class: 'muted bro-where' });
    const live = h('li', { class: 'bro-bro bro-wait' }, p, meta);
    thread.append(live); scroll();
    let shown = '', r = null;
    try {
      await stream({ text, history, private: priv, allow_cloud: !!allowCloud }, ev => {
        if (ev.type === 'route') meta.replaceChildren(ev.where === 'cloud' ? '🌐 ' : '🏠 ', ev.via || '', ev.model ? ' · ' + ev.model : '');
        else if (ev.type === 'turn') { shown = ''; p.textContent = t('BRO réfléchit…'); p.className = 'muted'; }
        else if (ev.type === 'tool') { p.textContent = t('BRO cherche dans vos données…'); p.className = 'muted'; }
        else if (ev.type === 'delta') { shown += ev.text; p.textContent = shown; p.className = ''; scroll(); }
        else if (ev.type === 'done') r = ev;
        else if (ev.type === 'error') throw new Error(ev.error);
      });
    } catch (e) { live.remove(); return bubble('err', e.message); }
    live.remove();
    if (!r) return bubble('err', t('La réponse a été interrompue.'));
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
  // Une question à la fois : la suivante attend la fin de la réponse (elle doit entrer dans ce que BRO relit).
  const go = h('button', {}, t('Poser la question'));
  const form = h('form', { class: 'bro-say' }, input, go);
  form.onsubmit = ev => {
    ev.preventDefault();
    const text = input.value.trim(); if (!text || busy) return;
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
