'use strict';
// Fiche plante d'une photo (#/plante?path=/Photos/x.jpg), comme la fiche d'un film : l'espèce (proposée par Pl@ntNet si la
// coopérative a une clé, ou tapée à la main), puis ce qu'on en sait — comestible ?, préparation, propriétés médicinales,
// dangers, notes —, prérempli depuis Wikipédia et toujours modifiable. Rangée chiffrée dans l'uCloud du membre (/_/Plantes),
// cherchable depuis Fichiers. Rien ne part sans un geste du membre.
const FIELDS = [['description', 'En bref'], ['edible', 'Comestible ?'], ['preparation', 'Préparation'], ['medicinal', 'Propriétés médicinales'], ['dangers', 'Dangers'], ['notes', 'Mes notes']];

// Toutes mes plantes (#/plantes) + Atlas botanique collaboratif du cercle (#6, mailles UMAP sans GPS fin).
async function plantList(root, K) {
  const { h, t } = K;
  const css = h('link', { rel: 'stylesheet', href: '/ui/m-plant.css' }); document.head.append(css); K.cleanup(() => css.remove());
  let items = [], atlas = null, mode = 'mine', filter = '';
  const find = h('input', { type: 'search', placeholder: t('Chercher une plante'), 'aria-label': t('Chercher une plante'), maxlength: 80, oninput: () => draw() });
  const tabs = h('div', { class: 'bar' },
    h('button', { class: 'ghost', 'data-m': 'mine', onclick: () => { mode = 'mine'; draw(); } }, '🌿 ' + t('Mes plantes')),
    h('button', { class: 'ghost', 'data-m': 'atlas', onclick: async () => { mode = 'atlas'; if (!atlas) { try { atlas = (await K.api('/api/v1/ucloud/plants/atlas')).points || []; } catch (e) { note.textContent = e.message; atlas = []; } } draw(); } }, '🗺 ' + t('Atlas du cercle')));
  const body = h('div', {}), note = h('p', { class: 'muted', role: 'status' });
  root.replaceChildren(h('div', { class: 'bar' }, find), tabs, note, body);
  const drawAtlas = () => {
    const q = find.value.trim().toLowerCase();
    const pts = (atlas || []).filter(p => {
      if (filter === 'edible' && !p.edible) return false;
      if (filter === 'medicinal' && !p.medicinal) return false;
      if (filter === 'toxic' && !p.dangers) return false;
      return !q || (p.title + ' ' + p.scientific + ' ' + (p.common || []).join(' ') + ' ' + p.umap).toLowerCase().includes(q);
    });
    note.textContent = t('{0} observation(s) sur la maille du cercle (arrondie à ~1 km, sans GPS fin)', pts.length);
    const flt = h('div', { class: 'bar' }, [['', 'Toutes'], ['edible', 'Comestible'], ['medicinal', 'Médicinale'], ['toxic', 'Toxique']].map(([k, l]) =>
      h('button', { class: 'ghost', 'aria-pressed': String(filter === k), onclick: () => { filter = k; draw(); } }, t(l))));
    // Mini-carte SVG autonome (hors ligne) projetant les mailles UMAP du cercle.
    let svg = '';
    if (pts.length) {
      const lats = pts.map(p => p.lat), lons = pts.map(p => p.lon);
      const minLa = Math.min(...lats) - 0.05, maxLa = Math.max(...lats) + 0.05, minLo = Math.min(...lons) - 0.05, maxLo = Math.max(...lons) + 0.05;
      const NS = 'http://www.w3.org/2000/svg', el = document.createElementNS(NS, 'svg');
      el.setAttribute('viewBox', '0 0 320 140'); el.setAttribute('role', 'img'); el.setAttribute('aria-label', t('Atlas du cercle'));
      el.style.cssText = 'width:100%;max-height:10rem;border:1px solid var(--line);border-radius:var(--r);background:var(--panel);margin-block:.5rem';
      for (const p of pts) {
        const cx = 16 + ((p.lon - minLo) / (maxLo - minLo || 1)) * 288, cy = 124 - ((p.lat - minLa) / (maxLa - minLa || 1)) * 108;
        const c = document.createElementNS(NS, 'circle');
        c.setAttribute('cx', cx.toFixed(1)); c.setAttribute('cy', cy.toFixed(1)); c.setAttribute('r', '5');
        c.setAttribute('fill', p.dangers ? '#c0392b' : p.edible ? '#27ae60' : '#2980b9');
        const ti = document.createElementNS(NS, 'title'); ti.textContent = `${p.title} (${p.umap})`; c.append(ti); el.append(c);
      }
      svg = el;
    }
    const cards = pts.length ? h('div', { class: 'pl-cards' }, pts.map(p => h('a', { class: 'mm-card pl-card', href: '#/mediatheque?k=plant' },
      h('b', {}, p.title || p.scientific), h('small', { class: 'muted' }, [p.scientific, p.umap, p.author_label].filter(Boolean).join(' · ')),
      h('small', {}, [p.edible ? '🍽 ' + t('Comestible') : '', p.medicinal ? '💊 ' + t('Médicinale') : '', p.dangers ? '☠ ' + t('Toxique') : '', p.paid_by_me ? '✓' : ''].filter(Boolean).join(' · ')))))
      : h('p', { class: 'muted' }, t('Aucune plante partagée avec zone géographique pour l’instant.'));
    body.replaceChildren(flt, svg, cards);
  };
  const draw = () => {
    tabs.querySelectorAll('button').forEach(b => b.setAttribute('aria-pressed', String(b.dataset.m === mode)));
    if (mode === 'atlas') return drawAtlas();
    const q = find.value.trim().toLowerCase(), shown = items.filter(i => !q || (i.name + ' ' + i.scientific).toLowerCase().includes(q));
    note.textContent = items.length ? t('{0} plante(s)', items.length) : '';
    body.replaceChildren(h('div', { class: 'pl-cards' }, ...(shown.length ? shown.map(i => {
      const img = h('img', { alt: '', loading: 'lazy' });
      K.blob(i.photo, true).then(u => { img.src = u; }, () => img.remove());
      return h('a', { class: 'mm-card pl-card', href: '#/plante?path=' + encodeURIComponent(i.photo) }, img, h('b', {}, i.name || t('Plante sans nom')),
        h('small', { class: 'muted' }, i.scientific || ''),
        h('small', {}, [i.edible ? '🍽 ' + t('Comestible') : '', i.medicinal ? '💊 ' + t('Médicinale') : '', i.toxic ? '☠ ' + t('Toxique') : ''].filter(Boolean).join(' · ')));
    }) : [h('p', { class: 'muted' }, items.length ? t('Aucune plante ne correspond.') : t('Aucune fiche pour l’instant : ouvrez une photo de plante dans la visionneuse et choisissez « Fiche plante ».'))])));
  };
  try { items = (await K.api('/api/v1/ucloud/plants')).items || []; } catch (e) { note.textContent = e.message; }
  draw();
}

