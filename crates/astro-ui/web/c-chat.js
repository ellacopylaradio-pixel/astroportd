'use strict';
// Chat privé : conversations 1 à 1, chiffrées de bout en bout (NIP-17). Liste à gauche (recherche, non lus, la station épinglée en
// tête), fil à droite. Les fichiers du chat (`c-*`) ont leur propre budget et leur dictionnaire (`c-lang-<lg>.js`).
//  - message : appui long, clic droit ou « ⋯ » → Répondre, Copier, Supprimer pour moi ; ✓ remis à sa station, ✓✓ lu ;
//  - pièces jointes et messages vocaux (c-media.js) : rangés dans /Chat du uCloud, partagés avec l'ami, affichés dans le fil ;
//  - « ✏️ Nouveau message » (c-pick.js) : proches et fiches du carnet qui ont une clé Nostr, avec ce qui les rend joignables ;
//  - la station écrit au membre (virements, alertes) : un fil à part, en cartes, sans réponse possible ;
//  - groupes (NIP-17, plusieurs destinataires) : l'auteur de chaque message est nommé ; vocaux « du tac au tac » (c-voice.js) ;
//  - aperçu riche des liens (rendu par la station), notifications (c-notify.js), balayage tactile (c-swipe.js).

export default async function chat(root, K, query) {
  const { h, t, api } = K;
  for (const f of ['m-book.css', 'c-chat.css']) { const l = h('link', { rel: 'stylesheet', href: '/ui/' + f }); document.head.append(l); K.cleanup(() => l.remove()); }
  const [media, voice, gest] = await Promise.all(['c-media.js', 'c-voice.js', 'c-swipe.js'].map(f => K.mod(f)));
  const s = { peer: null, label: '', station: false, group: null, convs: [], msgs: [], more: false, reply: null, voice: null, stranger: false };
  const list = h('ul', { class: 'ab-list', role: 'listbox', 'aria-label': t('Conversations') }), found = h('ul', { class: 'ab-list ch-found', hidden: true });
  const thread = h('div', { class: 'ch-thread', role: 'log', 'aria-live': 'polite' });
  const detail = h('section', { class: 'ab-detail ch-detail' }), main = h('div', { class: 'ab' });
  const q = h('input', { type: 'search', placeholder: t('Chercher'), 'aria-label': t('Chercher dans les conversations'), autocomplete: 'off', maxlength: 100 });
  const side = h('div', { class: 'ab-side' });
  const compose = h('button', { type: 'button', class: 'ch-new', title: t('Nouveau message'), 'aria-label': t('Nouveau message'), onclick: () => K.mod('c-pick.js').then(m => m.default(K, side, openConv), e => K.toast(e.message)) }, '✏️');
  const top = h('div', { class: 'ab-search ch-top' }, q, compose);
  side.append(top, h('div', { class: 'ab-scroll' }, list, found));
  K.mod('c-notify.js').then(m => m.default(K)).then(b => top.append(b), () => {});
  main.append(side, detail);
  root.replaceChildren(main);

  const initial = n => (n || '?')[0].toUpperCase();
  // Présence visio des amis mutuels qui l'ont choisie : pastille sur l'avatar, statut en clair dans l'en-tête.
  let pres = {}, reachable = true;
  const PRES = { online: 'en ligne', visio: 'en appel' };
  const dot = hex => (PRES[pres[hex]] ? h('span', { class: 'ch-dot ' + pres[hex], role: 'img', 'aria-label': t(PRES[pres[hex]]) }) : null);
  const presText = h('small', { class: 'ch-pres' });
  const paintPres = () => { presText.textContent = s.peer && PRES[pres[s.peer]] ? '● ' + t(PRES[pres[s.peer]]) : ''; presText.className = 'ch-pres ' + (pres[s.peer] || ''); };
  const loadPres = () => api('/api/v1/presence').then(r => { pres = r.friends; reachable = r.reachable; paintPres(); loadList(); }, () => {});
  const day = at => new Date(at * 1000).toLocaleDateString(K.lang, { weekday: 'long', day: 'numeric', month: 'long' });
  const hour = at => new Date(at * 1000).toLocaleTimeString(K.lang, { hour: '2-digit', minute: '2-digit' });
  const when = at => (day(at) === day(Date.now() / 1000) ? hour(at) : new Date(at * 1000).toLocaleDateString(K.lang, { day: 'numeric', month: 'short' }));

  // La station écrit au membre (virements reçus, alertes) : elle se nomme « Votre station », en tête de liste, et on ne lui répond pas.
  const name = c => (c.station ? t('Votre station') : c.group ? c.gname || t('Groupe de {0} personnes', c.size) : c.label);
  // Groupe : le nom de l'auteur d'un message.
  const who = hex => s.group?.members.find(x => x.hex === hex)?.label || (hex || '').slice(0, 8);
  async function loadList() {
    try { const r = await api('/api/v1/chat'); s.convs = r.conversations.sort((a, b) => b.station - a.station); } catch (e) { list.replaceChildren(h('li', { class: 'err', role: 'alert' }, e.message)); return; }
    const k = q.value.trim().toLowerCase(), shown = s.convs.filter(c => !k || (name(c) + ' ' + c.last).toLowerCase().includes(k));
    list.replaceChildren(...shown.map(c => {
      const li = h('li', { role: 'option', 'aria-selected': String(c.peer === s.peer), class: (c.peer === s.peer ? 'on' : '') + (c.station ? ' ch-pin' : '') },
        h('button', { type: 'button', class: 'ab-item', onclick: () => openConv(c.peer, name(c), c.station, false, null, c.group) },
          h('span', { class: 'ab-av' }, c.station ? '📡' : c.group ? '👥' : initial(c.label), dot(c.peer)),
          h('span', { class: 'ab-name' }, h('strong', {}, name(c)), h('small', { class: 'muted' }, (c.mine ? t('Vous : ') : '') + c.last)),
          h('span', { class: 'ch-meta' }, h('small', { class: 'muted' }, when(c.at)), c.unread ? h('span', { class: 'ch-badge', 'aria-label': t('{0} non lu(s)', c.unread) }, String(c.unread)) : null)));
      // Balayage : vers la droite, marquer lu ; vers la gauche, effacer (après confirmation).
      gest.swipe(li, { right: () => api('/api/v1/chat/read', { method: 'POST', body: { peer: c.peer } }).then(loadList, () => {}), left: () => clearPeer(c.peer, name(c)) });
      return li;
    }));
    if (!shown.length) list.replaceChildren(h('li', { class: 'muted ab-empty' }, k ? t('Aucune conversation ne correspond.') : t('Aucune conversation. Touchez ✏️ pour écrire à un proche.')));
    K.chatUnread?.(s.convs.reduce((n, c) => n + c.unread, 0));
  }
  // Recherche : les conversations se filtrent en direct ; dès deux lettres, les messages qui contiennent le texte s'ajoutent.
  let timer = 0;
  q.oninput = () => {
    loadList(); clearTimeout(timer);
    const v = q.value.trim();
    if (v.length < 2) { found.hidden = true; return; }
    timer = setTimeout(async () => {
      let r; try { r = await api('/api/v1/chat/search?' + new URLSearchParams({ q: v })); } catch { return; }
      const label = p => { const c = s.convs.find(x => x.peer === p || x.channels?.some(ch => ch.peer === p)); return c ? name(c) : p.slice(0, 8); };
      found.hidden = !r.results.length;
      found.replaceChildren(h('li', { class: 'muted ch-sec' }, t('Messages')), ...r.results.map(m => h('li', {}, h('button', { type: 'button', class: 'ab-item', onclick: () => openConv(m.peer, label(m.peer), false, false, m.id, s.convs.find(x => x.peer === m.peer)?.group) },
        h('span', { class: 'ab-name' }, h('strong', {}, label(m.peer)), h('small', { class: 'muted' }, (m.mine ? t('Vous : ') : '') + m.text)), h('small', { class: 'muted ch-meta' }, when(m.at))))));
    }, 300);
  };

  // Visio : l'appel part de l'en-tête de la conversation ; un appel manqué (texte « 📹 … » rangé par la station) propose de rappeler.
  const callBack = () => K.mod('m-call.js').then(m => m.call(K, s.peer, s.label), e => K.toast(e.message));
  const linkify = text => text.split(/(https?:\/\/[^\s<>"]+)/).map((p, i) => (i % 2 ? h('a', { href: p, target: '_blank', rel: 'noopener noreferrer' }, p) : p));
  // Sans texte, un envoi de fichiers se légende « 📎 nom » ou « 🎤 Message vocal » (ce que voit un autre client Nostr) : la bulle ne
  // répète pas la légende sous la pièce jointe.
  const caption = f => (/^audio\//.test(f.mime) ? '🎤 ' + t('Message vocal') : '📎 ' + f.name.replace(/^\d{8}-\d{6}-/, ''));
  const onlyFiles = m => m.files?.length && m.text.split('\n').length === m.files.length && m.text.split('\n').every(l => /^(📎|🎤) /u.test(l));

  // Actions sur un message : un seul menu ouvert à la fois, fermé par Échap ou un clic ailleurs.
  let menuEl = null;
  const closeMenu = () => { menuEl?.remove(); menuEl = null; };
  document.addEventListener('click', closeMenu); K.cleanup(() => document.removeEventListener('click', closeMenu));
  function menu(m, el) {
    closeMenu();
    const item = (label, run) => h('button', { type: 'button', role: 'menuitem', onclick: ev => { ev.stopPropagation(); closeMenu(); run(); } }, label);
    menuEl = h('div', { class: 'ch-menu', role: 'menu', onkeydown: ev => { if (ev.key === 'Escape') { closeMenu(); el.focus(); } } },
      s.station ? null : item('↩️ ' + t('Citer et répondre'), () => setReply(m)),
      item('📋 ' + t('Copier'), () => navigator.clipboard.writeText(m.text).then(() => K.toast(t('Copié'), 1), () => K.toast(t('Copie impossible')))),
      item('🗑️ ' + t('Supprimer pour moi'), () => remove(m)));
    el.append(menuEl); menuEl.querySelector('button').focus();
  }
  async function remove(m) {
    if (!confirm(t('Supprimer ce message ? Il disparaît pour vous seulement.'))) return;
    try { await api('/api/v1/chat/delete', { method: 'POST', body: { id: m.id } }); await refresh(false); loadList(); } catch (e) { K.toast(e.message); }
  }
  function bubble(m, prev) {
    const out = [], missed = m.text.startsWith('📹');
    if (!prev || day(prev.at) !== day(m.at)) out.push(h('div', { class: 'ch-day' }, day(m.at)));
    const tick = m.mine && !s.station ? (m.state === 'failed' ? h('span', { class: 'ch-fail' }, ' · ' + t('non remis'))
      : h('span', { class: 'ch-tick' + (m.seen ? ' seen' : ''), title: m.seen ? t('Lu') : t('Remis à sa station') }, m.seen ? ' ✓✓' : ' ✓')) : null;
    const el = h('div', { id: 'm-' + m.id, tabindex: 0, class: (s.station ? 'ch-card' : 'ch-msg ' + (m.mine ? 'mine' : 'theirs')) + (m.state === 'failed' ? ' failed' : ''),
      oncontextmenu: ev => { ev.preventDefault(); ev.stopPropagation(); menu(m, el); },
      onkeydown: ev => { if (ev.key === 'Enter' && ev.target === el) { ev.preventDefault(); menu(m, el); } } },
      s.group && !m.mine ? h('strong', { class: 'ch-author' }, who(m.author)) : null,
      m.quote ? h('button', { type: 'button', class: 'ch-quote', onclick: () => { jump(m.reply); if (m.reply_at != null) voice.play(m.reply, m.reply_at); } },
        h('strong', {}, m.quote.mine ? t('Vous') : s.group ? '' : s.label, m.reply_at != null ? ` · 🎤 ${voice.mmss(m.reply_at)}` : ''), h('span', {}, m.quote.text)) : null,
      ...(m.files || []).map(f => media.file(K, f, { audio: a => voice.track(a, m), extra: a => (m.mine || s.station ? null : h('button', { type: 'button', class: 'ghost ch-vr', title: t('Répondre à cet instant'), 'aria-label': t('Répondre à cet instant'), onclick: ev => { ev.stopPropagation(); a.pause(); voiceReply({ m, at: voice.mark(a) }); } }, '↩🎤')) })),
      m.files?.some(f => /^audio\//.test(f.mime)) ? voice.cuts(K, m, s.msgs, r => (r.mine ? t('Vous') : s.group ? who(r.author) : s.label)) : null,
      onlyFiles(m) ? null : h('p', {}, ...(missed ? [t(m.text)] : linkify(m.text))),
      !s.station && URL_RE.test(m.text) ? media.preview(K, m.text.match(URL_RE)[0]) : null,
      missed ? h('p', {}, h('button', { type: 'button', class: 'ghost', onclick: callBack }, t('Rappeler'))) : null,
      h('small', { class: 'muted' }, hour(m.at), tick),
      h('button', { type: 'button', class: 'ch-more', 'aria-label': t('Actions sur le message'), 'aria-haspopup': 'menu', onclick: ev => { ev.stopPropagation(); menu(m, el); } }, '⋯'));
    // Appui long (écran tactile) : le menu, comme un clic droit.
    let press = 0;
    el.onpointerdown = ev => { if (ev.pointerType === 'touch') press = setTimeout(() => menu(m, el), 500); };
    el.onpointerup = el.onpointermove = el.onpointercancel = () => clearTimeout(press);
    if (!s.station) gest.swipe(el, { right: () => setReply(m) });
    out.push(el);
    return out;
  }
  const URL_RE = /https?:\/\/[^\s<>"]+/;
  function jump(id) {
    const el = document.getElementById('m-' + id);
    if (!el) return K.toast(t('Message plus ancien : remontez le fil'));
    el.scrollIntoView({ block: 'center', behavior: 'smooth' }); el.classList.add('ch-flash'); setTimeout(() => el.classList.remove('ch-flash'), 1500);
  }
  function paint(stick) {
    const old = thread.scrollHeight - thread.scrollTop;
    const more = s.more ? [h('button', { type: 'button', class: 'chip', onclick: older }, t('Messages plus anciens'))] : [];
    const warn = s.stranger ? [h('p', { class: 'muted ch-warn' }, t('Vous ne vous suivez pas mutuellement : votre message peut ne pas arriver, et sa réponse ne s\'affichera que si vous suivez cette personne.'))] : [];
    const keep = voice.snapshot(); // un vocal en cours d'écoute continue malgré le rafraîchissement du fil
    thread.replaceChildren(...warn, ...more, ...s.msgs.flatMap((m, i) => bubble(m, s.msgs[i - 1])));
    thread.scrollTop = stick ? thread.scrollHeight : thread.scrollHeight - old;
    if (keep) voice.play(keep.id, keep.at);
  }
  async function older() {
    const r = await api('/api/v1/chat/messages?' + new URLSearchParams({ peer: s.peer, before: s.msgs[0].at }));
    s.msgs = [...r.messages, ...s.msgs]; s.more = r.more; paint(false);
  }
  async function refresh(stick) {
    if (!s.peer) return;
    const peer = s.peer, r = await api('/api/v1/chat/messages?' + new URLSearchParams({ peer }));
    if (peer !== s.peer) return;
    s.msgs = r.messages; s.more = r.more; s.group = r.group || null; paint(stick);
    if (document.visibilityState === 'visible' && s.msgs.some(m => !m.mine)) api('/api/v1/chat/read', { method: 'POST', body: { peer } }).then(loadList).catch(() => {});
  }
  const onShow = () => { if (document.visibilityState === 'visible') refresh(false); };
  document.addEventListener('visibilitychange', onShow); K.cleanup(() => document.removeEventListener('visibilitychange', onShow));

  // Brouillon par conversation, gardé le temps de l'onglet (rechargement, coupure réseau) : jamais au-delà, c'est un message privé.
  const draft = { get: p => { try { return sessionStorage.getItem('draft:' + p) || ''; } catch { return ''; } }, set: (p, v) => { try { v ? sessionStorage.setItem('draft:' + p, v) : sessionStorage.removeItem('draft:' + p); } catch { /* stockage indisponible */ } } };
  const box = h('textarea', { rows: 1, maxlength: 4000, placeholder: t('Écrire un message…'), 'aria-label': t('Message'), oninput: () => { s.peer && draft.set(s.peer, box.value); grow(); } });
  const grow = () => { box.style.height = 'auto'; box.style.height = Math.min(box.scrollHeight, 160) + 'px'; };
  const replyBar = h('div', { class: 'ch-replybar', hidden: true });
  function setReply(m) {
    s.reply = m; replyBar.hidden = !m;
    if (!m || s.voice?.m !== m) s.voice = null;
    const at = s.voice ? ` · 🎤 ${voice.mmss(s.voice.at)}` : '';
    if (m) replyBar.replaceChildren(h('span', {}, h('strong', {}, '↩️ ', m.mine ? t('Vous') : s.group ? who(m.author) : s.label, at), ' ', m.text.slice(0, 120)), h('button', { type: 'button', class: 'ghost', 'aria-label': t('Annuler la réponse'), onclick: () => setReply(null) }, '✕'));
    if (m && !s.voice) box.focus();
  }
  // Réponse vocale « du tac au tac » : le vocal écouté est en pause à `at` ; on enregistre la réponse, puis la lecture reprend.
  function voiceReply(v) {
    s.voice = null; setReply(v.m); s.voice = v; setReply(v.m);
    media.record(K, mic, blob => sendFiles([blob]));
  }
  async function post(body) {
    const v = s.voice && s.reply === s.voice.m ? s.voice : null;
    await api('/api/v1/chat/send', { method: 'POST', body: { peer: s.peer, reply: s.reply?.id, reply_at: v?.at, ...body } });
    setReply(null); await refresh(true); await loadList();
    if (v) voice.play(v.m.id, v.at);
  }
  async function send() {
    const text = box.value.trim(); if (!text || !s.peer) return;
    box.disabled = true;
    try { await post({ text }); box.value = ''; draft.set(s.peer, ''); grow(); }
    catch (e) { K.toast(e.message); } finally { box.disabled = false; box.focus(); }
  }
  // Fichiers et messages vocaux : rangés dans /Chat, partagés avec l'ami, puis annoncés dans le fil (avec le texte en cours, s'il y en a).
  async function sendFiles(files) {
    if (!files.length || !s.peer || s.station) return;
    const peer = s.peer;
    K.toast(t('Envoi de {0} fichier(s)…', files.length), 1);
    try {
      const out = await media.upload(K, files, peer);
      if (peer !== s.peer) return;
      await post({ text: box.value.trim() || out.map(caption).join('\n'), files: out }); box.value = ''; draft.set(peer, ''); grow();
    } catch (e) { K.toast(e.message); }
  }
  box.onkeydown = ev => { if (ev.key === 'Enter' && !ev.shiftKey && !ev.isComposing) { ev.preventDefault(); send(); } if (ev.key === 'Escape' && s.reply) setReply(null); };
  box.onpaste = ev => { const f = [...(ev.clipboardData?.files || [])]; if (f.length) { ev.preventDefault(); sendFiles(f); } };
  const pickFile = h('input', { type: 'file', multiple: true, hidden: true, onchange: () => { sendFiles([...pickFile.files]); pickFile.value = ''; } });
  const tool = (icon, label, run) => h('button', { type: 'button', class: 'ghost ch-tool', title: label, 'aria-label': label, onclick: run }, icon);
  // 🎤 pendant l'écoute d'un vocal reçu : la lecture s'arrête là et la réponse le cite à cet instant.
  const mic = tool('🎤', t('Message vocal'), () => { const v = !mic.classList.contains('ch-rec') && voice.pauseForReply(); if (v && !v.m.mine) voiceReply(v); else media.record(K, mic, blob => sendFiles([blob])); });
  const form = h('form', { class: 'ch-form', onsubmit: ev => { ev.preventDefault(); send(); } },
    tool('📎', t('Joindre des fichiers'), () => pickFile.click()), mic, box, h('button', { type: 'submit', 'aria-label': t('Envoyer') }, '➤'), pickFile);
  const composer = h('div', { class: 'ch-compose' }, replyBar, form);
  detail.ondragover = ev => { if (s.peer && !s.station) { ev.preventDefault(); detail.classList.add('ch-drop'); } };
  detail.ondragleave = () => detail.classList.remove('ch-drop');
  detail.ondrop = ev => { ev.preventDefault(); detail.classList.remove('ch-drop'); sendFiles([...(ev.dataTransfer?.files || [])]); };

  // Effacer : pour soi seul (l'autre personne garde la sienne) ; les messages effacés ne reviennent pas.
  const close = () => { s.peer = null; main.classList.remove('open'); hint(); loadList(); };
  async function clearPeer(peer, label) {
    if (!confirm(t('Effacer cette conversation ? Les messages disparaissent pour vous seulement : {0} garde les siens.', label))) return;
    try { await api('/api/v1/chat/clear', { method: 'POST', body: { peer } }); K.toast(t('Conversation effacée'), 1); if (peer === s.peer) close(); else loadList(); } catch (e) { K.toast(e.message); }
  }
  const clearConv = () => clearPeer(s.peer, s.label);
  // Ẑen : vers tout MULTIPASS de la constellation (le serveur vérifie l'origine de son portefeuille) ; annoncé dans la conversation.
  const zen = () => K.mod('m-credit.js').then(m => m.default(K, { mode: 'member', to: s.peer, label: s.label, done: n => post({ text: t('💸 Je vous ai envoyé {0} Ẑen.', n) }).catch(e => K.toast(e.message)) }), e => K.toast(e.message));
  async function openConv(peer, label, station, stranger, at, group) {
    s.peer = peer; s.label = label || peer.slice(0, 8); s.station = !!station; s.stranger = !!stranger; s.group = null; s.msgs = []; s.more = false; setReply(null);
    main.classList.add('open');
    const sub = h('small', { class: 'muted' }, s.station ? t('Notifications de la station') : '');
    // Actions moins fréquentes (payer, effacer) : nommées en toutes lettres dans le menu « ⋮ », pas en icônes dans l'en-tête.
    const kebab = h('button', { type: 'button', class: 'ghost ch-kebab', title: t('Plus d\'actions'), 'aria-label': t('Plus d\'actions'), 'aria-haspopup': 'menu', onclick: ev => {
      ev.stopPropagation(); closeMenu();
      const item = (label, run) => h('button', { type: 'button', role: 'menuitem', onclick: e => { e.stopPropagation(); closeMenu(); run(); } }, label);
      menuEl = h('div', { class: 'ch-menu', role: 'menu', onkeydown: e => { if (e.key === 'Escape') { closeMenu(); kebab.focus(); } } },
        s.station || group ? null : item('Ẑ ' + t('Envoyer des Ẑen à {0}', s.label), zen),
        item('🗑️ ' + t('Effacer la conversation'), clearConv));
      head.append(menuEl); menuEl.querySelector('button').focus();
    } }, '⋮');
    const head = h('div', { class: 'ch-head' }, h('button', { type: 'button', class: 'ab-back', onclick: close }, '← ' + t('Retour')),
      h('span', { class: 'ab-av' }, s.station ? '📡' : group ? '👥' : initial(s.label)), h('span', { class: 'ab-name' }, h('strong', {}, s.label), s.station || group ? sub : presText),
      s.station ? null : h('small', { class: 'muted ch-e2e' }, '🔒 ' + t('Chiffré de bout en bout')),
      s.station || group ? null : h('button', { type: 'button', class: 'ch-callbtn', title: t('Appeler en visio'), 'aria-label': t('Appeler {0} en visio', s.label), onclick: callBack }, '📹'),
      kebab);
    const foot = s.station ? h('p', { class: 'muted ch-note' }, t('Votre station vous prévient ici (virements reçus, alertes). Elle ne lit pas les réponses.')) : composer;
    detail.replaceChildren(head, thread, foot);
    paintPres(); box.value = draft.get(peer); grow();
    await refresh(true); await loadList();
    if (s.group) sub.textContent = s.group.members.map(x => x.label).join(', ');
    if (at) jump(at); else if (!s.station && matchMedia('(pointer:fine)').matches) box.focus();
  }
  const hint = () => detail.replaceChildren(h('p', { class: 'muted ab-hint' }, t('Choisissez une conversation, ou touchez ✏️ pour écrire à un proche.')),
    ...(reachable ? [] : [h('p', { class: 'muted ab-hint' }, t('Vos amis ne voient pas quand vous êtes là pour un appel visio.'), ' ', h('a', { href: '#/account' }, t('Être joignable')))]));
  K.listen(ev => {
    if (ev.source === 'chat:msg') { loadList(); if (ev.message === s.peer) refresh(true); }
    if (ev.source === 'chat:seen' && ev.message === s.peer) refresh(false);
    if (ev.kind === 'presence' && ev.data?.hex) { pres[ev.data.hex] = ev.data.status; paintPres(); loadList(); }
  });
  // Retour par un balayage depuis le bord gauche du fil (téléphone).
  gest.swipe(detail, { right: () => { if (s.peer && matchMedia('(max-width:52rem)').matches) close(); }, edge: 32, threshold: 80 });
  await Promise.all([loadList(), loadPres()]);
  const want = new URLSearchParams(query || '').get('peer');
  if (want) { const c = s.convs.find(x => x.peer === want); openConv(want, c ? name(c) : null, c?.station, false, null, c?.group); } else hint();
}
