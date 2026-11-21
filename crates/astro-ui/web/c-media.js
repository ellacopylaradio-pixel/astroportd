'use strict';
// Pièces jointes du chat : un fichier est rangé dans /Chat du uCloud (chiffré, comme tout le uCloud), puis partagé avec l'ami par le
// partage existant (sa station le reçoit sous /Partages/De_…/) ; le message n'en porte que le nom, le type et la taille.
// Messages vocaux : enregistrés dans le navigateur (MediaRecorder), envoyés comme un fichier.

const stamp = () => new Date().toISOString().slice(0, 19).replace(/[-:]/g, '').replace('T', '-');
const clean = n => (n || '').replace(/[/\\\u0000-\u001f]/g, '_').slice(-120) || 'fichier';
const MAX_VOICE_S = 300;

// Range les fichiers dans /Chat puis les partage avec `peer`. Rend `[{ name, mime, size, path }]` pour le message.
export async function upload(K, files, peer) {
  const dav = K.davRoot(), put = async (method, path, body, type) => {
    const url = new URL(dav + K.enc(path), location.origin).href, auth = await K.auth(method, url);
    return fetch(url, { method, headers: { ...(auth && { Authorization: auth }), ...(type && { 'Content-Type': type }) }, body });
  };
  await put('MKCOL', '/Chat').catch(() => {}); // existe déjà : 405, sans importance
  const out = [];
  for (const f of files) {
    const name = `${stamp()}-${clean(f.name)}`, path = '/Chat/' + name, mime = f.type || 'application/octet-stream';
    const r = await put('PUT', path, f, mime);
    if (!r.ok) throw new Error(K.t('Envoi de {0} impossible ({1})', f.name, r.status));
    out.push({ name, mime, size: f.size, path });
  }
  const r = await K.api('/api/v1/ucloud/send', { method: 'POST', body: { paths: out.map(f => f.path), friends: [peer] } });
  const bad = r.results.find(x => x.error);
  if (bad) throw new Error(bad.error);
  return out;
}

// Une pièce jointe dans une bulle : image (vignette, visionneuse au clic), son (lecteur), autre (téléchargement).
// Chez le destinataire, `path` reste vide tant que sa station rapatrie le fichier.
export function file(K, f, opts = {}) {
  const { h, t } = K, label = f.name.replace(/^\d{8}-\d{6}-/, '');
  if (!f.path) return h('p', { class: 'ch-file muted' }, '⏳ ', label, ' — ', t('en cours de réception'));
  const kind = /^image\//.test(f.mime) ? 'image' : /^audio\//.test(f.mime) || /^vocal\b/.test(label) ? 'audio' : 'file';
  if (kind === 'image') {
    const img = h('img', { alt: label, loading: 'lazy' });
    K.blob(f.path, true).then(u => { img.src = u; }, () => { img.alt = '⚠️ ' + label; });
    return h('button', { type: 'button', class: 'ch-img', 'aria-label': t('Agrandir {0}', label), onclick: () => K.open([{ path: f.path, mime: f.mime }], 0) }, img);
  }
  if (kind === 'audio') {
    const a = h('audio', { controls: true, preload: 'metadata' });
    K.blob(f.path).then(u => { a.src = u; }, () => {});
    opts.audio?.(a);
    return h('div', { class: 'ch-audio' }, a, opts.extra?.(a));
  }
  const get = async () => { try { const u = await K.blob(f.path); const a = h('a', { href: u, download: label }); document.body.append(a); a.click(); a.remove(); } catch (e) { K.toast(e.message); } };
  return h('button', { type: 'button', class: 'ch-doc', onclick: get }, '📄 ', h('span', {}, label), ' ', h('small', {}, K.bytes(f.size)));
}

// Enregistre un message vocal : un clic démarre, un second arrête et envoie ; la durée s'affiche sur le bouton.
let rec = null;
export async function record(K, btn, done) {
  const { t } = K;
  if (rec) { rec.stop(); return; }
  if (!navigator.mediaDevices?.getUserMedia || !window.MediaRecorder) { K.toast(t('Ce navigateur ne sait pas enregistrer le son')); return; }
  let stream;
  try { stream = await navigator.mediaDevices.getUserMedia({ audio: true }); } catch { K.toast(t('Micro refusé : autorisez-le dans le navigateur')); return; }
  const type = ['audio/webm;codecs=opus', 'audio/ogg;codecs=opus', 'audio/mp4'].find(x => MediaRecorder.isTypeSupported(x)) || '';
  const r = new MediaRecorder(stream, type ? { mimeType: type } : {}), parts = [], t0 = Date.now(), label = btn.textContent;
  rec = r; btn.classList.add('ch-rec'); btn.setAttribute('aria-label', t('Arrêter et envoyer'));
  const tick = setInterval(() => { const s = Math.round((Date.now() - t0) / 1000); btn.textContent = `⏹ ${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`; if (s >= MAX_VOICE_S) r.stop(); }, 500);
  r.ondataavailable = e => e.data.size && parts.push(e.data);
  r.onstop = () => {
    clearInterval(tick); stream.getTracks().forEach(x => x.stop()); rec = null;
    btn.classList.remove('ch-rec'); btn.textContent = label; btn.setAttribute('aria-label', t('Message vocal'));
    const mime = (r.mimeType || type || 'audio/webm').split(';')[0], ext = mime.includes('ogg') ? 'ogg' : mime.includes('mp4') ? 'm4a' : 'webm';
    if (Date.now() - t0 > 800 && parts.length) done(new File(parts, `vocal.${ext}`, { type: mime }));
  };
  r.start(1000);
}

// Aperçu riche d'un lien : la station va chercher la page (titre, description, image) ; le navigateur ne contacte pas le site.
const seen = new Map();
export function preview(K, url) {
  const { h } = K, box = h('a', { class: 'ch-prev', href: url, target: '_blank', rel: 'noopener noreferrer', hidden: true });
  if (!seen.has(url)) seen.set(url, K.api('/api/v1/chat/preview?' + new URLSearchParams({ url })).catch(() => ({ none: true })));
  seen.get(url).then(p => {
    if (p.none || !p.title) return;
    box.replaceChildren(...(p.image ? [h('img', { src: p.image, alt: '', loading: 'lazy' })] : []),
      h('span', {}, h('small', { class: 'muted' }, p.site || ''), h('strong', {}, p.title), p.description ? h('small', {}, p.description.slice(0, 200)) : null));
    box.hidden = false;
  });
  return box;
}
