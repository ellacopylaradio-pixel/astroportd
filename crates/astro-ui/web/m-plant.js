'use strict';
// Fiche plante d'une photo (#/plante?path=/Photos/x.jpg), comme la fiche d'un film : l'espèce (proposée par Pl@ntNet si la
// coopérative a une clé, ou tapée à la main), puis ce qu'on en sait — comestible ?, préparation, propriétés médicinales,
// dangers, notes —, prérempli depuis Wikipédia et toujours modifiable. Rangée chiffrée dans l'uCloud du membre (/_/Plantes),
// cherchable depuis Fichiers. Rien ne part sans un geste du membre.
const FIELDS = [['description', 'En bref'], ['edible', 'Comestible ?'], ['preparation', 'Préparation'], ['medicinal', 'Propriétés médicinales'], ['dangers', 'Dangers'], ['notes', 'Mes notes']];

// Toutes mes plantes (#/plantes) : une carte par fiche, cherchable, qui mène à la fiche.
async function plantList(root, K) {
  const { h, t } = K;
  const css = h('link', { rel: 'stylesheet', href: '/ui/m-plant.css' }); document.head.append(css); K.cleanup(() => css.remove());
  const find = h('input', { type: 'search', placeholder: t('Chercher une plante'), 'aria-label': t('Chercher une plante'), maxlength: 80, oninput: () => draw() });
  const body = h('div', { class: 'pl-cards' }), note = h('p', { class: 'muted', role: 'status' });
  root.replaceChildren(h('div', { class: 'bar' }, find), note, body);
  let items = [];
  const draw = () => {
    const q = find.value.trim().toLowerCase(), shown = items.filter(i => !q || (i.name + ' ' + i.scientific).toLowerCase().includes(q));
    note.textContent = items.length ? t('{0} plante(s)', items.length) : '';
    body.replaceChildren(...(shown.length ? shown.map(i => {
      const img = h('img', { alt: '', loading: 'lazy' });
      K.blob(i.photo, true).then(u => { img.src = u; }, () => img.remove());
      return h('a', { class: 'mm-card pl-card', href: '#/plante?path=' + encodeURIComponent(i.photo) }, img, h('b', {}, i.name || t('Plante sans nom')),
        h('small', { class: 'muted' }, i.scientific || ''),
        h('small', {}, [i.edible ? '🍽 ' + t('Comestible') : '', i.medicinal ? '💊 ' + t('Médicinale') : '', i.toxic ? '☠ ' + t('Toxique') : ''].filter(Boolean).join(' · ')));
    }) : [h('p', { class: 'muted' }, items.length ? t('Aucune plante ne correspond.') : t('Aucune fiche pour l’instant : ouvrez une photo de plante dans la visionneuse et choisissez « Fiche plante ».'))]));
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
        ' ', h('a', { class: 'btn ghost', href: '#/plantes' }, t('Toutes mes plantes'))));
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
