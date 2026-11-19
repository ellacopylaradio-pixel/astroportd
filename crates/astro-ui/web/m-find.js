'use strict';
// Recherche dans tout le uCloud (Fichiers › 🔍, la case est dans la barre de Fichiers : m-shell.js) : noms, texte lu dans les
// documents, ce que montrent les photos et les vidéos, personnes reconnues, villes, mois et années — tout est cherché par la
// station, rien ne la quitte. Les meilleurs résultats d'abord, puis tout, rangé en Documents / Photos et vidéos / Films et
// musique. Rien de sûr : « Peut-être », jamais un « aucun résultat » sec.
const day = 86400;
const utc = v => (v ? Date.parse(v + 'T00:00:00Z') / 1000 : null);
const ymd = s => new Date(s * 1000).toISOString().slice(0, 10);
const name = p => p.split('/').pop();
const dir = p => p.slice(0, p.lastIndexOf('/')) || '/';
const pic = it => /^(image|video)\//.test(it.mime || '');
const clock = ms => { const s = Math.round(ms / 1000); return Math.floor(s / 60) + ':' + String(s % 60).padStart(2, '0'); };

export default async function find(root, K, qs) {
  const { h, t } = K, A = '/api/v1/ucloud/search?';
  const q = (qs.get('q') || '').trim(), kind = qs.get('kind') || '', from = qs.get('from') || '', to = qs.get('to') || '';
  const css = h('link', { rel: 'stylesheet', href: '/ui/m-find.css' }); document.head.append(css); K.cleanup(() => css.remove());
  const go = p => { const n = new URLSearchParams({ q, kind, from, to, ...p }); [...n.keys()].forEach(k => !n.get(k) && n.delete(k)); location.hash = '#/find?' + n; };

  // ── Affiner : type et période, repliés tant qu'on ne s'en sert pas ──
  const kSel = h('select', { 'aria-label': t('Type') }, [['', 'Tout'], ['image', 'Photos'], ['video', 'Vidéos'], ['other', 'Autres fichiers']].map(([v, l]) => h('option', { value: v, selected: v === kind }, t(l))));
  const fIn = h('input', { type: 'date', 'aria-label': t('Du'), value: from }), tIn = h('input', { type: 'date', 'aria-label': t('Au'), value: to });
  const refine = h('details', { class: 'fd-refine', open: !!(kind || from || to) }, h('summary', {}, t('Affiner')),
    h('div', { class: 'bar' }, kSel, h('label', {}, t('Du'), ' ', fIn), h('label', {}, t('Au'), ' ', tIn),
      h('button', { onclick: () => go({ kind: kSel.value, from: fIn.value, to: tIn.value }) }, t('Appliquer')),
      kind || from || to ? h('button', { class: 'ghost', onclick: () => go({ kind: '', from: '', to: '' }) }, t('Tout effacer')) : null));

  // Ce que la station sait chercher (le serveur le dit) : l'aide ne promet que ce qui marche, et invite au bon moment.
  const hint = h('div', {});
  const can = K.api('/api/v1/me/reading').then(r => ({ text: r.state === 'local' || r.state === 'friend', image: r.state !== 'blocked' && (r.local || []).includes('vision'), off: r.state === 'off' }), () => ({}));
  can.then(c => { if (c.off) hint.replaceChildren(h('p', { class: 'muted' }, t('Le texte de vos documents n’est pas encore cherchable.'), ' ', h('a', { href: '#/account?tab=moi&card=reading' }, t('Documents et photos cherchables')))); });

  if (!q && !kind && !from && !to) {
    const c = await can, ex = [c.text && 'facture', c.image && 'vélo rouge', 'Lyon', 'mars 2024'].filter(Boolean);
    root.replaceChildren(h('h2', {}, t('Recherche')), h('section', { class: 'card wide' },
      h('p', {}, t('Tapez ce dont vous vous souvenez : un nom de fichier, un mot lu dans un document, ce que montre une photo, le prénom d’une personne reconnue, une ville, un mois ou une année.')),
      h('p', { class: 'fd-chips' }, t('Par exemple :'), ex.map(x => h('button', { class: 'ghost', onclick: () => go({ q: x }) }, x))), hint));
    return;
  }

  // ── Rendu : une photo ou une vidéo en vignette, le reste en ligne (nom, dossier, pourquoi c'est trouvé) ──
  const why = it => (it.found === 'seen' ? t('trouvée par ce qu’elle montre') : it.excerpt || '');
  const open = (it, list) => {
    if (pic(it)) { const l = list.filter(pic); return K.open(l, l.indexOf(it)); }
    if (it.media) { location.hash = '#/media'; return; }
    K.blob(it.path).then(u => h('a', { href: u, download: name(it.path) }).click(), e => K.toast(e.message));
  };
  const row = (it, list) => h('li', {}, h('span', { 'aria-hidden': 'true' }, it.media ? '🎬' : pic(it) ? '🖼' : '📄'),
    h('span', { class: 'fd-what' }, h('button', { class: 'link', onclick: () => open(it, list) }, name(it.path)),
      h('a', { class: 'fd-where', href: '#/cloud?p=' + encodeURIComponent(dir(it.path)) }, dir(it.path)),
      why(it) ? h('small', {}, why(it)) : null,
      it.at_ms ? h('button', { class: 'ghost fd-at', onclick: () => open(it, list) }, '▶ ' + t('vers {0}', clock(it.at_ms))) : null));
  // Vignettes : une pastille dit pourquoi une photo est là (👁 ce qu'elle montre, T le texte lu dedans).
  const tiles = (g, list, items) => {
    if (!items.length) return;
    K.tiles(g, list, items);
    [...g.children].slice(-items.length).forEach((b, i) => {
      const it = items[i], play = b.querySelector('.badge');
      if (it.at_ms && play) play.textContent = '▶ ' + clock(it.at_ms); // la visionneuse ouvre la vidéo à ce moment
      if (it.found) { b.append(h('span', { class: 'badge txt', 'aria-hidden': 'true' }, it.found === 'seen' ? '👁' : 'T')); b.title = why(it) + (it.at_ms ? ' · ' + t('vers {0}', clock(it.at_ms)) : ''); }
    });
  };
  // Un mélange (meilleurs, peut-être) : les photos et vidéos en vignettes, les autres en lignes.
  const mixed = items => {
    const list = [], g = h('div', { class: 'photos' }), ul = h('ul', { class: 'files fd' });
    tiles(g, list, items.filter(it => pic(it) && !it.media));
    items.filter(it => !pic(it) || it.media).forEach(it => ul.append(row(it, items)));
    return [g.children.length ? g : null, ul.children.length ? ul : null];
  };

  // ── Groupes : remplis page après page ──
  const group = (title, el) => { const n = h('small', {}), s = h('section', { class: 'card wide', hidden: true }, h('h3', {}, t(title), ' ', n), el); s.n = 0; s.add = k => { s.n += k; n.textContent = '· ' + s.n; s.hidden = !s.n; }; return s; };
  const docs = h('ul', { class: 'files fd' }), docList = [], grid = h('div', { class: 'photos' }), picList = [], films = h('ul', { class: 'files fd' }), filmList = [];
  const G = { docs: group('Documents', docs), pics: group('Photos et vidéos', grid), films: group('Films et musique', films) };
  const more = h('button', { class: 'ghost', hidden: true }, t('Plus de résultats'));
  const top = h('div', {}), says = h('div', { role: 'status', 'aria-live': 'polite' });
  let cursor = null;
  const page = async () => {
    const p = new URLSearchParams({ limit: 60 });
    if (q) p.set('q', q);
    if (kind) p.set('kind', kind);
    if (utc(from) != null) p.set('from', utc(from));
    if (utc(to) != null) p.set('to', utc(to) + day);
    if (cursor) { p.set('after_t', cursor.t); p.set('after_p', cursor.path); }
    more.disabled = true;
    try {
      const r = await K.api(A + p);
      if (!cursor) first(r);
      const f = r.items.filter(it => it.media), ph = r.items.filter(it => pic(it) && !it.media), d = r.items.filter(it => !pic(it) && !it.media);
      d.forEach(it => { docList.push(it); docs.append(row(it, docList)); }); G.docs.add(d.length);
      tiles(grid, picList, ph); G.pics.add(ph.length);
      f.forEach(it => { filmList.push(it); films.append(row(it, filmList)); }); G.films.add(f.length);
      cursor = r.next; more.hidden = !r.next;
    } catch (e) { says.replaceChildren(h('p', { class: 'err', role: 'alert' }, e.message)); }
    more.disabled = false;
  };
  // Première page : ce que la station a compris, les meilleurs, ou les « peut-être ».
  const LABEL = { person: 'personne reconnue', place: 'lieu', year: 'année', month: 'mois' };
  const first = r => {
    const u = (r.understood || []).map(x => h('span', { class: 'pill' }, `${x.text} · ${x.as.map(a => t(LABEL[a])).join(t(' ou '))}`));
    if (u.length) top.append(h('p', { class: 'fd-chips muted' }, t('Compris :'), u));
    if (r.best?.length) top.append(h('section', { class: 'card wide fd-best' }, h('h3', {}, t('Les plus proches')), mixed(r.best)));
    if (!r.items.length) {
      says.replaceChildren(h('p', {}, q ? t('Rien de sûr pour « {0} ».', q) : t('Aucun fichier dans cette période.')),
        ...(r.maybe?.length ? [h('section', { class: 'card wide' }, h('h3', {}, t('Peut-être :')), mixed(r.maybe))] : []),
        h('p', { class: 'muted' }, t('Essayez un autre mot, moins de mots, ou retirez un filtre.')), hint);
    }
  };
  more.onclick = page;
  root.replaceChildren(h('h2', {}, t('Recherche')), refine, says, top, h('div', { class: 'grid' }, G.docs, G.pics, G.films), h('p', {}, more));
  await page();
}
