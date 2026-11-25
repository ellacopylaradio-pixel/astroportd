'use strict';
// Options de la station (Capitaine) : moteur FaceID commutable à chaud et file de traitement ; la matrice du relai par kind
// est ici (`relayCard`) mais se montre dans les Écluses (les tunnels et l'entraide sont dans m-services.js). Rien ne redémarre :
// la station applique le réglage dès qu'il est enregistré, et le conserve.
const READ = { public: 'Tous', addressed: 'Auteur et destinataires', friends: 'Auteur et ses Proches', circle: 'Cercle social', deny: 'Personne' };
const WRITE = { circle: 'Cercle social', open: 'Tous (débit limité)', deny: 'Personne' };
const MODE = { off: 'Arrêté', local: 'ONNX local (cette machine)', remote: 'Brain GPU distant' };

// Matrice du relai par kind (lecture, écriture, projection), montrée dans les Écluses (m-locks.js) : chaque changement
// s'applique tout de suite ; `redraw` redessine l'écran qui l'accueille.
export async function relayCard(K, redraw) {
  const { h, t } = K, post = (p, body) => K.api('/api/v1/station/admin/' + p, { method: 'POST', body });
  const kinds = await K.api('/api/v1/station/admin/kinds');
  const sel = (choices, labels, cur) => h('select', {}, choices.map(c => h('option', { value: c, selected: c === cur }, t(labels[c] || c))));
  const rows = kinds.kinds.map(k => {
    const read = sel(kinds.choices.read, READ, k.read), write = sel(kinds.choices.write, WRITE, k.write), proj = h('input', { type: 'checkbox', checked: k.project, 'aria-label': t('Projection du kind {0}', k.kind) });
    const spec = () => `read=${read.value} write=${write.value} project=${proj.checked ? 'on' : 'off'}`;
    return { k, read, write, proj, spec, changed: () => spec() !== `read=${k.read} write=${k.write} project=${k.project ? 'on' : 'off'}` };
  });
  const addNo = h('input', { type: 'number', min: 0, max: 65535, placeholder: 'kind', 'aria-label': t('Ajouter un kind') });
  const save = h('button', { onclick: async ev => {
    const rules = {};
    rows.filter(r => r.changed()).forEach(r => { rules[r.k.kind] = r.spec(); });
    if (addNo.value) rules[addNo.value] = 'read=circle write=circle project=on';
    if (!Object.keys(rules).length) return K.toast(t('Aucune modification'));
    ev.target.disabled = true;
    try { await post('kinds', { rules }); K.toast(t('Matrice du relai appliquée'), 1); redraw(); } catch (e) { K.toast(e.message); ev.target.disabled = false; }
  } }, t('Appliquer la matrice'));
  const kindsCard = h('section', { class: 'card wide' }, h('h2', {}, t('Relai : lecture, écriture, projection par kind')),
    h('p', { class: 'muted' }, t('Lecture : qui reçoit les événements. Écriture : qui peut en publier. Projection : l\'événement alimente-t-il les tables dérivées (comptes, ledger…) ; il reste stocké et servi. S\'applique tout de suite.')),
    h('table', {}, h('caption', { class: 'muted' }, t('Un kind sans réglage suit les défauts du relai.')), h('thead', {}, h('tr', {}, [t('Kind'), t('Lecture'), t('Écriture'), t('Projeté'), t('Origine')].map(x => h('th', { scope: 'col' }, x)))),
      h('tbody', {}, rows.map(r => h('tr', {}, h('td', {}, h('strong', {}, String(r.k.kind)), r.k.label ? ' ' + t(r.k.label) : ''), h('td', {}, r.read), h('td', {}, r.write), h('td', {}, r.proj), h('td', { class: 'muted' }, t(r.k.source),
        r.k.source === 'capitaine' ? [' ', h('button', { class: 'ghost', onclick: async () => { try { await post('kinds', { reset: [r.k.kind] }); redraw(); } catch (e) { K.toast(e.message); } } }, t('Défaut'))] : null))))),
    h('p', {}, addNo, ' ', save), h('p', { class: 'muted' }, t('Fixes : 1059 — {0} ; 22242 — {1}.', t(kinds.fixed['1059']), t(kinds.fixed['22242']))));
  return kindsCard;
}

