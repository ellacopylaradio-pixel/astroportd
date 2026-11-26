'use strict';
// Fonctions Cascade / Communs de la Médiathèque (m-media.js) :
// #1 Mes retombées, #2 Merci chiffré NIP-59, #3 Ardoise groupée, #4 Sous-titres recette,
// liste officielle filtrée WoT, contresignature co-auteur et paiement Voie A en deux temps.

const A = '/api/v1/ucloud/media';
const fmtZen = dz => (dz / 10).toFixed(2).replace('.', ',') + ' Ẑ';

export function makeCascadeUi(root, K, { licences, art, load, detail }) {
  const { h, t, api, toast } = K;
  let slate = { items: [], count: 0, total_deci_zen: 0 };
  const slateBtn = h('button', { class: 'ghost', hidden: true, onclick: () => openSlateDialog() });

  const paintSlateBtn = () => {
    slateBtn.hidden = !slate.count;
    if (slate.count) slateBtn.textContent = '♡ ' + t('Ardoise ({0} · {1})', slate.count, fmtZen(slate.total_deci_zen));
  };
  const refreshSlate = async () => { try { slate = await api(A + '/slate'); paintSlateBtn(); } catch {} };

  // #1 Tableau de bord « Mes retombées » (créations, relais, top œuvres, derniers mercis).
  async function drawEarnings(body) {
    body.replaceChildren(h('p', { class: 'muted' }, t('Chargement…')));
    try {
      const e = await api(A + '/earnings');
      const roleLbl = r => (r === 'relay' ? t('Relais') : r === 'mixed' ? t('Création + Relais') : t('Création'));
      const stat = (lbl, val) => h('div', { class: 'card' }, h('small', { class: 'muted' }, lbl), h('div', {}, h('b', {}, val)));
      const top = (e.top_works || []).map(w => h('tr', {}, h('td', {}, w.title || w.work_sha.slice(0, 10)), h('td', {}, String(w.count)), h('td', {}, fmtZen(w.total_deci_zen))));
      const rec = (e.recent || []).map(r => h('li', {}, h('b', {}, '+' + fmtZen(r.deci_zen)), ' · ', roleLbl(r.role), ' · ', r.title || r.work_sha.slice(0, 10),
        ' — ', h('span', { class: 'muted' }, r.from_label || t('Un proche anonyme')), r.note ? h('div', {}, '« ' + r.note + ' »') : ''));
      body.replaceChildren(
        h('div', { class: 'mm-grid' },
          stat(t('Reçu (créations)'), fmtZen(e.creation_deci_zen || 0)),
          stat(t('Reçu (relais)'), fmtZen(e.relay_deci_zen || 0)),
          stat(t('Total reçu'), fmtZen(e.total_received_deci_zen || 0)),
          stat(t('Soutiens donnés'), fmtZen(e.total_given_deci_zen || 0))),
        top.length ? h('section', { class: 'card' }, h('h3', {}, t('Œuvres les plus soutenues')),
          h('table', {}, h('thead', {}, h('tr', {}, h('th', {}, t('Œuvre')), h('th', {}, t('Soutiens')), h('th', {}, t('Total')))), h('tbody', {}, top))) : '',
        h('section', { class: 'card' }, h('h3', {}, t('Derniers soutiens reçus')),
          rec.length ? h('ul', {}, ...rec) : h('p', { class: 'muted' }, t('Aucun soutien reçu pour l\'instant.'))));
    } catch (err) { body.replaceChildren(h('p', { class: 'err' }, err.message)); }
  }

  // #3 Dialogue d'ardoise : œuvres empilées + aperçu fusionné par bénéficiaire + règlement en 1 lot atomique.
  function openSlateDialog() {
    const listBox = h('div', {}), out = h('div', {}), err = h('p', { class: 'err', role: 'alert' });
    const cid = 'slate-' + Date.now().toString(36) + '-' + Math.random().toString(36).slice(2, 8);
    const paintList = () => {
      if (!slate.items.length) { listBox.replaceChildren(h('p', { class: 'muted' }, t('Votre ardoise est vide.'))); out.replaceChildren(); return; }
      listBox.replaceChildren(h('ul', {}, ...slate.items.map(e => h('li', {}, h('b', {}, e.title || e.path), ' · ', fmtZen(e.amount_deci_zen), ' ',
        h('button', { class: 'ghost', onclick: async () => { slate = await api(A + '/slate', { method: 'POST', body: { path: e.path, remove: true } }); paintSlateBtn(); paintList(); } }, '×')))));
    };
    const preview = async () => {
      err.textContent = ''; out.replaceChildren(h('p', { class: 'muted' }, t('Chargement…')));
      try {
        const p = await api(A + '/slate/settle', { method: 'POST', body: { preview: true } });
        const rows = (p.lines || []).map(l => h('tr', {}, h('td', {}, l.label || l.to_pubkey.slice(0, 12)), h('td', {}, fmtZen(l.deci_zen))));
        const btn = h('button', { disabled: !p.payable, onclick: async ev => {
          ev.target.disabled = true; err.textContent = '';
          try {
            const res = await api(A + '/slate/settle', { method: 'POST', body: { preview: false, credit_id: cid, confirm: true } });
            toast(t('Ardoise réglée ({0} bénéficiaire(s))', res.transfers), 1);
            await refreshSlate(); dlg.close();
          } catch (e) { err.textContent = e.message; ev.target.disabled = false; }
        } }, t('Régler toute l\'ardoise'));
        out.replaceChildren(
          h('table', {}, h('thead', {}, h('tr', {}, h('th', {}, t('Bénéficiaire')), h('th', {}, t('Part')))), h('tbody', {}, rows)),
          h('p', { class: 'muted' }, t('Total : {0} · Solde après : {1}', fmtZen(p.net_deci_zen), fmtZen(Math.round((p.payer_after_zen_cents || 0) / 10)))),
          h('p', {}, btn));
      } catch (e) { out.replaceChildren(); err.textContent = e.message; }
    };
    const dlg = h('dialog', { class: 'mm-pay', 'aria-label': t('Mon ardoise') },
      h('div', { class: 'bar' }, h('b', {}, t('Mon ardoise')), h('span', { class: 'sp' }), h('button', { class: 'ghost', onclick: () => dlg.close() }, t('Fermer'))),
      listBox, h('p', {}, h('button', { class: 'ghost', onclick: preview }, t('Voir la répartition groupée'))), err, out);
    dlg.addEventListener('close', () => dlg.remove());
    paintList(); root.append(dlg); dlg.showModal(); if (slate.items.length) preview();
  }

  // Paiement Voie A en deux temps + mot de merci chiffré (#2).
  function openPayDialog(it, commons, onPaid) {
    const defDz = commons.pricing?.price_deci_zen ?? commons.pricing?.suggested_deci_zen ?? commons.pricing?.min_deci_zen ?? 10;
    const amt = h('input', { type: 'number', min: '0.1', step: '0.1', value: (defDz / 10).toFixed(1), 'aria-label': t('Montant en Ẑen') });
    const noteIn = h('input', { maxlength: 280, placeholder: t('Un mot de merci (chiffré, facultatif)'), 'aria-label': t('Mot de merci') });
    const anonIn = h('input', { type: 'checkbox' });
    const out = h('div', {}), err = h('p', { class: 'err', role: 'alert' });
    const cid = 'cascade-' + Date.now().toString(36) + '-' + Math.random().toString(36).slice(2, 8);
    const preview = async () => {
      err.textContent = ''; out.replaceChildren(h('p', { class: 'muted' }, t('Chargement…')));
      const dz = Math.round(parseFloat(amt.value) * 10);
      if (!dz || dz < 1) { out.replaceChildren(); err.textContent = t('Indiquez un montant d\'au moins 0,10 Ẑen.'); return; }
      try {
        const p = await api(A + '/pay/preview', { method: 'POST', body: { path: it.fiche, amount_deci_zen: dz } });
        const rows = (p.lines || []).map(l => h('tr', {}, h('td', {}, l.label || l.email || l.to_pubkey.slice(0, 12)), h('td', {}, fmtZen(l.deci_zen))));
        const unreach = (p.unreachable || []).length ? h('p', { class: 'err' }, t('Certains bénéficiaires ne peuvent pas recevoir ce versement ({0}).', p.unreachable.map(u => u.reason).join(' ; '))) : '';
        const filtered = (p.filtered_relays || []).length ? h('p', { class: 'muted' }, t('{0} relais non vérifié(s) écarté(s) : leur part reste aux créateurs ou aux relais vérifiés.', p.filtered_relays.length)) : '';
        const confirmBtn = h('button', { disabled: !p.payable, onclick: async ev => {
          ev.target.disabled = true; err.textContent = '';
          try {
            const res = await api(A + '/pay', { method: 'POST', body: { path: it.fiche, amount_deci_zen: dz, credit_id: cid, confirm: true, note: noteIn.value.trim() || null, anonymous: anonIn.checked } });
            toast(res.already ? t('Versement déjà effectué') : t('Versement envoyé ({0} bénéficiaire(s))', res.transfers), 1);
            dlg.close(); if (onPaid) onPaid();
          } catch (e) { err.textContent = e.message; ev.target.disabled = false; }
        } }, t('Confirmer le versement'));
        out.replaceChildren(
          h('table', {}, h('thead', {}, h('tr', {}, h('th', {}, t('Bénéficiaire')), h('th', {}, t('Part')))), h('tbody', {}, rows)),
          h('p', { class: 'muted' }, t('Total : {0} · Solde après : {1}', fmtZen(p.net_deci_zen ?? p.amount_deci_zen), fmtZen(Math.round((p.payer_after_zen_cents || 0) / 10)))),
          filtered,
          h('p', { class: 'muted' }, p.island ? t('Mode île : promesses différées enregistrées jusqu\'au retour du réseau Ğ1.') : t('Les virements d\'un même lot peuvent être rapprochés sur le registre.')),
          unreach, !p.enough_funds ? h('p', { class: 'err' }, t('Solde insuffisant pour ce versement.')) : '',
          h('p', {}, confirmBtn));
      } catch (e) { out.replaceChildren(); err.textContent = e.message; }
    };
    const dlg = h('dialog', { class: 'mm-pay', 'aria-label': t('Soutenir en Ẑen') },
      h('div', { class: 'bar' }, h('b', {}, t('Soutenir en Ẑen')), h('span', { class: 'sp' }), h('button', { class: 'ghost', onclick: () => dlg.close() }, t('Fermer'))),
      h('p', {}, h('label', {}, t('Montant en Ẑen'), ' ', amt), ' ', h('button', { class: 'ghost', onclick: preview }, t('Voir la répartition'))),
      h('p', {}, noteIn),
      h('p', {}, h('label', {}, anonIn, ' ', t('Rester anonyme'))),
      err, out);
    dlg.addEventListener('close', () => dlg.remove());
    root.append(dlg); dlg.showModal(); preview();
  }

  // Formulaire de version dérivée « recette » (sans dupliquer le fichier parent, y compris sous-titres .vtt/.srt #4).
  function openRemixDialog(it, commons) {
    const minBps = commons.rights?.remix_min_bps || 0;
    const isVid = it.type === 'video' || it.type === 'film';
    const titleIn = h('input', { value: it.title + ' (dérivé)', maxlength: 300 });
    const lic = h('select', {}, licences.slice(1).map(([v, l]) => h('option', { value: v, selected: v === (commons.rights?.spdx || 'CC-BY-SA-4.0') }, t(l))));
    const shareIn = h('input', { type: 'number', min: String(Math.ceil(minBps / 100)), max: '100', value: String(Math.max(40, Math.ceil(minBps / 100))) });
    const priceIn = h('input', { type: 'number', min: '0', step: '0.1', value: '1.0' });
    const subIn = isVid ? h('input', { placeholder: '/SousTitres/film.fr.vtt' }) : null;
    const langIn = isVid ? h('input', { value: (K.lang || 'fr').slice(0, 2), maxlength: 8 }) : null;
    const err = h('p', { class: 'err', role: 'alert' });
    const dlg = h('dialog', { class: 'mm-pay', 'aria-label': t('Créer une version dérivée') },
      h('div', { class: 'bar' }, h('b', {}, t('Créer une version dérivée')), h('span', { class: 'sp' }), h('button', { class: 'ghost', onclick: () => dlg.close() }, t('Fermer'))),
      h('p', {}, h('label', {}, t('Titre'), titleIn)),
      h('p', {}, h('label', {}, t('Licence'), ' ', lic)),
      h('p', {}, h('label', {}, t('Part du créateur d\'origine (%)'), ' ', shareIn), h('small', { class: 'muted' }, ' ' + t('minimum {0} %', Math.ceil(minBps / 100)))),
      h('p', {}, h('label', {}, t('Prix suggéré (Ẑen, 0 = libre)'), ' ', priceIn)),
      ...(isVid ? [h('p', {}, h('label', {}, t('Fichier sous-titres (.vtt / .srt, facultatif)'), subIn)), h('p', {}, h('label', {}, t('Langue des sous-titres'), ' ', langIn))] : []),
      err,
      h('p', {}, h('button', { onclick: async ev => {
        ev.target.disabled = true; err.textContent = '';
        const dz = Math.round(parseFloat(priceIn.value || '0') * 10);
        const pricing = dz > 0 ? { mode: 'hat', suggested_deci_zen: dz } : { mode: 'free' };
        const sub = subIn?.value.trim() || null;
        try {
          const r = await api(A + '/remix', { method: 'POST', body: {
            title: titleIn.value.trim(), kind: isVid ? 'video' : it.type === 'plant' ? 'plant' : 'photo', spdx: lic.value, allow_remix: true, remix_min_bps: minBps, pricing,
            parent_fiche_path: it.fiche, parent_share_bps: Math.round(parseFloat(shareIn.value) * 100),
            ...(sub ? { subtitle_path: sub, subtitle_lang: langIn.value.trim() || 'fr' } : {}),
          } });
          toast(t('Version dérivée publiée dans votre médiathèque'), 1);
          dlg.close(); await load(); if (r.item) detail(r.item);
        } catch (e) { err.textContent = e.message; ev.target.disabled = false; }
      } }, t('Publier la version dérivée'))));
    dlg.addEventListener('close', () => dlg.remove());
    root.append(dlg); dlg.showModal();
  }

  // Provenance, compteur public (membres vérifiés), contresignature co-auteur, ardoise rapide (#3) et soutien en Ẑen.
  function commonsBox(it, visualEl) {
    const box = h('div', { class: 'mm-chain' });
    const refresh = async () => {
      try {
        const r = await api(A + '/chain?path=' + encodeURIComponent(it.fiche));
        if (r.source_photo_path && visualEl && !art(it)) {
          K.blob(r.source_photo_path, true).then(u => { const img = h('img', { alt: '', src: u }); visualEl.replaceWith(img); visualEl = img; }, () => {});
        }
        const parts = [];
        if (r.dag_verified === false) parts.push(h('p', { class: 'err' }, t('Attention : la répartition déclarée ne correspond pas aux fiches parentes locales.')));
        if (r.paid_by_me) parts.push(h('p', {}, h('span', { class: 'pill ok' }, '✓ ' + t('Déjà soutenu ({0})', fmtZen(r.paid_by_me.amount_deci_zen)))));
        if (r.chain?.hops?.length) {
          const hops = r.chain.hops.length, relays = Math.max(0, hops - 1);
          parts.push(h('p', { class: 'muted' }, relays ? t('Reçu via {0} proche(s) ({1}ᵉ main)', relays, hops) : t('Reçu directement de l\'auteur')));
        }
        if (r.official) {
          const o = r.official;
          const regBtn = h('button', { class: 'ghost', onclick: async ev => {
            ev.target.disabled = true;
            try { await api(A + '/chain/register', { method: 'POST', body: { path: it.fiche, register: !o.registered } }); toast(o.registered ? t('Retiré du compteur public') : t('Inscrit au compteur public'), 1); refresh(); }
            catch (e) { toast(e.message, 0); ev.target.disabled = false; }
          } }, o.registered ? t('Ne plus apparaître au compteur') : t('Apparaître au compteur public'));
          parts.push(h('p', { class: 'muted' }, t('Compteur public : {0} membre(s) vérifié(s)', o.wot_count), ' ', regBtn));
          parts.push(h('p', { class: 'muted' }, h('small', {}, t('Facultatif : rend votre relais visible (inutile pour recevoir votre part).'))));
        }
        if (r.commons) {
          const c = r.commons, actions = [];
          if (c.rights?.mode === 'open_common') {
            actions.push(h('button', { onclick: () => openPayDialog(it, c, refresh) }, 'ẑ ' + t('Soutenir en Ẑen')));
            actions.push(h('button', { class: 'ghost', onclick: async () => {
              try { slate = await api(A + '/slate', { method: 'POST', body: { path: it.fiche, amount_deci_zen: 5, add: true } }); paintSlateBtn(); toast(t('Ajouté à votre ardoise (+0,50 Ẑ)'), 1); }
              catch (e) { toast(e.message, 0); }
            } }, '♡ +0,50 Ẑ'));
            if (c.rights.allow_remix) actions.push(h('button', { class: 'ghost', onclick: () => openRemixDialog(it, c) }, '↻ ' + t('Créer une version dérivée')));
          }
          if (r.can_attest) {
            actions.push(h('button', { class: 'ghost', onclick: async ev => {
              ev.target.disabled = true;
              try { await api(A + '/credit/attest', { method: 'POST', body: { path: it.fiche } }); toast(t('Générique contresigné'), 1); refresh(); }
              catch (e) { toast(e.message, 0); ev.target.disabled = false; }
            } }, '✓ ' + t('Contresigner le générique')));
          }
          if (actions.length) parts.push(h('p', {}, ...actions.flatMap((b, i) => (i ? [' ', b] : [b]))));
        }
        box.replaceChildren(...parts);
      } catch { box.replaceChildren(); }
    };
    refresh();
    return box;
  }

  return { slateBtn, refreshSlate, drawEarnings, commonsBox };
}
