'use strict';
// Station › Membres : IA des membres (docs/design/IA.md) : prêt, droits, quota, prix, parrainage.
const MODES = [['off', 'Aucune IA'], ['station', 'L\'IA de la station'], ['own', 'La station, et la leur']];
const API = '/api/v1/station/ai';

// Vue du serveur et envoi d'un réglage ; `done` repeint l'écran avec la vue revenue (elle fait foi).
export async function load(K) {
  if (!document.querySelector('link[href="/ui/m-aiadmin.css"]')) {
    const css = K.h('link', { rel: 'stylesheet', href: '/ui/m-aiadmin.css' }); document.head.append(css); K.cleanup(() => css.remove());
  }
  return K.api(API).catch(() => null);
}

export function card(K, v, done) {
  const { h, t } = K;
  const send = async (body, el) => {
    if (el) el.disabled = true;
    try { done(await K.api(API, { method: 'POST', body })); K.toast(t('Réglage enregistré'), 1); } catch (e) { K.toast(e.message); if (el) el.disabled = false; }
  };
  const mine = v.mine
    ? h('p', {}, t('Votre IA : {0}', v.mine.family === 'cloud' ? t('par Internet, chez {0}', v.mine.provider || '?') : t('sur votre station')), v.mine.model ? ' · ' + v.mine.model : '', v.mine.family === 'cloud' ? ' · ' + t('{0} requêtes/min', v.mine.rpm) : '', ' ',
      v.test ? (v.test.ok ? h('span', { class: 'pill ok' }, t('testée')) : h('span', { class: 'pill degraded' }, t('ne répond pas'))) : '', ' ', h('a', { href: '#/devices?card=ai' }, t('Changer')))
    : h('p', {}, t('Vous n\'avez pas encore d\'IA.'), ' ', h('a', { href: '#/devices?card=ai' }, t('La déclarer') + ' →'));
  const share = h('input', { type: 'checkbox', role: 'switch', id: 'aa-share', checked: v.share, disabled: !v.mine });
  share.onchange = () => send({ share: share.checked }, share);

  // Droits par défaut.
  const d = v.default || {};
  const mode = h('select', { id: 'aa-mode' }, MODES.map(([k, l]) => h('option', { value: k, selected: d.mode === k }, t(l))));
  const quota = h('input', { id: 'aa-quota', inputmode: 'numeric', value: d.quota_tokens_week ? d.quota_tokens_week / 1000 : '', placeholder: t('sans limite') });
  const price = h('input', { id: 'aa-price', inputmode: 'numeric', value: d.price_zen_week || '', placeholder: '0' });
  const save = h('button', { type: 'button' }, t('Enregistrer'));
  save.onclick = () => { const r = rights(K, quota.value, price.value); if (r) send({ default: { mode: mode.value, ...r } }, save); };

  // Liens de parrainage.
  const refs = h('div', {}, (v.providers || []).map(p => {
    const inp = h('input', { id: 'aa-ref-' + p.id, type: 'url', value: (v.referrals || {})[p.id] || '', placeholder: 'https://…', autocomplete: 'off' });
    inp.onchange = () => send({ referral: { provider: p.id, url: inp.value.trim() || null } }, inp);
    return h('p', {}, h('label', { for: inp.id }, p.label), inp);
  }));

  return h('section', { class: 'card wide' }, h('h3', {}, '🤖 ', t('IA de vos membres')), mine,
    h('p', {}, h('label', { for: 'aa-share', class: 'ix-sw' }, share, ' ', t('Prêter mon IA à mes membres (ils ne voient jamais la clé)'))),
    v.shared_by_other ? h('p', { class: 'muted' }, t('Un autre Capitaine de cette station prête déjà la sienne.')) : '',
    h('h4', {}, t('Pour les membres que je n\'ai pas réglés un par un')),
    h('div', { class: 'aa-form' }, h('label', { for: 'aa-mode' }, t('Ce qu\'ils peuvent utiliser')), mode,
      h('label', { for: 'aa-quota' }, t('Milliers de jetons par semaine (vide : sans limite)')), quota,
      h('label', { for: 'aa-price' }, t('Prix de l\'IA, en ẑ par semaine (0 : comprise)')), price),
    h('p', {}, save),
    h('p', { class: 'muted' }, t('Le prix est prélevé chaque semaine sur le MULTIPASS avec l\'hébergement (locataires) et entre dans l\'économie de la coopérative (3×1/3). Le quota compte les jetons de l\'IA de la station.')),
    h('details', { class: 'fold' }, h('summary', {}, t('Liens de parrainage de la coopérative')),
      h('p', { class: 'muted' }, t('Proposés aux membres qui n\'ont pas de compte chez ce fournisseur, et présentés comme un parrainage de la coopérative. Vide : le lien de la coopérative (MixRoute). Pour mettre le vôtre, copiez-le depuis la console du fournisseur (MixRoute : votre lien d\'invitation).')), refs));
}

// Cellule d'un membre : ce qu'il peut utiliser (ou le défaut), et ses limites, repliées.
export function cell(K, v, a, done) {
  const { h, t } = K;
  const own = (v.members || {})[a.hex], r = own || v.default || {};
  const send = async (rights, el) => {
    el.disabled = true;
    try { done(await K.api(API, { method: 'POST', body: { member: { hex: a.hex, rights } } })); K.toast(t('Réglage enregistré'), 1); } catch (e) { K.toast(e.message); el.disabled = false; }
  };
  const id = 'aa-m-' + a.hex.slice(0, 12);
  const sel = h('select', { id, 'aria-label': t('IA de {0}', a.email || a.hex.slice(0, 12)) }, h('option', { value: '' , selected: !own }, t('Par défaut')),
    MODES.map(([k, l]) => h('option', { value: k, selected: own?.mode === k }, t(l))));
  sel.onchange = () => send(sel.value ? { mode: sel.value } : null, sel);
  const q = h('input', { inputmode: 'numeric', value: r.quota_tokens_week ? r.quota_tokens_week / 1000 : '', placeholder: t('sans limite'), 'aria-label': t('Milliers de jetons par semaine') });
  const z = h('input', { inputmode: 'numeric', value: r.price_zen_week || '', placeholder: '0', 'aria-label': t('ẑ par semaine') });
  const ok = h('button', { type: 'button', class: 'ghost' }, 'OK');
  ok.onclick = () => { const x = rights(K, q.value, z.value); if (x) send({ mode: sel.value || r.mode || 'station', ...x }, ok); };
  const sum = [r.quota_tokens_week ? t('{0} k jetons/sem.', r.quota_tokens_week / 1000) : t('sans limite'), r.price_zen_week ? `${r.price_zen_week} ẑ/sem.` : null].filter(Boolean).join(' · ');
  return h('span', { class: 'ix-cell' }, sel, h('details', { class: 'aa-lim' }, h('summary', {}, h('small', { class: 'muted' }, (own ? '' : t('par défaut') + ' · ') + sum)), q, z, ok));
}

// Saisie → { quota_tokens_week, price_zen_week } en entiers ; `null` (et un message) si elle est invalide.
function rights(K, quota, price) {
  const k = quota.trim() === '' ? 0 : Number(quota), z = price.trim() === '' ? 0 : Number(price);
  if (!Number.isInteger(k) || k < 0) return void K.toast(K.t('Nombre entier de milliers de jetons attendu'));
  if (!Number.isInteger(z) || z < 0) return void K.toast(K.t('Nombre entier de ẑ attendu'));
  return { quota_tokens_week: k * 1000, price_zen_week: z };
}
