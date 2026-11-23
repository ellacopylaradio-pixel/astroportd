'use strict';
// Moi › Appareils : ce qu'on branche à son compte. D'abord les appareils (un jeton WebDAV par téléphone ou ordinateur, confiné à
// un dossier), puis l'accès complet (jeton principal), puis les assistants : une IA déclarée fait apparaître BRO, la lecture des
// documents rend le texte cherchable, les agents empruntent le navigateur le temps choisi.
const GUIDE = 'https://papiche.github.io/astroportd/APPAREILS.html';

export default async function devicesPage(root, K) {
  const { h, t, tip, head } = K;
  root.replaceChildren(h('p', { class: 'muted' }, t('Chargement…')));
  const css = h('link', { rel: 'stylesheet', href: '/ui/m-account.css' }); document.head.append(css); K.cleanup(() => css.remove());
  const a = await K.api('/api/v1/account');
  const copy = (text, what) => h('button', { class: 'ghost', type: 'button', onclick: () => navigator.clipboard?.writeText(text).then(() => K.toast(t('{0} copié', what), 1), e => K.toast(e.message)) }, 'Copier');
  const code = v => h('code', {}, v || '—');
  // L'API donne un chemin (`/dav`) : on affiche et on copie l'adresse complète, celle qu'on saisit dans un client WebDAV.
  const davUrl = a.dav.url.startsWith('/') ? location.origin + a.dav.url : a.dav.url;
  const guide = h('p', {}, h('a', { href: GUIDE, target: '_blank', rel: 'noopener' }, '📖 ' + t('Guide pas à pas : Android, iPhone, ordinateur')));

  // ── Mes appareils : un jeton par appareil, son envoi arrive dans le dossier choisi, il ne voit ni ne touche rien d'autre ──
  const devices = h('section', { class: 'card' }, head('Appareils', 'Un jeton par appareil : son envoi WebDAV arrive dans le dossier de votre choix, et il ne voit ni ne touche rien d\'autre. Révocable à tout moment.'));
  // Le sens des échanges en un signe : ⬆ l'appareil envoie, ⬇ il lit, ⇅ les deux.
  const MODES = { write: 'Envoi seul', readwrite: 'Lecture et écriture', read: 'Lecture seule' }, SIGN = { write: '⬆', readwrite: '⇅', read: '⬇' };
  const way = m => h('span', { class: 'ac-way ac-way-' + m, title: t(MODES[m]), 'aria-hidden': 'true' }, SIGN[m]);
  const shownToken = h('div');
  const label = h('input', { id: 'dv-label', placeholder: 'Mon téléphone', maxlength: 60 }), dir = h('input', { id: 'dv-root', placeholder: '/Photos/Mon téléphone' });
  const mode = h('select', { id: 'dv-mode' }, Object.entries(MODES).map(([v, n]) => h('option', { value: v }, SIGN[v] + ' ' + t(n))));
  const rows = h('div');
  const form = h('details', { class: 'fold ac-add' }, h('summary', {}, '＋ ' + t('Ajouter un appareil')),
    h('label', { for: 'dv-label' }, 'Nom'), label, h('label', { for: 'dv-root' }, 'Dossier du uCloud (créé s\'il manque)'), dir, h('label', { for: 'dv-mode' }, 'Droits'), mode,
    h('p', {}, h('button', { type: 'button', onclick: async ev => {
      ev.target.disabled = true;
      try {
        const r = await K.api('/api/v1/devices', { method: 'POST', body: { label: label.value, root: dir.value || `/Photos/${label.value.trim()}`, mode: mode.value } });
        const inp = h('input', { value: r.token, readOnly: true, 'aria-label': 'Jeton', onfocus: () => inp.select() });
        shownToken.replaceChildren(h('div', { class: 'card wide', role: 'status' }, h('h3', {}, t('Appareil « {0} » ajouté', r.label)),
          h('p', { class: 'warn', role: 'note' }, r.note),
          h('dl', {}, h('dt', {}, 'Serveur'), h('dd', {}, code(davUrl), ' ', copy(davUrl, 'adresse')), h('dt', {}, 'Identifiant'), h('dd', {}, code(a.dav.login)), h('dt', {}, 'Mot de passe (jeton)'), h('dd', {}, inp, ' ', copy(r.token, 'jeton')), h('dt', {}, 'Dossier du uCloud'), h('dd', {}, code(r.root), ' · ', SIGN[r.mode], ' ', t(MODES[r.mode])))));
        K.toast(t('Appareil ajouté'), 1); label.value = ''; dir.value = ''; form.open = false;
        refresh();
      } catch (e) { K.toast(e.message); } finally { ev.target.disabled = false; }
    } }, 'Ajouter cet appareil')));
  const refresh = async () => {
    const l = await K.api('/api/v1/devices');
    // Sans appareil, le formulaire est ouvert : c'est la seule chose à faire ici.
    if (!l.devices.length) form.open = true;
    rows.replaceChildren(...(l.devices.length ? l.devices.map(d => h('div', { class: 'row' }, way(d.mode),
      h('div', { class: 't' }, h('b', {}, d.label), h('span', { class: 'muted' }, ` · ${t(MODES[d.mode])} · `), code(d.root), h('br'), h('span', { class: 'muted' }, d.last_used ? t('Dernier envoi : {0}', K.when(d.last_used)) : t('Jamais utilisé'))),
      K.sure('Révoquer', async ev => { ev.target.disabled = true; try { await K.api('/api/v1/devices/revoke', { method: 'POST', body: { id: d.id } }); K.toast(t('Appareil révoqué'), 1); } catch (e) { K.toast(e.message); } refresh(); }, t('Révoquer « {0} » ? Son jeton cesse de fonctionner tout de suite ; ce qu\'il a déposé reste dans votre uCloud.', d.label))))
      : [h('p', { class: 'muted' }, t('Aucun appareil branché pour l’instant.'))]));
  };
  devices.append(rows, shownToken, form, guide);
  try { await refresh(); } catch (e) { devices.append(h('p', { class: 'muted' }, e.message)); }

  // ── Accès complet (WebDAV) : le jeton principal ouvre tout l'uCloud ; on peut le réafficher sans le changer (route tracée) ──
  const shown = h('div', {});
  const rotate = K.sure('Remplacer le jeton principal', async ev => {
    ev.target.disabled = true;
    try {
      const r = await K.api('/api/v1/account/token', { method: 'POST', body: {} });
      K.login(r.login, r.token); // cette session suit le nouveau jeton
      shown.replaceChildren(h('p', { class: 'degraded', role: 'status' }, 'Nouveau jeton — il n\'est montré qu\'une fois :'), h('p', {}, code(r.token), ' ', copy(r.token, 'jeton')));
      K.toast(t('Jeton changé'), 1);
    } catch (e) { K.toast(e.message); }
    ev.target.disabled = false;
  }, t('Changer le jeton ? L\'ancien cesse de fonctionner tout de suite : téléphone, DAVx⁵ et autres clients WebDAV devront être reconfigurés.'));
  const reveal = () => h('button', { type: 'button', class: 'ghost', onclick: async ev => {
    ev.target.disabled = true;
    try {
      const r = await K.api('/api/v1/account/token/show', { method: 'POST', body: {} });
      const inp = h('input', { value: r.token, readOnly: true, 'aria-label': t('Jeton principal'), class: 'ac-token', onfocus: () => inp.select() });
      tokenBox.replaceChildren(inp, ' ', copy(r.token, 'jeton'), ' ', h('button', { type: 'button', class: 'ghost', onclick: () => tokenBox.replaceChildren(reveal(), gen) }, t('Masquer')));
    } catch (e) { K.toast(e.message); ev.target.disabled = false; }
  } }, '👁 ' + t('Afficher'));
  const gen = a.token_generation ? h('small', { class: 'muted' }, ' ', t('remplacé {0} fois', a.token_generation)) : '';
  const tokenBox = h('dd', {}, reveal(), gen);
  const dav = h('section', { class: 'card' }, head(t('Accès complet (WebDAV)'), 'Pour monter vos fichiers (uCloud) sur un ordinateur ou un téléphone.', t('Le jeton principal ouvre tout votre uCloud, contacts et agenda compris : gardez-le pour vos propres appareils.')),
    h('dl', {}, h('dt', {}, 'Adresse'), h('dd', {}, code(davUrl), ' ', copy(davUrl, 'adresse')), h('dt', {}, 'Identifiant'), h('dd', {}, code(a.dav.login), ' ', copy(a.dav.login, t('Identifiant'))), h('dt', {}, t('Jeton principal')), tokenBox),
    h('details', { class: 'fold' }, h('summary', {}, 'Remplacer le jeton principal'),
      h('div', { class: 'ac-head' }, h('span', { class: 'muted' }, t('Seulement s’il a fuité.')), tip('À faire seulement si vous pensez qu\'il a fuité : l\'ancien cesse de fonctionner tout de suite, et les clients WebDAV branchés avec lui (ordinateur, DAVx⁵…) devront être reconfigurés. Pour brancher un téléphone, créez plutôt un jeton d\'appareil (carte « Appareils »).')),
      h('p', {}, rotate)), shown);

  // ── Assistants : l'IA de la station ou d'un fournisseur fait apparaître BRO (carte m-ai.js) ──
  const ai = h('section', { class: 'card', id: 'ai' });
  K.mod('m-ai.js').then(m => m.default(ai, K), e => ai.append(h('p', { class: 'err', role: 'alert' }, e.message)));

  // ── Lecture de vos documents : sa station lit, ou une station amie que le membre choisit (m-reading.js) ──
  const reading = h('section', { class: 'card', id: 'reading' });
  K.mod('m-reading.js').then(m => m.default(reading, K), e => reading.append(h('p', { class: 'err', role: 'alert' }, e.message)));

  // Agents (OpenClaw, Claude…) qui demandent à agir pour le membre sur un site : écran m-agents.js.
  const agentsCard = h('section', { class: 'card' }, head(t('Mes agents'), t('Des assistants (IA, robots) peuvent agir pour vous sur un site où vous êtes inscrit : vous vous connectez vous-même, puis vous leur prêtez le navigateur, le temps choisi.')),
    h('p', {}, h('a', { class: 'btn', href: '#/agents' }, '🤖 ' + t('Gérer mes agents'))));

  root.replaceChildren(h('div', { class: 'grid' }, devices, dav),
    h('h2', { class: 'ac-sec' }, t('Assistants')), h('div', { class: 'grid' }, ai, reading, agentsCard));

  // Arrivée depuis un lien qui vise une carte (« Documents et photos cherchables » depuis la recherche) : on l'amène sous les yeux.
  // Les cartes du dessus finissent de se charger après coup et la repousseraient : on la garde en vue 2 s au plus, sauf si le
  // membre fait défiler lui-même.
  const aim = new URLSearchParams(location.hash.split('?')[1] || '').get('card'), target = { reading, ai }[aim];
  if (target) {
    const keep = () => target.scrollIntoView({ block: 'start' });
    const ro = new ResizeObserver(keep), stop = () => { ro.disconnect(); ['wheel', 'touchstart', 'keydown'].forEach(e => removeEventListener(e, stop)); };
    ['wheel', 'touchstart', 'keydown'].forEach(e => addEventListener(e, stop, { passive: true }));
    ro.observe(root); setTimeout(stop, 2000); K.cleanup(stop);
    requestAnimationFrame(() => { keep(); target.tabIndex = -1; target.focus({ preventScroll: true }); });
  }
}
