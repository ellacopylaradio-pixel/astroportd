'use strict';
// Lecteur de QR partagé par /scan (Ğ1Billets) et « Payer » (m-pay.js) : BarcodeDetector, sinon jsQR chargé à la demande.
let jsqr = null;
const loadJsqr = () => window.jsQR ? Promise.resolve() : (jsqr ||= new Promise((ok, ko) => { const s = document.createElement('script'); s.src = '/ui/jsqr.min.js'; s.onload = ok; s.onerror = () => ko(new Error('lecteur QR indisponible')); document.head.append(s); }));

// Rend `async (vidéo | image) => texte | null`.
export async function makeReader() {
  if ('BarcodeDetector' in window) {
    try {
      if (!BarcodeDetector.getSupportedFormats || (await BarcodeDetector.getSupportedFormats()).includes('qr_code')) {
        const d = new BarcodeDetector({ formats: ['qr_code'] });
        return async src => (await d.detect(src))[0]?.rawValue || null;
      }
    } catch { /* repli sur jsQR */ }
  }
  await loadJsqr();
  const c = document.createElement('canvas'), x = c.getContext('2d', { willReadFrequently: true });
  return async src => {
    const w = src.videoWidth || src.width, h = src.videoHeight || src.height;
    if (!w || !h) return null;
    const k = Math.min(1, 720 / Math.max(w, h)); c.width = Math.round(w * k); c.height = Math.round(h * k);
    x.drawImage(src, 0, 0, c.width, c.height);
    const d = x.getImageData(0, 0, c.width, c.height);
    return window.jsQR(d.data, d.width, d.height, { inversionAttempts: 'attemptBoth' })?.data || null;
  };
}

// Filme (caméra arrière) dans `video`, appelle `onText` à chaque QR lu ; rend `stop()`. Caméra refusée : NotAllowedError.
export async function watch(video, onText) {
  if (!navigator.mediaDevices?.getUserMedia) throw new Error('pas de caméra');
  const stream = await navigator.mediaDevices.getUserMedia({ video: { facingMode: { ideal: 'environment' }, width: { ideal: 1280 } }, audio: false });
  let on = true, timer = 0;
  const stop = () => { on = false; clearTimeout(timer); stream.getTracks().forEach(t => t.stop()); video.srcObject = null; };
  try {
    video.srcObject = stream; video.muted = video.playsInline = true; await video.play();
    const read = await makeReader();
    const loop = async () => {
      try { const s = on && await read(video); if (s && on) onText(s); } catch { /* image sans QR */ }
      if (on) timer = setTimeout(loop, 160);
    };
    timer = setTimeout(loop); // première lecture après le retour de `stop` à l'appelant
  } catch (e) { stop(); throw e; }
  return stop;
}
