'use strict';
// Station › Santé › Détails techniques : nœud, relais, sous-systèmes. Ancien écran « Station » (#/station).
export default async function machine(root, K) {
  const { h, t, api } = K;
  const row = (k, v) => [h('dt', {}, k), h('dd', {}, v)];
  const draw = async () => {
    const [d, z] = await Promise.all([api('/api/v1/dashboard'), fetch('/health').then(r => r.json())]);
    const s = d.station, r = d.relay, sub = z.subsystems || {};
    root.replaceChildren(h('h2', {}, t('Détails techniques')), h('div', { class: 'grid' },
      h('section', { class: 'card' }, h('h2', {}, t('Station'), ' ', K.pill(z.status)),
        h('dl', {}, row(t('Nœud'), s.node), row('PeerID', s.peer_id || '—'), row(t('Domaine'), s.domain || '—'), row(t('Version'), s.version), row(t('En marche depuis'), K.span(s.uptime_secs)))),
      h('section', { class: 'card' }, h('h2', {}, t('Relais'), ' ', K.pill(sub.relay?.status || 'ok')),
        h('p', { class: 'big' }, String(r.events ?? '—'), h('small', { class: 'muted' }, ' ' + t('événements'))),
        h('dl', {}, row(t('Dernier événement'), K.when(r.last_event_at)), row(t('Base de données'), K.bytes(r.db_bytes)))),
      d.backup_rs && (d.backup_rs.packets || d.backup_rs.held_for_friends) ? h('section', { class: 'card' }, h('h2', {}, t('Sauvegarde k-sur-n'), ' ', K.pill(d.backup_rs.frags_lost || d.backup_rs.suspects ? 'warn' : 'ok')),
        h('dl', {}, row(t('Paquets'), `${d.backup_rs.packets}` + (d.backup_rs.staged ? ` (+${d.backup_rs.staged} ${t('en compactage')})` : '')),
          row(t('Fragments'), `${d.backup_rs.frags_acked} ${t('acquittés')} · ${d.backup_rs.frags_pending} ${t('en attente')} · ${d.backup_rs.frags_lost} ${t('perdus')}`),
          row(t('Réparations en cours'), String(d.backup_rs.repairing)), row(t('Retraits en file'), String(d.backup_rs.drops_queued)),
          row(t('Gardiens fautifs'), `${d.backup_rs.faulty_guardians}` + (d.backup_rs.suspects ? ` · ${d.backup_rs.suspects} ${t('contrôle(s) à finir')}` : '')),
          row(t('Fragments gardés pour des amis'), String(d.backup_rs.held_for_friends)))) : '',
      h('section', { class: 'card' }, h('h2', {}, t('Sous-systèmes')), h('dl', {}, Object.entries(sub).flatMap(([n, v]) => row(n, [K.pill(v.status), ' ', v.detail]))))));
  };
  await draw();
  K.every(30000, () => draw().catch(() => {}));
}
