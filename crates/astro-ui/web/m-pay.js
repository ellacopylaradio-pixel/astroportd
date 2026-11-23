'use strict';
// Face à face, de téléphone à téléphone (docs/design/PAYER_QR.md). Un écran, trois vues :
//  - Recevoir (#/recevoir) : mon code de paiement, avec un montant si je veux ; l'écran dit quand l'argent arrive ;
//  - Payer (#/payer) : la caméra lit le code de l'autre ;
//  - Se rencontrer (#/rencontre) : mon code « rencontre », scanner le sien, ou chercher parmi les MULTIPASS de la
//    constellation ; les demandes reçues s'acceptent d'un geste.
// Tout code lu (ou personne choisie) ouvre la même fiche : le nom que MA station connaît (jamais un nom venu du QR), et deux
// gestes, Payer (dialogue habituel m-credit.js, île comprise) et Devenir proches.
// Anti-Sybil (Ğ1-N²) : le code « rencontre » porte une preuve de présence signée qui change toutes les 2 minutes ; l'ajout
// fait en le scannant est une rencontre PROUVÉE, l'ajout depuis une liste, un lien ou un code de paiement une NOMINATION.
import { watch } from '/ui/scan-cam.js';

const HEX = /^[0-9a-f]{64}$/, PROOF = /^\d{1,12}\.[0-9a-f]{128}$/;
const zen = c => (c / 100).toLocaleString(undefined, { minimumFractionDigits: 0, maximumFractionDigits: 2 });
const B32 = 'qpzry9x8gf2tvdw0s3jn54khce6mua7l';
// npub (bech32, NIP-19) → clé hex. La somme de contrôle n'est pas refaite : le QR corrige ses erreurs, la station refuse un inconnu.
const npubHex = s => {
  s = s.toLowerCase();
  if (!/^npub1[02-9ac-hj-np-z]{58}$/.test(s)) return null;
  let acc = 0, bits = 0; const out = [];
  for (const ch of s.slice(5, -6)) { acc = (acc << 5) | B32.indexOf(ch); bits += 5; while (bits >= 8) { bits -= 8; out.push((acc >> bits) & 255); } acc &= (1 << bits) - 1; }
  return out.length === 32 ? out.map(b => b.toString(16).padStart(2, '0')).join('') : null;
};
const amountOf = v => { const a = Number(v); return Number.isInteger(a) && a > 0 && a <= 1e6 ? a : null; };
/** Ce que dit un QR : `{ to, a, m }` (clé hex, montant en Ẑen, preuve de présence), ou null si ce n'est pas une personne. */
export function parse(text) {
  const s = String(text || '').trim(), n = s.replace(/^nostr:/i, '');
  if (/^npub1/i.test(n)) { const to = npubHex(n); return to ? { to, a: null, m: null } : null; }
  try {
    const u = new URL(s), [path, qs] = u.hash.slice(1).split('?');
    if (path !== '/payer' && path !== '/rencontre') return null;
    const q = new URLSearchParams(qs || ''), to = (q.get('to') || '').toLowerCase(), m = q.get('m') || '';
    return HEX.test(to) ? { to, a: amountOf(q.get('a')), m: path === '/rencontre' && PROOF.test(m) ? m : null } : null;
  } catch { return null; }
}
const qrImg = (K, svg, alt) => K.h('img', { alt, width: 256, height: 256, src: 'data:image/svg+xml;base64,' + btoa(svg) });

export default async function pay(root, K, qs) {
  const link = K.h('link', { rel: 'stylesheet', href: '/ui/m-pay.css' }); document.head.append(link); K.cleanup(() => link.remove());
  const me = await K.api('/api/v1/me'), hash = location.hash;
  return hash.startsWith('#/recevoir') ? receive(root, K, me) : face(root, K, qs, me, hash.startsWith('#/rencontre'));
}

