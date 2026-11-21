'use strict';
// Service worker d'Astroport : seule la COQUE de l'application (page, CSS, JS, icône) est mise en cache, pour un
// démarrage instantané et un écran qui s'ouvre hors ligne. Les données (API, WebDAV, liens magiques, santé,
// WebSocket) ne passent JAMAIS par le cache : elles sont privées et doivent toujours venir de la station.
const V = 'astroport-v1', SHELL = ['/', '/ui/app.css', '/ui/app.js', '/ui/icon.svg', '/ui/manifest.webmanifest'];
self.addEventListener('install', e => e.waitUntil(caches.open(V).then(c => Promise.all(SHELL.map(u => fetch(new Request(u, { cache: 'reload', headers: { Accept: 'text/html,*/*' } })).then(r => (r.ok ? c.put(u, r) : 0))))).then(() => self.skipWaiting())));
self.addEventListener('activate', e => e.waitUntil(caches.keys().then(ks => Promise.all(ks.filter(k => k !== V).map(k => caches.delete(k)))).then(() => self.clients.claim())));
self.addEventListener('fetch', e => {
  const r = e.request, u = new URL(r.url), page = r.mode === 'navigate' && u.pathname === '/';
  if (r.method !== 'GET' || u.origin !== location.origin || !(page || (u.pathname.startsWith('/ui/') && u.pathname !== '/ui/sw.js'))) return;
  const key = page ? '/' : u.pathname;
  // Cache d'abord (démarrage immédiat), réseau en parallèle pour rafraîchir la copie.
  e.respondWith(caches.open(V).then(async c => {
    const hit = await c.match(key, { ignoreVary: true });
    const net = fetch(r).then(res => { if (res.ok) c.put(key, res.clone()); return res; }).catch(() => null);
    return hit || (await net) || new Response('Hors ligne : la station est injoignable.', { status: 503, headers: { 'Content-Type': 'text/plain; charset=utf-8' } });
  }));
});
// Réveil du chat (Web Push sans contenu) : la station ne dit rien du message au service de push ; on affiche « Nouveau message »,
// sauf si l'application est déjà ouverte et visible. Un clic ouvre (ou ramène) les conversations.
const NEW = { fr: 'Nouveau message', en: 'New message', es: 'Nuevo mensaje' }, OPEN = { fr: 'Touchez pour lire', en: 'Tap to read', es: 'Toque para leer' };
self.addEventListener('push', e => e.waitUntil(self.clients.matchAll({ type: 'window', includeUncontrolled: true }).then(cs => {
  if (cs.some(c => c.visibilityState === 'visible')) return;
  const lg = (self.navigator.language || 'fr').slice(0, 2), l = NEW[lg] ? lg : 'fr';
  return self.registration.showNotification(NEW[l], { body: OPEN[l], tag: 'chat', renotify: true, icon: '/ui/icon.svg', badge: '/ui/icon.svg', data: { url: '/#/chat' } });
})));
self.addEventListener('notificationclick', e => {
  e.notification.close();
  e.waitUntil(self.clients.matchAll({ type: 'window', includeUncontrolled: true }).then(cs => {
    const c = cs.find(x => new URL(x.url).origin === location.origin);
    return c ? c.focus().then(w => w.navigate?.('/#/chat')) : self.clients.openWindow('/#/chat');
  }));
});
