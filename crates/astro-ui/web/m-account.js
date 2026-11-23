'use strict';
// Moi › Compte : qui je suis (profil publié), mes préférences (langue, style, visio : appliquées tout de suite), mon espace et ce
// que je publie. Les appareils sont dans Moi › Appareils (m-devices.js), la protection dans Moi › Sauvegarde (m-backup.js).
// La station détient les clés de ses comptes : modifier le profil, c'est lui faire signer un nouveau kind 0.
const LANG = { fr: 'Français', en: 'English', es: 'Español' }, ROLE = { multipass: 'MULTIPASS', zencard: 'ZenCard', captain: 'Capitaine', node: 'Nœud', love: 'LOVE' };

export default async function account(root, K) {
  const { h, t } = K, api = (p, body) => K.api('/api/v1/account' + p, body ? { method: 'POST', body } : undefined);
  root.replaceChildren(h('p', { class: 'muted' }, 'Chargement…'));
  // Les adresses d'avant la refonte (#/account?tab=…&card=…) mènent à la page qui a pris le relais.
  const q = new URLSearchParams(location.hash.split('?')[1] || ''), moved = { appareils: '#/devices', securite: '#/backup' }[q.get('tab')] || (q.get('card') === 'reading' ? '#/devices?card=reading' : null);
  if (moved) { location.replace(moved); return; }
  const css = h('link', { rel: 'stylesheet', href: '/ui/m-account.css' }); document.head.append(css); K.cleanup(() => css.remove());
  const a = await api('');
  const copy = (text, what) => h('button', { class: 'ghost', type: 'button', onclick: () => navigator.clipboard?.writeText(text).then(() => K.toast(t('{0} copié', what), 1), e => K.toast(e.message)) }, 'Copier');
  const code = v => h('code', {}, v || '—');
  const { tip, head } = K;

  // Téléverser une image de profil : réduite ici (cadre de l'image ou du bandeau) puis envoyée en JPEG ; la station la range, la publie
  // à son adresse et met le profil (kind 0) à jour. L'aperçu ne s'affiche que pour une image de cette station (CSP).
  const FRAME = { picture: [320, 320], banner: [1200, 400] };
  // Lu avant imageTools, qui s'en sert pour proposer les images de /Public.
  const site = await K.api('/api/v1/account/site').catch(() => null);
  const imageTools = (kind, input) => {
    const file = h('input', { type: 'file', accept: 'image/*', hidden: true }), msg = h('span', { class: 'muted', role: 'status' });
    const pick = h('button', { type: 'button', class: 'ghost', onclick: () => file.click() }, t('Choisir une image sur cet appareil'));
    file.onchange = async () => {
      const f = file.files[0]; file.value = '';
      if (!f) return;
      pick.disabled = true; msg.textContent = t('Envoi…');
      try {
        const [w, hh] = FRAME[kind], bmp = await createImageBitmap(f), k = Math.max(w / bmp.width, hh / bmp.height), c = h('canvas', { width: w, height: hh });
        c.getContext('2d').drawImage(bmp, (w - bmp.width * k) / 2, (hh - bmp.height * k) / 2, bmp.width * k, bmp.height * k);
        const blob = await new Promise(ok => c.toBlob(ok, 'image/jpeg', .85)), data = await new Promise(ok => { const r = new FileReader(); r.onload = () => ok(String(r.result).split(',')[1]); r.readAsDataURL(blob); });
        await api('/image', { kind, data, origin: location.origin }); K.toast(t('Image enregistrée'), 1); account(root, K);
      } catch (e) { msg.textContent = ''; K.toast(e.message); pick.disabled = false; }
    };
    // Choisir une image déjà publiée dans /Public : la station la relit elle-même (aucun autre dossier n'est accepté).
    const grid = h('div', { class: 'ac-grid' });
    const pickPublic = h('button', { type: 'button', class: 'ghost', onclick: async () => {
      let r; try { r = await K.api('/api/v1/public/images'); } catch (e) { return K.toast(e.message); }
      grid.replaceChildren(...(r.images.length ? r.images.slice(0, 24).map(im => h('button', { type: 'button', class: 'ac-thumb', title: im.name, onclick: async () => {
        try { await api('/image', { kind, public_path: im.path, origin: location.origin }); K.toast(t('Image enregistrée'), 1); account(root, K); } catch (e) { K.toast(e.message); }
      } }, h('img', { src: `/site/${site.name}/${im.rel.split('/').map(encodeURIComponent).join('/')}`, alt: im.name, loading: 'lazy' }))) : [h('p', { class: 'muted' }, t('Aucune image dans Public : déplacez-en une avec « Publier » dans l’uCloud.'))]));
    } }, t('Choisir dans mes fichiers publics'));
    let own = false; try { own = input.value && new URL(input.value).origin === location.origin; } catch { /* pas une adresse */ }
    return h('div', {}, h('p', { class: 'row' }, pick, ' ', site ? pickPublic : null, file, ' ', msg), grid, own ? h('img', { src: input.value, alt: '', class: kind === 'banner' ? 'ac-banner' : 'ac-pic' }) : null);
  };

  // ── Profil : ce que vos Proches voient. Un seul bouton Enregistrer, pour ces champs seulement. ──
  const f = { name: h('input', { id: 'ac-name', value: a.profile.name || '', maxlength: 64, autocomplete: 'nickname' }), about: h('textarea', { id: 'ac-about', rows: 3, maxlength: 500 }, a.profile.about || ''), banner: h('input', { id: 'ac-ban', value: a.profile.banner || '', placeholder: 'https://…', maxlength: 256, inputMode: 'url' }), picture: h('input', { id: 'ac-pic', value: a.profile.picture || '', placeholder: 'https://…', maxlength: 256, inputMode: 'url' }) };
  const save = h('button', { onclick: async ev => {
    const patch = {};
    for (const k of ['name', 'about', 'picture', 'banner']) if (f[k].value.trim() !== (a.profile[k] || '')) patch[k] = f[k].value.trim();
    if (!Object.keys(patch).length) return K.toast(t('Aucune modification'));
    ev.target.disabled = true;
    try { await api('/profile', patch); K.toast(t('Profil enregistré'), 1); account(root, K); } catch (e) { K.toast(e.message); ev.target.disabled = false; }
  } }, 'Enregistrer');
  // Les adresses d'image sont pour qui sait : on choisit d'abord une image, l'adresse se règle dans le repli.
  const profile = h('section', { class: 'card' }, head('Profil', 'Publié sur votre relai : vos Proches le voient.'),
    h('label', { for: 'ac-name' }, 'Nom affiché'), f.name, h('label', { for: 'ac-about' }, 'Présentation'), f.about,
    h('h3', {}, t('Photo de profil')), imageTools('picture', f.picture),
    h('h3', {}, t('Bandeau')), h('p', { class: 'muted small' }, t('Le fond de votre profil et de vos Ğ1Billets.')), imageTools('banner', f.banner),
    h('details', { class: 'fold' }, h('summary', {}, t('Par adresse web')), h('label', { for: 'ac-pic' }, 'Image (https)'), f.picture, h('label', { for: 'ac-ban' }, t('Bandeau (https)')), f.banner),
    h('p', {}, save));

  // ── Préférences : chaque réglage s'applique tout de suite, sans bouton (le changement se voit, il se défait de la même façon). ──
  const lang = h('select', { id: 'ac-lang', onchange: async () => {
    lang.disabled = true;
    try { await api('/profile', { lang: lang.value }); K.setLang(lang.value); } catch (e) { K.toast(e.message); lang.value = K.lang; lang.disabled = false; }
  } }, a.langs.map(l => h('option', { value: l, selected: l === K.lang }, LANG[l] || l)));
  const top = document.getElementById('style');
  const style = top && h('select', { id: 'ac-style', onchange: () => { top.value = style.value; top.dispatchEvent(new Event('change')); } }, [...top.options].map(o => h('option', { value: o.value, selected: o.value === top.value }, o.textContent)));
  const prefs = h('section', { class: 'card' }, head(t('Préférences'), t('Appliquées tout de suite, sur tous vos appareils pour la langue, sur celui-ci pour le style.')),
    h('label', { for: 'ac-lang' }, 'Langue de l\'interface'), lang, style ? [h('label', { for: 'ac-style' }, t('Style de l\'interface')), style] : null);
  // ── Visio : être joignable (mes amis réciproques voient si je suis là ou en appel) et notifications d'appel sur cet appareil ──
  const visio = h('div', { class: 'ac-visio' }, h('div', { class: 'ac-head' }, h('h3', {}, t('Visio')), tip(t('Seuls vos amis réciproques voient si vous êtes en ligne ou en appel, et seulement quand une page Astroport est ouverte. Rien n\'est gardé.'))));
  prefs.append(visio);
  K.api('/api/v1/presence').then(p => {
    const box = h('input', { type: 'checkbox', id: 'vi-reach', checked: p.reachable, onchange: async () => {
      box.disabled = true;
      try {
        await K.api('/api/v1/presence', { method: 'POST', body: { reachable: box.checked } });
        K.toast(box.checked ? t('Vos amis voient quand vous êtes là') : t('Vous n\'êtes plus visible'), 1);
        if (box.checked) K.mod('m-call.js').then(m => m.askNotify(), () => {});
      } catch (e) { box.checked = !box.checked; K.toast(e.message); }
      box.disabled = false;
    } });
    const N = window.Notification;
    const notif = !N ? null : N.permission === 'granted' ? h('p', { class: 'muted' }, t('Notifications d\'appel : activées sur cet appareil.'))
      : N.permission === 'denied' ? h('div', { class: 'ac-head muted' }, h('span', {}, t('Notifications d\'appel bloquées')), tip(t('Notifications d\'appel bloquées sur cet appareil : autorisez-les dans le navigateur pour être prévenu quand l\'onglet est en arrière-plan.')))
      : h('p', {}, h('button', { type: 'button', class: 'ghost', onclick: async ev => { await N.requestPermission().catch(() => {}); ev.target.replaceWith(h('span', { class: 'muted' }, N.permission === 'granted' ? t('Notifications d\'appel activées') : t('Notifications refusées'))); } }, t('Être prévenu des appels sur cet appareil')));
    visio.append(h('p', {}, h('label', { for: 'vi-reach' }, box, ' ', t('Être joignable en visio'))), notif || '');
  }, () => { visio.hidden = true; });

  // ── Mon espace (m-start.js : la part de chaque nature de fichier) ──
  const space = h('section', { class: 'card', hidden: true });
  K.mod('m-start.js').then(m => m.space(K, space), () => {});

  // ── Mon site public : le dossier /Public de l'uCloud, servi en clair à tous (modèle : déplacer un fichier dans /Public le publie) ──
  const sitebox = site && h('section', { class: 'card' }, head(t('Mon site public'), t('Tout ce que vous mettez dans le dossier Public de votre uCloud est public : lisible par tout le monde, sans mot de passe. Déplacez-y un fichier pour le publier, sortez-le pour le retirer. Vous pouvez y construire un site (pages HTML, images, CSS) avec votre client WebDAV.'), t('Vos fichiers restent chiffrés sur la station : elle ne les déchiffre que pour ce dossier. Les scripts d’une page tournent isolés de la station. Les visites sont comptées par jour et par page, sans garder aucune adresse ni identité de visiteur.')),
    h('dl', { class: 'stack' },
      h('dt', {}, t('Adresse de votre site')), h('dd', {}, h('a', { href: site.path_url, target: '_blank', rel: 'noopener' }, location.origin + site.path_url), ' ', copy(location.origin + site.path_url, t('Adresse'))),
      site.host_url ? [h('dt', {}, t('Adresse par nom (DNS)')), h('dd', {}, h('a', { href: site.host_url, target: '_blank', rel: 'noopener' }, site.host_url), ' ', copy(site.host_url, t('Adresse')))] : null,
      h('dt', {}, t('Page d’accueil')), h('dd', {}, site.has_index ? t('index.html de votre dossier Public') : t('votre blog : articles, notes et fichiers')),
      h('dt', {}, t('Fichiers publiés')), h('dd', {}, String(site.files)),
      site.visits ? [h('dt', {}, t('Visites')), h('dd', {}, `${t('aujourd’hui')} ${site.visits.today} · 7 ${t('jours')} ${site.visits.week} · 90 ${t('jours')} ${site.visits.days90}`,
        site.visits.top.length ? h('ul', { class: 'muted' }, site.visits.top.map(x => h('li', {}, `${x.path} : ${x.hits}`))) : null)] : null,
      h('dt', {}, 'WebDAV'), h('dd', {}, code(a.dav.url), ' ', t('dossier'), ' ', code('/Public'))),
    h('p', {}, h('a', { class: 'btn', href: '#/write' }, '✍️ ' + t('Écrire sur mon blog'))));

  // ── Mes images publiques : avatar, bandeau et images envoyées depuis un client Nostr. Publiques et en clair, elles entament le quota. ──
  const pics = h('section', { class: 'card', hidden: true }, h('h2', {}, t('Mes images publiques')));
  const picsList = () => K.api('/api/v1/account/images').then(r => {
    pics.hidden = false;
    pics.replaceChildren(head(t('Mes images publiques'), t('Votre photo de profil, votre bandeau et les images que vous avez mises dans vos publications. Tout le monde peut les voir.'), t('Retirer une image libère de la place, mais elle disparaît aussi des publications où elle apparaît.')),
      r.images.length ? h('div', { class: 'ac-grid' }, r.images.map(im => h('figure', { class: 'ac-img' },
        h('img', { src: `/ipfs/${im.cid}`, alt: '', loading: 'lazy' }),
        h('figcaption', { class: 'muted' }, K.bytes(im.size), ' · ', K.when(im.added_at), im.in_use ? [h('br'), im.in_use === 'banner' ? t('Utilisée comme bandeau') : t('Utilisée comme photo de profil')] : null),
        h('span', { class: 'row' }, copy(`${location.origin}/ipfs/${im.cid}`, t('Adresse')), ' ',
          im.in_use ? null : K.sure(t('Retirer'), async ev => { ev.target.disabled = true; try { await K.api(`/api/v1/account/images/${im.cid}`, { method: 'DELETE' }); K.toast(t('Image retirée'), 1); picsList(); } catch (e) { K.toast(e.message); ev.target.disabled = false; } }, t('Retirer cette image ? Elle ne s’affichera plus dans vos publications.'))))))
        : h('p', { class: 'muted' }, t('Aucune image publiée pour l’instant.')));
  }, () => { pics.hidden = true; });
  picsList();

  // ── Mes autres comptes : l'annonce (publique, sur demande) que mes comptes d'autres espaces de la station sont les miens ──
  // Le membre ne voit ni constellation ni clé : seulement qu'il peut se faire joindre depuis ses autres comptes.
  const planes = h('section', { class: 'card', hidden: true }, h('h2', {}, t('Mes autres comptes')));
  K.api('/api/v1/me/links').then(r => {
    if (!r.siblings.length) return;
    planes.hidden = false;
    planes.firstChild.replaceWith(head(t('Mes autres comptes'), t('Vous avez d\'autres comptes sur cette station. L\'annoncer permet à vos amis de vous écrire depuis le compte qu\'ils utilisent : le lien est public, signé par vos deux comptes.')));
    planes.append(
      h('p', {}, K.sure(t('Annoncer mes autres comptes'), async ev => {
        ev.target.disabled = true;
        try { const a = await K.api('/api/v1/me/links', { method: 'POST', body: {} }); K.toast(t('{0} lien(s) annoncé(s)', a.linked.length), 1); } catch (e) { K.toast(e.message); } finally { ev.target.disabled = false; }
      }, t('Annoncer publiquement que vos autres comptes sont les vôtres ?'), { id: 'announce-links', class: '' }), ' ', h('small', { class: 'muted' }, t('À faire une fois sur chacun de vos comptes.'))));
  }, () => { /* sans annuaire sur cette station : pas de carte */ });

  // Compte invité : l'adresse a été générée par la station. On ne promet pas une fonction qui n'existe pas (changer d'adresse).
  const guest = a.guest ? h('div', { class: 'warn ac-head', role: 'note' }, h('span', {}, h('strong', {}, 'Compte invité. '), t('Gardez votre clé de secours et votre code PASS.')), tip('Votre adresse a été créée par la station : c\'est votre identifiant UPlanet, pas une boîte aux lettres. Gardez la clé de secours imprimée à la création et votre code PASS : sans email, ce sont vos seuls moyens de récupérer le compte.')) : null;
  // Bandeau d'identité : la photo de profil si elle est sur cette station (CSP), sinon l'initiale.
  const title = a.profile.name || a.email || 'Mon compte';
  const pic = (() => { try { return a.profile.picture && new URL(a.profile.picture).origin === location.origin ? a.profile.picture : null; } catch { return null; } })();
  const hero = h('div', { class: 'hero' }, pic ? h('img', { class: 'avatar', src: pic, alt: '' }) : h('div', { class: 'avatar', 'aria-hidden': 'true' }, [...title][0].toUpperCase()),
    h('div', { class: 'who' }, h('h2', {}, title), a.email && a.email !== title ? h('div', { class: 'sub' }, a.email) : null, h('span', { class: 'pill' }, ROLE[a.role] || a.role), a.guest ? h('span', { class: 'pill' }, 'Invité') : null));
  // Ce que je publie (site, images publiques, autres comptes) : utile, mais pas tous les jours ; replié sous les cartes du quotidien.
  const pub = h('details', { class: 'card wide fold ac-pub' }, h('summary', {}, t('Ce que je publie'), ' ', h('small', { class: 'muted' }, t('site public, images, autres comptes'))), h('div', { class: 'grid' }, [sitebox, pics, planes].filter(Boolean)));
  root.replaceChildren(...[hero, guest, h('div', { class: 'grid' }, profile, prefs, space, pub)].filter(Boolean));
}