async function receive(root, K, me) {
  const { h, t, api } = K;
  const qr = h('div', { class: 'pay-qr' });
  const said = h('p', { class: 'ok pay-got', role: 'status', 'aria-live': 'polite' });
  const amount = h('input', { id: 'pay-amount', type: 'number', min: 1, max: 1000000, step: 1, inputmode: 'numeric', placeholder: t('libre') });
  const draw = async () => {
    const a = amountOf(amount.value);
    let r;
    try { r = await api(`/api/v1/me/zen/receive${a ? `?amount=${a}` : ''}`); } catch (e) { qr.replaceChildren(h('p', { class: 'err', role: 'alert' }, e.message)); return; }
    qr.replaceChildren(h('p', { class: 'pay-ask' }, r.amount_zen ? t('{0} Ẑen demandés', r.amount_zen) : t('Montant au choix de la personne')),
      qrImg(K, r.qr_svg, t('Mon code pour être payé')), h('p', { class: 'pay-who' }, me.email || ''));
  };
  // Le code s'affiche tout de suite (montant libre) ; le montant précis se règle juste dessous, sur une ligne, sans changer d'écran.
  root.replaceChildren(
    h('section', { class: 'card' }, qr, said,
      h('form', { class: 'pay-form pay-row', onsubmit: ev => { ev.preventDefault(); draw(); } },
        h('label', { for: 'pay-amount' }, t('Montant précis (Ẑen)')), amount, h('button', { type: 'submit' }, t('Demander'))),
      h('p', { class: 'muted pay-help' }, t('Montrez ce code à la personne qui vous paie : elle le vise avec « Payer » dans son application.'))),
    h('p', { class: 'pay-acts' }, h('a', { class: 'ck-btn', href: '#/payer' }, '📷 ', t('Payer quelqu\'un'))));
  await draw();
  // Dire quand l'argent arrive : le solde monte, ou une promesse arrive (réseau Ğ1 coupé).
  let base, seen;
  const look = async () => {
    const [d, isle] = await Promise.all([api('/api/v1/dashboard').catch(() => null), api('/api/v1/island').catch(() => null)]);
    const z = d?.balance?.zen_cents, got = (isle?.promises || []).filter(p => !p.sent);
    if (!seen) { seen = new Set(got.map(p => p.id)); base = z; return; }
    const fresh = got.filter(p => !seen.has(p.id)); fresh.forEach(p => seen.add(p.id));
    if (fresh.length) said.textContent = '🏝️ ' + t('Promesse reçue : {0} Ẑen, réglée en Ğ1 au retour du réseau', fresh.map(p => p.amount_zen).join(' + '));
    else if (z != null && base != null && z > base) { said.textContent = '✅ ' + t('Paiement reçu : +{0} Ẑen', zen(z - base)); navigator.vibrate?.([60, 60, 60]); }
    if (z != null) base = z;
  };
  await look().catch(() => {});
  K.every(8000, () => look().catch(() => {}));
}