export default async function plant(root, K, qs) {
  if (!qs.get('path')) return plantList(root, K);
  const { h, t } = K, path = qs.get('path') || '';
  const lang = (K.lang || 'fr').slice(0, 2), name = path.split('/').pop();
  root.replaceChildren(h('h2', {}, '🌿 ', t('Fiche plante')), h('p', { class: 'muted' }, t('Chargement…')));
  let v;
  try { v = await K.api('/api/v1/ucloud/plant?path=' + encodeURIComponent(path)); } catch (e) { root.lastChild.replaceWith(h('p', { class: 'err', role: 'alert' }, e.message)); return; }
  const css = h('link', { rel: 'stylesheet', href: '/ui/m-plant.css' }); document.head.append(css); K.cleanup(() => css.remove());
  const img = h('img', { class: 'pl-photo', alt: name });
  K.blob(path, true).then(u => { img.src = u; }, () => img.remove());
  // Une miniature vide (fichier sans vignette) ne doit pas laisser un cadre blanc : on retombe sur la photo entière.
  img.addEventListener('load', () => { if (!img.naturalWidth && !img.dataset.full) { img.dataset.full = '1'; K.blob(path, false).then(u => { img.src = u; }, () => img.remove()); } });

  const field = (id, label, value, rows) => {
    const el = rows ? h('textarea', { id: 'pl-' + id, rows, value }) : h('input', { id: 'pl-' + id, value });
    return [h('label', { for: el.id }, t(label)), el];
  };
  const val = id => root.querySelector('#pl-' + id)?.value.trim() || '';
  let gbif = null;

  const paint = (f, warning) => {
    const sp = f.species || {};
    gbif = sp.gbif || null;
    const choose = m => () => {
      root.querySelector('#pl-sci').value = m.scientific; root.querySelector('#pl-common').value = m.common.join(', '); root.querySelector('#pl-family').value = m.family; gbif = m.gbif;
    };
    const props = f.proposals.length ? h('fieldset', { class: 'pl-props' }, h('legend', {}, t('Propositions de Pl@ntNet')),
      f.proposals.map((m, i) => h('label', { class: 'pl-prop' }, h('input', { type: 'radio', name: 'pl-prop', checked: m.scientific === sp.scientific, onchange: choose(m) }),
        ' ', h('span', {}, h('strong', {}, m.common[0] || m.scientific), ' ', h('em', {}, m.scientific), ` · ${m.score} %`, m.danger ? h('span', { class: 'pl-bad', title: m.danger }, ' ⚠') : null, i === 0 ? h('small', { class: 'muted' }, ' · ', t('la plus probable')) : null))))
      : null;
    const recog = v.available ? h('button', { type: 'button', class: f.proposals.length ? 'ghost' : '', onclick: ev => run(ev.target, () => K.api('/api/v1/ucloud/plant', { method: 'POST', body: { path, lang } })) },
      f.proposals.length ? t('Relancer la reconnaissance') : t('Reconnaître la plante')) : null;
    const src = f.source ? h('p', { class: 'muted' }, t('Prérempli depuis Wikipédia : '), h('a', { href: f.source.url, target: '_blank', rel: 'noopener noreferrer' }, f.source.title), ` (${f.source.license}). `, t('À vérifier et compléter.')) : null;
    root.replaceChildren(
      h('h2', {}, '🌿 ', sp.common?.[0] || sp.scientific || t('Fiche plante')),
      h('section', { class: 'card wide pl-head' }, img, h('div', {},
        h('p', { class: 'muted' }, name),
        h('p', { class: 'pl-warn', role: 'note' }, '⚠ ', t('Une reconnaissance peut se tromper : ne consommez jamais une plante sur la seule foi de cette fiche.')),
        warning ? h('p', { class: 'degraded', role: 'status' }, warning) : null)),
      h('section', { class: 'card wide' }, h('h3', {}, t('Quelle plante ?')),
        v.available ? null : h('p', { class: 'muted' }, t('Écrivez son nom : la reconnaissance automatique n’est pas activée sur cette station.')),
        recog, props,
        h('div', { class: 'pl-grid' }, ...field('sci', 'Nom latin (ou le nom que vous connaissez)', sp.scientific || ''), ...field('common', 'Noms communs (séparés par des virgules)', (sp.common || []).join(', ')), ...field('family', 'Famille botanique', sp.family || '')),
        h('p', {}, h('button', { type: 'button', class: 'ghost', onclick: ev => run(ev.target, () => save(true)) }, t('Compléter depuis Wikipédia')),
          ' ', h('small', { class: 'muted' }, t('Remplit seulement les rubriques vides.')))),
      h('section', { class: 'card wide' }, h('h3', {}, t('Ce qu’on en sait')), src,
        h('div', { class: 'pl-grid' }, FIELDS.flatMap(([id, label]) => field(id, label, f[id] || '', id === 'notes' || id === 'description' ? 3 : 5)))),
      h('p', { class: 'pl-save' }, h('button', { type: 'button', onclick: ev => run(ev.target, () => save(false), t('Fiche de la plante enregistrée')) }, t('Enregistrer la fiche')),
        ' ', h('a', { class: 'btn ghost', href: '#/plantes' }, t('Toutes mes plantes'))),
      sp.scientific ? publishBox(sp) : null);
  };
  const publishBox = sp => {
    const pf = v.parent_fiche || null;
    const minPct = pf ? Math.ceil((pf.remix_min_bps || 0) / 100) : 0;
    const lic = h('select', { 'aria-label': t('Licence') }, [['CC-BY-SA-4.0', 'CC BY-SA'], ['CC-BY-4.0', 'CC BY'], ['CC0-1.0', 'CC0']].map(([val, l]) => h('option', { value: val, selected: pf?.spdx === val }, l)));
    const price = h('input', { type: 'number', min: '0', step: '0.1', value: '0', 'aria-label': t('Prix suggéré (Ẑen)') });
    const shareIn = pf ? h('input', { type: 'number', min: String(minPct), max: '100', value: String(Math.max(40, minPct)), 'aria-label': t('Part du photographe (%)') }) : null;
    const done = h('span', {});
    const pub = async ev => {
      const b = ev.target; b.disabled = true;
      try {
        await save(false);
        const dz = Math.round(parseFloat(price.value || '0') * 10);
        const body = {
          title: sp.common?.[0] ? `${sp.scientific} — ${sp.common[0]}` : sp.scientific,
          kind: 'plant', spdx: lic.value, allow_remix: true, remix_min_bps: 3000,
          pricing: dz > 0 ? { mode: 'hat', suggested_deci_zen: dz } : { mode: 'free' },
          plant_from_photo: path,
          ...(pf ? { parent_fiche_path: pf.path, parent_share_bps: Math.round(parseFloat(shareIn.value || String(minPct)) * 100) } : { file_path: path }),
        };
        await K.api('/api/v1/ucloud/media/remix', { method: 'POST', body });
        K.toast(t('Fiche plante publiée dans votre médiathèque'), 1);
        done.replaceChildren(' ', h('a', { class: 'btn ghost', href: '#/mediatheque?k=plant' }, t('Ouvrir dans la médiathèque')));
      } catch (e) { K.toast(e.message, 0); }
      b.disabled = false;
    };
    return h('section', { class: 'card wide' }, h('h3', {}, t('Publier dans le commun')),
      h('p', { class: 'muted' }, t('Vos notes personnelles et votre position GPS exacte ne sont jamais publiées.')),
      pf ? h('p', { class: 'muted' }, t('Photo d’origine par {0} ({1}, minimum {2} %) : publiée sans dupliquer l’image.', pf.author_label || '…', pf.spdx, minPct)) : null,
      h('div', { class: 'pl-grid' },
        h('label', {}, t('Licence')), lic,
        h('label', {}, t('Prix suggéré (Ẑen, 0 = libre)')), price,
        ...(pf ? [h('label', {}, t('Part du photographe (%)')), shareIn] : [])),
      h('p', {}, h('button', { type: 'button', class: 'ghost', onclick: pub }, '🌿 ' + t('Publier dans le commun')), done));
  };
  const save = wiki => {
    const sci = val('sci');
    const fiche = { species: sci ? { scientific: sci, common: val('common').split(',').map(s => s.trim()).filter(Boolean), family: val('family'), gbif } : null, wiki };
    for (const [id] of FIELDS) fiche[id] = val(id);
    if (wiki && !sci) throw new Error(t('Écrivez d’abord le nom de la plante.'));
    return K.api('/api/v1/ucloud/plant/fiche', { method: 'POST', body: { path, lang, fiche } });
  };
  // Un geste à la fois : bouton occupé pendant l'appel ; la fiche revenue fait foi.
  const run = async (b, call, ok) => {
    b.disabled = true; b.dataset.label = b.textContent; b.textContent = t('Un instant…');
    try { const r = await call(); v.exists = true; paint(r.fiche, r.warning); if (ok) K.toast(ok, 1); }
    catch (e) { K.toast(e.message); b.disabled = false; b.textContent = b.dataset.label; }
  };
  paint(v.fiche);
}
