'use strict';
// Station › Membres : la liste des comptes hébergés, réglable sur place comme l'administration des utilisateurs d'un
// Nextcloud. Chaque cellule se modifie dans la ligne et part tout de suite (la vue revenue fait foi) : quota et prix de
// l'hébergement, Ğ1Billets, lecture des documents (IA), IA des membres ; jeton de secours, PASS, éjection d'un compte jamais
// ouvert, retrait d'un compte inactif depuis plus de 30 jours (réversible 30 jours, src/member_activity.rs). Une aide « ? »
// par colonne. Tri par dernière activité, disque occupé (%) ou ancienneté. Réglages généraux repliés en tête (m-aiadmin.js).
// À 400 px, chaque ligne devient une carte (m-members.css).
const LIFE = { active_rental: 'Locataire', cooperative_member: 'Coopérateur', grace_period: 'En grâce', suspended: 'Suspendu' };
const DAY = 86400, MONTH = 30 * DAY;
export const FILTERS = [['all', 'Tous', () => true], ['ok', 'Actifs', a => !a.removed_at && !['grace_period', 'suspended'].includes(a.status)], ['idle', 'Inactifs +30 j', a => !!a.removable], ['removed', 'Retirés', a => !!a.removed_at], ['grace_period', 'En grâce', a => a.status === 'grace_period'], ['suspended', 'Suspendus', a => a.status === 'suspended'], ['locked', 'PASS verrouillé', a => !!a.pass_locked_until]];
// Part du disque occupée (0…1) ; un compte sans quota (Capitaine) ne pèse rien.
export const usage = a => (a.unlimited || !a.quota_bytes ? 0 : Math.min(a.used_bytes / a.quota_bytes, 1));
// Tris : dernière activité (les plus anciennes d'abord : ce sont elles qu'on regarde), disque (%), ancienneté, adresse.
export const SORTS = {
  idle: [(x, y) => (x.last_activity ?? 0) - (y.last_activity ?? 0), 'Les moins actifs d’abord'],
  recent: [(x, y) => (y.last_activity ?? 0) - (x.last_activity ?? 0), 'Les plus actifs d’abord'],
  disk: [(x, y) => usage(y) - usage(x) || y.used_bytes - x.used_bytes, 'Disque le plus rempli'],
  since: [(x, y) => (x.since ?? Infinity) - (y.since ?? Infinity), 'Les plus anciens'],
  name: [(x, y) => (x.email || x.hex).localeCompare(y.email || y.hex), 'Adresse'],
};
export const sorted = (list, k) => [...list].sort((SORTS[k] || SORTS.idle)[0]);
// « il y a 3 j » : durée écoulée lisible, `null` sans date.
export const ago = (t, now) => (t ? Math.max(0, Math.floor((now - t) / DAY)) : null);

