'use strict';
// « Je viens de Google ou d'Apple » : déménager une archive Google Takeout ou un export iCloud, en un geste. Tout se lit ICI, dans
// le navigateur : l'archive (zip, zip64 au-delà de 4 Go, en plusieurs parties) n'est jamais envoyée d'un bloc ; chaque photo est
// décompressée, son empreinte comparée à ce que le compte possède déjà (doublons évités avant l'envoi), puis envoyée chiffrée.
// Les dates, lieux et descriptions de l'archive (.json de Google, Photo Details.csv d'iCloud) sont appliqués ensuite ; les albums
// deviennent des dossiers, les contacts passent par l'import du carnet, l'agenda est découpé en un objet CalDAV par événement.
const PHOTO = /\.(jpe?g|png|webp|heic|heif|gif|tiff?|dng|avif)$/i, VIDEO = /\.(mp4|mov|m4v|3gp|avi|mkv|webm)$/i;
const YEAR = /^(Photos from|Photos de|Fotos de|Fotos del?|Fotos aus|Foto del) (\d{4})$/i, SKIP = /(^|\/)(archive_browser\.html|print-subscriptions\.json|shared_album_comments\.json|user-generated-memory-titles\.json)$/i;
// Corbeilles : Google (Trash / Corbeille / Papelera) et Apple (Recently Deleted / Supprimés récemment) ne déménagent pas.
const TRASH = /(^|\/)(Trash|Corbeille|Papelera|Bin|Recently Deleted|Supprimés récemment|Eliminado recientemente)\//i;
// Fichier d'informations de Google : `<titre>.json`, `<titre>.supplemental-metadata.json`, tronqué à 51 caractères
// (`.supplemental-metad.json`, `.suppl.json`…), et pour une photo en double `<titre>(1).json` ou `….supplemental-metadata(1).json`.
const SIDE = /^(.*?)(?:\.suppl[a-z-]*)?(\(\d+\))?\.json$/i;
const KEEP = 'astro-move-done';

// ── Lecture d'un zip par morceaux (table centrale, zip64) ──
async function bytes(file, at, n) { return new Uint8Array(await file.slice(at, at + n).arrayBuffer()); }
const u16 = (b, i) => b[i] | (b[i + 1] << 8), u32 = (b, i) => (b[i] | (b[i + 1] << 8) | (b[i + 2] << 16) | (b[i + 3] << 24)) >>> 0;
const u64 = (b, i) => u32(b, i) + u32(b, i + 4) * 2 ** 32;
async function entries(file) {
  const tail = await bytes(file, Math.max(0, file.size - 65_557), 65_557);
  let e = -1;
  for (let i = tail.length - 22; i >= 0; i--) if (u32(tail, i) === 0x06054b50) { e = i; break; }
  if (e < 0) throw new Error('pas une archive zip');
  let count = u16(tail, e + 10), size = u32(tail, e + 12), off = u32(tail, e + 16);
  if (count === 0xffff || size === 0xffffffff || off === 0xffffffff) {
    const loc = e - 20;
    if (loc < 0 || u32(tail, loc) !== 0x07064b50) throw new Error('archive zip64 incomplète');
    const z = await bytes(file, u64(tail, loc + 8), 56);
    count = u64(z, 32); size = u64(z, 40); off = u64(z, 48);
  }
  const cd = await bytes(file, off, size), dec = new TextDecoder(), list = [];
  for (let p = 0, k = 0; k < count && p + 46 <= cd.length; k++) {
    if (u32(cd, p) !== 0x02014b50) break;
    const method = u16(cd, p + 10), nl = u16(cd, p + 28), xl = u16(cd, p + 30), cl = u16(cd, p + 32);
    let csize = u32(cd, p + 20), usize = u32(cd, p + 24), local = u32(cd, p + 42);
    for (let x = p + 46 + nl; x < p + 46 + nl + xl;) {
      const id = u16(cd, x), len = u16(cd, x + 2);
      if (id === 1) { let q = x + 4; if (usize === 0xffffffff) { usize = u64(cd, q); q += 8; } if (csize === 0xffffffff) { csize = u64(cd, q); q += 8; } if (local === 0xffffffff) local = u64(cd, q); }
      x += 4 + len;
    }
    const name = dec.decode(cd.subarray(p + 46, p + 46 + nl));
    if (!name.endsWith('/')) list.push({ file, name, method, csize, usize, local });
    p += 46 + nl + xl + cl;
  }
  return list;
}
async function read(en) {
  const h = await bytes(en.file, en.local, 30), start = en.local + 30 + u16(h, 26) + u16(h, 28);
  const raw = en.file.slice(start, start + en.csize);
  if (en.method === 0) return new Uint8Array(await raw.arrayBuffer());
  if (en.method !== 8) throw new Error('compression non gérée');
  return new Uint8Array(await new Response(raw.stream().pipeThrough(new DecompressionStream('deflate-raw'))).arrayBuffer());
}
const text = async en => new TextDecoder().decode(await read(en));
const hex = b => [...new Uint8Array(b)].map(x => x.toString(16).padStart(2, '0')).join('');

