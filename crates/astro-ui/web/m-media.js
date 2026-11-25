'use strict';
// Médiathèque : films, séries, vidéos et morceaux déposés avec leur fiche. Lecture en flux (adresse /play/… que la station
// signe : avance rapide sans charger le fichier entier), file d'écoute qui continue quand on change d'écran, fiches corrigées
// et enrichies ici (titre, genres, mots-clés, distribution, album…), recherche sur tout ce qu'elles disent.

const A = '/api/v1/ucloud/media';
const norm = s => String(s ?? '').normalize('NFD').replace(/[\u0300-\u036f]/g, '').toLowerCase();
// Durée lisible : « 2 h 05 », « 42 min », « 4:05 » (un morceau).
const dur = s => (!s ? '' : s >= 3600 ? `${Math.floor(s / 3600)} h ${String(Math.floor(s / 60) % 60).padStart(2, '0')}` : s >= 600 ? `${Math.round(s / 60)} min` : `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`);
const albumKey = it => `${it.music?.album_artist || it.music?.artist || ''}\n${it.music?.album || ''}`;
const byTitle = (a, b) => a.title.localeCompare(b.title);
let P = null; // lecteur audio : un seul, gardé d'un écran à l'autre

// Adresses de lecture (valables 12 h) : demandées une fois par fichier.
const urls = new Map();
async function playUrls(K, paths) {
  const want = paths.filter(p => !(urls.get(p)?.until > Date.now()));
  if (want.length) {
    const r = await K.api(A + '/play', { method: 'POST', body: { paths: want } });
    for (const [p, u] of Object.entries(r.urls)) urls.set(p, { u, until: r.expires_at * 1000 - 60000 });
  }
  return paths.map(p => urls.get(p)?.u);
}

// ── Lecteur audio : barre en bas de l'écran, file d'écoute, commandes de l'écran verrouillé ──
function player(K) {
  if (P) return P;
  const { h, t } = K;
  const audio = h('audio', { controls: true, preload: 'none' }), img = h('img', { alt: '', hidden: true }), label = h('span', { class: 'mm-now' });
  const play = h('button', { class: 'ghost', 'aria-label': t('Lecture ou pause'), onclick: () => (audio.paused ? audio.play() : audio.pause()) }, '⏯');
  const bar = h('div', { class: 'mm-player', role: 'region', 'aria-label': t('Lecteur'), hidden: true }, img, label,
    h('button', { class: 'ghost', 'aria-label': t('Précédent'), onclick: () => P.go(P.at - 1) }, '⏮'), play,
    h('button', { class: 'ghost', 'aria-label': t('Suivant'), onclick: () => P.go(P.at + 1) }, '⏭'), audio,
    h('button', { class: 'ghost', 'aria-label': t('Fermer le lecteur'), onclick: () => { audio.pause(); audio.removeAttribute('src'); bar.hidden = true; document.body.classList.remove('mm-on'); } }, '✕'));
  document.body.append(bar);
  P = { queue: [], at: 0, seq: 0, audio, async go(i) {
    if (i < 0 || i >= P.queue.length) return;
    P.at = i;
    const my = ++P.seq, it = P.queue[i], [u] = await playUrls(K, [it.path]);
    if (my !== P.seq) return; // « suivant » pressé entre-temps : seule la dernière demande joue
    if (!u) return K.toast(t('Ce fichier ne se lit pas'), 0);
    bar.hidden = false; document.body.classList.add('mm-on');
    label.replaceChildren(h('b', {}, it.title), h('small', { class: 'muted' }, it.music?.artist || ''));
    img.hidden = true;
    if (it.art?.cover) K.blob(it.art.cover, true).then(c => { img.src = c; img.hidden = false; }, () => {});
    audio.src = u; audio.play().catch(() => {});
    if ('mediaSession' in navigator) navigator.mediaSession.metadata = new MediaMetadata({ title: it.title, artist: it.music?.artist || '', album: it.music?.album || '' });
  } };
  audio.addEventListener('ended', () => P.go(P.at + 1));
  if ('mediaSession' in navigator) {
    navigator.mediaSession.setActionHandler('previoustrack', () => P.go(P.at - 1));
    navigator.mediaSession.setActionHandler('nexttrack', () => P.go(P.at + 1));
  }
  return P;
}
const listen = (K, list, i = 0) => { const p = player(K); p.queue = list; p.go(i); };

