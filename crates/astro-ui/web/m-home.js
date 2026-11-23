'use strict';
// Accueil d'un membre : quatre gestes (photos, argent, proches, assistant ou smartphone), « Bien démarrer » coché d'après l'état réel (le seul
// endroit où il apparaît), et une alerte
// seulement si quelque chose le demande. Les chiffres (solde, loyer, disque…) sont dans Mes Ẑen : #/money.
export function steps({ dash, faces, friends, backup, me, devices, reading }) {
  const files = dash?.sovereignty?.encrypted_files || 0;
  const list = [
    { done: files > 0, title: 'Envoyer vos premières photos', why: 'Elles sont chiffrées avant de quitter votre appareil.', href: '#/cloud' },
    { done: (devices?.devices || []).length > 0, title: 'Synchroniser votre smartphone', why: 'Photos, contacts et agenda quittent Google et Apple pour votre station.', href: '#/devices' },
    ...(faces?.enabled ? [{ done: (faces.people || []).length > 0, title: 'Reconnaître un visage', why: 'Nommez une personne : ses photos se rangent toutes seules.', href: '#/people' }] : []),
    { done: (friends?.friends || []).length > 0, title: 'Retrouver un proche', why: 'Suivez-vous mutuellement pour partager vos photos.', href: '#/people' },
    { done: backup?.mine?.status === 'accepted', title: 'Mettre vos fichiers à l\'abri chez un ami', why: 'Si votre station tombe en panne, rien n\'est perdu.', href: '#/backup' },
    { done: !!me?.ai, title: 'Brancher un assistant', why: 'BRO répond avec votre propre IA, sans rien envoyer ailleurs.', href: '#/devices?card=ai' },
    // Seulement si la station sait lire les documents (réponse de /api/v1/me/reading).
    ...(reading?.state ? [{ done: reading.state === 'local' || reading.state === 'friend', title: 'Rendre vos documents cherchables', why: 'Retrouvez une photo ou un papier par le texte qu\'il contient.', href: '#/devices?card=reading' }] : []),
  ];
  return { list, left: list.filter(s => !s.done).length, files };
}

// « Pour vous » : ce qui attend un geste du membre aujourd'hui (3 au plus), tiré de l'état réel ; rien = encart absent.
export function news({ faces, shares, backup, fp, agents }) {
  const list = [], sug = (faces?.suggestions || []).length, unk = faces?.unknown || 0, pv = fp?.privacy;
  // Un ami qui a perdu sa station attend ma confirmation : toujours en tête.
  for (const r of (backup?.hosting || []).filter(x => x.rescue).slice(0, 2)) list.push({ icon: '🆘', text: ['{0} a perdu sa station et vous demande de l\'aide', r.label], label: 'Répondre', href: '#/backup' });
  // Un agent attend une réponse (elle expire au bout de 30 minutes) : la plus récente, juste après l'appel à l'aide d'un ami.
  const ask = (agents?.requests || []).find(r => r.state === 'pending');
  // L'appelant : le nom donné à l'agent, et le programme qui parle s'il dit autre chose (« OpenClaw (Claude Code 2.1) »).
  const caller = a => (a.client && !a.client.toLowerCase().startsWith(a.agent.toLowerCase()) ? `${a.agent} (${a.client})` : a.agent);
  if (ask) list.push({ icon: '🤖', text: [ask.mode === 'navigateur' ? '{0} voudrait agir sur {1} pour vous' : '{0} demande l\'accès à {1}', caller(ask), ask.site], label: 'Répondre', href: '#/agents' });
  // Après un rétablissement : les fichiers reviennent de chez les amis.
  const rs = backup?.restore;
  if (rs && rs.state !== 'done') list.push({ icon: '📦', text: ['Vos fichiers reviennent : {0} ami(s) ont renvoyé, {1} attendu(s)', rs.returned.length, rs.waiting.length], label: 'Voir', href: '#/backup' });
  // Aucun gardien hors de cette station : une panne de la station emporterait le compte.
  if (backup?.guardians && !backup.guardians.some(g => !g.same_station)) list.push({ icon: '⚠️', text: ['Aucun gardien : si votre station tombe, votre compte est perdu.'], label: 'Choisir un gardien', href: '#/backup' });
  // Vigie de Public (lisible par tous).
  const pvn = pv?.sensitive || 0; // les photos partent déjà sans lieu ni appareil
  if (pvn) list.push({ icon: '🔒', text: ['{0} fichier(s) de Public en disent trop (mot de passe, contacts, papiers…)', pvn], label: 'Voir', href: '#/cloud?p=%2FPublic' });
  for (const b of (shares?.pending || []).slice(0, 2)) list.push({ icon: '📸', text: ['{0} photo(s) de {1} attendent votre accord pour lui être envoyées', b.count, b.name], label: 'Décider', href: '#/people/tri' });
  if (sug) list.push({ icon: '🫂', text: ['{0} visage(s) à confirmer', sug], label: 'Trier en 1 clic', href: '#/people/tri?grand' });
  else if (unk) list.push({ icon: '🫂', text: ['{0} visage(s) à nommer', unk], label: 'Trier en 1 clic', href: '#/people/tri?grand' });
  return list.slice(0, 3);
}

