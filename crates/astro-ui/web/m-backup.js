'use strict';
// Moi › Sauvegarde : tout ce qui protège le membre, sur une seule page. En tête, un bilan en trois lignes (le compte, les
// fichiers, les publications), chacune menant à sa carte. Puis ce qui demande un geste (un ami en détresse, le retour des
// fichiers), les gardiens (qui hébergent la sauvegarde ET gardent une part de la clé : une seule façon d'en demander un),
// la procédure « Si je perds tout », la répartition des données, ce que l'on garde pour ses amis, et, repliés, les copies chez
// les autres stations (m-replication.js) et les clés.
export default async function backupPage(root, K) {
  const { h, t, head } = K, soft = p => K.api(p).catch(() => null);
  root.replaceChildren(h('p', { class: 'muted' }, t('Chargement…')));
  const css = h('link', { rel: 'stylesheet', href: '/ui/m-account.css' }); document.head.append(css); K.cleanup(() => css.remove());
  const fpP = soft('/api/v1/me/footprint');
  const [bk, a, rep, fp] = await Promise.all([soft('/api/v1/backup'), K.api('/api/v1/account'), soft('/api/v1/replication'), fpP]);
  const redraw = () => backupPage(root, K);
  const post = (path, body) => K.api(path, { method: 'POST', body }).then(redraw, e => K.toast(e.message));
  const gb = n => K.bytes(n);

  // ── Gardiens (m-guard.js) et retour des fichiers après un rétablissement ──
  const [back, guard] = bk?.guardians ? (await K.mod('m-guard.js')).default(bk, K, redraw) : [null, null];

  // ── Ma demande en cours (l'ami qui héberge) : dans la carte des gardiens quand elle existe, sinon sa propre carte ──
  const mine = bk?.mine, STATE = { pending: 'En attente de réponse', accepted: 'Acceptée', declined: 'Refusée', revoked: 'Révoquée' };
  const status = mine?.enabled ? [
    h('h3', {}, t('Ma sauvegarde chez un ami')),
    h('p', {}, h('span', { class: mine.status === 'accepted' ? 'pill ok' : 'pill' }, t(STATE[mine.status] || mine.status)), ` ${mine.label}`),
    mine.status === 'accepted' ? h('p', {}, t('{0} fichier(s) · {1} sur {2}', mine.files, gb(mine.backed_up_bytes), gb(mine.quota_bytes))) : null,
    h('p', {}, K.sure(t('Suspendre'), () => post('/api/v1/backup', { enabled: false }), t('Suspendre la sauvegarde chez {0} ? Plus rien ne lui est envoyé.', mine.label)))] : [];
  let own = null;
  if (guard) guard.append(...status.filter(Boolean));
  else if (bk) {
    // Station sans gardiens (ancienne API) : le choix de l'ami se fait ici.
    own = h('section', { class: 'card', id: 'ac-guard' }, head(t('Sauvegarde privée'), t('Vos fichiers et contacts non partagés sont copiés, déjà chiffrés, chez un ami qui l\'accepte : il ne peut rien lire et la copie compte dans son quota. Envoi une fois par nuit (20h12 solaire).')));
    const friend = h('select', { id: 'bk-friend', 'aria-label': t('Ami qui héberge') }, bk.friends.map(f => h('option', { value: f.hex }, f.label)));
    if (mine?.enabled) own.append(...status.filter(Boolean));
    else if (!bk.friends.length) own.append(h('p', { class: 'muted' }, t('Aucun proche réciproque : suivez-vous mutuellement pour choisir un hébergeur.')));
    else own.append(h('label', { for: 'bk-friend' }, t('Ami qui héberge ma sauvegarde')), friend, h('p', {}, h('button', { onclick: () => post('/api/v1/backup', { enabled: true, host: friend.value }) }, t('Demander à cet ami'))));
  }

  // ── Pour mes amis : leurs demandes, ce que je garde pour eux, et leurs appels au secours (toujours en tête de page) ──
  const alerts = [], hosting = h('section', { class: 'card', id: 'bk-hosting', hidden: true }, head(t('Ce que je garde pour mes amis'), t('Leurs sauvegardes sont chiffrées : vous ne pouvez rien y lire. Elles comptent dans votre espace.')));
  for (const r of bk?.hosting || []) {
    hosting.hidden = false;
    const size = h('input', { type: 'number', min: 1, step: 1, value: Math.max(1, Math.round(r.quota_bytes / 2 ** 30)) || 5, 'aria-label': t('Volume en Go'), class: 'bk-size' });
    hosting.append(h('div', { class: 'bk-friend' }, h('p', {}, h('strong', {}, r.label), r.status === 'pending' ? ' ' + t('demande à vous confier sa sauvegarde chiffrée.') : ' · ' + t('{0} sur {1} hébergés pour lui', gb(r.used_bytes), gb(r.quota_bytes))),
      r.status === 'pending' ? h('p', { class: 'row' }, h('label', {}, t('Volume mis à disposition (Go)'), ' ', size), ' ', h('button', { onclick: () => post('/api/v1/backup/respond', { owner: r.owner, accept: true, quota_bytes: Math.round(Number(size.value) * 2 ** 30) }) }, t('Accepter')), ' ', h('button', { class: 'ghost', onclick: () => post('/api/v1/backup/respond', { owner: r.owner, accept: false }) }, t('Refuser')))
        : h('p', {}, K.sure(t('Révoquer et libérer l\'espace'), () => post('/api/v1/backup/respond', { owner: r.owner, accept: false }), t('Révoquer : sa sauvegarde sera supprimée de votre espace.')))));
    // Secours : cet ami dit avoir perdu sa station. Je confirme seulement après lui avoir parlé : sa part part alors seule.
    if (r.rescue) {
      const ok = h('input', { type: 'checkbox' }), answer = accept => post('/api/v1/backup/rescue', { owner: r.owner, accept });
      const yes = h('button', { disabled: true, onclick: () => answer(true) }, t('Rendre son compte à {0}', r.label));
      ok.onchange = () => { yes.disabled = !ok.checked; };
      alerts.push(h('section', { class: 'card wide warn', role: 'alert' },
        h('p', {}, h('strong', {}, t('🆘 {0} dit avoir perdu sa station', r.label)), ' ', t('et veut retrouver son compte sur « {0} ».', r.rescue.station)),
        h('p', {}, t('Appelez {0} avant de confirmer. Si vous ne reconnaissez pas sa voix, refusez : votre Capitaine sera prévenu.', r.label)),
        h('p', {}, h('label', {}, ok, ' ', t('J\'ai parlé à {0} et je l\'ai reconnu(e)', r.label))),
        h('p', {}, yes, ' ', h('button', { class: 'ghost danger', onclick: () => answer(false) }, t('Ce n\'est pas {0}', r.label)))));
    }
    // « J'appelle un ami » : cet ami me confie une part de sa clé de secours ; je la lui montre au téléphone (texte ou QR).
    if (r.share) {
      const shown = h('div', { hidden: true });
      const svg = r.share_svg && new DOMParser().parseFromString(r.share_svg, 'image/svg+xml').documentElement;
      if (svg && svg.nodeName === 'svg') shown.append(document.importNode(svg, true));
      shown.append(h('p', { class: 'muted bk-share' }, r.share));
      hosting.append(h('p', { class: 'muted' }, t('{0} vous a confié une part de sa clé de secours. Seule, elle ne sert à rien. Montrez-la-lui seulement s\'il vous appelle et que vous reconnaissez sa voix.', r.label)),
        h('p', {}, h('button', { class: 'ghost', onclick: ev => { shown.hidden = !shown.hidden; ev.target.textContent = shown.hidden ? t('Montrer sa part de secours') : t('Cacher'); } }, t('Montrer sa part de secours'))), shown);
    }
  }

  // ── Si je perds tout : la procédure de la clé de secours (3 morceaux, 2 suffisent), dite sans cryptographie ──
  const rescue = h('section', { class: 'card', id: 'bk-rescue' }, head(t('Si je perds tout'), t('Votre clé est découpée en 3 morceaux ; 2 suffisent pour tout retrouver : la carte de secours imprimée chez vous, la station, et l\'ami qui garde votre sauvegarde.')),
    a.guest ? h('p', { class: 'warn' }, h('strong', {}, t('Compte invité. ')), t('Gardez votre clé de secours et votre code PASS.')) : null,
    h('details', { class: 'fold' }, h('summary', {}, t('J\'ai oublié mon code PASS')), h('p', {}, t('Votre station marche toujours : demandez à son Capitaine un code de secours. Rien n\'est perdu.'))),
    h('details', { class: 'fold' }, h('summary', {}, t('Ma station est détruite, ou je déménage')), h('ol', {},
      h('li', {}, t('Sur une autre station de la même constellation, ouvrez la page « Rétablir mon compte » (adresse /restore).')),
      h('li', {}, t('Scannez votre carte de secours imprimée.')),
      h('li', {}, t('Appuyez sur « Prévenir mes gardiens », puis appelez l\'ami qui garde votre sauvegarde : il confirme d\'un geste dans Moi › Sauvegarde. Reconnaître votre voix est la seule vérification.')),
      h('li', {}, t('Dès qu\'il confirme, votre compte renaît tout seul. Notez le nouveau code PASS affiché.')),
      h('li', {}, t('Vos amis, vos contacts et vos fichiers reviennent d\'eux-mêmes au fil des heures.')))));

  // ── Où sont vos données (m-start.js), avec l'empreinte déjà chargée ──
  const spread = h('section', { class: 'card', id: 'bk-spread', hidden: true });
  K.mod('m-start.js').then(m => m.spread(K, spread, fpP), () => {});

  // ── Copies de vos publications chez les autres stations (m-replication.js), chargées à l'ouverture seulement ──
  const repBox = h('div');
  const copies = h('details', { class: 'card wide fold', id: 'bk-copies' }, h('summary', {}, t('Copies chez les autres stations'), ' ', h('small', { class: 'muted' }, t('vos publications et vos fichiers, station par station'))), repBox);
  copies.addEventListener('toggle', () => { if (copies.open && !repBox.firstChild) K.mod('m-replication.js').then(m => m.default(repBox, K), e => repBox.append(h('p', { class: 'err', role: 'alert' }, e.message))); });

  // ── Mes clés : l'identité et l'export, repliés (action délibérée, tracée) ──
  const code = v => h('code', {}, v || '—'), out = h('div', {});
  const copy = (text, what) => h('button', { class: 'ghost', type: 'button', onclick: () => navigator.clipboard?.writeText(text).then(() => K.toast(t('{0} copié', what), 1), e => K.toast(e.message)) }, 'Copier');
  const ok = h('input', { type: 'checkbox', id: 'ac-ok' }), exp = h('button', { class: 'ghost', disabled: true, onclick: async ev => {
    ev.target.disabled = true;
    try {
      const k = await K.api('/api/v1/account/keys', { method: 'POST', body: { confirm: true } });
      out.replaceChildren(h('p', { class: 'degraded', role: 'alert' }, 'Ces clés donnent le contrôle total du compte : ne les copiez que dans un gestionnaire de mots de passe.'),
        h('label', {}, 'Clé secrète Nostr (nsec)'), h('textarea', { rows: 2, readOnly: true, 'aria-label': 'nsec' }, k.nsec),
        ...(k.dunikey ? [h('label', {}, 'Clé Ğ1 (.dunikey)'), h('textarea', { rows: 4, readOnly: true, 'aria-label': 'dunikey' }, k.dunikey)] : [h('p', { class: 'err', role: 'alert' }, t('Clé Ğ1 non exportée : {0}', k.dunikey_error))]),
        h('p', {}, h('button', { class: 'ghost', onclick: () => out.replaceChildren() }, 'Masquer')));
    } catch (e) { K.toast(e.message); }
    ok.checked = false;
  } }, 'Exporter mes clés');
  ok.onchange = () => { exp.disabled = !ok.checked; };
  const keys = h('details', { class: 'card wide fold', id: 'bk-keys' }, h('summary', {}, t('Mes clés et mon identité'), ' ', h('small', { class: 'muted' }, t('pour reprendre votre compte ailleurs'))),
    h('dl', { class: 'stack' },
      h('dt', {}, 'Compte'), h('dd', {}, a.email || '—'),
      h('dt', {}, 'Clé publique Nostr'), h('dd', {}, code(a.npub), ' ', copy(a.npub, 'npub')),
      h('dt', {}, 'Clé Ğ1'), h('dd', {}, code(a.g1pub)),
      h('dt', {}, 'Station d\'accueil'), h('dd', {}, code(a.home_station)),
      h('dt', {}, 'Relais où vous publiez'), h('dd', {}, a.relays.length ? a.relays.map(r => h('div', {}, code(r.url), ` ${r.read && r.write ? '' : r.read ? t('(lecture)') : t('(écriture)')}`)) : h('span', { class: 'muted' }, t('aucun')))),
    h('p', { class: 'muted' }, t('Votre station garde vos clés. Vous pouvez les récupérer pour reprendre le contrôle de votre identité ailleurs (3 exports par heure ; chaque export est tracé).')),
    h('p', {}, ok, h('label', { for: 'ac-ok' }, ' ' + t('Je comprends que quiconque possède ces clés possède mon compte.'))), h('p', {}, exp), out);

  // ── Bilan : trois réponses à « suis-je à l'abri ? », chacune mène à la carte qui permet d'agir ──
  const n = (bk?.guardians || []).filter(g => !g.same_station).length, sf = bk?.safety || {}, sp = fp?.disk?.spread, m = rep?.mine;
  // Des mots de membre sur chaque pastille, pas l'état d'une machine (« Panne », « Inactif »).
  const WORD = { ok: 'Bien', degraded: 'À renforcer', down: 'À faire', disabled: 'Rappel' };
  const go = el => () => { if (el.tagName === 'DETAILS') el.open = true; el.scrollIntoView({ behavior: 'smooth', block: 'start' }); };
  const line = (icon, what, text, level, el) => el && h('li', {}, h('button', { type: 'button', class: 'bk-line', onclick: go(el) },
    h('span', { class: 'bk-ico', 'aria-hidden': 'true' }, icon), h('span', { class: 't' }, h('b', {}, what), h('small', { class: 'muted' }, text)), h('span', { class: 'pill ' + level }, t(WORD[level]))));
  const acct = bk?.guardians
    ? [n ? (sf.permille != null ? t('{0} % de chances d\'être retrouvé', Math.floor(sf.permille / 10)) : t('{0} gardien(s) hors de cette station', n)) : t('Aucun gardien : si votre station tombe, votre compte est perdu.'), n >= 2 ? 'ok' : n ? 'degraded' : 'down']
    : mine?.status === 'accepted' ? [t('Sauvegardé chez {0}', mine.label), 'ok'] : [t('Aucun ami ne garde encore vos données.'), 'down'];
  const pcFiles = sp?.total ? Math.round(100 * sp.safe / sp.total) : null, pcPub = m?.events ? Math.round(100 * m.replicated / m.events) : null;
  const summary = h('section', { class: 'card wide bk-sum' }, head(t('Suis-je à l’abri ?'), t('Une panne, un vol ou un incendie ne doivent rien vous faire perdre. Chaque ligne mène à ce qui l’améliore.')),
    h('ul', { class: 'bk-lines' },
      line('🗝️', t('Mon compte'), acct[0], acct[1], guard || own),
      pcFiles != null ? line('📁', t('Mes fichiers'), t('{0} % à l’abri chez vos amis', pcFiles), pcFiles >= 90 ? 'ok' : pcFiles ? 'degraded' : 'down', spread) : null,
      pcPub != null ? line('📣', t('Mes publications'), t('{0} % copiées sur d’autres stations', pcPub), pcPub >= 90 ? 'ok' : pcPub ? 'degraded' : 'down', copies) : null,
      line('🖨️', t('Ma carte de secours'), t('Imprimée à la création du compte : rangez-la chez vous.'), 'disabled', rescue)));

  root.replaceChildren(...alerts, back ? h('div', { class: 'grid' }, back) : '', h('div', { class: 'grid' }, summary, guard || own, rescue, spread, hosting, copies, keys));
  // Arrivée par #/backup?open=copies (ancienne adresse de la réplication) : la carte des copies s'ouvre.
  if (new URLSearchParams(location.hash.split('?')[1] || '').get('open') === 'copies') copies.open = true; // l'événement toggle charge les copies
}
