'use strict';
// Réplication de vos données (module chargé à la demande) : où, et par qui. Les chiffres viennent de la station,
// qui interroge les relais des autres stations (par le maillage P2P) pour vos derniers événements et fichiers.
const VIA = { p2p: 'Maillage P2P', wss: 'Adresse publique (repli)', direct: 'Adresse directe', down: 'Injoignable' };
const LEVEL = { 1: 'Proche', 2: 'Proche de proche' };
const short = t => t.replace(/^p2p:\/\//, '').replace(/\|.*$/, '').slice(-10);

function ring(h, part, total, label) {
  const ns = 'http://www.w3.org/2000/svg', svg = document.createElementNS(ns, 'svg'), pct = total ? part / total : 0, c = 2 * Math.PI * 42;
  svg.setAttribute('viewBox', '0 0 100 100'); svg.setAttribute('class', 'rp-ring'); svg.setAttribute('role', 'img'); svg.setAttribute('aria-label', label);
  const circle = (cls, dash) => { const e = document.createElementNS(ns, 'circle'); e.setAttribute('cx', 50); e.setAttribute('cy', 50); e.setAttribute('r', 42); e.setAttribute('class', cls); if (dash != null) { e.setAttribute('stroke-dasharray', `${dash} ${c}`); e.setAttribute('transform', 'rotate(-90 50 50)'); } svg.append(e); };
  circle('rp-bg'); circle('rp-fg', c * pct);
  const t = document.createElementNS(ns, 'text'); t.setAttribute('x', 50); t.setAttribute('y', 56); t.setAttribute('text-anchor', 'middle'); t.setAttribute('class', 'rp-txt'); t.textContent = `${Math.round(pct * 100)} %`; svg.append(t);
  return svg;
}

export default async function replication(root, K) {
  const { h, t } = K, link = h('link', { rel: 'stylesheet', href: '/ui/m-replication.css' });
  document.head.append(link); K.cleanup(() => link.remove());
  const ago = at => at ? ' ' + t('Mesuré il y a {0}.', K.span(Date.now() / 1000 - at)) : '';
  const draw = async () => {
    const r = await K.api('/api/v1/replication'), m = r.mine, c = r.crawl;
    const peers = m ? m.peers : [], ok = peers.filter(p => !p.error), copies = ok.filter(p => p.have > 0), near = copies.filter(p => p.level).length;
    const pct = m?.events ? Math.round(m.replicated / m.events * 100) : 0;
    const hero = m
      ? h('section', { class: 'card rp-hero' }, ring(h, m.replicated, m.events, t('{0} % de vos données sont copiées ailleurs', pct)),
        h('div', {}, h('h2', {}, t('Vos données ailleurs')),
          m.events ? h('p', { class: 'big' }, t('{0} sur {1}', m.replicated, m.events), h('small', { class: 'muted' }, ' ' + t('éléments copiés ailleurs'))) : h('p', { class: 'muted' }, t('Rien à copier pour le moment.')),
          h('p', { class: 'muted' }, copies.length ? (near ? t('Présents chez {0} autre(s) station(s), dont {1} de vos proches.', copies.length, near) : t('Présents chez {0} autre(s) station(s).', copies.length)) + ago(m.at) : t('Aucune autre station ne détient encore vos données.')),
          m.unreachable ? h('p', { class: 'degraded', role: 'status' }, t('{0} station(s) n\'ont pas répondu : leur contenu est inconnu, pas absent.', m.unreachable)) : null))
      : h('section', { class: 'card' }, h('h2', {}, t('Vos données ailleurs')), h('p', { class: 'muted' }, t('Pas encore de mesure : la première passe a lieu quelques minutes après le démarrage de la station.')));
    const stations = m && peers.length
      ? h('section', { class: 'card wide' }, h('h2', {}, t('Qui détient quoi'), ' ', h('small', {}, t('{0} station(s) de la constellation', peers.length))),
        K.table(t('Chaque station est interrogée sur vos dernières données'), [t('Station'), t('Capitaine'), t('Lien'), t('Copies'), t('État')],
          peers.slice().sort((a, b) => (b.have ?? -1) - (a.have ?? -1)).map(p => [p.name, p.captain || '—', p.level ? t(LEVEL[p.level]) : t('Autre station'),
            p.error ? '—' : [h('meter', { min: 0, max: Math.max(m.events, 1), value: p.have, 'aria-label': t('{0} sur {1}', p.have, m.events) }), ` ${p.have} / ${m.events}`],
            p.error ? [K.pill('down'), ' ', p.error] : K.pill(p.have === m.events ? 'ok' : p.have ? 'degraded' : 'disabled')])))
      : h('section', { class: 'card wide' }, h('h2', {}, t('Qui détient quoi')), h('p', { class: 'muted' }, m ? t('Aucune autre station connue pour l\'instant.') : '—'));
    const md = m?.media;
    const media = h('section', { class: 'card wide' }, h('h2', {}, t('Vos fichiers')),
      md ? [h('p', { class: 'big' }, t('{0} sur {1}', md.replicated, md.files), h('small', { class: 'muted' }, ' ' + t('fournis par d\'autres stations'))),
        md.peers.length ? h('ul', { class: 'muted' }, md.peers.map(p => h('li', {}, t('{0} : {1} fichier(s)', p.name, p.have)))) : h('p', { class: 'muted' }, t('Aucune autre station ne fournit encore vos derniers fichiers.')),
        md.lookup_failed ? h('p', { class: 'degraded' }, t('{0} recherche(s) sans réponse.', md.lookup_failed)) : null]
        : h('p', { class: 'muted' }, t('Non mesuré sur cette station.')));
    // Les voies réseau (P2P, adresse publique) n'intéressent que les curieux : repliées.
    const routes = r.routes || [];
    const sync = h('section', { class: 'card wide' }, h('h2', {}, t('Ce que vos proches publient')),
      c ? h('p', {}, c.error ? h('span', { class: 'down' }, t('Dernier passage interrompu : {0}', c.error)) : t('Dernier passage il y a {0} : {1} nouveauté(s) reçue(s) de {2} personne(s).', K.span(Date.now() / 1000 - c.at), c.inserted, c.keys)) : h('p', { class: 'muted' }, t('Aucun passage encore : le premier a lieu une minute après le démarrage.')),
      c?.errors?.length ? h('p', { class: 'degraded' }, t('{0} échec(s), par exemple : {1}', c.errors.length, c.errors[0].reason)) : null,
      routes.length ? h('details', {}, h('summary', {}, t('Détails techniques')), K.table(t('Voie utilisée pour joindre chaque station'), [t('Station'), t('Voie'), t('Dernier essai'), t('Détail')], routes.map(x => [h('code', {}, short(x.target)), [K.pill(x.via === 'down' ? 'down' : x.via === 'wss' ? 'degraded' : 'ok'), ' ', t(VIA[x.via] || x.via)], K.when(x.at), x.error || '—']))) : null);
    root.replaceChildren(h('div', { class: 'grid' }, hero, media, stations, sync));
  };
  await draw();
  K.every(30000, () => draw().catch(() => {}));
}