// Heure UTC de l'archive → heure murale (la station range les photos à l'heure de l'appareil photo, comme l'EXIF).
const wall = utc => utc - new Date(utc * 1000).getTimezoneOffset() * 60;
// iCloud : « Monday March 2,2020 3:45 PM GMT ».
const icloudDate = s => { const t = Date.parse(String(s).replace(/^\w+day\s+/i, '').replace(/,(\d{4})/, ', $1')); return Number.isFinite(t) ? Math.floor(t / 1000) : null; };
function csv(t) {
  const rows = []; let row = [], f = '', q = false;
  for (let i = 0; i < t.length; i++) {
    const c = t[i];
    if (q) { if (c === '"' && t[i + 1] === '"') { f += '"'; i++; } else if (c === '"') q = false; else f += c; }
    else if (c === '"') q = true; else if (c === ',') { row.push(f); f = ''; } else if (c === '\n' || c === '\r') { if (c === '\r' && t[i + 1] === '\n') i++; row.push(f); rows.push(row); row = []; f = ''; } else f += c;
  }
  if (f || row.length) { row.push(f); rows.push(row); }
  const [head, ...body] = rows; return body.filter(r => r.some(x => x.trim())).map(r => Object.fromEntries(head.map((k, i) => [k.trim(), r[i]])));
}
// Un .ics de Google (des milliers d'événements) → un objet par UID, chacun avec les fuseaux qu'il cite (exigence CalDAV).
function splitIcs(t) {
  const lines = t.replace(/\r?\n[ \t]/g, '\u0000').split(/\r?\n/), zones = [], byUid = new Map();
  let cur = null, depth = 0, kind = '';
  for (const l of lines) {
    const u = l.toUpperCase();
    if (depth === 0 && /^BEGIN:(VEVENT|VTODO|VTIMEZONE)$/.test(u)) { cur = [l]; kind = u.slice(6); depth = 1; continue; }
    if (cur) { cur.push(l); if (u.startsWith('BEGIN:')) depth++; else if (u.startsWith('END:')) depth--; if (depth === 0) { if (kind === 'VTIMEZONE') zones.push(cur); else { const uid = (cur.find(x => /^UID[:;]/i.test(x)) || '').split(':').slice(1).join(':').trim() || crypto.randomUUID(); if (!byUid.has(uid)) byUid.set(uid, []); byUid.get(uid).push(cur); } cur = null; } }
  }
  const out = l => l.join('\r\n').replace(/\u0000/g, '\r\n ');
  return [...byUid].map(([uid, comps]) => ({ uid, ics: ['BEGIN:VCALENDAR', 'VERSION:2.0', 'PRODID:-//Astroport//Demenagement//FR', ...zones.map(out), ...comps.map(out), 'END:VCALENDAR', ''].join('\r\n') }));
}

