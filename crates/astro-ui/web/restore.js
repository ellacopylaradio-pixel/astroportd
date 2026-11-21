'use strict';
// Rétablir son MULTIPASS sur cette station depuis deux parts de la clé de secours (QR ou texte), ou l'ancienne carte complète. Elles restent dans cette page : elles ne sont
// envoyées qu'à cette station (POST /api/v1/restore), jamais stockées, et le champ est vidé dès la réponse.
const $ = s => document.querySelector(s);
const lang = (() => { const l = (navigator.language || 'fr').slice(0, 2); return ['fr', 'en', 'es'].includes(l) ? l : 'en'; })();
const T = {
  en: { skip: 'Skip to content', title: 'Restore my account here', label: 'Your rescue card (and, if your friend reads it to you, their share: one per line)', ask: 'Alert my guardians',
    manual: 'No answer? Your friend can also read you their share (My account › My security › “Show their rescue share”): paste it under your card, one per line.',
    p1: 'Scan or paste your printed rescue card first.', searching: 'Looking for your contacts on the network… (a few minutes)', waiting: 'Call the friend who keeps your backup: as soon as they confirm, your account comes back here by itself. Keep this page open (or come back with your card within 24 h).', asked: 'alerted', offline: 'station unreachable since {0}', confirmed: 'confirmed', refused: 'did not recognise the request', anon: 'A contact without a public name', note: 'Only the friends who hosted your backup keep a share: other contacts see nothing and will not answer.', nobody: 'Nobody could be found to alert: this station sees no contact of this account on the network, so no guardian can give your share back. The only other way: if your old station still works, log in there (its Captain can give you a rescue code).', ready: '{0} confirmed: restoring…', scan: 'Scan the QR with the camera', go: 'Restore my account', done: 'Your account is restored', open: 'Open my space',
    lead: 'Your old station is down, or you are moving? Scan your <strong>rescue card</strong> (the printed QR), then <strong>alert your guardians</strong>: the friend who keeps your backup gets your request, you call each other, and they confirm with one tap. Your account then comes back here by itself; your friends, contacts and files return on their own.',
    after: '<strong>Write down your new PASS code</strong>: the old one cannot be recovered. Your space is usable right away. Your files come back from your friends over the next hours; if one of them is offline, the station asks again every day for 30 days and whatever comes back is added without overwriting anything. Follow it in My account › My security.',
    pass: 'New PASS code', email: 'Login (email)', token: 'Web token', busy: 'Restoring…', cam: 'Camera unavailable: paste the codes instead.', down: 'Station unreachable.', fail: 'Restoration failed.' },
  es: { skip: 'Ir al contenido', title: 'Restablecer mi cuenta aquí', label: 'Su tarjeta de rescate (y, si su amigo se la lee, su parte: una por línea)', ask: 'Avisar a mis guardianes',
    manual: '¿Sin respuesta? Su amigo también puede leerle su parte (Mi cuenta › Mi seguridad › «Mostrar su parte de rescate»): péguela bajo su tarjeta, una por línea.',
    p1: 'Primero escanee o pegue su tarjeta de rescate impresa.', searching: 'Buscando sus contactos en la red… (unos minutos)', waiting: 'Llame al amigo que guarda su copia: en cuanto confirme, su cuenta renace aquí sola. Mantenga esta página abierta (o vuelva con su tarjeta en 24 h).', asked: 'avisado', offline: 'estación inaccesible desde las {0}', confirmed: 'ha confirmado', refused: 'no reconoció la solicitud', anon: 'Un contacto sin nombre público', note: 'Solo los amigos que alojaban su copia guardan una parte: los demás contactos no ven nada y no responderán.', nobody: 'No se encontró a nadie a quien avisar: esta estación no ve ningún contacto de esta cuenta en la red, así que ningún guardián puede devolverle su parte. Única otra vía: si su antigua estación aún funciona, conéctese allí (su Capitán puede darle un código de rescate).', ready: '{0} confirmó: restableciendo…', scan: 'Escanear el QR con la cámara', go: 'Restablecer mi cuenta', done: 'Su cuenta está restablecida', open: 'Abrir mi espacio',
    lead: '¿Su antigua estación está averiada o se muda? Escanee su <strong>tarjeta de rescate</strong> (el QR impreso) y luego <strong>avise a sus guardianes</strong>: el amigo que guarda su copia recibe su solicitud, se llaman y él confirma con un gesto. Su cuenta renace aquí sola; sus amigos, contactos y archivos vuelven por sí mismos.',
    after: '<strong>Anote su nuevo código PASS</strong>: el anterior no se puede recuperar. Su espacio se puede usar enseguida. Sus archivos vuelven de sus amigos a lo largo de las horas; si uno está sin conexión, la estación se lo vuelve a pedir cada día durante 30 días y lo que vuelva se añade sin sobrescribir nada. Siga el progreso en Mi cuenta › Mi seguridad.',
    pass: 'Nuevo código PASS', email: 'Identificador (correo)', token: 'Token web', busy: 'Restableciendo…', cam: 'Cámara no disponible: pegue los códigos.', down: 'Estación inaccesible.', fail: 'Error al restablecer.' },
};
const fr = { p1: 'Scannez ou collez d’abord votre carte de secours imprimée.', searching: 'Recherche de vos contacts sur le réseau… (quelques minutes)', waiting: 'Appelez l’ami qui garde votre sauvegarde : dès qu’il confirme, votre compte renaît ici, tout seul. Gardez cette page ouverte (ou revenez avec votre carte dans les 24 h).', asked: 'prévenu', offline: 'station injoignable depuis {0}', confirmed: 'a confirmé', refused: 'n’a pas reconnu la demande', anon: 'Un contact sans nom public', note: 'Seuls les amis qui hébergeaient votre sauvegarde gardent une part : les autres contacts ne voient rien et ne répondront pas.', nobody: 'Personne n’a été trouvé à prévenir : cette station ne voit aucun contact de ce compte sur le réseau, donc aucun gardien ne peut vous rendre votre part. Seule autre voie : si votre ancienne station fonctionne encore, connectez-vous-y (son Capitaine peut vous donner un code de secours).', ready: '{0} a confirmé : rétablissement…', pass: 'Nouveau code PASS', email: 'Identifiant de connexion (email)', token: 'Jeton de connexion web', busy: 'Rétablissement…', cam: 'Caméra indisponible : collez les codes.', down: 'Station injoignable.', fail: 'Échec du rétablissement.' };
const t = (k, ...a) => a.reduce((s, v, i) => s.replace(`{${i}}`, v), (T[lang] || fr)[k] ?? fr[k]);
if (lang !== 'fr') {
  document.documentElement.lang = lang;
  // Les traductions ne portent que des <strong> : on les reconstruit en nœuds (jamais d'innerHTML).
  const rich = v => v.split(/(<strong>.*?<\/strong>)/).filter(Boolean).map(p => p.startsWith('<strong>') ? Object.assign(document.createElement('strong'), { textContent: p.slice(8, -9) }) : document.createTextNode(p));
  for (const el of document.querySelectorAll('[data-t]')) { const v = (T[lang] || {})[el.dataset.t]; if (typeof v === 'string') el.replaceChildren(...rich(v)); }
}
let result = null, stream = null, timer = 0;
const stopCam = () => { clearInterval(timer); if (stream) for (const tr of stream.getTracks()) tr.stop(); stream = null; $('#cam').hidden = true; };
if ('BarcodeDetector' in window && navigator.mediaDevices?.getUserMedia) {
  $('#scan').hidden = false;
  $('#scan').onclick = async () => {
    $('#err').textContent = '';
    try {
      const detector = new BarcodeDetector({ formats: ['qr_code'] });
      stream = await navigator.mediaDevices.getUserMedia({ video: { facingMode: 'environment' } });
      const v = $('#cam'); v.srcObject = stream; v.hidden = false; await v.play();
      timer = setInterval(async () => {
        try {
          // Chaque QR lu s'ajoute (une part par ligne) ; la caméra s'arrête quand il y a deux parts, ou une ancienne carte complète.
          for (const c of await detector.detect(v)) {
            const code = c.rawValue, box = $('#card');
            if (!/^astro-(share|secret):/.test(code) || box.value.includes(code)) continue;
            box.value = (box.value.trim() + '\n' + code).trim();
            const shares = box.value.split(/\s+/).filter(w => w.startsWith('astro-share:')).length;
            if (shares >= 2 || code.startsWith('astro-secret:')) stopCam();
            else if (p1() && !poll) ask(false); // une demande aux gardiens est peut-être déjà ouverte
          }
        } catch { /* image pas prête : on réessaie */ }
      }, 300);
    } catch { stopCam(); $('#err').textContent = t('cam'); }
  };
}
$('#f').onsubmit = async ev => {
  ev.preventDefault();
  stopCam();
  const b = $('#go'); b.disabled = true; $('#err').textContent = ''; $('#info').textContent = t('busy');
  try {
    const r = await fetch('/api/v1/restore', { method: 'POST', cache: 'no-store', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ card: $('#card').value.trim(), lang }) });
    const j = await r.json().catch(() => ({}));
    if (!r.ok) { $('#err').textContent = j.error || t('fail'); return; }
    result = j;
    clearInterval(poll); poll = 0; $('#rs').textContent = ''; $('#who').replaceChildren(); $('#note').hidden = true;
    $('#card').value = '';
    const dl = $('#out');
    dl.replaceChildren();
    for (const [k, label] of [['email', t('email')], ['pass', t('pass')], ['token', t('token')]]) {
      const dt = document.createElement('dt'), dd = document.createElement('dd');
      dt.textContent = label; dd.textContent = j[k] || '';
      dl.append(dt, dd);
    }
    $('#f').hidden = true; $('#done').hidden = false; $('#done-t').focus();
  } catch { $('#err').textContent = t('down'); } finally { b.disabled = false; $('#info').textContent = ''; }
};
// Secours par les gardiens : seule la carte imprimée (part 1) part, vers cette station seulement. La station prévient les
// contacts du compte ; la part de l'ami qui confirme arrive sur la station, jamais dans cette page. Relevé toutes les 10 s.
const p1 = () => $('#card').value.split(/\s+/).find(w => w.startsWith('astro-share:') && w.split('|')[5] === '1');
let poll = 0;
const ask = async send => {
  const card = p1();
  if (!card) { if (send) $('#err').textContent = t('p1'); return; }
  try {
    const r = await fetch('/api/v1/restore/ask', { method: 'POST', cache: 'no-store', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ card, send }) });
    const j = await r.json().catch(() => ({}));
    if (!r.ok) { if (send) $('#err').textContent = j.error || t('fail'); return; }
    if (j.status === 'none') return;
    // Les contacts prévenus, nommés par leur seul nom public, et ce que cette station en sait (rien de plus).
    const st = { asked: t('asked'), confirmed: t('confirmed'), denied: t('refused') };
    $('#who').replaceChildren(...(j.contacts || []).map(c => Object.assign(document.createElement('li'), { textContent: `${c.name || t('anon')} — ${c.state === 'offline' ? t('offline', new Date(c.since * 1000).toLocaleTimeString(lang, { hour: '2-digit', minute: '2-digit' })) : st[c.state] || ''}` })));
    $('#note').hidden = !(j.contacts || []).length;
    if (!poll) poll = setInterval(() => ask(false), 10000);
    if (j.status === 'ready') {
      clearInterval(poll); poll = 0;
      $('#rs').textContent = t('ready', j.by || '');
      $('#f').requestSubmit(); // la station joint d'elle-même la part rendue à la carte
      return;
    }
    $('#rs').textContent = { searching: t('searching'), nobody: t('nobody') }[j.status] || t('waiting');
  } catch { if (send) $('#err').textContent = t('down'); }
};
$('#ask').onclick = async () => { const b = $('#ask'); stopCam(); b.disabled = true; $('#err').textContent = ''; await ask(true); b.disabled = false; };
// Revenir avec sa carte reprend une demande déjà ouverte (24 h).
$('#card').addEventListener('input', () => { if (p1() && !poll) ask(false); });

$('#open').onclick = () => {
  if (!result) return;
  try { localStorage.setItem('basic', btoa(result.email + ':' + result.token)); } catch { /* connexion manuelle */ }
  location.href = '/';
};
