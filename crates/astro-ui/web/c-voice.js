'use strict';
// Messages vocaux « du tac au tac » : pendant l'écoute d'un vocal reçu, appuyer sur 🎤 met la lecture en pause à l'instant
// exact, enregistre la réponse, l'envoie en citant ce vocal à cet instant, puis la lecture reprend là où elle s'était arrêtée.
// Chaque réponse découpe le vocal d'origine : sous lui, un tronçon par réponse (« ▶ 0:00 », « ▶ 0:42 ↩ Vous »…), qu'un
// toucher rejoue seul, de son début jusqu'à la réponse suivante.

let playing = null; // { a, m } : le vocal en cours d'écoute
// Le point d'intervention est l'instant exact de la pause : ce que l'on entend au moment où l'on répond.
export const mark = a => Math.round(a.currentTime * 1000);
export const mmss = ms => { const s = Math.floor((ms || 0) / 1000); return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`; };

// Suit un lecteur : un seul vocal joue à la fois ; un tronçon s'arrête de lui-même à sa fin.
export function track(a, m) {
  a.addEventListener('play', () => { if (playing && playing.a !== a) playing.a.pause(); playing = { a, m }; });
  a.addEventListener('timeupdate', () => { if (a._stopAt && a.currentTime * 1000 >= a._stopAt) { a._stopAt = 0; a.pause(); } });
}

// Le vocal en cours est mis en pause pour y répondre : rend `{ m, at, a }` (instant en ms), sinon null.
export function pauseForReply() {
  if (!playing || playing.a.paused) return null;
  playing.a.pause();
  return { m: playing.m, at: mark(playing.a), a: playing.a };
}

// Rejoue le vocal du message `id` à partir de `from` ms (jusqu'à `to` ms si donné).
export function play(id, from, to) {
  const a = document.querySelector(`#m-${CSS.escape(id)} audio`);
  if (!a) return false;
  const go = () => { a.currentTime = (from || 0) / 1000; a._stopAt = to || 0; a.play().catch(() => {}); };
  if (a.readyState >= 1) go(); else a.addEventListener('loadedmetadata', go, { once: true });
  return true;
}

// Tronçons d'un vocal découpé par les réponses (null s'il n'y en a pas).
export function cuts(K, m, msgs, who) {
  const replies = msgs.filter(x => x.reply === m.id && x.reply_at != null).sort((x, y) => x.reply_at - y.reply_at);
  if (!replies.length) return null;
  const { h, t } = K, starts = [0, ...replies.map(r => r.reply_at)];
  return h('div', { class: 'ch-cuts', role: 'group', 'aria-label': t('Tronçons du message vocal') }, ...starts.map((s, i) => {
    const r = replies[i - 1];
    return h('button', { type: 'button', class: 'chip', title: r ? t('Réponse de {0} à {1}', who(r), mmss(s)) : t('Début'), onclick: ev => { ev.stopPropagation(); play(m.id, s, starts[i + 1]); } },
      '▶ ', mmss(s), r ? ` ↩ ${who(r)}` : '');
  }));
}

// Le vocal en cours d'écoute (pour le reprendre après un rafraîchissement du fil), sinon null.
export function snapshot() {
  return playing && !playing.a.paused ? { id: playing.m.id, at: Math.round(playing.a.currentTime * 1000) } : null;
}