export default async function move(root, K) {
  const { h, t } = K;
  const css = h('link', { rel: 'stylesheet', href: '/ui/m-move.css' }); document.head.append(css); K.cleanup(() => css.remove());
  let acct = null; K.api('/api/v1/account').then(a => { acct = a; }, () => {});
  const dav = () => K.davRoot();
  const put = async (path, body, type) => {
    const url = new URL(dav() + K.enc(path), location.origin).href, a = await K.auth('PUT', url);
    const r = await fetch(url, { method: 'PUT', body, headers: { ...(a && { Authorization: a }), 'Content-Type': type || 'application/octet-stream' } });
    if (!r.ok && r.status !== 204) throw new Error(r.status === 507 ? t('Votre espace est plein.') : t('Erreur {0}', r.status));
  };
  const made = new Set();
  const mkdirs = async dir => { let acc = ''; for (const s of dir.split('/').filter(Boolean)) { acc += '/' + s; if (!made.has(acc)) { await K.req(dav() + K.enc(acc), { method: 'MKCOL' }); made.add(acc); } } };
  const clean = s => s.replace(/[\\:*?"<>|]+/g, ' ').replace(/\s+/g, ' ').trim();

  const pick = h('input', { type: 'file', accept: '.zip,application/zip', multiple: true, hidden: true, onchange: () => scan([...pick.files]) });
  const drop = h('div', { class: 'mv-drop', tabIndex: 0, role: 'button', onclick: () => pick.click(), onkeydown: e => e.key === 'Enter' && pick.click() },
    h('b', {}, '📦 ', t('Choisir mes archives (.zip)')), h('small', {}, t('Toutes les parties d’un coup : takeout-…-001.zip, -002.zip… ou l’export iCloud. Rien n’est envoyé avant votre accord.')));
  drop.ondragover = e => { e.preventDefault(); drop.classList.add('over'); }; drop.ondragleave = () => drop.classList.remove('over');
  drop.ondrop = e => { e.preventDefault(); drop.classList.remove('over'); scan([...e.dataTransfer.files].filter(f => /\.zip$/i.test(f.name))); };
  const stage = h('div', {});
  const help = h('details', { class: 'card wide fold' }, h('summary', {}, t('Comment obtenir mon archive ?')),
    h('ol', {}, h('li', {}, h('b', {}, 'Google : '), t('ouvrez takeout.google.com, gardez Google Photos, Contacts, Agenda et Drive, choisissez « .zip » et 50 Go, puis téléchargez toutes les parties.')),
      h('li', {}, h('b', {}, 'iPhone / iCloud : '), t('sur privacy.apple.com, « Obtenir une copie de vos données », cochez Photos iCloud, Contacts et Calendriers.')),
      h('li', {}, t('Sur un ordinateur, c’est plus rapide : l’archive reste chez vous, la station ne reçoit que ce qui lui manque.'))));
  root.replaceChildren(h('div', { class: 'grid' },
    h('section', { class: 'card wide mv-hero' }, h('h2', {}, '🚚 ', t('Je viens de Google ou d’Apple')),
      h('p', {}, t('Photos avec leurs dates, lieux et albums, contacts, agenda et documents : tout arrive chiffré dans votre uCloud. Les doublons sont évités, les visages rangés ensuite.')), drop, pick),
    help, h('section', { class: 'card wide', hidden: true }, stage)));
  const box = stage.parentElement;

  // ── Analyse : la table de chaque archive, les métadonnées (petites) lues d'abord, de toutes les parties ──
  async function scan(files) {
    if (!files.length) return;
    box.hidden = false; stage.replaceChildren(h('p', { class: 'muted' }, h('span', { class: 'mv-spin' }), t('Lecture des archives…')));
    box.scrollIntoView({ behavior: 'smooth', block: 'start' });
    let all = [];
    try { for (const f of files) all = all.concat(await entries(f)); } catch (e) { return stage.replaceChildren(h('p', { class: 'err', role: 'alert' }, t('Archive illisible : {0}', e.message))); }
    all = all.filter(e => !SKIP.test(e.name) && !/(^|\/)\._/.test(e.name));
    const google = all.some(e => /(^|\/)Takeout\//.test(e.name)), src = google ? 'Google' : 'iCloud';
    all = all.filter(e => !TRASH.test(e.name));
    const meta = new Map(), icloud = new Map(), albums = new Map(), titles = new Map();
    for (const e of all) {
      if (/\.json$/i.test(e.name) && e.usize < 200_000 && /Photos|Fotos/i.test(e.name)) {
        try {
          const j = JSON.parse(await text(e)), dir = e.name.replace(/[^/]+$/, ''), file = e.name.split('/').pop();
          if (file === 'metadata.json' || file === 'métadonnées.json') { if (j.title) titles.set(dir, j.title); continue; } // album : son vrai nom
          if (!j.title) continue;
          // `IMG.jpg(1).json` décrit `IMG(1).jpg` : le rang de doublon passe avant l'extension.
          const dup = file.match(SIDE)?.[2] || '', name = dup ? j.title.replace(/(\.[^.]+)$/, dup + '$1') : j.title;
          meta.set(dir + name, j);
        } catch { /* .json d'autre nature */ }
      } else if (/Photo Details.*\.csv$/i.test(e.name)) {
        for (const r of csv(await text(e))) icloud.set(r.imgName, r);
      } else if (/(^|\/)Albums\/[^/]+\.csv$/i.test(e.name)) {
        const name = e.name.split('/').pop().replace(/\.csv$/i, '');
        for (const r of csv(await text(e))) albums.set(r.Images || r.imgName || Object.values(r)[0], name);
      }
    }
    const photos = [], contacts = [], cals = [], drive = [];
    for (const e of all) {
      const parts = e.name.split('/'), base = parts.pop(), dir = parts[parts.length - 1] || '';
      if (PHOTO.test(base) || VIDEO.test(base)) {
        if (google ? !/Google Photos|Google Fotos/i.test(e.name) : /(^|\/)iCloud Drive\//i.test(e.name)) { drive.push(e); continue; }
        const y = dir.match(YEAR), album = google ? (y ? null : titles.get(e.name.replace(/[^/]+$/, '')) || dir) : albums.get(base) || null;
        e.dest = `/Photos/${src}/${clean(album || (y ? y[2] : t('Photos')))}`;
        e.album = !!album; e.base = base;
        const m = meta.get(e.name) || meta.get(e.name.replace(/\(\d+\)(\.\w+)$/, '$1')) || meta.get(e.name.replace(/-(edited|modifié)(\.\w+)$/i, '$2'));
        const ic = icloud.get(base);
        const people = (m?.people || []).map(x => x.name).filter(Boolean);
        e.meta = m ? { taken_at: (m.photoTakenTime || m.creationTime)?.timestamp ? wall(Number((m.photoTakenTime || m.creationTime).timestamp)) : null, lat: m.geoData?.latitude || m.geoDataExif?.latitude || null, lon: m.geoData?.longitude || m.geoDataExif?.longitude || null,
          desc: [m.description, people.length ? t('Avec {0}', people.join(', ')) : ''].filter(Boolean).join(' · ') || null }
          : ic ? { taken_at: icloudDate(ic.originalCreationDate) && wall(icloudDate(ic.originalCreationDate)) } : null;
        photos.push(e);
      } else if (/\.vcf$/i.test(base)) contacts.push(e);
      else if (/\.ics$/i.test(base)) cals.push(e);
      else if (/\.(json|csv|html)$/i.test(base)) continue;
      else if (!google || /(^|\/)Takeout\/(Drive|Google Drive)\//i.test(e.name)) drive.push(e);
    }
    // Les albums d'abord : une photo d'album et sa copie dans « Photos from 2019 » ne font qu'une, rangée dans l'album.
    photos.sort((a, b) => b.album - a.album);
    for (const e of drive) e.dest = `/${clean(src === 'Google' ? 'Drive Google' : 'iCloud Drive')}/` + e.name.split('/').slice(google ? 2 : 1, -1).map(clean).join('/');
    plan({ src, photos, contacts, cals, drive, key: files.map(f => f.name + f.size).join('|') });
  }

  // ── Ce qui va arriver : l'accord du membre, par catégorie ──
  function plan(p) {
    const size = l => K.bytes(l.reduce((n, e) => n + e.usize, 0));
    const rows = [['photos', '📷', t('Photos et vidéos'), p.photos, t('{0} fichiers · {1}', p.photos.length, size(p.photos)) + (p.photos.some(e => e.meta) ? ' · ' + t('dates et lieux retrouvés') : '')],
      ['contacts', '📇', t('Contacts'), p.contacts, t('{0} carnet(s)', p.contacts.length)], ['cals', '📅', t('Agenda'), p.cals, t('{0} agenda(s)', p.cals.length)],
      ['drive', '🗂️', t('Documents'), p.drive, t('{0} fichiers · {1}', p.drive.length, size(p.drive))]].filter(r => r[3].length);
    if (!rows.length) return stage.replaceChildren(h('p', { class: 'err' }, t('Rien à importer dans ces archives.')));
    const boxes = Object.fromEntries(rows.map(([k]) => [k, h('input', { type: 'checkbox', checked: true })]));
    const go = h('button', { type: 'button', onclick: () => run(p, Object.fromEntries(rows.map(([k]) => [k, boxes[k].checked]))) }, '🚚 ', t('Déménager'));
    stage.replaceChildren(h('h2', {}, t('Archive {0} prête', p.src)), h('ul', { class: 'mv-plan' }, rows.map(([k, ico, name, , info]) => h('li', {}, h('label', {}, boxes[k], h('span', { class: 'mv-ico', 'aria-hidden': 'true' }, ico), h('span', { class: 't' }, h('b', {}, name), h('small', { class: 'muted' }, info)))))),
      h('p', { class: 'muted' }, t('Gardez cette page ouverte pendant le déménagement. Vous pouvez l’interrompre : il reprendra où il s’est arrêté.')), h('p', {}, go));
  }

  // ── Le déménagement ──
  async function run(p, want) {
    let lock = null; try { lock = await navigator.wakeLock?.request('screen'); } catch { /* sans verrou d'écran */ }
    let stopped = false;
    const done = (() => { try { return new Set(JSON.parse(localStorage.getItem(KEEP) || '{}')[p.key] || []); } catch { return new Set(); } })();
    const remember = () => { try { const all = JSON.parse(localStorage.getItem(KEEP) || '{}'); all[p.key] = [...done]; localStorage.setItem(KEEP, JSON.stringify(all)); } catch { /* stockage plein ou privé */ } };
    const n = { sent: 0, dup: 0, err: 0, bytes: 0, contacts: 0, events: 0 }, total = (want.photos ? p.photos.length : 0) + (want.drive ? p.drive.length : 0);
    const bar = h('progress', { max: total || 1, value: 0 }), line = h('p', { class: 'mv-now muted', 'aria-live': 'polite' }), counts = h('div', { class: 'mv-counts' }), errs = h('ul', { class: 'mv-errs' });
    const stop = h('button', { type: 'button', class: 'ghost', onclick: () => { stopped = true; stop.disabled = true; stop.textContent = t('Interruption…'); } }, '⏸ ', t('Interrompre'));
    const draw = () => { bar.value = n.sent + n.dup + n.err; counts.replaceChildren(...[['⬆', t('envoyés'), n.sent], ['♻', t('déjà là'), n.dup], ['📇', t('contacts'), n.contacts], ['📅', t('événements'), n.events], ['⚠', t('erreurs'), n.err]].filter(x => x[2] || x[0] === '⬆').map(([i, l, v]) => h('span', {}, h('b', {}, i, ' ', String(v)), ' ', l))); };
    stage.replaceChildren(h('h2', {}, '🚚 ', t('Déménagement en cours')), bar, counts, line, h('p', {}, stop), errs); draw();
    const fail = (name, e) => { n.err++; if (errs.children.length < 50) errs.append(h('li', {}, name.split('/').pop(), ' : ', e.message)); };

    if (want.contacts) for (const e of p.contacts) {
      line.textContent = t('Contacts…');
      try { const r = await K.api('/api/v1/book/import', { method: 'POST', body: { vcf: await text(e) } }); n.contacts += r.imported; } catch (x) { fail(e.name, x); }
      draw();
    }
    if (want.cals && acct) for (const e of p.cals) {
      line.textContent = t('Agenda…');
      for (const { uid, ics } of splitIcs(await text(e))) {
        if (stopped) break;
        const id = 'cal:' + uid; if (done.has(id)) { n.events++; continue; }
        try { await put(`/calendars/${acct.dav.login}/default/${uid.replace(/[^\w.@-]+/g, '_').slice(0, 120)}.ics`, new Blob([ics], { type: 'text/calendar' }), 'text/calendar; charset=utf-8'); n.events++; done.add(id); } catch (x) { fail(uid, x); }
        if (n.events % 50 === 0) { draw(); remember(); }
      }
      draw();
    }

    // Photos et documents : par lots de 24, empreintes comparées d'abord, 3 envois à la fois ; métadonnées appliquées par lots.
    const queue = [...(want.photos ? p.photos : []), ...(want.drive ? p.drive : [])].filter(e => !done.has(e.name));
    n.dup += total - queue.length; draw();
    const metas = [], flush = async () => { while (metas.length) { const items = metas.splice(0, 400); try { await K.api('/api/v1/ucloud/import-meta', { method: 'POST', body: { items } }); } catch (x) { fail(t('dates et lieux'), x); } } };
    for (let i = 0; i < queue.length && !stopped; i += 24) {
      const lot = queue.slice(i, i + 24), got = [];
      const twins = new Set(); // une même photo deux fois dans le lot (album et « Photos from ») : une seule part
      for (const e of lot) {
        try { const b = await read(e), sha = hex(await crypto.subtle.digest('SHA-256', b)); if (twins.has(sha)) { n.dup++; done.add(e.name); continue; } twins.add(sha); got.push({ e, b, sha }); } catch (x) { fail(e.name, x); }
      }
      let have = new Map();
      try { have = new Map((await K.api('/api/v1/ucloud/have', { method: 'POST', body: { shas: got.map(g => g.sha) } })).have?.map(x => [x.sha, x.path]) || []); } catch { /* on envoie tout : le serveur dédoublonne aussi les blocs */ }
      const send = async g => {
        const e = g.e;
        if (have.has(g.sha)) { n.dup++; done.add(e.name); return; }
        line.textContent = e.name.split('/').pop();
        try {
          await mkdirs(e.dest);
          const path = `${e.dest}/${clean(e.base || e.name.split('/').pop())}`;
          await put(path, new Blob([g.b]));
          have.set(g.sha, path); n.sent++; n.bytes += g.b.length; done.add(e.name);
          if (e.meta && (e.meta.taken_at || e.meta.lat || e.meta.desc)) metas.push({ path, ...e.meta });
        } catch (x) { fail(e.name, x); if (/espace est plein/.test(x.message)) stopped = true; }
      };
      for (let k = 0; k < got.length; k += 3) await Promise.all(got.slice(k, k + 3).map(send));
      draw(); remember();
      if (metas.length >= 400) await flush();
    }
    await flush(); remember(); try { await lock?.release(); } catch { /* déjà relâché */ }
    line.textContent = '';
    stage.replaceChildren(h('h2', {}, stopped ? '⏸ ' + t('Déménagement interrompu') : '✅ ' + t('Déménagement terminé')), counts,
      h('p', {}, stopped ? t('Relancez avec les mêmes archives : ce qui est déjà arrivé ne sera pas renvoyé.') : t('{0} envoyés ({1}), {2} déjà présents évités.', n.sent, K.bytes(n.bytes), n.dup)),
      h('p', { class: 'muted' }, t('La station range maintenant les visages et lit le texte des photos : cela peut prendre quelques heures.')),
      h('div', { class: 'mv-next' }, h('a', { class: 'btn', href: '#/library' }, '📷 ', t('Voir mes photos')), h('a', { class: 'btn ghost', href: '#/people/tri?grand' }, '🙂 ', t('Ranger les visages')), h('a', { class: 'btn ghost', href: '#/people' }, '📇 ', t('Mon carnet'))),
      h('p', { class: 'muted' }, t('Astroport est en bêta : une archive mal lue, un oubli ?'), ' ', h('button', { type: 'button', class: 'ghost', onclick: () => K.mod('m-beta.js').then(m => m.ask(K, { category: 'bug', title: t('Déménagement {0} : ', p.src) })) }, '🐞 ', t('Dites-le-nous'))), errs);
  }
}
