'use strict';
// Favoris : les liens rangés en privé, un fichier `.url` (raccourci Internet standard) par lien dans /Favoris du uCloud. Chiffrés comme
// le reste, ouverts d'un double-clic sur un ordinateur (WebDAV), reçus depuis « Partager » du téléphone (Mon blog › Ranger en privé).
const DIR = '/Favoris';

export default async function favs(root, K) {
  const { h, t } = K, dav = () => K.davRoot();
  const css = h('link', { rel: 'stylesheet', href: '/ui/m-write.css' }); document.head.append(css); K.cleanup(() => css.remove());
  const url = h('input', { type: 'url', placeholder: 'https://…', 'aria-label': t('Adresse du lien'), required: true });
  const title = h('input', { placeholder: t('Nom (facultatif)'), 'aria-label': t('Nom'), maxlength: 80 });
  const find = h('input', { type: 'search', placeholder: t('Chercher dans mes favoris'), 'aria-label': t('Chercher dans mes favoris') });
  const list = h('ul', { class: 'wr-list wr-favs' }), count = h('small', { class: 'muted' });
  const clean = s => s.replace(/[\\/:*?"<>|]+/g, ' ').trim().slice(0, 80);
  let all = [];

  const draw = () => {
    const q = find.value.trim().toLowerCase(), shown = all.filter(f => !q || (f.name + ' ' + f.url).toLowerCase().includes(q));
    count.textContent = t('{0} favori(s)', all.length);
    list.replaceChildren(...(shown.length ? shown.map(f => {
      let host = ''; try { host = new URL(f.url).hostname.replace(/^www\./, ''); } catch { /* lien abîmé */ }
      return h('li', {}, h('span', { class: 'wr-noimg', 'aria-hidden': 'true' }, (host[0] || '🔖').toUpperCase()),
        h('span', { class: 't' }, h('a', { href: f.url, target: '_blank', rel: 'noopener noreferrer' }, h('b', {}, f.name)), h('small', { class: 'muted' }, host, f.mtime ? ' · ' + K.when(f.mtime) : '')),
        h('span', { class: 'wr-acts' },
          h('button', { type: 'button', class: 'ghost', onclick: () => navigator.clipboard?.writeText(f.url).then(() => K.toast(t('Lien copié'), 1)) }, t('Copier')),
          K.sure(t('Retirer'), async () => { const r = await K.req(dav() + K.enc(f.path), { method: 'DELETE' }); if (r.ok) { all = all.filter(x => x !== f); draw(); } else K.toast(t('Erreur {0}', r.status)); }, t('Retirer ce favori ?'), { class: 'ghost danger' })));
    }) : [h('li', { class: 'muted' }, all.length ? t('Aucun favori ne correspond.') : t('Aucun favori. Ajoutez un lien ci-dessus, ou depuis « Partager » de votre téléphone.'))]));
  };
  const load = async () => {
    let r;
    try { r = await K.api('/api/v1/ucloud/list?path=' + encodeURIComponent(DIR)); } catch { all = []; return draw(); }
    const files = r.entries.filter(e => e.type === 'file' && /\.url$/i.test(e.name)).slice(0, 500);
    all = (await Promise.all(files.map(async e => {
      try { const body = await (await K.req(dav() + K.enc(e.path))).text(); const u = body.match(/^URL=(.+)$/m)?.[1]?.trim(); return u && /^https?:\/\//i.test(u) ? { name: e.name.replace(/\.url$/i, ''), url: u, path: e.path, mtime: e.mtime } : null; } catch { return null; }
    }))).filter(Boolean).sort((a, b) => (b.mtime || 0) - (a.mtime || 0));
    draw();
  };
  const add = h('form', { class: 'wr-bar', onsubmit: async ev => {
    ev.preventDefault();
    let u; try { u = new URL(url.value.trim()); } catch { return K.toast(t('Adresse invalide')); }
    if (!/^https?:$/.test(u.protocol)) return K.toast(t('Adresse invalide'));
    const name = clean(title.value) || clean(u.hostname + u.pathname.replace(/\/$/, '')) || 'lien';
    await K.req(dav() + K.enc(DIR), { method: 'MKCOL' });
    // PUT brut (K.req encode en JSON) : même authentification que l'application.
    const target = new URL(dav() + K.enc(`${DIR}/${name}.url`), location.origin).href, auth = await K.auth('PUT', target);
    const r = await fetch(target, { method: 'PUT', body: `[InternetShortcut]\r\nURL=${u.href}\r\n`, headers: { ...(auth && { Authorization: auth }), 'Content-Type': 'application/internet-shortcut' } });
    if (!r.ok) return K.toast(t('Erreur {0}', r.status));
    url.value = ''; title.value = ''; K.toast(t('Favori ajouté'), 1); load();
  } }, url, title, h('button', {}, '＋ ', t('Ajouter')));
  root.replaceChildren(h('div', { class: 'grid' }, h('section', { class: 'card wide' },
    h('div', { class: 'wr-bar' }, h('h2', {}, '🔖 ', t('Mes favoris')), count, h('span', { class: 'sp' })),
    h('p', { class: 'muted' }, t('Privés et chiffrés dans le dossier Favoris de votre uCloud.')), add, find, list)));
  find.oninput = draw;
  load();
}
