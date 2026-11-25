'use strict';
// Plier les sections, pour toute l'interface : le titre (h2/h3) de chaque carte `section.card` devient un bouton qui la plie
// ou la déplie, et un <details> garde l'état que lui donne le membre. Ce qu'il choisit est retenu par écran et par titre
// (localStorage, s'il est disponible) et réappliqué après chaque re-rendu (les écrans redessinent souvent tout) grâce à un
// MutationObserver sur <main>. Par défaut, rien ne change : tout est déplié, sauf ce que l'écran replie déjà lui-même.
// Jamais dans un dialogue, jamais une bulle d'aide « ? », jamais une carte sans titre ou sans rien d'autre à cacher.
const STORE = 'folds', MAX = 400;
const read = () => { try { return JSON.parse(localStorage.getItem(STORE) || '{}') || {}; } catch { return {}; } };
const write = m => { try { const k = Object.keys(m); if (k.length > MAX) for (const x of k.slice(0, k.length - MAX)) delete m[x]; localStorage.setItem(STORE, JSON.stringify(m)); } catch { /* stockage indisponible : le pli vaut pour la page */ } };

// Clé : l'écran (sans ses paramètres) et le titre, sans ses compteurs (« semaine 12 », « 3 ») ni ses précisions en <small>.
export const screen = hash => (hash || '').replace(/^#/, '').split('?')[0] || '/';
export function titleKey(el) {
  const c = el.cloneNode(true);
  c.querySelectorAll('small, .tip, .muted').forEach(x => x.remove());
  return c.textContent.replace(/\d+([.,]\d+)?/g, '#').replace(/\s+/g, ' ').trim().slice(0, 80);
}
// Titre d'une carte : son premier enfant s'il est un h2/h3, ou le h2 d'un en-tête à bulle d'aide (K.head).
export function headOf(sec) {
  const f = sec.firstElementChild;
  if (!f) return null;
  if (/^H[23]$/.test(f.tagName)) return { bar: f, h: f };
  if (f.classList.contains('ac-head') && f.querySelector(':scope > h2')) return { bar: f, h: f.querySelector(':scope > h2') };
  return null;
}

export function folds(main) {
  const seen = new WeakSet();
  const key = t => `${screen(location.hash)}|${t}`;
  const remember = (k, folded) => { const m = read(); m[k] = folded ? 1 : 0; write(m); };
  const setFolded = (sec, btn, on) => { sec.classList.toggle('folded', on); btn.setAttribute('aria-expanded', String(!on)); };
  const card = sec => {
    if (sec.closest('dialog')) return;
    const hd = headOf(sec);
    if (!hd || sec.children.length < 2 || hd.h.querySelector('a, button, input, select, textarea, details')) return;
    if (hd.h.querySelector(':scope > .fold-t')) return; // déjà un bouton (l'écran n'a pas redessiné ce titre)
    const k = key(titleKey(hd.h));
    if (k.endsWith('|')) return; // titre vide
    const btn = document.createElement('button');
    btn.type = 'button'; btn.className = 'fold-t';
    btn.append(...hd.h.childNodes);
    btn.onclick = () => { const on = !sec.classList.contains('folded'); setFolded(sec, btn, on); remember(k, on); };
    hd.h.append(btn); hd.bar.classList.add('fold-h');
    setFolded(sec, btn, read()[k] === 1);
  };
  const details = d => {
    if (seen.has(d)) return;
    seen.add(d);
    const s = d.querySelector(':scope > summary');
    if (!s || d.classList.contains('tip') || d.closest('dialog')) return;
    const k = key('▸' + titleKey(s)), v = read()[k];
    if (v === 1 || v === 0) d.open = v === 0;
    // Seul un geste du membre est retenu (pas les ouvertures faites par l'écran lui-même).
    s.addEventListener('click', () => setTimeout(() => remember(k, !d.open)));
  };
  const scan = m => { m.querySelectorAll('section.card').forEach(card); m.querySelectorAll('details').forEach(details); };
  // Chaque navigation remplace <main> par un neuf (app.js `route`) : on suit le remplacement, puis chaque re-rendu dans le nouveau.
  let queued = false, cur = null, inner = null;
  const watch = m => {
    inner?.disconnect(); cur = m;
    if (!m) return;
    inner = new MutationObserver(() => { if (!queued) { queued = true; queueMicrotask(() => { queued = false; scan(m); }); } });
    inner.observe(m, { childList: true, subtree: true });
    scan(m);
  };
  const root = main.parentNode || document.body;
  new MutationObserver(() => { const m = document.getElementById('main'); if (m !== cur) watch(m); }).observe(root, { childList: true });
  watch(main);
}
