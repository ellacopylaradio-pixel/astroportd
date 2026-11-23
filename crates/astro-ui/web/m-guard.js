'use strict';
// Mes gardiens (Moi › Sauvegarde) : les amis qui hébergent la sauvegarde ET gardent une part de la clé ; ceux de cette
// même station ne comptent pas (ils tomberaient avec elle). Niveau, chances estimées par la station (‰), demande à un proche,
// et, après un rétablissement, le suivi nominatif du retour des fichiers. Rend [suivi ou null, carte des gardiens].
export default function guardCards(bk, K, refresh) {
  const { h, t } = K;
  const guard = h('section', { class: 'card', id: 'ac-guard' }, h('h2', {}, t('Mes gardiens')));
  {
    const g = bk.guardians, n = g.filter(x => !x.same_station).length, sf = bk.safety || {}, pc = p => Math.floor(p / 10);
    const level = [[t('Aucun gardien : si votre station tombe, votre compte est perdu.'), 'err'], [t('Un seul gardien : c\'est fragile.'), 'warn'], [t('Deux gardiens : c\'est bien.'), 'muted'], [t('{0} gardiens : c\'est solide.', n), 'muted']][Math.min(n, 3)];
    guard.append(h('p', { class: level[1], role: n ? null : 'alert' }, h('strong', {}, level[0])),
      g.length ? h('ul', {}, g.map(x => h('li', {}, x.label, x.same_station ? h('span', { class: 'muted' }, ' — ', t('sur cette même station : ne vous protège pas d\'une panne')) : null))) : '',
      sf.permille != null ? h('p', {}, h('strong', {}, t('Votre compte : {0} % de chances d\'être retrouvé.', pc(sf.permille))), sf.files_permille != null && sf.files_permille < sf.permille ? ' ' + t('Vos fichiers : {0} %.', pc(sf.files_permille)) : null)
        : n ? h('p', { class: 'muted' }, t('Pas encore assez de recul pour chiffrer vos chances : une semaine d\'observation.')) : '',
      sf.next_permille != null ? h('p', {}, t('Ajoutez un gardien pour passer à {0} %.', pc(sf.next_permille))) : '',
      h('p', { class: 'muted' }, t('Votre carte de secours imprimée + un seul gardien suffisent pour tout retrouver. Une part seule ne révèle rien à celui qui la garde.')));
    // Fichiers répartis chez les gardiens : sécurité estimée (cible 99,9 %), place économisée, réparation en cours.
    const c = bk.coded;
    if (c?.packets) {
      const num = (x, d) => x.toLocaleString(K.lang, { maximumFractionDigits: d }), ok = c.safety_permille >= c.target_permille;
      guard.append(h('h3', {}, t('Vos fichiers chez vos gardiens')),
        h('p', { class: ok ? 'ok' : 'warn' }, h('strong', {}, t('Sécurité estimée : {0} % (objectif {1} %).', num(c.safety_permille / 10, 1), num(c.target_permille / 10, 1)))),
        c.holders > c.need ? h('p', {}, t('{0} de vos {1} gardiens suffisent pour tout retrouver.', c.need, c.holders)) : '',
        c.ratio_pct && c.ratio_pct < c.holders * 90 ? h('p', {}, t('Vos amis gardent {0} fois vos fichiers au lieu de {1}.', num(c.ratio_pct / 100, 1), c.holders)) : '',
        c.thin ? h('p', { class: 'warn', role: 'status' }, t('Une partie de vos fichiers n\'a plus de marge : votre station la remet à l\'abri chez un autre gardien dès qu\'il a de la place.'))
          : c.repairing ? h('p', { class: 'muted', role: 'status' }, t('Réparation en cours : un gardien ne répondait plus, votre station refait sa part chez un autre.'))
            : c.compacting ? h('p', { class: 'muted', role: 'status' }, t('Rangement en cours : votre station regroupe vos fichiers pour prendre moins de place chez vos amis.')) : '');
    }
    // Proches rangés par la station, le conseillé d'abord : un autre Astroport, puis le cercle le plus proche, puis rencontré.
    const others = (bk.friends || []).filter(f => !g.some(x => x.label === f.label)), away = others.filter(f => !f.same_station).length;
    const ICO = { 5: '🏠', 4: '💛', 3: '🤝', 2: '👋', 1: '📇' };
    if (others.length) {
      const pick = h('select', { id: 'gd-friend', 'aria-label': t('Proche à qui le demander') }, others.map(f => h('option', { value: f.hex },
        `${ICO[f.level] || ''} ${f.same_station ? t('{0} (même station)', f.label) : f.label}${f.met_verified ? ' ✓' : ''}`.trim())));
      guard.append(h('label', { for: 'gd-friend' }, t('Demander à un proche de devenir gardien')), pick,
        away ? h('p', { class: 'muted small' }, t('En tête : vos proches sur un autre Astroport, du cercle le plus proche au plus lointain (Mes cercles).'))
          : h('p', { class: 'warn' }, t('Vos proches sont tous sur cette station : un gardien doit surtout être sur un autre Astroport, pour survivre à une panne de la vôtre.'), ' ',
            h('a', { href: '#/rencontre' }, t('Se rencontrer'))),
        h('p', {}, h('button', { onclick: ev => { ev.target.disabled = true; K.api('/api/v1/backup', { method: 'POST', body: { enabled: true, host: pick.value } }).then(() => { K.toast(t('Demande envoyée au proche'), 1); refresh(); }, e => { ev.target.disabled = false; K.toast(e.message); }); } }, t('Lui demander'))),
        n ? h('p', { class: 'muted small' }, t('Chaque gardien reçoit une part de vos nouveaux fichiers ; quelques-uns suffisent pour tout retrouver.')) : '');
    }
  }
  const rs = bk?.restore, back = rs && h('section', { class: 'card', role: 'status' }, h('h2', {}, rs.state === 'done' ? t('Vos fichiers sont revenus') : t('Vos fichiers reviennent')),
    h('p', {}, t('{0} ami(s) ont renvoyé vos fichiers.', rs.returned.length), rs.returned.length ? ' (' + rs.returned.join(', ') + ')' : ''),
    rs.waiting.length ? h('ul', {}, rs.waiting.map(w => h('li', {}, w.seen_days == null ? t('{0} n\'a pas encore répondu', w.label) : w.seen_days ? t('{0} est hors ligne depuis {1} jour(s)', w.label, w.seen_days) : t('{0} a été vu aujourd\'hui', w.label)))) : null,
    rs.state === 'late' ? h('p', { class: 'muted' }, t('Votre espace est utilisable. La station leur redemande chaque jour pendant 30 jours ; ce qui revient s\'ajoute sans rien écraser.')) : null);

  return [back, guard];
}
