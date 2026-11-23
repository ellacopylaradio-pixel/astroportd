'use strict';
// Mon blog : écrire une note ou un article, le relire en direct, l'enregistrer en brouillon, le publier (ou le programmer), et
// retrouver ses articles. La station signe pour le membre (sa clé ne quitte jamais la station) : notes (kind 1), articles (30023),
// brouillons (30024), retraits (5). Aussi : répondre à une note ou commenter un article (#/write?reply=…|a=…&to=…) et recevoir
// ce que le téléphone partage (« Partager » → Astroport : #/write?shared=1, déposé par le service worker).
const A = '/api/v1/me/blog', LOCAL = 'astro-write-draft';
const slugify = s => s.normalize('NFD').replace(/[\u0300-\u036f]/g, '').toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '').slice(0, 80) || 'article';
const keep = { get: () => { try { return JSON.parse(localStorage.getItem(LOCAL) || 'null'); } catch { return null; } }, set: v => { try { localStorage.setItem(LOCAL, JSON.stringify(v)); } catch { /* navigation privée */ } }, del: () => { try { localStorage.removeItem(LOCAL); } catch { /* idem */ } } };

// Adresse NIP-19 (nevent / naddr, éventuellement préfixée nostr: ou web+nostr:) → cible de la réponse : { reply, to } ou { a, to }.
const B32 = 'qpzry9x8gf2tvdw0s3jn54khce6mua7l';
function nip19(raw) {
  const s = decodeURIComponent(raw || '').toLowerCase().replace(/^(web\+)?(nostr|astro):/, ''), i = s.lastIndexOf('1');
  if (i < 1) return null;
  const hrp = s.slice(0, i), words = [...s.slice(i + 1, -6)].map(c => B32.indexOf(c));
  if (words.some(w => w < 0)) return null;
  const bytes = []; let acc = 0, bits = 0;
  for (const w of words) { acc = (acc << 5) | w; bits += 5; if (bits >= 8) { bits -= 8; bytes.push((acc >> bits) & 255); } }
  const tlv = {}, hex = b => b.map(x => x.toString(16).padStart(2, '0')).join('');
  for (let p = 0; p + 2 <= bytes.length;) { const t = bytes[p], l = bytes[p + 1], v = bytes.slice(p + 2, p + 2 + l); (tlv[t] ||= v); p += 2 + l; }
  if (!tlv[0] || !tlv[2]) return null;
  if (hrp === 'nevent') return { reply: hex(tlv[0]), to: hex(tlv[2]) };
  if (hrp === 'naddr') return { a: `${tlv[3] ? tlv[3].reduce((n, b) => n * 256 + b, 0) : 30023}:${hex(tlv[2])}:${new TextDecoder().decode(new Uint8Array(tlv[0]))}`, to: hex(tlv[2]) };
  return null;
}

