'use strict';
// Naviguer ensemble : un membre se fait aider par un ami sur un site. La station ouvre la page et la montre ici à tous ; le membre
// choisit ce que chaque ami peut faire (regarder, montrer, prendre la main — mots de passe compris, la personne aidée peut ne pas
// pouvoir taper) et peut tout arrêter. On y arrive depuis une conversation (⋮ › Naviguer ensemble), depuis « Partager » du
// téléphone (#/cosurf?url=…), ou par le lien d'invitation reçu dans le chat (#/cosurf?id=…&station=…).
export default async function cosurf(root, K, q) {
  const { h, t } = K;
  const css = h('link', { rel: 'stylesheet', href: '/ui/m-agents.css' }); document.head.append(css); K.cleanup(() => css.remove());
  const S = await K.mod('m-screen.js');
  const post = (url, body) => K.api(url, { method: 'POST', body });
  const id = q.get('id'), station = q.get('station') || '';
  const LEVELS = [['watch', '👀 ' + t('Regarder')], ['point', '👉 ' + t('Montrer')], ['drive', '🖐 ' + t('Prendre la main')]];
  const say = h('p', { class: 'sr-only', role: 'status', 'aria-live': 'polite' }); // annonces pour les lecteurs d'écran
  if (!id) return start();

  // ── Une navigation en cours ──
  const qs = new URLSearchParams({ id, ...(station && { station }) }).toString();
  let v;
  try { v = await K.api('/api/v1/cosurf/view?' + qs); } catch (e) { root.replaceChildren(h('section', { class: 'card' }, h('p', { class: 'err', role: 'alert' }, e.message), h('p', {}, h('a', { href: '#/chat' }, t('Retour aux conversations'))))); return; }
  const myMode = l => (v.mine ? 'drive' : l || 'watch');
  const view = S.viewer(K, { stream: '/api/v1/cosurf/screen?' + qs, send: g => post('/api/v1/cosurf/input', { id, ...(station && { station }), ...g }), mode: myMode(v.level) });
  K.cleanup(view.stop);
  const status = h('p', { class: 'cs-status' }), left = h('span', { class: 'pill' });
  const phrase = l => (v.mine ? t('Vous gardez toujours la main. Choisissez ce que chaque ami peut faire.') : l === 'drive' ? t('Vous avez la main : vous pouvez cliquer et taper pour {0}.', v.owner_label) : l === 'point' ? t('Vous pouvez montrer un endroit : touchez la page.') : t('Vous regardez la page de {0}.', v.owner_label));
  const tick = () => { left.textContent = t('reste {0} min', Math.max(0, Math.ceil((v.until * 1000 - Date.now()) / 60000))); };
  tick(); const timer = setInterval(tick, 30000); K.cleanup(() => clearInterval(timer));
  status.textContent = phrase(v.level);
  view.on.level = d => { if (v.mine) return; v.level = d.level; view.setMode(myMode(d.level)); status.textContent = say.textContent = phrase(d.level); };
  const call = (hex, label) => K.mod('m-call.js').then(m => m.call(K, hex, label), e => K.toast(e.message));
  const guests = h('ul', { class: 'ag-list' });
  const showGuests = () => guests.replaceChildren(...v.guests.map(g => {
    const sel = h('select', { 'aria-label': t('Ce que {0} peut faire', g.label) }, LEVELS.map(([k, l]) => h('option', { value: k, selected: k === g.level }, l)));
    sel.onchange = async () => {
      try { v = await post('/api/v1/cosurf/level', { id, guest: g.hex, level: sel.value }); showGuests(); say.textContent = t('{0} : {1}', g.label, LEVELS.find(x => x[0] === sel.value)[1]); } catch (e) { K.toast(e.message); }
    };
    return h('li', {}, h('strong', {}, g.label), h('span', { class: 'ag-row' }, sel, h('button', { type: 'button', class: 'ghost', 'aria-label': t('Appeler {0} en visio', g.label), onclick: () => call(g.hex, g.label) }, '📹')));
  }));
  const end = v.mine
    ? K.sure('⏹ ' + t('Terminer'), () => post('/api/v1/cosurf/stop', { id }).then(() => { K.toast(t('Navigation partagée terminée'), 1); location.hash = '#/chat'; }, e => K.toast(e.message)), t('La page se ferme pour tout le monde.'))
    : h('button', { type: 'button', class: 'ghost', onclick: () => post('/api/v1/cosurf/stop', { id, ...(station && { station }) }).then(() => { location.hash = '#/chat'; }, e => K.toast(e.message)) }, t('Quitter'));
  const side = v.mine ? h('section', { class: 'card' }, h('h2', {}, t('Avec vous')), guests, h('p', { class: 'muted' }, t('« Prendre la main » : votre ami peut cliquer et taper à votre place, même un mot de passe. Un seul ami à la fois.')), h('p', {}, end))
    : h('section', { class: 'card' }, h('div', { class: 'ag-row' }, h('button', { type: 'button', onclick: () => call(v.owner, v.owner_label) }, '📹 ' + t('Appeler {0} en visio', v.owner_label)), end));
  if (v.mine) {
    showGuests();
    K.listen(ev => { if (ev.kind === 'cosurf' && ev.data?.cosurf === id) K.api('/api/v1/cosurf/view?' + qs).then(x => { v = x; showGuests(); }, () => {}); });
  }
  view.ended.then(() => { status.textContent = say.textContent = t('La navigation partagée est terminée.'); view.setMode('watch'); });
  const host = (() => { try { return new URL(v.url).host; } catch { return v.url; } })();
  root.replaceChildren(h('section', { class: 'card wide' }, h('h2', {}, '🧭 ', v.mine ? t('Vous naviguez sur {0}', host) : t('Page de {0}', v.owner_label), ' ', left), status, say, view.el), side);

  // ── Lancer une navigation partagée ──
  async function start() {
    const me = await K.api('/api/v1/me');
    const conv = q.get('conv'), names = (q.get('names') || '').split(',');
    let invited = (q.get('with') || '').split(',').map((x, i) => ({ hex: x, label: decodeURIComponent(names[i] || '') })).filter(g => /^[0-9a-f]{64}$/.test(g.hex) && g.hex !== me.hex);
    const pick = h('fieldset', { class: 'cs-pick' }, h('legend', {}, t('Avec qui ?')));
    if (!invited.length) {
      const r = await K.api('/api/v1/contacts').catch(() => ({ contacts: [] }));
      const friends = (r.contacts || []).filter(c => c.pubkey && c.friends && c.pubkey !== me.hex);
      pick.append(...(friends.length ? friends.map(c => h('label', {}, h('input', { type: 'checkbox', value: c.pubkey, 'data-label': c.name }), ' ', c.name)) : [h('p', { class: 'muted' }, t('Aucun ami réciproque dans votre carnet pour l\'instant.'))]));
    } else pick.append(h('p', {}, invited.map(g => g.label || '…' + g.hex.slice(-8)).join(', ')));
    const url = h('input', { name: 'url', required: true, inputmode: 'url', autocapitalize: 'off', spellcheck: false, value: q.get('url') || '', placeholder: 'impots.gouv.fr', 'aria-label': t('Adresse du site') });
    const levels = h('fieldset', { class: 'cs-pick' }, h('legend', {}, t('Ce que votre ami pourra faire')),
      ...[['drive', t('Tout faire pour moi : cliquer et taper, même un mot de passe')], ['point', t('Me montrer où cliquer, sans toucher')], ['watch', t('Seulement regarder')]].map(([k, l], i) => h('label', {}, h('input', { type: 'radio', name: 'level', value: k, checked: i === 0 }), ' ', l)),
      h('small', { class: 'muted' }, t('Vous pourrez changer d\'avis à tout moment.')));
    const go = h('button', { type: 'submit' }, '🧭 ' + t('Ouvrir la page et inviter'));
    const form = h('form', { class: 'cs-form' }, h('label', {}, t('Sur quel site ?'), url), pick, levels, go);
    form.onsubmit = async ev => {
      ev.preventDefault();
      const chosen = invited.length ? invited : [...pick.querySelectorAll('input:checked')].map(i => ({ hex: i.value, label: i.dataset.label }));
      if (!chosen.length) return K.toast(t('Choisissez au moins un ami'));
      go.disabled = true;
      try {
        const r = await post('/api/v1/cosurf', { url: url.value, guests: chosen, level: form.level.value });
        // L'invitation part dans la conversation : l'ami l'ouvre dans SON application (sa station relaie au besoin).
        const link = `#/cosurf?id=${r.id}${r.station ? '&station=' + r.station : ''}`, host = new URL(r.url).host;
        const text = `🧭 ${t('Venez naviguer avec moi sur {0}', host)} ${link}`;
        await Promise.all((conv ? [conv] : chosen.map(g => g.hex)).map(peer => post('/api/v1/chat/send', { peer, text }).catch(e => K.toast(e.message))));
        location.hash = `#/cosurf?id=${r.id}`;
      } catch (e) { K.toast(e.message); go.disabled = false; }
    };
    root.replaceChildren(h('section', { class: 'card wide' }, h('h2', {}, '🧭 ', t('Naviguer ensemble')),
      h('p', { class: 'muted' }, t('Un ami vous aide sur un site, en direct : vous voyez tous les deux la même page, il peut vous montrer où cliquer ou le faire pour vous. Vous pouvez aussi vous appeler en visio.')),
      h('p', { class: 'muted' }, t('La page s\'ouvre dans un navigateur neuf : connectez-vous au site dedans si besoin. Tout est effacé à la fin.')), form));
  }
}