export default async function members(root, K, q) {
  const { h, t, api, toast, tip } = K;
  const css = h('link', { rel: 'stylesheet', href: '/ui/m-members.css' }); document.head.append(css); K.cleanup(() => css.remove());
  let flt = FILTERS.some(f => f[0] === q?.get('f')) ? q.get('f') : 'all', sort = SORTS[q?.get('s')] ? q.get('s') : 'idle', find = '';
  const AA = await K.mod('m-aiadmin.js');
  let data = null;
  const keep = () => history.replaceState(null, '', '#/members' + (flt === 'all' && sort === 'idle' ? '' : `?f=${flt}&s=${sort}`));

  // Envoi d'un réglage : le champ est figé le temps de l'aller-retour ; en cas d'erreur, il revient et le dit.
  const busy = async (el, run) => {
    if (el) el.disabled = true;
    try { await run(); toast(t('Réglage enregistré'), 1); await draw(); } catch (e) { toast(e.message); if (el) el.disabled = false; }
  };
  const token = async a => {
    const dlg = document.getElementById('share'), box = document.getElementById('share-b');
    document.getElementById('share-t').textContent = t('Jeton de secours');
    box.replaceChildren(h('p', { class: 'muted' }, t('Génération…')));
    if (!dlg.open) dlg.showModal();
    try {
      const r = await api(`/api/v1/station/accounts/${a.hex}/token`, { method: 'POST' });
      const inp = h('input', { value: r.token, readOnly: true, 'aria-label': t('Jeton'), onfocus: () => inp.select() });
      toast(t('Jeton généré'), 1);
      box.replaceChildren(h('p', { class: 'muted' }, t('Compte {0}. À donner de vive voix ou par canal chiffré. Action tracée dans le journal.', r.login)), inp);
    } catch (e) { box.replaceChildren(h('p', { class: 'err', role: 'alert' }, e.message)); }
  };
  const edit = h('section', { class: 'card wide', hidden: true });
  const defaults = async () => { edit.hidden = false; (await K.mod('m-hosting.js')).default(edit, K, null, data.hd); edit.scrollIntoView?.({ behavior: 'smooth' }); };

  // ── Hébergement dans la ligne : quota (Go) et prix (ẑ/semaine) ; vide = palier par défaut. Le serveur attend les deux. ──
  const hostCell = a => {
    if (a.unlimited) return h('span', { class: 'muted' }, t('illimité (Capitaine)'));
    if (a.removed_at) return h('span', { class: 'muted' }, '—');
    const gb = b => (b ? String(Math.round(b / 2 ** 30)) : '');
    const id = a.hex.slice(0, 12), d = data.hd;
    const qi = h('input', { id: 'mb-q-' + id, inputmode: 'decimal', value: gb(a.own_quota_bytes), placeholder: d.default_quota_bytes == null ? t('offre') : gb(d.default_quota_bytes), 'aria-label': t('Quota de {0}, en Go (vide : palier par défaut)', a.email || id) });
    const zi = h('input', { id: 'mb-z-' + id, inputmode: 'numeric', value: a.own_weekly_zen ?? '', placeholder: String(d.default_weekly_zen || 0), 'aria-label': t('Prix de {0}, en ẑ par semaine (0 : gratuit ; vide : prix par défaut)', a.email || id) });
    const send = el => {
      const g = qi.value.trim() === '' ? null : Number(qi.value.replace(',', '.')), z = zi.value.trim() === '' ? null : Number(zi.value);
      if (g !== null && !(g >= 0 && Number.isFinite(g))) return toast(t('Nombre de Go attendu'));
      if (z !== null && !(Number.isInteger(z) && z >= 0)) return toast(t('Nombre entier de ẑ attendu'));
      busy(el, () => api(`/api/v1/station/accounts/${a.hex}/hosting`, { method: 'POST', body: { quota_bytes: g === null ? null : Math.round(g * 2 ** 30), weekly_zen: z } }));
    };
    qi.onchange = () => send(qi); zi.onchange = () => send(zi);
    const bi = h('input', { type: 'checkbox', id: 'mb-b-' + id, checked: !!a.billet_issuer });
    bi.onchange = () => busy(bi, () => api(`/api/v1/station/accounts/${a.hex}/hosting`, { method: 'POST', body: { quota_bytes: a.own_quota_bytes ?? null, weekly_zen: a.own_weekly_zen ?? null, billet_issuer: bi.checked } }));
    return h('span', { class: 'mb-host' },
      h('label', { class: 'mb-in' }, qi, ' ', t('Go')), h('label', { class: 'mb-in' }, zi, ' ', t('ẑ/sem.')),
      a.role === 'multipass' ? h('label', { for: bi.id, class: 'mb-chk' }, bi, ' 🎫 ', t('Ğ1Billets')) : null,
      h('small', { class: 'muted' }, t('Prélevé : {0}', a.weekly_zen > 0 ? `${a.weekly_zen} ẑ / ${t('semaine')}` : t('gratuit'))));
  };

  // ── Lecture des documents (IA) : interrupteur par compte ; la vue revenue fait foi. ──
  const IX = '/api/v1/station/admin/index';
  const setIndex = (body, input) => busy(input, async () => { data.ix = await api(IX, { method: 'POST', body }); });
  const sw = (id, on, label, onchange, disabled) => {
    const input = h('input', { type: 'checkbox', role: 'switch', id, checked: on, disabled: !!disabled });
    input.onchange = () => onchange(input.checked, input);
    return h('label', { for: id, class: 'ix-sw' }, input, ' ', label);
  };
  const readCell = a => {
    const m = data.ix?.members.find(x => x.hex === a.hex);
    if (!m) return h('span', { class: 'muted' }, '—');
    const off = !data.ix.enabled;
    return h('span', { class: 'ix-cell' },
      sw('ix-m-' + a.hex.slice(0, 12), m.enabled && !off, off ? t('Coupée') : m.enabled ? t('Activée') : t('Désactivée'), (on, input) => setIndex({ member: { hex: a.hex, enabled: on } }, input), off),
      h('small', { class: 'muted' }, [m.own ? null : t('par défaut'), m.read ? t('{0} lu(s)', m.read) : null, m.friend ? t('via {0}', m.friend) : null].filter(Boolean).join(' · ')),
      m.own ? h('button', { type: 'button', class: 'ghost ix-reset', title: t('Revenir au réglage par défaut'), onclick: ev => setIndex({ member: { hex: a.hex, enabled: null } }, ev.target) }, t('Par défaut')) : null);
  };
  const indexBody = ix => {
    const lend = ix.lend === 'off' || ix.lend === 'friends'
      ? sw('ix-lend', ix.lend === 'friends', t('Prêter la puissance de cette station à mes Capitaines amis'), (on, el) => setIndex({ lend: on ? 'friends' : 'off' }, el))
      : ix.lend ? h('p', { class: 'muted' }, t('Prêt aux amis : réglage particulier ({0}), voir Réseau › Partages.', ix.lend)) : null;
    return [h('p', { class: 'muted' }, ix.engine.length ? t('Cette station lit elle-même le texte des images et des PDF de ses membres.') : t('Pas de moteur sur cette station : chaque membre peut confier la lecture à une station amie, s’il le souhaite (Mon compte).')),
      h('p', {}, sw('ix-on', ix.enabled, t('Lecture des documents sur cette station'), (on, el) => setIndex({ enabled: on }, el))),
      h('p', {}, sw('ix-def', ix.default, t('Activée pour les comptes que je n’ai pas réglés un par un'), (on, el) => setIndex({ default: on }, el), !ix.enabled)),
      lend ? h('p', {}, lend) : null,
      h('p', { class: 'muted' }, t('Un compte désactivé n’est plus lu, ni ici ni chez une station amie ; ce qui a déjà été lu reste dans sa recherche. Réglage par compte : colonne « Lecture » ci-dessous.'))];
  };

  // ── Suppression complète : sans retour. Elle dit ce qui sera perdu et ne part qu'une fois l'adresse retapée. ──
  const wipe = a => {
    const dlg = document.getElementById('share'), box = document.getElementById('share-b');
    document.getElementById('share-t').textContent = t('Supprimer {0} sans retour', a.email);
    const inp = h('input', { id: 'mb-wipe', autocomplete: 'off', spellcheck: false, 'aria-label': t('Retapez l’adresse du compte pour confirmer'), placeholder: a.email });
    const err = h('p', { class: 'err', role: 'alert' });
    const go = h('button', { type: 'button', class: 'danger', disabled: true }, t('Supprimer définitivement'));
    inp.oninput = () => { go.disabled = inp.value.trim().toLowerCase() !== a.email.toLowerCase(); };
    go.onclick = async () => {
      go.disabled = inp.disabled = true; err.textContent = '';
      try {
        const r = await api(`/api/v1/station/accounts/${a.hex}/delete`, { method: 'POST', body: { confirm: inp.value.trim() } });
        dlg.close(); toast(t('{0} supprimé : {1} fichier(s), {2} effacé(s)', r.deleted, r.files, K.bytes(r.bytes)), 1); await draw();
      } catch (e) { err.textContent = e.message; inp.disabled = false; go.disabled = false; }
    };
    box.replaceChildren(
      h('p', {}, h('strong', {}, t('On ne peut pas revenir en arrière.'))),
      h('ul', {}, [t('{0} fichier(s) de son uCloud, {1} : effacés (ceux qu’un autre membre a aussi restent pour lui)', a.files, K.bytes(a.used_bytes)), t('sa recherche, ses visages, ses messages, son carnet, ses appareils et ses réglages'),
        t('ses publications : un adieu signé de sa clé demande aux relais de les oublier'), t('ses clés : effacées, même celles de la corbeille'), t('ses Ğ1 reviennent d’abord à la coopérative')].map(x => h('li', {}, x))),
      h('label', { for: 'mb-wipe' }, t('Retapez l’adresse du compte pour confirmer')), inp, err,
      h('p', { class: 'row' }, go, ' ', h('button', { type: 'button', class: 'ghost', onclick: () => dlg.close() }, t('Annuler'))));
    if (!dlg.open) dlg.showModal();
    inp.focus?.();
  };

  // ── Actions : chacune dit ce qu'elle fait ; les irréversibles se confirment en deux temps (K.sure, ou l'adresse retapée). ──
  const actions = a => [
    a.removed_at ? null : h('button', { class: 'ghost', title: t('Nouveau jeton de connexion, à remettre au membre qui a perdu le sien'), onclick: () => token(a) }, t('Jeton')),
    a.pass_locked_until && !a.removed_at ? h('button', { title: t('Trop de codes PASS faux : le compte attend. Le lever tout de suite.'), onclick: ev => busy(ev.target, () => api(`/api/v1/station/accounts/${a.hex}/unlock-pass`, { method: 'POST' })) }, t('Déverrouiller')) : null,
    a.onboarding?.ejectable && !a.removed_at ? K.sure(t('Éjecter'), () => busy(null, () => api(`/api/v1/station/accounts/${a.hex}/eject`, { method: 'POST' })), t('Aucune connexion en 24 h ni Ğ1 reçue : ses clés sont rangées, l\'adresse pourra se réinscrire.')) : null,
    a.removable && !a.onboarding?.ejectable ? K.sure(t('Retirer le compte'), () => busy(null, () => api(`/api/v1/station/accounts/${a.hex}/remove`, { method: 'POST' })), t('Plus de 30 jours sans activité : ses Ğ1 reviennent à la coopérative, ses clés vont à la corbeille 30 jours (récupérables), il n’est plus hébergé ici.'), { class: 'ghost danger' }) : null,
    a.deletable ? h('button', { type: 'button', class: 'ghost danger', title: t('Tout effacer, sans retour'), onclick: () => wipe(a) }, t('Supprimer…')) : null,
  ].filter(Boolean);

  // ── Identité d'une ligne, tirée de son profil public : bandeau discret, logo rond (ou l'initiale), nom, adresse, site.
  // Les images viennent de cette station seulement (`/ipfs/<cid>`, la CSP n'admet rien d'autre) ; le site, en http(s). ──
  const short = u => { try { const x = new URL(u); return (x.host + x.pathname).replace(/\/$/, '').replace(/^www\./, ''); } catch { return u; } };
  const ident = a => {
    const p = a.profile || {}, label = p.name || a.email || a.hex.slice(0, 12);
    return [
      p.banner ? h('img', { class: 'mb-ban', src: p.banner, alt: '', loading: 'lazy', decoding: 'async' }) : null,
      h('span', { class: 'mb-id' },
        p.picture ? h('img', { class: 'mb-av', src: p.picture, alt: '', width: 40, height: 40, loading: 'lazy', decoding: 'async' }) : h('span', { class: 'mb-av', 'aria-hidden': 'true' }, [...label][0].toUpperCase()),
        h('span', { class: 'mb-names' }, h('strong', {}, label), p.name && a.email ? h('small', { class: 'muted' }, a.email) : null)),
      p.website ? h('a', { class: 'mb-site', href: p.website, target: '_blank', rel: 'noopener noreferrer external', title: p.website }, '↗ ', short(p.website)) : null,
    ].filter(Boolean);
  };

  // ── Colonnes : titre, aide « ? », contenu. ──
  const now = () => Math.floor(Date.now() / 1000);
  const COLS = () => [
    [t('Compte'), [t('Adresse du MULTIPASS et son état : locataire (paie l’hébergement), coopérateur (part sociale), en grâce (paiement en retard, 4 semaines pour régulariser), suspendu.'), t('« depuis » : création du compte ici, ou première trace connue.'), t('Logo, bandeau, nom et site : son profil public, quand ses images sont sur cette station.')], a => h('span', { class: 'mb-who' }, ...ident(a),
      a.removed_at ? h('span', {}, K.pill('disabled'), ' ', t('Retiré le {0}', new Date(a.removed_at * 1000).toLocaleDateString())) : h('span', {}, K.pill(a.status === 'grace_period' ? 'degraded' : a.status === 'suspended' ? 'down' : a.status ? 'ok' : 'disabled'), ' ', t(LIFE[a.status] || a.status || 'inconnu'), a.grace_ends_at ? ` (${t('jusqu\'au')} ${new Date(a.grace_ends_at * 1000).toLocaleDateString()})` : ''),
      h('small', { class: 'muted' }, [a.since ? t('depuis {0} ({1} j)', new Date(a.since * 1000).toLocaleDateString(), ago(a.since, now())) : null, a.onboarding ? (a.onboarding.proved_at ? t('clés reçues') : t('jamais connecté')) : null, a.onboarding?.primo === 'paid' ? '1 Ğ1' : null].filter(Boolean).join(' · ')))],
    [t('Activité'), [t('La plus récente de ses traces : connexion, fichier déposé, message ou note publiée, appareil synchronisé.'), t('Après 30 jours sans rien, le compte peut être retiré (bouton « Retirer le compte »).')], a => {
      const d = ago(a.last_activity, now());
      return h('span', { class: 'mb-act' + (a.removable ? ' mb-idle' : '') }, d == null ? t('aucune trace') : d === 0 ? t('aujourd’hui') : t('il y a {0} j', d), a.last_activity ? h('small', { class: 'muted' }, K.when(a.last_activity)) : null);
    }],
    [t('Disque'), [t('Place occupée dans son uCloud chiffré (et ses images publiques), sur son quota.'), t('Le pourcentage sert au tri « Disque le plus rempli ».')], a => h('span', { class: 'mb-disk' },
      h('meter', { min: 0, max: 1, low: .7, high: .9, optimum: 0, value: usage(a), 'aria-label': t('Disque') }),
      h('small', {}, `${K.bytes(a.used_bytes)} / ${a.unlimited ? t('illimité') : K.bytes(a.quota_bytes)}${a.unlimited ? '' : ` · ${Math.round(usage(a) * 100)} %`} · ${t('{0} fichier(s)', a.files)}`))],
    [t('Hébergement'), [t('Quota en Go et prix en ẑ par semaine propres à ce compte. Vide : le palier par défaut (en gris).'), t('🎫 Ğ1Billets : le membre peut créer des billets ; l’option apparaît dans son interface.'), t('Chaque champ est enregistré dès que vous le quittez.')], hostCell],
    ...(data.ix ? [[t('Lecture'), [t('Lecture du texte des images et des PDF du membre, pour sa recherche. « par défaut » : suit le réglage général, plus haut.')], readCell]] : []),
    ...(data.ai ? [[t('IA'), [t('Ce que le membre peut utiliser comme IA (aucune, celle de la station, ou la sienne), son quota de jetons et son prix par semaine.')], a => AA.cell(K, data.ai, a, v => { data.ai = v; paint(); })]] : []),
    [t('Actions'), [t('Jeton : nouveau jeton de connexion, à remettre au membre. Déverrouiller : lève l’attente après des codes PASS faux.'), t('Éjecter : compte jamais ouvert en 24 h. Retirer le compte : plus de 30 jours sans activité ; ses clés restent 30 jours dans la corbeille de la station.'), t('Supprimer… : un compte inactif depuis 30 jours ou déjà retiré, et toutes ses données, sans retour. Il faut retaper son adresse.')], a => h('span', { class: 'acts' }, actions(a))],
  ];

  // Recherche, tri, filtres et liste gardés d'un affichage à l'autre : la saisie dans la recherche ne repeint que la liste
  // (elle ne perd ni le focus ni le curseur).
  const sel = h('select', { id: 'mb-sort', onchange: ev => { sort = ev.target.value; keep(); list(); } }, Object.entries(SORTS).map(([k, [, l]]) => h('option', { value: k, selected: k === sort }, t(l))));
  const search = h('input', { type: 'search', id: 'mb-find', placeholder: t('Chercher une adresse'), 'aria-label': t('Chercher une adresse'), oninput: ev => { find = ev.target.value; list(); } });
  const chipsBox = h('p', { class: 'filters', role: 'group', 'aria-label': t('Filtrer') }), listBox = h('div', { class: 'mb-wrap' });
  const list = () => {
    const { ac } = data, fn = FILTERS.find(f => f[0] === flt)[2], needle = find.toLowerCase();
    const rows = sorted(ac.accounts.filter(a => fn(a) && (!needle || (a.email || a.hex).toLowerCase().includes(needle))), sort);
    chipsBox.replaceChildren(...FILTERS.map(([k, label, f]) => h('button', { type: 'button', class: k === flt ? '' : 'ghost', 'aria-pressed': String(k === flt), onclick: () => { flt = k; keep(); list(); } }, `${t(label)} ${ac.accounts.filter(f).length}`)));
    const cols = COLS();
    listBox.replaceChildren(rows.length ? h('table', { class: 'mb-list' }, h('caption', { class: 'muted' }, t('{0} compte(s) affiché(s)', rows.length)),
      h('thead', {}, h('tr', {}, cols.map(([title, help]) => h('th', { scope: 'col' }, h('span', { class: 'mb-th' }, title, tip(...help)))))),
      h('tbody', {}, rows.map(a => h('tr', { class: a.removable ? 'mb-idle-row' : '' }, cols.map(([title, , cell]) => h('td', { 'data-label': title }, cell(a))))))) : h('p', { class: 'muted' }, t('Aucun compte.')));
  };
  const paint = () => {
    const { ac, hd } = data;
    const fold = (title, kids) => h('details', { class: 'card wide fold' }, h('summary', {}, h('strong', {}, title)), ...kids);
    root.replaceChildren(
      h('div', { class: 'ac-head' }, h('h2', {}, t('Membres'), ' ', h('small', { class: 'muted' }, String(ac.accounts.length))),
        tip(t('Chaque réglage se fait dans la ligne et part tout de suite.'), t('Les comptes les moins actifs viennent d’abord : un compte resté plus de 30 jours sans rien faire peut être retiré.'))),
      h('p', { class: 'muted' }, `${t('Formule par défaut')} : ${hd.default_quota_bytes == null ? t('celle de leur offre') : K.bytes(hd.default_quota_bytes)} · ${hd.default_weekly_zen > 0 ? `${hd.default_weekly_zen} ẑ / ${t('semaine')}` : t('gratuit')} `, h('button', { class: 'ghost', onclick: defaults }, t('Modifier'))),
      edit,
      ...(data.ix ? [fold('📄 ' + t('Analyse des documents et photos (IA)') + ` · ${data.ix.enabled ? t('Activée') : t('Coupée')}`, indexBody(data.ix))] : []),
      ...(data.ai ? [h('div', { class: 'mb-ai' }, AA.card(K, data.ai, v => { data.ai = v; paint(); }))] : []),
      h('div', { class: 'mb-bar' }, chipsBox, h('span', { class: 'mb-tools' }, search, h('label', { for: 'mb-sort', class: 'muted' }, t('Trier')), sel)),
      listBox);
    // La carte de l'IA est longue : repliée elle aussi (son titre reste visible).
    const ai = root.querySelector('.mb-ai > section');
    if (ai) { const d = h('details', { class: 'card wide fold' }, h('summary', {}, h('strong', {}, '🤖 ' + t('IA de vos membres')))); d.append(...[...ai.childNodes].slice(1)); ai.replaceWith(d); }
    list();
  };
  const draw = async () => {
    // La lecture des documents et l'IA sont facultatives : une station qui ne les a pas garde son écran Membres tel quel.
    const [ac, hd, ix, ai] = await Promise.all([api('/api/v1/station/accounts'), api('/api/v1/station/hosting'), api(IX).catch(() => null), AA.load(K)]);
    const open = [...root.querySelectorAll('details.fold')].map(d => d.open);
    data = { ac, hd, ix, ai }; paint();
    root.querySelectorAll('details.fold').forEach((d, i) => { d.open = !!open[i]; });
  };
  await draw();
  // Pas de rafraîchissement pendant une saisie : il effacerait ce qui est en cours d'écriture.
  K.every(30000, () => (root.contains(document.activeElement) && /INPUT|SELECT|TEXTAREA/.test(document.activeElement.tagName) ? null : draw().catch(() => {})));
}