export default async function home(root, K) {
  const { h, t } = K, soft = p => K.api(p).catch(() => null);
  // Bêta : créée une fois, gardée d'un rafraîchissement à l'autre (un retour en cours de saisie n'est jamais effacé).
  const beta = h('section', { class: 'card bt-card' });
  K.mod('m-beta.js').then(m => beta.replaceChildren(...m.card(K)), () => { beta.hidden = true; });
  const draw = async () => {
    const health = await fetch('/health').then(r => r.json()).catch(() => null);
    const [me, dash, faces, friends, backup, fp, devices, shares, reading, agents] = await Promise.all([K.api('/api/v1/me'), K.api('/api/v1/dashboard'), soft('/api/v1/faces'), soft('/api/v1/friends'), soft('/api/v1/backup'), soft('/api/v1/me/footprint'), soft('/api/v1/devices'), soft('/api/v1/faces/pending-shares'), soft('/api/v1/me/reading'), soft('/api/v1/agents/requests')]);
    const s = steps({ dash, faces, friends, backup, me, devices, reading }), nFriends = (friends?.friends || []).length, nDevices = (devices?.devices || []).length;
    const disk = fp?.disk, full = disk && !disk.unlimited && disk.quota_bytes && disk.used_bytes / disk.quota_bytes >= .9;
    const tile = (icon, title, line, href) => h('a', { class: 'card hm-tile', href }, h('span', { class: 'hm-ico', 'aria-hidden': 'true' }, icon), h('strong', {}, t(title)), h('span', { class: 'muted' }, line));
    const first = (me.email || '').split('@')[0];
    // « Il y a un an » : les photos prises ce jour-là les années passées (la plus récente année d'abord), 4 vignettes.
    const now = new Date(), two = n => String(n).padStart(2, '0');
    const mem = (await soft(`/api/v1/ucloud/memories?md=${two(now.getMonth() + 1)}-${two(now.getDate())}&y=${now.getFullYear()}`))?.memories?.[0];
    const memCard = mem ? (() => { const g = h('div', { class: 'photos' }); K.tiles(g, [], mem.items.slice(0, 4)); return h('section', { class: 'card hm-mem' },
      h('h2', {}, '🕰️ ', mem.ago === 1 ? t('Il y a 1 an') : t('Il y a {0} ans', mem.ago), h('small', { class: 'muted' }, ' · ' + t('{0} photo(s) ce jour-là', mem.items.length))), g,
      h('p', {}, h('a', { href: '#/library' }, t('Tous les souvenirs')), ' · ', h('a', { href: '#/library?tab=trips' }, t('Mes voyages')))); })() : null;
    root.replaceChildren(...[
      h('section', { class: 'hm-hero' }, h('h2', {}, first ? t('Bonjour {0}', first) : t('Bonjour')), h('p', { class: 'muted' }, s.files ? t('{0} fichier(s) chiffré(s) sur votre station, à vous seul.', s.files) : t('Votre espace est prêt. Commencez par y déposer quelques photos.'))),
      full ? h('p', { class: 'degraded', role: 'status' }, t('Votre espace est presque plein ({0} %).', Math.round(100 * disk.used_bytes / disk.quota_bytes)), ' ', h('a', { href: '#/money' }, t('Voir mon espace'))) : null,
      ...(n => n.length ? [h('section', { class: 'card hm-news', 'aria-label': t('Pour vous') }, h('ul', {}, n.map(x => h('li', {}, h('span', { class: 'hm-ico', 'aria-hidden': 'true' }, x.icon), h('span', {}, t(...x.text)), h('a', { class: 'ck-btn', href: x.href }, t(x.label))))))] : [])(news({ faces, shares, backup, fp, agents })),
      memCard,
      me.captain ? h('a', { class: 'card hm-cap', href: '#/cockpit' }, h('span', { class: 'hm-ico', 'aria-hidden': 'true' }, '⚓'), h('div', {}, h('strong', {}, t('Votre station')), h('p', { class: 'muted' }, health?.status === 'ok' ? t('✅ Tout fonctionne. Ouvrir le cockpit.') : health ? t('🟠 Quelque chose demande votre attention : ouvrir le cockpit.') : t('État inconnu : ouvrir le cockpit.')))) : null,
      h('div', { class: 'grid hm-tiles' },
        tile('📸', 'Mes photos', s.files ? t('{0} fichier(s)', s.files) : t('Envoyer des photos'), s.files ? '#/library' : '#/cloud'),
        tile('💰', 'Mes Ẑen', t('Solde, loyer, espace'), '#/money'),
        tile('👥', 'Mes proches', nFriends ? t('{0} ami(s)', nFriends) : t('Retrouver vos proches'), '#/people'),
        // Sans assistant (pas de Brain), le geste mis en avant est de quitter Google et Apple : la synchro du smartphone.
        me.ai ? tile('🤖', 'Mon assistant', t('BRO vous répond'), '#/bro') : tile('📱', 'Mon smartphone', nDevices ? t('{0} appareil(s) synchronisé(s)', nDevices) : t('Quitter Google et Apple : photos, contacts, agenda'), '#/devices')),
      s.left ? h('section', { class: 'card' }, h('h2', {}, t('Bien démarrer'), h('small', { class: 'muted' }, ` · ${s.list.length - s.left}/${s.list.length}`)),
        h('progress', { max: s.list.length, value: s.list.length - s.left, 'aria-label': t('Étapes terminées') }),
        h('ul', { class: 'hm-steps' }, s.list.map(x => h('li', { class: x.done ? 'done' : '' }, h('span', { 'aria-hidden': 'true' }, x.done ? '✅' : '⬜'), h('div', {}, x.done ? h('span', {}, t(x.title)) : h('a', { href: x.href }, h('strong', {}, t(x.title))), x.done ? null : h('p', { class: 'muted' }, t(x.why)))))),
        // Qui arrive avec tout chez Google ou Apple n'a qu'un geste à faire : le déménagement.
        s.files ? null : h('p', {}, '🚚 ', h('a', { href: '#/move' }, t('Je viens de Google ou d’Apple')), ' · ', h('span', { class: 'muted' }, t('Photos, albums, contacts, agenda et documents en un geste, sans doublons.')))) : null,
      beta
    ].filter(Boolean));
  };
  const link = h('link', { rel: 'stylesheet', href: '/ui/m-home.css' }); document.head.append(link); K.cleanup(() => link.remove());
  await draw();
  K.every(60000, () => draw().catch(() => {}));
  // Une demande d'agent arrive ou se termine : « Pour vous » le montre tout de suite.
  K.listen(ev => { if (ev.kind === 'agent') draw().catch(() => {}); });
}
