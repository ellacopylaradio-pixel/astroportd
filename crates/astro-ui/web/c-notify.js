'use strict';
// Notifications du chat : le navigateur s'abonne aux réveils de la station (Web Push, clé VAPID de la station). Le réveil ne
// porte aucun contenu ; le service worker affiche « Nouveau message ». Le bouton dit l'état : 🔔 actives, 🔕 non.
const b64 = s => { const p = '='.repeat((4 - (s.length % 4)) % 4), r = atob((s + p).replace(/-/g, '+').replace(/_/g, '/')); return Uint8Array.from(r, c => c.charCodeAt(0)); };

async function current() {
  if (!('serviceWorker' in navigator) || !('PushManager' in window) || !window.Notification) return { can: false };
  const reg = await navigator.serviceWorker.getRegistration('/');
  return { can: !!reg, reg, sub: reg && (await reg.pushManager.getSubscription()) };
}

export default async function bell(K) {
  const { h, t } = K;
  const btn = h('button', { type: 'button', class: 'ghost ch-tool ch-bell' });
  const paint = st => {
    const on = !!st.sub && Notification.permission === 'granted';
    btn.textContent = on ? '🔔' : '🔕';
    btn.title = !st.can ? t('Notifications indisponibles dans ce navigateur') : on ? t('Notifications actives : touchez pour les couper') : t('Être prévenu des nouveaux messages, même application fermée');
    btn.setAttribute('aria-label', btn.title); btn.setAttribute('aria-pressed', String(on)); btn.disabled = !st.can;
  };
  let st = await current().catch(() => ({ can: false }));
  paint(st);
  btn.onclick = async () => {
    btn.disabled = true;
    try {
      if (st.sub) {
        await K.api('/api/v1/push/subscribe', { method: 'POST', body: { endpoint: st.sub.endpoint, on: false } }).catch(() => {});
        await st.sub.unsubscribe(); K.toast(t('Notifications coupées'), 1);
      } else {
        if ((await Notification.requestPermission()) !== 'granted') { K.toast(t('Notifications refusées : autorisez-les dans le navigateur')); return; }
        const { key } = await K.api('/api/v1/push/key');
        const sub = await st.reg.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: b64(key) });
        await K.api('/api/v1/push/subscribe', { method: 'POST', body: { endpoint: sub.endpoint, on: true } });
        K.toast(t('Notifications activées'), 1);
      }
    } catch (e) { K.toast(e.message); } finally { st = await current().catch(() => ({ can: false })); paint(st); }
  };
  return btn;
}