export default async function write(root, K) {
  const { h, t } = K, q = new URLSearchParams(location.hash.split('?')[1] || '');
  const n19 = q.get('nostr') && nip19(q.get('nostr'));
  if (n19) Object.entries(n19).forEach(([k, v]) => q.set(k, v));
  const css = h('link', { rel: 'stylesheet', href: '/ui/m-write.css' }); document.head.append(css); K.cleanup(() => css.remove());
  root.replaceChildren(h('p', { class: 'muted' }, t('Chargement…')));
  let me;
  try { me = await K.api(A); } catch (e) { return root.replaceChildren(h('p', { class: 'err', role: 'alert' }, e.message)); }
  const post = (body, path = '/publish') => K.api(A + path, { method: 'POST', body });
  const publish = ev => post({ ...ev, tags: ev.tags || [] }).then(r => r.event);

  // Image : réduite ici si elle est très grande, envoyée dans Public/images ; rend son adresse absolue (lisible par tout client Nostr).
  const upload = async file => {
    let blob = file;
    if (file.size > 2.5e6 && /^image\/(jpeg|png|webp)$/.test(file.type)) {
      const bmp = await createImageBitmap(file), k = Math.min(1, 2400 / Math.max(bmp.width, bmp.height)), c = h('canvas', { width: Math.round(bmp.width * k), height: Math.round(bmp.height * k) });
      c.getContext('2d').drawImage(bmp, 0, 0, c.width, c.height); blob = await new Promise(ok => c.toBlob(ok, 'image/jpeg', .88));
    }
    const data = await new Promise(ok => { const r = new FileReader(); r.onload = () => ok(String(r.result).split(',')[1]); r.readAsDataURL(blob); });
    const r = await post({ name: file.name || 'image.jpg', data }, '/image');
    return location.origin + r.path;
  };
  const picker = (multiple, onFiles) => { const f = h('input', { type: 'file', accept: 'image/jpeg,image/png,image/webp', multiple, hidden: true, onchange: () => { const l = [...f.files]; f.value = ''; if (l.length) onFiles(l); } }); return f; };

  // ── Note (et réponse) ──
  const reply = q.get('reply'), coord = q.get('a'), to = q.get('to');
  function noteEditor(prefill = '', files = []) {
    const ta = h('textarea', { rows: 5, maxlength: 8000, placeholder: reply || coord ? t('Votre réponse…') : t('Quoi de neuf ?'), 'aria-label': t('Texte de la note'), value: prefill });
    const thumbs = h('div', { class: 'wr-thumbs' }), count = h('small', { class: 'muted' });
    const tick = () => { count.textContent = t('{0} caractères', ta.value.length); };
    ta.oninput = tick; tick();
    const addImages = async list => {
      for (const f of list) {
        const ph = h('span', { class: 'wr-thumb busy' }); thumbs.append(ph);
        try { const url = await upload(f); ph.replaceWith(h('img', { class: 'wr-thumb', src: url, alt: '' })); ta.value = (ta.value.trimEnd() + '\n' + url).trimStart(); tick(); } catch (e) { ph.remove(); K.toast(e.message); }
      }
    };
    const pick = picker(true, addImages);
    const go = h('button', { type: 'button', onclick: async () => {
      const text = ta.value.trim();
      if (!text) return ta.focus();
      go.disabled = true;
      // NIP-10 (réponse à une note : marqueur « root » en 4e place) ; NIP-23 (commentaire d'article : son adresse en `a`).
      const tags = (reply ? [['e', reply, '', 'root'], ['p', to || '']] : coord ? [['a', coord], ['p', to || '']] : []).filter(x => x[1]);
      try {
        await publish({ kind: 1, content: text, tags });
        K.toast(reply || coord ? t('Réponse publiée') : t('Note publiée'), 1); ta.value = ''; thumbs.replaceChildren(); tick();
        if (reply || coord) history.length > 1 ? history.back() : (location.hash = '#/write');
      } catch (e) { K.toast(e.message); }
      go.disabled = false;
    } }, reply || coord ? t('Répondre') : t('Publier la note'));
    return h('section', { class: 'card wide wr-note' },
      reply || coord ? h('p', { class: 'wr-ctx' }, '💬 ', coord ? t('Vous commentez un article.') : t('Vous répondez à une note.'), ' ', t('Votre réponse est publique et signée de votre nom.')) : null,
      ta, thumbs, h('div', { class: 'wr-bar' }, h('button', { type: 'button', class: 'ghost', onclick: () => pick.click() }, '📷 ', t('Images')), count, h('span', { class: 'sp' }), go), pick,
      files.length ? (addImages(files), null) : null);
  }

  // ── Article ──
  function articleEditor(a = keep.get() || {}) {
    const known = !!a.d && me.posts.some(p => p.d === a.d);
    const f = {
      title: h('input', { class: 'wr-title', placeholder: t('Titre'), 'aria-label': t('Titre'), maxlength: 200, value: a.title || '' }),
      summary: h('textarea', { rows: 2, placeholder: t('Chapeau : une ou deux phrases qui donnent envie de lire'), 'aria-label': t('Chapeau'), maxlength: 400, value: a.summary || '' }),
      tags: h('input', { placeholder: t('Étiquettes, séparées par des virgules'), 'aria-label': t('Étiquettes'), value: (a.tags || []).join(', ') }),
      body: h('textarea', { class: 'wr-md', rows: 18, placeholder: t('Écrivez ici. **gras**, *italique*, ## intertitre, > citation, - liste'), 'aria-label': t('Texte de l’article'), value: a.content || '' }),
      when: h('input', { type: 'datetime-local', 'aria-label': t('Date de publication') }),
      audience: h('select', { 'aria-label': t('Qui peut lire') }, [['public', '🌍 ' + t('Tout le monde')], ['friends', '🤝 ' + t('Mes amis')], ...(me.groups || []).map(g => ['group:' + g.id, '👥 ' + (g.name || t('Groupe')) + ` (${g.members})`])].map(([v, l]) => h('option', { value: v, selected: (a.audience || 'public') === v }, l))),
    };
    if (a.published_at) f.when.value = new Date((a.published_at - new Date().getTimezoneOffset() * 60) * 1000).toISOString().slice(0, 16);
    let cover = a.image || '', d = a.d || '';
    const coverBox = h('div', { class: 'wr-cover' }), coverPick = picker(false, async ([file]) => { coverBox.classList.add('busy'); try { cover = await upload(file); drawCover(); save(); } catch (e) { K.toast(e.message); } coverBox.classList.remove('busy'); });
    const drawCover = () => coverBox.replaceChildren(...(cover ? [h('img', { src: cover, alt: '' }), h('button', { type: 'button', class: 'ghost', onclick: ev => { ev.stopPropagation(); cover = ''; drawCover(); save(); } }, t('Retirer'))] : [h('span', {}, '🖼️ ', t('Ajouter une image de couverture'))]));
    coverBox.onclick = () => coverPick.click(); drawCover();

    // Barre d'outils : entoure la sélection (ou insère en début de ligne) ; tout reste du Markdown lisible.
    const wrap = (pre, post = pre, line = false) => () => {
      const ta = f.body, [s, e] = [ta.selectionStart, ta.selectionEnd], v = ta.value;
      if (line) { const ls = v.lastIndexOf('\n', s - 1) + 1; ta.setRangeText(pre, ls, ls, 'end'); } else ta.setRangeText(pre + v.slice(s, e) + post, s, e, 'select');
      ta.focus(); changed();
    };
    const imgPick = picker(true, async list => { for (const file of list) { try { const url = await upload(file); f.body.setRangeText(`\n![](${url})\n`, f.body.selectionStart, f.body.selectionEnd, 'end'); changed(); } catch (e) { K.toast(e.message); } } });
    const tools = [['B', t('Gras'), wrap('**')], ['I', t('Italique'), wrap('*')], ['H2', t('Intertitre'), wrap('## ', '', true)], ['H3', t('Sous-titre'), wrap('### ', '', true)], ['❝', t('Citation'), wrap('> ', '', true)], ['•', t('Liste'), wrap('- ', '', true)], ['1.', t('Liste numérotée'), wrap('1. ', '', true)], ['🔗', t('Lien'), wrap('[', '](https://)')], ['📷', t('Image'), () => imgPick.click()], ['</>', t('Code'), wrap('`')], ['―', t('Séparateur'), wrap('\n---\n', '', true)]]
      .map(([l, title, run]) => h('button', { type: 'button', class: 'ghost', title, 'aria-label': title, onclick: run }, l));
    const preview = h('div', { class: 'wr-preview prose', 'aria-live': 'polite' }), minutes = h('small', { class: 'muted' });
    let timer = 0;
    const render = async () => {
      try {
        const r = await post({ markdown: f.body.value }, '/preview');
        const doc = new DOMParser().parseFromString(`<body>${r.html}</body>`, 'text/html');
        preview.replaceChildren(...[...doc.body.childNodes].map(n => document.importNode(n, true)));
        if (!f.body.value.trim()) preview.replaceChildren(h('p', { class: 'muted' }, t('L’aperçu apparaît ici, tel que vos lecteurs le verront.')));
        minutes.textContent = t('{0} min de lecture', r.minutes);
      } catch { /* l'aperçu reviendra à la prochaine frappe */ }
    };
    const save = () => keep.set({ d, title: f.title.value, summary: f.summary.value, tags: f.tags.value.split(','), content: f.body.value, image: cover, published_at: a.published_at });
    const changed = () => { save(); clearTimeout(timer); timer = setTimeout(render, 500); };
    Object.values(f).forEach(x => { x.oninput = changed; });
    render();

    const event = kind => {
      const title = f.title.value.trim();
      if (!title) { f.title.focus(); K.toast(t('Donnez un titre à votre article.')); return null; }
      d = d || slugify(title);
      const at = f.when.value ? Math.floor(new Date(f.when.value).getTime() / 1000) : Math.floor(Date.now() / 1000);
      const tags = [['d', d], ['title', title], ['published_at', String(a.published_at && !f.when.value ? a.published_at : at)]];
      if (f.summary.value.trim()) tags.push(['summary', f.summary.value.trim()]);
      if (cover) tags.push(['image', cover]);
      for (const x of new Set(f.tags.value.split(',').map(s => s.trim().replace(/^#/, '').toLowerCase()).filter(Boolean))) tags.push(['t', x]);
      return { kind, content: f.body.value, tags, ...(kind === 30023 ? { audience: f.audience.value } : {}) };
    };
    const act = (label, cls, run) => h('button', { type: 'button', class: cls, onclick: async ev => { ev.target.disabled = true; try { await run(); } catch (e) { K.toast(e.message); } ev.target.disabled = false; } }, label);
    const draft = act(t('Enregistrer le brouillon'), 'ghost', async () => { const ev = event(30024); if (!ev) return; await publish(ev); K.toast(t('Brouillon enregistré'), 1); });
    const go = act(t('Publier'), '', async () => {
      const ev = event(30023); if (!ev) return;
      await publish(ev);
      await publish({ kind: 5, content: '', tags: [['a', `30024:${me.hex}:${d}`]] }).catch(() => {}); // le brouillon a servi
      keep.del();
      const later = Number(ev.tags.find(x => x[0] === 'published_at')[1]) > Date.now() / 1000 + 60;
      K.toast(later ? t('Article programmé') : t('Article publié'), 1);
      location.hash = '#/write?tab=list';
    });
    const remove = known ? K.sure(t('Retirer'), async () => {
      await publish({ kind: 5, content: '', tags: [['a', `30023:${me.hex}:${d}`], ['a', `30024:${me.hex}:${d}`]] });
      keep.del(); K.toast(t('Article retiré'), 1); location.hash = '#/write?tab=list';
    }, t('Retirer cet article de votre blog ? Les copies déjà reçues par d’autres relais peuvent subsister.'), { class: 'ghost danger' }) : null;
    const fresh = h('button', { type: 'button', class: 'ghost', onclick: () => { keep.del(); location.hash = '#/write?tab=article&new=' + Date.now(); } }, t('Nouvel article'));
    const panes = h('div', { class: 'wr-panes' }, h('div', { class: 'wr-edit' }, h('div', { class: 'wr-tools', role: 'toolbar', 'aria-label': t('Mise en forme') }, tools), f.body), h('div', { class: 'wr-side' }, h('div', { class: 'wr-side-h' }, h('b', {}, t('Aperçu')), minutes), preview));
    return h('section', { class: 'card wide wr-article' }, coverBox, coverPick, imgPick, f.title, f.summary, panes, f.tags,
      h('div', { class: 'wr-bar' }, h('label', { class: 'wr-when' }, '👁 ', t('Qui peut lire'), ' ', f.audience), h('label', { class: 'wr-when' }, '📅 ', t('Publier le'), ' ', f.when), h('span', { class: 'sp' }), ...[remove, fresh, draft, go].filter(Boolean)),
      h('p', { class: 'muted wr-hint' }, t('Réservé à vos amis ou à un groupe : seuls eux le lisent, dans leur application ; il n’apparaît jamais sur votre site public.')),
      h('p', { class: 'muted wr-hint' }, known ? t('Vous modifiez un article : le publier remplace la version en ligne.') : t('Rien n’est perdu : votre texte est gardé sur cet appareil jusqu’à la publication.')));
  }

  // ── Mes articles ──
  function listView() {
    if (!me.posts.length) return h('section', { class: 'card wide' }, h('p', { class: 'muted' }, t('Aucun article pour l’instant.')), h('p', {}, h('a', { class: 'btn', href: '#/write?tab=article' }, t('Écrire mon premier article'))));
    const seen = new Set();
    return h('section', { class: 'card wide' }, h('ul', { class: 'wr-list' }, me.posts.filter(p => !seen.has(p.d) && seen.add(p.d)).map(p => h('li', {},
      p.image ? h('img', { src: p.image, alt: '', loading: 'lazy' }) : h('span', { class: 'wr-noimg', 'aria-hidden': 'true' }, '📰'),
      h('span', { class: 't' }, h('b', {}, p.title), h('small', { class: 'muted' }, h('span', { class: 'pill' + (p.draft ? '' : p.scheduled ? ' warn' : ' ok') }, p.draft ? t('Brouillon') : p.scheduled ? t('Programmé') : t('Publié')), p.audience && p.audience !== 'public' ? [' ', h('span', { class: 'pill' }, '🔒 ', p.audience === 'friends' ? t('Mes amis') : t('Groupe'))] : null, ' ', new Date(p.published_at * 1000).toLocaleDateString(K.lang, { day: 'numeric', month: 'long', year: 'numeric' }))),
      h('span', { class: 'wr-acts' }, p.draft || p.scheduled || (p.audience && p.audience !== 'public') ? null : h('a', { class: 'btn ghost', href: `${me.site}blog/${encodeURIComponent(p.d)}`, target: '_blank', rel: 'noopener' }, t('Voir')),
        h('button', { type: 'button', class: 'ghost', onclick: () => { keep.set(p); location.hash = '#/write?tab=article&edit=' + encodeURIComponent(p.d); } }, t('Modifier')))))));
  }

  // ── Chez mes amis : leurs articles publics et ceux qu'ils m'ont réservés ; on lit ici, on commente d'un geste. ──
  async function friendsView() {
    const box = h('section', { class: 'card wide' }, h('p', { class: 'muted' }, t('Chargement…')));
    K.api(A + '/friends').then(r => {
      // Depuis la fiche d'une personne (`&who=<clé>`) : ses articles seulement, et un geste pour revenir à tous.
      const who = q.get('who'), mine = who ? r.posts.filter(p => p.author === who) : r.posts;
      const only = who ? h('p', { class: 'wr-only' }, h('span', { class: 'pill' }, '📰 ', mine[0] ? t('Articles de {0}', mine[0].name) : t('Une seule personne')), ' ', h('a', { href: '#/write?tab=friends' }, t('Tous mes amis'))) : null;
      if (!mine.length) return box.replaceChildren(...[only, h('p', { class: 'muted' }, who ? t('Cette personne n’a encore rien publié pour vous.') : t('Vos amis réciproques n’ont encore rien écrit. Leurs articles, publics ou réservés pour vous, arriveront ici.'))].filter(Boolean));
      r = { ...r, posts: mine };
      const reader = h('article', { class: 'wr-reader', hidden: true });
      const open = p => {
        const doc = new DOMParser().parseFromString(`<body>${p.html}</body>`, 'text/html');
        reader.replaceChildren(...[h('button', { type: 'button', class: 'ghost', onclick: () => { reader.hidden = true; list.hidden = false; } }, '← ', t('Retour')),
          p.image ? h('img', { class: 'wr-reader-cover', src: p.image, alt: '' }) : null, h('p', { class: 'muted' }, p.name, ' · ', new Date(p.published_at * 1000).toLocaleDateString(K.lang, { day: 'numeric', month: 'long', year: 'numeric' }), ' · ', t('{0} min de lecture', p.minutes), p.reserved ? [' · ', h('span', { class: 'pill' }, '🔒 ', t('Réservé pour vous'))] : null),
          h('h1', {}, p.title), p.summary ? h('p', { class: 'wr-lede' }, p.summary) : null, h('div', { class: 'wr-preview prose' }, [...doc.body.childNodes].map(n => document.importNode(n, true))),
          h('p', { class: 'wr-bar' }, h('a', { class: 'btn', href: `#/write?a=${encodeURIComponent(`30023:${p.author}:${p.d}`)}&to=${p.author}` }, '💬 ', t('Commenter')), h('a', { class: 'btn ghost', href: `#/chat?peer=${p.author}` }, '✉️ ', t('Lui écrire')), p.site && !p.reserved ? h('a', { class: 'btn ghost', href: `${p.site}blog/${encodeURIComponent(p.d)}`, target: '_blank', rel: 'noopener' }, t('Sur son blog'), ' ↗') : null)].filter(Boolean));
        list.hidden = true; reader.hidden = false; reader.scrollIntoView({ block: 'start' });
      };
      const list = h('ul', { class: 'wr-list' }, r.posts.map(p => h('li', { class: 'wr-friend', tabIndex: 0, onclick: () => open(p), onkeydown: ev => ev.key === 'Enter' && open(p) },
        p.image ? h('img', { src: p.image, alt: '', loading: 'lazy' }) : h('span', { class: 'wr-noimg', 'aria-hidden': 'true' }, p.reserved ? '🔒' : '📰'),
        h('span', { class: 't' }, h('b', {}, p.title), h('small', { class: 'muted' }, p.name, ' · ', new Date(p.published_at * 1000).toLocaleDateString(K.lang, { day: 'numeric', month: 'short' }), p.reserved ? ' · 🔒 ' + t('Réservé pour vous') : '')))));
      box.replaceChildren(...[only, list, reader].filter(Boolean));
      // Lien direct vers un article (`#/write?tab=friends&open=<auteur>:<d>`, depuis la recherche).
      const want = r.posts.find(p => `${p.author}:${p.d}` === q.get('open'));
      if (want) open(want);
    }, e => box.replaceChildren(h('p', { class: 'err', role: 'alert' }, e.message)));
    return box;
  }

  // ── Ranger en privé ce que le téléphone a partagé : liens en favoris (.url), photos et textes dans un dossier du uCloud. ──
  function privateSave(sh) {
    const url = sh.url || (sh.text || '').match(/https?:\/\/\S+/)?.[0] || '', text = [sh.title, sh.text].filter(Boolean).join('\n').trim();
    const folder = h('input', { value: url && !sh.list.length ? '/Favoris' : '/Partagés', 'aria-label': t('Dossier') });
    folder.setAttribute('list', 'wr-dirs'); // `list` n'a qu'un accesseur : seul l'attribut le règle
    const name = h('input', { value: (sh.title || (url ? new URL(url).hostname : '') || t('Partage')).slice(0, 80), 'aria-label': t('Nom') });
    // PUT brut (K.req encode en JSON) : même authentification que l'application.
    const dav = K.davRoot(), put = async (path, body, type) => { const url = new URL(dav + K.enc(path), location.origin).href, a = await K.auth('PUT', url); const r = await fetch(url, { method: 'PUT', body, headers: { ...(a && { Authorization: a }), 'Content-Type': type } }); if (!r.ok) throw new Error(t('Erreur {0}', r.status)); };
    const mkdirs = async dir => { let acc = ''; for (const s of dir.split('/').filter(Boolean)) { acc += '/' + s; await K.req(dav + K.enc(acc), { method: 'MKCOL' }); } };
    const clean = s => s.replace(/[\\/:*?"<>|]+/g, ' ').trim() || 'partage';
    const go = h('button', { type: 'button', onclick: async () => {
      go.disabled = true;
      try {
        const dir = '/' + folder.value.split('/').filter(Boolean).join('/');
        await mkdirs(dir);
        const base = clean(name.value);
        if (url) await put(`${dir}/${base}.url`, `[InternetShortcut]\r\nURL=${url}\r\n`, 'application/internet-shortcut');
        if (text && text !== url && text !== sh.title) await put(`${dir}/${base}.txt`, text + (url ? '\n\n' + url : '') + '\n', 'text/plain; charset=utf-8');
        for (const f of sh.list) await put(`${dir}/${clean(f.name)}`, f, f.type || 'application/octet-stream');
        K.toast(t('Rangé dans {0}', dir), 1);
        location.hash = dir === '/Favoris' ? '#/favoris' : '#/cloud?p=' + encodeURIComponent(dir);
      } catch (e) { K.toast(e.message); go.disabled = false; }
    } }, '🔒 ', t('Ranger'));
    return h('section', { class: 'card wide' }, h('p', { class: 'muted' }, t('Seul vous le voyez : chiffré dans votre uCloud, rien n’est publié.')),
      url ? h('p', {}, '🔖 ', h('b', {}, t('Lien')), ' ', h('code', {}, url)) : null, sh.list.length ? h('p', {}, '🖼️ ', t('{0} image(s)', sh.list.length)) : null,
      h('label', {}, t('Nom')), name, h('label', {}, t('Dossier')), folder, h('datalist', { id: 'wr-dirs' }, ['/Favoris', '/Partagés', '/Photos/Partagés', '/Documents'].map(v => h('option', { value: v }))),
      h('div', { class: 'wr-bar' }, h('span', { class: 'sp' }), go));
  }

  // Ce que le téléphone a partagé (service worker) : texte, lien et images préremplissent une note.
  let shared = null;
  if (q.get('shared') && 'caches' in window) {
    try {
      const c = await caches.open('astro-share'), meta = await c.match('/ui/share/meta'), files = [];
      if (meta) { shared = await meta.json(); for (let i = 0; i < (shared.files || 0); i++) { const r = await c.match('/ui/share/file/' + i); if (r) files.push(new File([await r.blob()], decodeURIComponent(r.headers.get('X-Name') || 'image.jpg'), { type: r.headers.get('Content-Type') || 'image/jpeg' })); } shared.list = files; }
      await caches.delete('astro-share');
    } catch { shared = null; }
  }

  const tab = reply || coord ? 'note' : q.get('tab') || 'note';
  const themes = h('div', { class: 'wr-themes', role: 'radiogroup', 'aria-label': t('Thème du site') }, [['aurore', 'Aurore'], ['sobre', 'Sobre'], ['kraft', 'Kraft']].map(([v, l]) => h('button', { type: 'button', role: 'radio', class: 'wr-theme wr-t-' + v, 'aria-checked': String(me.theme === v), onclick: async ev => {
    try { await post({ theme: v }, '/settings'); me.theme = v; themes.querySelectorAll('button').forEach(b => b.setAttribute('aria-checked', String(b === ev.currentTarget))); K.toast(t('Thème du site : {0}', l), 1); } catch (e) { K.toast(e.message); }
  } }, l)));
  const TABS = [['note', '✏️ ' + t('Note')], ['article', '📰 ' + t('Article')], ['list', '🗂 ' + t('Mes articles')], ['friends', '🤝 ' + t('Chez mes amis')]];
  const head = h('div', { class: 'wr-head' }, h('div', { class: 'wr-tabs', role: 'tablist' }, TABS.map(([k, l]) => h('a', { role: 'tab', href: '#/write?tab=' + k, 'aria-selected': String(k === tab) }, l))),
    h('span', { class: 'sp' }), themes, h('a', { class: 'btn ghost', href: me.site, target: '_blank', rel: 'noopener' }, t('Voir mon site'), ' ↗'));
  const note = () => noteEditor(shared ? [shared.title, shared.text, shared.url].filter(Boolean).join('\n') : '', shared?.list || []);
  let body = tab === 'article' ? articleEditor() : tab === 'list' ? listView() : tab === 'friends' ? await friendsView() : note();
  // Un partage du téléphone : publier sur le blog OU ranger en privé (favori, dossier du uCloud). Privé par défaut pour un simple lien.
  if (shared) {
    const pane = h('div', { class: 'grid' }), pick = (k, b) => { choice.querySelectorAll('button').forEach(x => x.setAttribute('aria-pressed', String(x === b))); pane.replaceChildren(k === 'blog' ? note() : privateSave(shared)); };
    const choice = h('div', { class: 'wr-choice', role: 'group', 'aria-label': t('Que faire de ce partage ?') },
      h('button', { type: 'button', onclick: ev => pick('private', ev.currentTarget) }, h('b', {}, '🔒 ', t('Ranger en privé')), h('small', {}, t('Favori ou dossier de mon uCloud'))),
      h('button', { type: 'button', onclick: ev => pick('blog', ev.currentTarget) }, h('b', {}, '📣 ', t('Publier une note')), h('small', {}, t('Visible de vos amis et sur votre site'))),
      shared.url ? h('a', { class: 'btn ghost', href: '#/cosurf?url=' + encodeURIComponent(shared.url) }, h('b', {}, '🧭 ', t('Naviguer avec un ami')), h('small', {}, t('Se faire aider sur cette page'))) : null);
    root.replaceChildren(h('h2', {}, t('Partagé depuis votre téléphone')), choice, pane);
    pick(shared.list.length && !shared.url ? 'blog' : 'private', choice.querySelectorAll('button')[shared.list.length && !shared.url ? 1 : 0]);
    return;
  }
  root.replaceChildren(...[reply || coord ? null : head, h('div', { class: 'grid' }, body)].filter(Boolean));
  root.querySelector('textarea, .wr-title')?.focus();
}