// ── Vidéo : plein écran dans une fenêtre, sous-titres de la fiche ──
async function watch(K, root, it) {
  const { h, t } = K;
  const subs = it.subtitles || [], got = await playUrls(K, [it.path, ...subs.map(s => s.path)]);
  if (!got[0]) return K.toast(t('Ce fichier ne se lit pas'), 0);
  P?.audio.pause();
  const video = h('video', { src: got[0], controls: true, autoplay: true, playsinline: true },
    subs.map((s, i) => got[i + 1] && h('track', { kind: 'subtitles', src: got[i + 1], srclang: s.lang || '', label: s.lang || '?', default: i === 0 && s.lang === K.lang })));
  const dlg = h('dialog', { class: 'mm-video', 'aria-label': it.title }, h('div', { class: 'bar' }, h('b', {}, it.title), h('span', { class: 'sp' }), h('button', { class: 'ghost', onclick: () => dlg.close() }, t('Fermer'))), video);
  dlg.addEventListener('close', () => { video.pause(); video.removeAttribute('src'); dlg.remove(); });
  root.append(dlg); dlg.showModal();
}

export default async function media(root, K, q) {
  const { h, t, api, toast } = K;
  const css = h('link', { rel: 'stylesheet', href: '/ui/m-media.css' }); document.head.append(css); K.cleanup(() => css.remove());
  const KINDS = [['', 'Tout'], ['film', 'Films'], ['episode', 'Séries'], ['video', 'Vidéos'], ['music', 'Musique'], ['friends', 'Chez mes amis']];
  let items = [], kind = q.get('k') || '', open = null, friends = null, polling = false, safety = {};
  const find = h('input', { type: 'search', placeholder: t('Titre, artiste, genre, acteur, mot-clé…'), 'aria-label': t('Rechercher dans la médiathèque'), maxlength: 120, oninput: () => draw() });
  const kinds = h('div', { class: 'bar mm-kinds', role: 'group', 'aria-label': t('Type') }, KINDS.map(([k, l]) => h('button', { class: 'ghost', 'data-k': k, onclick: () => { kind = k; open = null; draw(); } }, t(l))));
  const body = h('div', { class: 'mm-body' }), note = h('p', { class: 'muted', role: 'status' });
  root.replaceChildren(h('div', { class: 'bar' }, find), kinds, note, body);

  const thumb = (path, emoji) => {
    const img = h('img', { alt: '', loading: 'lazy' });
    if (path) K.blob(path, true).then(u => { img.src = u; }, () => img.replaceWith(h('span', { class: 'mm-ph', 'aria-hidden': 'true' }, emoji)));
    return path ? img : h('span', { class: 'mm-ph', 'aria-hidden': 'true' }, emoji);
  };
  const art = it => it.art?.poster || it.art?.cover || it.art?.still || it.art?.backdrop;
  // Copies vérifiées chez des amis d'un fichier partagé : pastille discrète dans la liste, phrase dans la fiche.
  const sfOf = it => (it.level === 'circle' ? safety[it.path] : null);
  const dot = it => { const f = sfOf(it); return !f ? '' : f.safe ? '🟢 ' : f.copies || f.packed ? '🟡 ' : '⏳ '; };
  const ago = s => { const hrs = Math.max(1, Math.round((Date.now() / 1000 - s) / 3600)); return hrs < 48 ? t('vérifié il y a {0} h', hrs) : t('vérifié il y a {0} j', Math.round(hrs / 24)); };
  const safeLine = it => {
    const f = sfOf(it);
    if (!f) return '';
    const who = f.at.slice(0, 3).join(', '), when = f.verified_at ? ' · ' + ago(f.verified_at) : '';
    const txt = f.safe ? t('🟢 En sécurité : {0} copies chez vos amis', f.copies)
      : f.copies ? (f.copies > 1 ? t('🟡 {0} copies chez {1}', f.copies, who) : t('🟡 1 copie chez {0}', who)) + (f.packed ? ' ' + t('+ votre sauvegarde') : '')
        : f.packed ? t('🟡 Gardé par votre sauvegarde, pas encore de copie vérifiée chez vos amis') : t('⏳ Pas encore à l\'abri : il part cette nuit chez vos gardiens');
    return h('p', { class: 'muted mm-safe', role: 'status' }, txt + when);
  };
  const card = (title, sub, path, emoji, go) => h('button', { class: 'mm-card', onclick: go }, thumb(path, emoji), h('b', {}, title), h('small', { class: 'muted' }, sub || ''));
  const back = () => h('button', { class: 'ghost', onclick: () => { open = null; draw(); } }, '← ' + t('Médiathèque'));
  const hay = it => norm([it.title, it.original_title, it.year, it.overview, it.genres, it.keywords, it.cast, it.directors, it.series?.name, it.music?.artist, it.music?.album, it.music?.album_artist, it.music?.composer].flat().join(' '));

  async function load() {
    try {
      // Mise à l'abri des fichiers partagés : une seule lecture pour toute la médiathèque.
      const [r, sf] = await Promise.all([api(A), api(A + '/safety').catch(() => null)]);
      items = r.items; safety = sf?.files || {};
      note.textContent = r.pending ? t('{0} fiche(s) en cours de lecture…', r.pending) : '';
      if (r.pending) setTimeout(() => root.isConnected && load(), 1500);
    } catch (e) { note.textContent = e.message; }
    draw();
  }

  // Liste filtrée : type choisi, puis chaque mot de la recherche retrouvé quelque part dans la fiche.
  const shown = () => { const words = norm(find.value).split(/\s+/).filter(Boolean); return items.filter(it => (!kind || it.type === kind) && words.every(w => hay(it).includes(w))); };

  function draw() {
    kinds.querySelectorAll('button').forEach(b => b.setAttribute('aria-pressed', String(b.dataset.k === kind)));
    if (open) return open();
    if (kind === 'friends') return drawFriends();
    const list = shown();
    if (!items.length) return body.replaceChildren(h('div', { class: 'card' }, h('h2', {}, t('Votre médiathèque est vide')),
      h('p', {}, t('Les films, séries, vidéos et morceaux ajoutés avec leur fiche (outil « media » de la station, ou « Ouvrir avec » depuis Firefox) apparaissent ici, prêts à être lus.'))));
    if (!list.length) return body.replaceChildren(h('p', { class: 'muted' }, t('Rien ne correspond à cette recherche.')));
    const films = list.filter(i => i.type === 'film' || i.type === 'video').sort(byTitle), eps = list.filter(i => i.type === 'episode'), songs = list.filter(i => i.type === 'music');
    const series = [...new Set(eps.map(i => i.series?.name || i.title))].sort();
    const albums = [...new Set(songs.map(albumKey))].sort();
    const grid = (title, cards) => cards.length ? h('section', {}, h('h2', {}, title, ' ', h('small', { class: 'muted' }, String(cards.length))), h('div', { class: 'mm-grid' }, cards)) : '';
    body.replaceChildren(
      grid(t('Films et vidéos'), films.map(it => card(it.title, dot(it) + [it.year, dur(it.duration_s)].filter(Boolean).join(' · '), art(it), '🎬', () => detail(it)))),
      grid(t('Séries'), series.map(n => { const e = eps.filter(i => (i.series?.name || i.title) === n); return card(n, t('{0} épisode(s)', e.length), e.map(art).find(Boolean), '📺', () => show(n, e)); })),
      grid(t('Musique'), albums.map(k => { const s = songs.filter(i => albumKey(i) === k), [who, alb] = k.split('\n'); return card(alb || t('Morceaux'), who, s.map(art).find(Boolean), '🎵', () => album(k, s)); })));
  }

  const meta = it => [it.year, dur(it.duration_s), ...(it.genres || [])].filter(Boolean).join(' · ');
  const shared = it => (it.level === 'circle' ? h('span', { class: 'pill ok' }, t('Partagé avec mes amis')) : '');
  const play = it => (it.type === 'music' ? listen(K, [it]) : watch(K, root, it));
  // Créer la fiche d'une vidéo à soi, avec la licence choisie (celle des étiquettes du partage) : sans fiche, pas de partage.
  const LICENCES = [['', 'Aucune licence'], ['CC-BY-4.0', 'CC BY'], ['CC-BY-SA-4.0', 'CC BY-SA'], ['CC-BY-NC-4.0', 'CC BY-NC'], ['CC-BY-NC-SA-4.0', 'CC BY-NC-SA'], ['CC0-1.0', 'CC0 (domaine public)']];
  const make = it => {
    const lic = h('select', { 'aria-label': t('Licence') }, LICENCES.map(([v, l]) => h('option', { value: v }, t(l))));
    return h('span', {}, lic, ' ', h('button', { class: 'ghost', onclick: async ev => {
      ev.target.disabled = true;
      try { const r = await api(A + '/make', { method: 'POST', body: { path: it.path, licence: lic.value || null } }); items = items.map(x => (x.path === it.path ? r.item : x)); toast(t('Fiche créée : vous pouvez la corriger et la partager'), 1); open = null; draw(); }
      catch (e) { ev.target.disabled = false; toast(e.message, 0); }
    } }, '✎ ' + t('Créer la fiche')));
  };
  const edit = it => it.bare ? make(it) : h('button', { class: 'ghost', onclick: () => form(it) }, '✎ ' + t('Corriger la fiche'));
  const line = (label, v) => (v?.length ? h('p', {}, h('b', {}, t(label) + ' : '), [v].flat().join(', ')) : '');

  // « Partager avec mes amis » (un film, ou une série / un album comme liste) et la trace : qui l'a copié, de qui je le tiens,
  // qui le repartage. Le fichier circule sous le même blob : rien n'est rechiffré, aucun nouveau CID.
  function shareBox(list, group) {
    const fiches = list.map(i => i.fiche), sw = h('input', { type: 'checkbox', id: 'mm-sh', checked: list.every(i => i.level === 'circle') }), tr = h('div', { class: 'muted' });
    const trace = async () => {
      try {
        const rs = await Promise.all(fiches.slice(0, 20).map(f => api(A + '/trace?path=' + encodeURIComponent(f))));
        const copies = [...new Map(rs.flatMap(r => r.copies).map(c => [c.label, c])).values()], from = [...new Set(rs.map(r => r.from).filter(Boolean))], again = [...new Set(rs.flatMap(r => r.reshared))];
        tr.replaceChildren(...[from.length && h('p', {}, t('Reçu de {0}', from.join(', '))),
          copies.length ? h('p', {}, t('Copié par : {0}', copies.map(c => `${c.label} (${K.when(c.at)})`).join(', '))) : sw.checked && h('p', {}, t('Personne ne l\'a encore copié.')),
          again.length && h('p', {}, t('Repartagé par : {0}', again.join(', ')))].filter(Boolean));
      } catch (e) { tr.textContent = e.message; }
    };
    sw.onchange = async () => {
      sw.disabled = true;
      try {
        await api(A + '/share', { method: 'POST', body: { paths: fiches, on: sw.checked, ...(group && { list: group }) } });
        list.forEach(i => { i.level = sw.checked ? 'circle' : 'private'; });
        toast(sw.checked ? t('Partagé avec vos amis') : t('Partage retiré : les copies déjà faites restent chez vos amis'), 1);
        trace();
      } catch (e) { sw.checked = !sw.checked; toast(e.message, 0); }
      sw.disabled = false;
    };
    trace();
    return h('div', { class: 'mm-share' }, h('label', { for: 'mm-sh' }, sw, ' ', t(!group ? 'Partager avec mes amis' : group.kind === 'series' ? 'Partager la série avec mes amis' : 'Partager l\'album avec mes amis')), tr);
  }

  function detail(it) {
    open = () => body.replaceChildren(back(), h('article', { class: 'card mm-detail' }, thumb(art(it), it.type === 'music' ? '🎵' : '🎬'),
      h('div', {}, h('h2', {}, it.title, ' ', shared(it)), h('p', { class: 'muted' }, meta(it)), it.original_title && it.original_title !== it.title ? h('p', { class: 'muted' }, it.original_title) : '',
        h('p', {}, h('button', { onclick: () => play(it) }, '▶ ' + t('Lire')), ' ', edit(it)), it.bare ? h('p', { class: 'muted' }, t('Créez la fiche de cette vidéo pour la corriger et la partager avec vos amis.')) : shareBox([it]), safeLine(it),
        it.overview ? h('p', {}, it.overview) : '', line('Réalisation', it.directors), line('Avec', it.cast), line('Mots-clés', it.keywords))));
    draw();
  }

  function show(name, eps) {
    eps.sort((a, b) => (a.series?.season ?? 0) - (b.series?.season ?? 0) || (a.series?.episode ?? 0) - (b.series?.episode ?? 0));
    open = () => body.replaceChildren(back(), h('section', { class: 'card' }, h('h2', {}, name), shareBox(eps, { kind: 'series', title: name }), h('ol', { class: 'mm-tracks' }, eps.map(e =>
      h('li', {}, h('button', { class: 'ghost', 'aria-label': t('Lire') + ' ' + e.title, onclick: () => watch(K, root, e) }, '▶'),
        h('span', {}, `S${String(e.series?.season ?? 0).padStart(2, '0')}E${String(e.series?.episode ?? 0).padStart(2, '0')} · ${e.title}`), h('small', { class: 'muted' }, dur(e.duration_s)),
        h('button', { class: 'ghost', 'aria-label': t('Corriger la fiche') + ' ' + e.title, onclick: () => form(e) }, '✎'))))));
    draw();
  }

  function album(key, songs) {
    songs.sort((a, b) => (a.music?.track ?? 999) - (b.music?.track ?? 999) || byTitle(a, b));
    const [who, alb] = key.split('\n'), mix = songs.some(s => s.music?.mix_start_s != null);
    open = () => body.replaceChildren(back(), h('section', { class: 'card' }, h('div', { class: 'mm-head' }, thumb(songs.map(art).find(Boolean), '🎵'),
      h('div', {}, h('h2', {}, alb || t('Morceaux')), h('p', { class: 'muted' }, (mix ? t('Mix de {0}', who) : who) + ' · ' + t('{0} morceau(x)', songs.length)),
        h('p', {}, h('button', { onclick: () => listen(K, songs) }, '▶ ' + t('Tout lire')), ' ', h('button', { class: 'ghost', onclick: () => listen(K, [...songs].sort(() => Math.random() - 0.5)) }, '🔀 ' + t('Au hasard'))), shareBox(songs, { kind: 'album', title: `${who} — ${alb || t('Morceaux')}` }))),
      h('ol', { class: 'mm-tracks' }, songs.map((s, i) => h('li', {}, h('button', { class: 'ghost', 'aria-label': t('Lire') + ' ' + s.title, onclick: () => listen(K, songs, i) }, '▶'),
        h('span', {}, dot(s), s.title, ' ', h('small', { class: 'muted' }, s.music?.artist !== who ? s.music?.artist || '' : ''), ' ', shared(s)), h('small', { class: 'muted' }, dur(s.duration_s)),
        h('button', { class: 'ghost', 'aria-label': t('Corriger la fiche') + ' ' + s.title, onclick: () => form(s) }, '✎'))))));
    draw();
  }

  // ── Correction d'une fiche : champs adaptés au type ; listes séparées par des virgules ──
  async function form(it) {
    let f;
    try { f = await api(A + '/fiche?path=' + encodeURIComponent(it.fiche)); } catch (e) { return toast(e.message, 0); }
    const fields = [['title', 'Titre'], ['original_title', 'Titre original'], ['year', 'Année', 'number'], ['overview', 'Résumé', 'area'], ['genres', 'Genres', 'list'], ['keywords', 'Mots-clés', 'list'],
      ...(f.type === 'music' ? [['music.artist', 'Artiste'], ['music.album', 'Album'], ['music.album_artist', 'Artiste de l\'album'], ['music.track', 'Piste', 'number'], ['music.composer', 'Compositeur']]
        : [['directors', 'Réalisation', 'list'], ['cast', 'Distribution', 'list'], ...(f.type === 'episode' ? [['series.name', 'Série'], ['series.season', 'Saison', 'number'], ['series.episode', 'Épisode', 'number']] : [])])];
    const get = k => k.split('.').reduce((o, x) => o?.[x], f);
    const inputs = fields.map(([k, l, type], n) => {
      const v = get(k), id = 'mm-f' + n, el = type === 'area' ? h('textarea', { id, rows: 5, maxlength: 4000 }) : h('input', { id, type: type === 'number' ? 'number' : 'text', maxlength: type === 'list' ? 2000 : 300 });
      el.value = Array.isArray(v) ? v.join(', ') : v ?? '';
      return [k, type, el, h('label', { for: id }, t(l), type === 'list' ? h('small', { class: 'muted' }, ' ' + t('(séparés par des virgules)')) : '')];
    });
    const err = h('p', { class: 'err', role: 'alert' });
    const save = async ev => {
      ev.preventDefault();
      const set = {};
      for (const [k, type, el] of inputs) {
        const v = type === 'list' ? el.value.split(',').map(s => s.trim()).filter(Boolean) : el.value.trim();
        const [a, b] = k.split('.');
        if (b) (set[a] ??= {})[b] = v; else set[a] = v;
      }
      try {
        const r = await api(A + '/edit', { method: 'POST', body: { path: it.fiche, set } });
        toast(t('Fiche corrigée'), 1);
        items = items.map(x => (x.fiche === it.fiche ? r.item : x));
        detailOf(r.item);
      } catch (e) { err.textContent = e.message; }
    };
    const detailOf = x => (x.type === 'music' ? album(albumKey(x), items.filter(i => albumKey(i) === albumKey(x))) : x.type === 'episode' ? show(x.series?.name || x.title, items.filter(i => i.type === 'episode' && i.series?.name === x.series?.name)) : detail(x));
    open = () => body.replaceChildren(h('button', { class: 'ghost', onclick: () => detailOf(it) }, '← ' + t('Annuler')),
      h('form', { class: 'card mm-form', onsubmit: save }, h('h2', {}, t('Corriger la fiche')), h('p', { class: 'muted' }, t('Ces informations servent à ranger et à retrouver vos médias.')),
        inputs.map(([, , el, label]) => h('p', {}, label, el)), err, h('p', {}, h('button', { type: 'submit' }, t('Enregistrer')))));
    draw();
  }

  // ── Chez mes amis : leurs films et morceaux ; « Lire » demande une copie, rangée chez vous comme chez eux ──
  async function loadFriends(force) {
    try { friends = await api(A + '/friends' + (force ? '?force=1' : '')); } catch (e) { friends = { items: [], friends: [], error: e.message }; }
    if (!open && kind === 'friends') draw();
  }
  const who = it => { const s = (it.seeders || []).map(x => x.label); return s.length > 2 ? `${s.slice(0, 2).join(', ')} +${s.length - 2}` : s.join(', '); };
  function drawFriends() {
    if (!friends) { body.replaceChildren(h('p', { class: 'muted' }, t('Chargement…'))); return loadFriends(false); }
    const words = norm(find.value).split(/\s+/).filter(Boolean);
    const list = friends.items.filter(it => words.every(w => norm([hay(it), it.author_label, who(it), it.episode_name].join(' ')).includes(w)));
    const down = friends.friends.filter(f => f.error).map(f => f.label);
    body.replaceChildren(
      h('p', {}, h('button', { class: 'ghost', onclick: () => { friends = null; loadFriends(true); draw(); } }, '↻ ' + t('Actualiser')), ' ',
        down.length ? h('small', { class: 'muted' }, t('Station injoignable : {0} (liste d\'avant)', down.join(', '))) : ''),
      friends.error ? h('p', { class: 'err' }, friends.error) : '',
      !list.length ? h('p', { class: 'muted' }, friends.items.length ? t('Rien ne correspond à cette recherche.') : t('Vos amis ne partagent encore aucun film ni morceau avec vous.')) : '',
      h('div', { class: 'mm-grid' }, list.map(it => card(it.type === 'episode' ? `${it.series?.name || ''} · ${it.title}` : it.title, (it.copied ? '✓ ' : '') + t('chez {0}', who(it)), null, it.type === 'music' ? '🎵' : it.type === 'episode' ? '📺' : '🎬', () => fdetail(it)))));
  }
  function fdetail(it) {
    const n = (it.seeders || []).length;
    const state = it.copied ? h('p', { class: 'ok' }, t('Déjà dans votre médiathèque : {0}', it.copied))
      : it.error ? h('p', { class: 'err' }, it.error)
        : it.asked_at ? h('p', { class: 'muted', role: 'status' }, t('Copie demandée : elle arrive dans votre médiathèque, rangée comme chez votre ami.')) : '';
    open = () => body.replaceChildren(back(), h('article', { class: 'card' }, h('h2', {}, it.title),
      h('p', { class: 'muted' }, [it.year, dur(it.duration_s), ...(it.genres || [])].filter(Boolean).join(' · ')),
      h('p', {}, (n > 1 ? t('{0} copies chez vos amis : {1}', n, who(it)) : t('Chez {0}', who(it)))),
      state, h('p', {}, h('button', { onclick: () => fplay(it) }, '▶ ' + t('Lire')))));
    draw();
  }
  async function fplay(it) {
    if (it.copied) return playMine(it.copied, it);
    try {
      const r = await api(A + '/friends/request', { method: 'POST', body: { event_id: it.event_id } });
      if (r.state === 'copied') { it.copied = r.path; return playMine(r.path, it); }
      it.asked_at = Date.now() / 1000; it.error = null;
      toast(r.state === 'sent' ? t('Copie faite : « {0} » est dans votre médiathèque.', it.title) : t('Demande envoyée à {0} : le fichier arrivera dans votre médiathèque.', it.author_label), 1);
      watchArrivals(); fdetail(it);
    } catch (e) { it.error = e.message; fdetail(it); }
  }
  // Tant qu'une copie est attendue : la liste est relue toutes les 10 s, et l'arrivée est annoncée.
  function watchArrivals() {
    if (polling) return;
    polling = true;
    K.every(10000, async () => {
      const before = new Set((friends?.items || []).filter(i => i.copied).map(i => i.x));
      await loadFriends(false);
      for (const i of friends.items) if (i.copied && !before.has(i.x) && i.asked_at) { toast(t('« {0} » est arrivé : il se lit maintenant.', i.title), 1); load(); }
    });
  }
  async function playMine(path, it) {
    if (!items.some(i => i.path === path)) await load();
    const mine = items.find(i => i.path === path) || { path, title: it.title, type: it.type, music: it.music };
    play(mine);
  }

  await load();
}
