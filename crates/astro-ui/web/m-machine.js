'use strict';
// Station › Santé › Détails techniques : nœud, relais, sous-systèmes. Ancien écran « Station » (#/station).
// Plus la mise à jour : la station lance install.sh (binaire, modèles de recherche, plugins) hors de son service, puis redémarre.
const STATE = { ajour: 'à jour', maj: 'mise à jour disponible', nouveau: 'nouveau', absent: 'non installé' };
export default async function machine(root, K) {
  const { h, t, api } = K;
  const row = (k, v) => [h('dt', {}, k), h('dd', {}, v)];
  const main = h('div'), upd = h('section', { class: 'card', 'aria-live': 'polite' }, h('h2', {}, t('Mise à jour')), h('p', { class: 'muted' }, t('Recherche de la dernière version…')));
  root.replaceChildren(h('h2', {}, t('Détails techniques')), main, upd);
  const draw = async () => {
    const [d, z] = await Promise.all([api('/api/v1/dashboard'), fetch('/health').then(r => r.json())]);
    const s = d.station, r = d.relay, sub = z.subsystems || {};
    main.replaceChildren(h('div', { class: 'grid' },
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
  // Pendant la mise à jour, le journal défile ; quand la station redémarre, on attend son retour puis on recharge la page.
  let armed = false, gone = false;
  const update = async fresh => {
    let u;
    try { u = await api('/api/v1/station/update' + (fresh ? '?fresh=1' : '')); } catch (e) {
      if (armed) { gone = true; upd.replaceChildren(h('h2', {}, t('Mise à jour')), h('p', {}, '⏳ ', t('La station redémarre : la page se rechargera à son retour.'))); return; }
      upd.replaceChildren(h('h2', {}, t('Mise à jour')), h('p', { class: 'err' }, e.message)); return;
    }
    if (gone) { location.reload(); return; }
    armed = u.running;
    const missing = u.models.filter(m => !m.installed), stale = u.plugins.filter(p => p.state === 'maj' || p.state === 'nouveau');
    const go = K.sure(u.newer ? t('Mettre à jour vers {0}', u.latest) : t('Vérifier modèles et plugins'), async ev => {
      const b = ev.target; b.disabled = true;
      try { await K.api('/api/v1/station/update', { method: 'POST' }); armed = true; await update(); } catch (e) { K.toast(e.message); b.disabled = false; }
    }, t('La station redémarre : vos membres la perdent 1 à 3 minutes.'), { class: 'primary' });
    upd.replaceChildren(...[h('h2', {}, t('Mise à jour'), ' ', K.pill(u.running ? 'run' : u.newer || missing.length || stale.length ? 'info' : 'ok')),
      h('p', {}, u.newer ? h('strong', {}, t('Nouvelle version : {0} (vous avez {1})', u.latest, u.current)) : t('Version {0} : à jour', u.current)),
      u.error ? h('p', { class: 'muted' }, u.error) : null,
      h('dl', {},
        row(t('Recherche'), u.models.filter(m => m.installed).map(m => m.id).join(', ') || t('confiée à un Capitaine ami')),
        missing.length ? row(t('Modèles proposés'), missing.map(m => `${m.label} (≈ ${m.mb} Mo)`).join(' · ')) : '',
        row(t('Plugins'), u.plugins.length ? u.plugins.map(p => `${p.name} ${p.installed || ''} : ${t(STATE[p.state] || p.state)}`).join(' · ') : t('aucun'))),
      u.running ? [h('p', {}, '⏳ ', t('Mise à jour en cours…')), h('pre', { class: 'log' }, u.log.join('\n'))]
        : [h('p', { class: 'muted' }, t('La mise à jour vérifie le binaire, met à jour les modèles et les plugins installés, ajoute les modèles que la machine porte, puis redémarre la station.')),
          u.can_run ? h('p', {}, go, ' ', h('button', { type: 'button', class: 'ghost', onclick: () => update(true) }, t('Vérifier à nouveau')))
            : h('p', {}, t('Sur la machine :'), ' ', h('code', {}, u.command)),
          u.log.length ? h('details', {}, h('summary', {}, t('Dernier compte rendu')), h('pre', { class: 'log' }, u.log.join('\n'))) : null]].flat().filter(Boolean));
  };
  await draw();
  update().catch(() => {});
  K.every(30000, () => draw().catch(() => {}));
  K.every(3000, () => { if (armed || gone) update().catch(() => {}); });
}
