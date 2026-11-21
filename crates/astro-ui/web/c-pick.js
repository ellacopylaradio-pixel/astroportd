'use strict';
// « Nouveau message » : à qui peut-on écrire ? Les proches réciproques (vous vous suivez : le message arrive, la réponse aussi) et
// les fiches du carnet qui portent une clé Nostr (sans suivi réciproque, le message peut ne pas arriver : on le dit avant).
// « 👥 Nouveau groupe » : on coche les participants (2 au moins), on nomme le groupe, et la conversation s'ouvre.
const HEX = /^[0-9a-f]{64}$/;
const norm = s => (s || '').normalize('NFD').replace(/\p{M}/gu, '').toLowerCase();

export default async function pick(K, host, open) {
  const { h, t, api } = K;
  host.querySelector('.ch-pick')?.remove();
  const q = h('input', { type: 'search', placeholder: t('Nom ou email…'), 'aria-label': t('Chercher une personne'), autocomplete: 'off', maxlength: 100 });
  const list = h('ul', { class: 'ab-list', role: 'listbox', 'aria-label': t('Personnes') }, h('li', { class: 'muted ab-empty' }, t('Chargement…')));
  const panel = h('div', { class: 'ch-pick', role: 'dialog', 'aria-label': t('Nouveau message'), onkeydown: ev => { if (ev.key === 'Escape') panel.remove(); } },
    h('div', { class: 'ab-search ch-top' }, h('button', { type: 'button', class: 'ghost', 'aria-label': t('Fermer'), onclick: () => panel.remove() }, '←'), q),
    h('div', { class: 'ab-scroll' }, list));
  // Mode groupe : cases à cocher, nom, bouton de création.
  const picked = new Set(), gname = h('input', { maxlength: 80, placeholder: t('Nom du groupe (facultatif)'), 'aria-label': t('Nom du groupe') });
  const make = h('button', { type: 'button', disabled: true }), foot = h('div', { class: 'ch-gfoot', hidden: true }, gname, make);
  let grouping = false;
  const toggle = h('button', { type: 'button', class: 'ghost ch-gtoggle', onclick: () => { grouping = !grouping; picked.clear(); foot.hidden = !grouping; toggle.textContent = grouping ? t('Annuler le groupe') : '👥 ' + t('Nouveau groupe'); count(); paint(); } }, '👥 ' + t('Nouveau groupe'));
  const count = () => { make.textContent = t('Créer le groupe ({0})', picked.size + 1); make.disabled = picked.size < 2; };
  make.onclick = async () => {
    make.disabled = true;
    try { const g = await api('/api/v1/chat/group', { method: 'POST', body: { name: gname.value.trim(), members: [...picked] } }); panel.remove(); open(g.id, g.name || t('Groupe de {0} personnes', g.members.length), false, false, null, true); }
    catch (e) { K.toast(e.message); count(); }
  };
  panel.insertBefore(toggle, panel.lastChild); panel.append(foot);
  host.append(panel); q.focus();
  const [friends, book] = await Promise.all([api('/api/v1/friends').then(r => r.friends, () => []), api('/api/v1/book').then(r => r.contacts, () => [])]);
  const people = new Map();
  for (const f of friends) people.set(f.hex, { hex: f.hex, label: f.label, mutual: true });
  for (const c of book) if (!c.me && HEX.test(c.pubkey || '') && !people.has(c.pubkey)) people.set(c.pubkey, { hex: c.pubkey, label: c.name || c.pubkey.slice(0, 8), sub: (c.emails || [])[0] || '', mutual: false });
  const all = [...people.values()].sort((a, b) => b.mutual - a.mutual || a.label.localeCompare(b.label));
  const paint = () => {
    const k = norm(q.value.trim()), shown = all.filter(p => !k || norm(`${p.label} ${p.sub || ''}`).includes(k));
    list.replaceChildren(...shown.map(p => h('li', { role: 'option' }, h('button', { type: 'button', class: 'ab-item', 'aria-pressed': grouping ? String(picked.has(p.hex)) : null, onclick: ev => {
      if (grouping) { picked.has(p.hex) ? picked.delete(p.hex) : picked.add(p.hex); ev.currentTarget.setAttribute('aria-pressed', String(picked.has(p.hex))); count(); return; }
      panel.remove(); open(p.hex, p.label, false, !p.mutual);
    } },
      grouping ? h('span', { class: 'ch-check', 'aria-hidden': 'true' }) : null,
      h('span', { class: 'ab-av' }, (p.label[0] || '?').toUpperCase()),
      h('span', { class: 'ab-name' }, h('strong', {}, p.label), h('small', { class: 'muted' }, p.mutual ? t('Vous vous suivez') : '⚠️ ' + t('Pas de suivi réciproque : le message peut ne pas arriver')))))));
    if (!shown.length) list.replaceChildren(h('li', { class: 'muted ab-empty' }, all.length ? t('Personne ne correspond.') : t('Personne à qui écrire pour l\'instant : suivez quelqu\'un qui vous suit, ou ajoutez sa clé Nostr à sa fiche du carnet.')));
  };
  q.oninput = paint; paint();
}