export default async function options(root, K) {
  const { h, t } = K, api = p => K.api('/api/v1/station/admin/' + p), post = (p, body) => K.api('/api/v1/station/admin/' + p, { method: 'POST', body });
  root.replaceChildren(h('p', { class: 'muted' }, t('Chargement…')));
  const [face, tk] = await Promise.all([api('face'), api('tasks').catch(() => null)]);
  const ok = (v, text) => h('li', {}, v ? '✔ ' : '✘ ', text);

  // ── Moteur FaceID ──
  const url = h('input', { id: 'fo-url', placeholder: 'http://brain.local:8190/faces', value: face.url || '', autocomplete: 'off', disabled: !!face.locked });
  const radios = Object.entries(MODE).map(([m, label]) => h('label', {}, h('input', { type: 'radio', name: 'fo-mode', value: m, checked: face.mode === m, disabled: !!face.locked }), ' ' + t(label)));
  const apply = h('button', { disabled: !!face.locked, onclick: async ev => {
    const mode = radios.map(r => r.firstChild).find(r => r.checked)?.value;
    ev.target.disabled = true;
    try { await post('face', { mode, url: url.value.trim() }); K.toast(t('Moteur FaceID appliqué'), 1); options(root, K); } catch (e) { K.toast(e.message); ev.target.disabled = false; }
  } }, t('Appliquer'));
  const running = face.label ? t('En service : {0}', face.label) : t('Aucun moteur en service : les nouvelles photos attendent.');
  const faceCard = h('section', { class: 'card' }, h('h2', {}, t('Reconnaissance des visages')),
    h('p', { class: face.label ? '' : 'degraded', role: 'status' }, running),
    face.locked ? h('p', { class: 'muted' }, t('Imposé par {0} sur la ligne de commande : retirez l\'option pour le régler ici.', face.locked)) : null,
    h('fieldset', { class: 'coop-s' }, h('legend', {}, t('Moteur')), radios, h('label', { for: 'fo-url' }, t('Adresse du Brain (mode distant)')), url),
    h('ul', { class: 'muted' }, ok(face.onnx_compiled, t('fonction ONNX compilée dans ce binaire')), ok(face.models, face.models ? t('modèles : {0}', face.models) : t('modèles introuvables (det_10g.onnx, w600k_r50.onnx)')), ok(face.library, face.library ? t('onnxruntime : {0}', face.library) : t('libonnxruntime introuvable (ORT_DYLIB_PATH)'))),
    h('p', { class: 'muted' }, t('{0} photo(s) en attente · {1} abandonnée(s).', face.pending, face.failed)), h('p', {}, apply));

  // ── File de traitement ──
  let tasksCard = null, tasksSummary = '';
  if (tk) {
    const KIND = { facecloud: 'Reconnaissance des visages', 'bro.upload': 'Dépôts demandés à BRO' };
    const failed = tk.kinds.reduce((n, k) => n + k.failed, 0), waiting = tk.kinds.reduce((n, k) => n + k.pending + k.running, 0);
    tasksSummary = t('{0} en cours ou en attente · {1} abandonnée(s)', waiting, failed);
    const retry = async (kind, ev) => { const btn = ev.currentTarget; btn.disabled = true; try { const r = await post('tasks', { action: 'retry', kind }); K.toast(t('{0} tâche(s) relancée(s)', r.retried), 1); options(root, K); } catch (e) { K.toast(e.message); btn.disabled = false; } };
    const retryAll = async ev => { const btn = ev.currentTarget; btn.disabled = true; try { let n = 0; for (const k of tk.kinds.filter(x => x.failed)) n += (await post('tasks', { action: 'retry', kind: k.kind })).retried; K.toast(t('{0} tâche(s) relancée(s)', n), 1); options(root, K); } catch (e) { K.toast(e.message); btn.disabled = false; } };
    tasksCard = h('section', { class: 'card wide' }, h('h2', {}, t('File de traitement')),
      h('p', { class: 'muted' }, t('Les tâches en arrière-plan (reconnaissance des visages, dépôts demandés à BRO). Une tâche abandonnée après plusieurs essais reste ici jusqu\'à ce que vous la relanciez.')),
      tk.kinds.length ? h('table', {}, h('thead', {}, h('tr', {}, ['Tâche', 'En attente', 'En cours', 'Terminées', 'Abandonnées'].map(x => h('th', { scope: 'col' }, t(x))))),
        h('tbody', {}, tk.kinds.map(k => h('tr', {}, h('td', {}, t(KIND[k.kind] || k.kind)), h('td', {}, String(k.pending)), h('td', {}, String(k.running)), h('td', {}, String(k.done)), h('td', { class: k.failed ? 'down' : '' }, String(k.failed), k.failed ? [' ', h('button', { class: 'ghost', onclick: ev => retry(k.kind, ev) }, t('Relancer'))] : null))))) : h('p', { class: 'muted' }, t('Aucune tâche pour le moment.')),
      tk.kinds.filter(x => x.failed).length > 1 ? h('p', {}, h('button', { onclick: retryAll }, t('Tout relancer'))) : null,
      tk.failed.length ? [h('h3', {}, t('Dernières abandonnées')), h('ul', {}, tk.failed.map(f => h('li', {}, `#${f.id} ${t(KIND[f.kind] || f.kind)} · ${f.attempts} ` + t('essai(s)') + (f.why ? ` — ${f.why}` : ''))))] : null);
  }

  // Les réglages experts sont repliés (avec un résumé de leur état) ; ce qui est ouvert le reste d'un redessin à l'autre.
  const opened = root._opened ||= new Set();
  const fold = (sec, summary) => {
    if (!sec) return null;
    const [title, ...body] = [...sec.childNodes], key = title.textContent;
    const d = h('details', { class: 'card wide fold', open: opened.has(key) }, h('summary', {}, h('strong', {}, key), ' ', h('span', { class: 'muted' }, summary)), ...body);
    d.addEventListener('toggle', () => (d.open ? opened.add(key) : opened.delete(key)));
    return d;
  };
  root.replaceChildren(h('h2', {}, t('Options de la station')), h('p', { class: 'muted' }, t('Les services partagés avec les autres Capitaines et l\'entraide sont dans Réseau ›'), ' ', h('a', { href: '#/services' }, t('Partages')), '.'), h('div', { class: 'grid' },
    fold(tasksCard, tasksSummary), fold(faceCard, t(MODE[face.mode] || ''))),
    h('p', { class: 'muted' }, t('Relai : lecture, écriture, projection par kind'), ' — ', h('a', { href: '#/locks' }, t('réglages avancés du relai')), '.'));
}
