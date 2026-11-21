'use strict';
// Balayage tactile (comme Telegram) : l'élément suit le doigt à l'horizontale ; au-delà du seuil, l'action part au relâcher.
// Un geste surtout vertical reste un défilement. `edge` : le geste ne compte que s'il part du bord gauche (retour).
export function swipe(el, { right, left, threshold = 64, edge = 0 }) {
  let x0 = 0, y0 = 0, dx = 0, on = false, dir = 0;
  const reset = () => { on = false; dx = 0; el.style.transform = ''; el.classList.remove('ch-swiping'); };
  el.addEventListener('touchstart', e => {
    const p = e.touches[0];
    if (e.touches.length > 1 || (edge && p.clientX > edge)) return;
    x0 = p.clientX; y0 = p.clientY; dx = 0; dir = 0; on = true;
  }, { passive: true });
  el.addEventListener('touchmove', e => {
    if (!on) return;
    const p = e.touches[0], mx = p.clientX - x0, my = p.clientY - y0;
    if (!dir) { if (Math.abs(mx) < 10 && Math.abs(my) < 10) return; dir = Math.abs(mx) > Math.abs(my) ? 1 : -1; }
    if (dir < 0 || (mx > 0 && !right) || (mx < 0 && !left)) return;
    dx = Math.max(-120, Math.min(120, mx));
    if (!edge) el.style.transform = `translateX(${dx}px)`;
    el.classList.toggle('ch-swiping', Math.abs(dx) >= threshold);
  }, { passive: true });
  el.addEventListener('touchend', () => {
    if (!on) return;
    const d = dx; reset();
    if (d >= threshold) right?.(); else if (d <= -threshold) left?.();
  });
  el.addEventListener('touchcancel', reset);
}