// Payer (meet = false) ou Se rencontrer (meet = true).
async function face(root, K, qs, me, meet) {
  const { h, t, api } = K;
  const panel = h('section', { class: 'card' });
  const hint = h('p', { class: 'pay-hint', role: 'status', 'aria-live': 'polite' });
  const video = h('video', { 'aria-label': t('Caméra'), muted: true, playsinline: true });
  const cam = h('div', { class: 'pay-cam' }, video, h('div', { class: 'frame' }));
  let stop = null, last = '', off = true, poll = 0, codeTimer = 0;
  // Choisir la personne sans la filmer (proches) : sous la caméra, ou à sa place quand il n'y en a pas.
  const pick = h('button', { type: 'button', class: 'ghost', onclick: () => chooseFriend() }, t('Choisir dans mes proches'));
  const halt = () => { off = true; stop?.(); stop = null; };
  const quiet = () => { halt(); clearInterval(poll); clearInterval(codeTimer); };
  K.cleanup(quiet);
  const again = () => h('button', { type: 'button', class: 'ghost', onclick: () => scan() }, '📷 ', t('Scanner un autre code'));

  // Mon code « rencontre » : preuve redemandée chaque minute (elle vaut quelques minutes : une photo envoyée au loin expire).
  const myCode = box => {
    const draw = () => api('/api/v1/me/meet/code').then(r => box.replaceChildren(qrImg(K, r.qr_svg, t('Mon code pour se rencontrer')), h('p', { class: 'pay-who' }, me.email || '')),
      e => box.replaceChildren(h('p', { class: 'err', role: 'alert' }, e.message)));
    clearInterval(codeTimer); codeTimer = setInterval(draw, 60000);
    return draw();
  };

  // La fiche d'une personne : son nom connu ici, où en est le lien, Payer et Devenir proches.
  const person = async c => {
    quiet();
    let v;
    try { v = await api(`/api/v1/me/meet/${c.to}`); } catch (e) { panel.replaceChildren(h('p', { class: 'err', role: 'alert' }, e.message), h('p', { class: 'pay-acts' }, again())); return; }
    const err = h('p', { class: 'err', role: 'alert' });
    const state = v.friends ? h('p', { class: 'ok', role: 'status' }, '👥 ', t('Votre proche'))
      : v.i_follow ? h('p', { class: 'muted', role: 'status' }, '⏳ ', t('Demande faite : {0} l\'accepte sur son téléphone, ou scanne votre code.', v.label))
      : v.follows_me ? h('p', { class: 'ok', role: 'status' }, '🤝 ', t('{0} vous a ajouté à ses proches.', v.label))
      : h('p', { class: 'degraded', role: 'status' }, '⚠️ ', t('Pas encore dans vos proches : vérifiez que c\'est bien la bonne personne.'));
    // Cercle (privé) : réglable ici d'un geste ; il dit aussi combien prévenir au moment de payer.
    const ring = v.level ? await K.mod('m-circles.js') : null, circle = ring?.LEVELS.find(x => x[0] === v.level);
    const level = ring ? h('p', { class: 'pay-level' }, h('span', { class: 'muted' }, t('Son cercle, connu de vous seul :')), ' ',
      ring.levelSelect(K, v, lv => api('/api/v1/me/circles', { method: 'POST', body: { hex: c.to, level: lv } }).then(() => person({ ...c, m: null }), e => { err.textContent = e.message; }))) : '';
    const payBtn = h('button', { type: 'button', class: meet ? 'ghost' : 'big-btn', onclick: async () => {
      const note = v.friends && v.level >= 3 ? h('p', { class: 'ok', role: 'status' }, circle[1], ' ', t('{0}, votre proche ({1})', v.label, t(circle[2])))
        : v.level === 2 ? h('p', { class: 'degraded', role: 'status' }, '👋 ', t('{0} : une connaissance. Vérifiez son nom dans le récapitulatif avant de confirmer.', v.label))
        : h('p', { class: 'degraded', role: 'status' }, '⚠️ ', t('Cette personne n\'est pas dans vos proches : vérifiez son nom dans le récapitulatif avant de confirmer.'));
      (await K.mod('m-credit.js')).default(K, { mode: 'member', to: c.to, label: v.label, amount: c.a || undefined, note });
    } }, '💸 ', c.a ? t('Payer {0} Ẑen', c.a) : t('Payer'));
    const friendBtn = v.i_follow ? null : h('button', { type: 'button', class: meet ? 'big-btn' : 'ghost', onclick: async () => {
      try { await api(`/api/v1/me/meet/${c.to}`, { method: 'POST', body: c.m ? { proof: c.m } : {} }); person({ ...c, m: null }); }
      catch (e) { err.textContent = e.message; }
    } }, '🤝 ', v.follows_me ? t('Accepter : devenir proches') : t('Devenir proches'));
    const waiting = v.i_follow && !v.friends ? h('div', { class: 'pay-qr' }) : null;
    panel.replaceChildren(h('h3', { class: 'pay-name' }, v.label), state, level,
      v.met?.verified ? h('p', { class: 'muted' }, '✓ ', t('Rencontrés en personne')) : '',
      c.m && !v.i_follow ? h('p', { class: 'muted' }, t('Code scanné face à face : votre rencontre sera vérifiée.')) : '',
      h('p', { class: 'pay-acts' }, ...(meet ? [friendBtn, payBtn] : [payBtn, friendBtn]).filter(Boolean)), err, waiting || '', h('p', { class: 'pay-acts' }, again()));
    if (waiting) {
      await myCode(waiting);
      poll = setInterval(() => api(`/api/v1/me/meet/${c.to}`).then(n => { if (n.friends) { person(c); K.toast(t('Vous êtes proches avec {0}', n.label), 1); navigator.vibrate?.([60, 60, 60]); } }, () => {}), 4000);
    }
  };

  const onText = text => {
    if (off || text === last) return; last = text;
    const c = parse(text);
    if (!c) { hint.textContent = t('Ce code n\'est pas celui d\'une personne.'); return; }
    if (c.to === me.hex) { hint.textContent = t('C\'est votre propre code : visez celui de l\'autre personne.'); return; }
    cam.classList.add('hit'); navigator.vibrate?.(40);
    person(c);
  };
  const scan = async () => {
    quiet(); last = ''; off = false; cam.classList.remove('hit'); pick.hidden = false;
    hint.textContent = t('Visez le code de la personne.');
    panel.replaceChildren(cam, hint);
    try { const s = await watch(video, onText); if (off) s(); else stop = s; } // écran quitté pendant l'autorisation
    catch (e) {
      halt(); hint.textContent = e?.name === 'NotAllowedError' ? t('La caméra est refusée : autorisez-la pour ce site dans les réglages du navigateur.') : t('Pas de caméra disponible ici : choisissez la personne dans vos proches.');
      // Sans caméra, le cadre noir ne sert à rien : le geste possible prend toute la place.
      panel.replaceChildren(hint, meet ? '' : h('p', { class: 'pay-acts' }, h('button', { type: 'button', class: 'big-btn', onclick: chooseFriend }, '👥 ', t('Choisir dans mes proches'))));
      if (!meet) pick.hidden = true;
    }
  };
  const chooseFriend = async () => { quiet(); (await K.mod('m-credit.js')).default(K, { mode: 'member' }); };
  const find = () => {
    quiet();
    const out = h('ul', { class: 'files pay-found' });
    let timer = 0;
    const input = h('input', { id: 'meet-q', type: 'search', autocomplete: 'off', placeholder: t('Prénom, nom…'), oninput: () => { clearTimeout(timer); timer = setTimeout(go, 300); } });
    const go = async () => {
      const q = input.value.trim();
      if (q.length < 2) { out.replaceChildren(); return; }
      try {
        const r = await api(`/api/v1/me/meet?q=${encodeURIComponent(q)}`);
        out.replaceChildren(...(r.people.length ? r.people.map(p => h('li', {}, h('button', { type: 'button', class: 'ghost pay-pick', onclick: () => person({ to: p.hex, a: null, m: null }) },
          h('strong', {}, p.label), ' ', h('small', { class: 'muted' }, p.friends ? t('votre proche') : p.here ? t('sur cette station') : t('autre station'))))) : [h('li', { class: 'muted' }, t('Personne de ce nom dans la constellation.'))]));
      } catch (e) { out.replaceChildren(h('li', { class: 'err' }, e.message)); }
    };
    panel.replaceChildren(h('label', { for: 'meet-q' }, t('Chercher dans la constellation'), input), out,
      h('p', { class: 'muted' }, t('Choisir quelqu\'un dans la liste le relie à vous sans rencontre vérifiée : scannez son code quand vous vous voyez.')));
    input.focus();
  };

  if (!meet) {
    root.replaceChildren(panel,
      h('p', { class: 'pay-acts' }, pick,
        h('a', { class: 'ck-btn', href: '#/recevoir' }, '📲 ', t('Recevoir'))));
  } else {
    const tabs = [['code', '📲 ', 'Mon code'], ['scan', '📷 ', 'Scanner son code'], ['find', '🔎 ', 'Chercher']];
    const bar = h('p', { class: 'pay-tabs', role: 'group' });
    const open = id => {
      bar.querySelectorAll('button').forEach(b => b.setAttribute('aria-pressed', String(b.dataset.v === id)));
      if (id === 'scan') return scan();
      if (id === 'find') return find();
      quiet();
      const box = h('div', { class: 'pay-qr' });
      panel.replaceChildren(box, h('p', { class: 'muted pay-help' }, t('Faites scanner ce code par la personne en face de vous. Il change chaque minute : une photo envoyée au loin ne vaut pas rencontre.')));
      return myCode(box);
    };
    bar.replaceChildren(...tabs.map(([id, ico, l]) => h('button', { type: 'button', class: 'ghost', 'data-v': id, onclick: () => open(id) }, ico, t(l))));
    // Demandes reçues : qui vient de m'ajouter (30 min) ; accepter d'un geste, ou plus tard.
    const later = new Set(), asks = h('section', { class: 'card', hidden: true, 'aria-live': 'polite' });
    const ask = () => api('/api/v1/me/meet').then(r => {
      const list = r.requests.filter(x => !later.has(x.hex));
      asks.hidden = !list.length;
      asks.replaceChildren(h('h3', {}, t('Demandes')), h('ul', { class: 'files' }, list.map(x => h('li', {}, '🤝 ', t('{0} vient de vous ajouter', x.label), ' ',
        h('button', { type: 'button', onclick: () => api(`/api/v1/me/meet/${x.hex}`, { method: 'POST', body: {} }).then(() => { K.toast(t('Vous êtes proches avec {0}', x.label), 1); ask(); }, e => K.toast(e.message, 0)) }, t('Accepter')), ' ',
        h('button', { type: 'button', class: 'ghost', onclick: () => { later.add(x.hex); ask(); } }, t('Plus tard'))))));
    }, () => {});
    root.replaceChildren(h('p', { class: 'muted' }, t('Montrez-vous vos codes, ou cherchez la personne par son nom.')), bar, asks, panel);
    K.every(5000, ask); ask();
    const direct = parse(`${location.origin}/#/rencontre?${qs?.toString() || ''}`);
    if (direct && direct.to !== me.hex) return person(direct);
    return open('code');
  }
  // Lien ouvert par l'appareil photo du téléphone (ou depuis un message) : droit à la fiche de la personne.
  const direct = parse(`${location.origin}/#/payer?${qs?.toString() || ''}`);
  if (direct && direct.to !== me.hex) return person(direct);
  return scan();
}
