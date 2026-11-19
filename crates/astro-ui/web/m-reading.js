'use strict';
// « Documents et photos cherchables » (Mon compte) : qui lit le texte des images et des PDF du membre, où en est l'analyse, et — quand
// sa station ne sait pas lire — le choix, éclairé et réversible, d'une station amie (le texte seulement : les photos ne se cherchent
// par ce qu'elles montrent que sur une station qui le sait elle-même). Un état à la fois ; le Capitaine décide au-dessus.

export default function reading(card, K) {
  const { h, t } = K;
  const A = '/api/v1/me/reading';
  const title = h('h2', {}, t('Documents et photos cherchables'));
  const status = h('div', { role: 'status', 'aria-live': 'polite' });
  card.replaceChildren(title, status);
  const say = (...nodes) => status.replaceChildren(...nodes.flat().filter(Boolean));
  const fail = e => h('p', { class: 'err', role: 'alert' }, e.message);
  const day = s => new Date(s * 1000).toLocaleDateString(K.lang, { day: 'numeric', month: 'long', year: 'numeric' });

  // Où en est l'analyse : documents lus, photos analysées (si la station le sait), ce qui attend ; une barre s'il reste du travail.
  const many = (n, more) => (more ? t('plus de {0}', n) : n);
  const progress = v => {
    const p = v.progress, f = v.photos || { seen: 0, waiting: 0 };
    const done = p.read + f.seen, total = done + p.waiting + f.waiting;
    return h('div', { class: 'rd-progress' },
      h('p', {}, t('{0} document(s) lu(s)', p.read), p.waiting ? [' · ', t('{0} en attente', many(p.waiting, p.more))] : null, p.failed ? [' · ', t('{0} illisible(s) (abîmés ou protégés)', p.failed)] : null),
      f.seen || f.waiting ? h('p', {}, (f.videos ? t('{0} photo(s) ou vidéo(s) analysée(s)', f.seen) : t('{0} photo(s) analysée(s)', f.seen)), f.waiting ? [' · ', t('{0} en attente', many(f.waiting, f.more))] : null) : null,
      total > done ? h('progress', { max: total, value: done, 'aria-label': t('Avancement de l’analyse') }) : null);
  };
  // Ce que la station fait elle-même : la recherche par ce que montrent les photos n'existe que si elle le sait.
  const here = v => new Set(v.local || []);
  const sees = v => here(v).has('vision') ? h('p', {}, v.photos?.videos ? t('Vos photos et vos vidéos se retrouvent aussi par ce qu’elles montrent : essayez « vélo rouge » ou « coucher de soleil sur la mer ».') : t('Vos photos se retrouvent aussi par ce qu’elles montrent : essayez « vélo rouge » ou « coucher de soleil sur la mer ».')) : null;
  const keeps = () => h('p', { class: 'muted' }, t('La recherche par nom et description de vos photos fonctionne dans tous les cas.'));
  // La recherche par l'image juge une photo par rapport aux autres : il en faut assez.
  const few = v => (here(v).has('vision') && v.photos && v.photos.seen < 14 ? h('p', { class: 'muted' }, t('La recherche par ce que montrent les photos commence à partir de 14 photos analysées.')) : null);
  // Le membre garde la main : pause (ce qui est fait reste cherchable), et effacement de ce qui a été calculé, avec confirmation.
  const post = async (b, body, msg) => { b.disabled = true; try { await K.api(A, { method: 'POST', body }); K.toast(msg, 1); show(); } catch (e) { b.disabled = false; K.toast(e.message); } };
  const mine = (v, run = true) => {
    const made = v.progress.read + (v.photos?.seen || 0) > 0 || v.pack;
    const pause = h('button', { type: 'button', class: 'ghost' }, v.paused ? t('Reprendre l’analyse') : t('Mettre en pause l’analyse de mes fichiers'));
    pause.onclick = () => post(pause, { pause: !v.paused }, v.paused ? t('L’analyse reprend.') : t('Analyse en pause. Ce qui est déjà fait reste cherchable.'));
    const erase = made ? K.sure(t('Effacer ce qui a été calculé'), ev => post(ev.target, { erase: true }, t('Effacé. L’analyse est en pause : reprenez-la quand vous voulez.')), t('Le texte lu et les photos regardées seront oubliés ; l’analyse se met en pause.'), { class: 'ghost danger' }) : null;
    return [v.paused ? h('p', {}, h('span', { class: 'pill' }, t('En pause')), ' ', t('Rien de nouveau n’est analysé ; ce qui est déjà fait reste cherchable.')) : null,
      v.pack ? h('p', { class: 'muted' }, t('Votre index occupe {0} dans votre espace, et part avec vos sauvegardes.', K.bytes(v.pack))) : null,
      run || erase ? h('div', { class: 'bar' }, run ? pause : null, erase) : null];
  };

  async function show() {
    say(h('p', { class: 'muted' }, t('Chargement…')));
    let v;
    try { v = await K.api(A); } catch (e) { return say(fail(e)); }
    const pill = (on, label) => h('span', { class: 'pill ' + (on ? 'ok' : '') }, label);
    if (v.state === 'blocked') {
      return say(h('p', {}, pill(false, t('Désactivée'))),
        h('p', {}, v.blocked === 'station' ? t('Votre Capitaine a désactivé la lecture des documents sur cette station.') : t('Votre Capitaine a désactivé la lecture des documents pour votre compte. Parlez-lui-en si vous souhaitez l’activer.')),
        v.progress.read ? h('p', { class: 'muted' }, t('Ce qui a déjà été lu reste dans votre recherche.')) : null, keeps(), mine(v, false));
    }
    if (v.state === 'local') {
      return say(h('p', {}, pill(true, t('Active'))),
        h('p', {}, t('Votre station lit elle-même le texte de vos images et de vos PDF : tapez un mot qui s’y trouve dans la recherche de Fichiers. Rien ne quitte la station.')),
        sees(v), few(v), progress(v), mine(v));
    }
    if (v.state === 'friend') {
      // Une ou plusieurs stations : chacune avec ce qu'elle a lu et son propre « Retirer » ; tout arrêter reste à un geste.
      const stop = h('button', { type: 'button', class: 'ghost danger' }, v.stations.length > 1 ? t('Tout arrêter') : t('Arrêter'));
      const revoke = post;
      stop.onclick = () => revoke(stop, { revoke: true }, t('Arrêté : plus rien n’est envoyé. Ce qui a été lu reste dans votre recherche.'));
      const rows = v.stations.map(s => {
        const out = h('button', { type: 'button', class: 'ghost rd-out', 'aria-label': t('Retirer {0}', s.label) }, t('Retirer'));
        out.onclick = () => revoke(out, { revoke: s.peer }, t('{0} ne lit plus vos documents.', s.label));
        return h('li', { class: 'rd-lender' }, h('span', {}, h('strong', {}, s.label), h('small', {}, t('depuis le {0}', day(s.since)), s.read ? ' · ' + t('{0} lu(s)', s.read) : '')), v.stations.length > 1 ? out : null);
      });
      return say(h('p', {}, pill(true, t('Active'))),
        h('p', {}, v.stations.length > 1 ? t('Vos documents sont lus par ces stations amies, qui se partagent le travail :') : t('Vos documents sont lus par cette station amie :')),
        h('ul', { class: 'rd-lenders' }, rows),
        h('p', { class: 'muted' }, t('Chaque document passe par une station le temps d’être lu, chiffré pendant le trajet ; rien n’y est gardé. Si elle est occupée, il attend son tour ou va chez une autre.')),
        progress(v),
        h('div', { class: 'bar' }, h('button', { type: 'button', class: 'ghost', onclick: () => choose(v) }, t('Ajouter une station')), stop), mine(v));
    }
    if (v.state === 'unavailable') {
      return say(h('p', {}, pill(false, t('Indisponible'))),
        h('p', {}, t('Votre station ne sait pas lire les documents, et elle n’est reliée à aucune station amie pour l’instant.')), keeps(), mine(v, false));
    }
    // `off` : personne ne lit ; le membre peut confier la lecture.
    say(h('p', {}, pill(false, t('Inactive'))),
      h('p', {}, t('Retrouvez une facture ou une ordonnance en tapant un mot qu’elle contient. Votre station ne sait pas lire vos documents elle-même : une station amie peut le faire pour vous.')),
      sees(v), few(v), v.progress.waiting ? h('p', { class: 'muted' }, t('{0} document(s) attendent.', many(v.progress.waiting, v.progress.more))) : null,
      h('p', {}, h('button', { type: 'button', onclick: () => choose(v) }, t('Voir les stations amies disponibles'))), mine(v, here(v).has('vision')));
  }

  // Choisir une ou plusieurs stations et donner son accord : la station, ce qui se passe exactement, puis le consentement.
  async function choose(v) {
    say(h('p', { class: 'muted' }, h('span', { class: 'rd-spin', 'aria-hidden': 'true' }), t('Recherche des stations amies… (quelques secondes)')));
    let o;
    try { o = await K.api(A + '/offers'); } catch (e) { return say(fail(e), h('button', { type: 'button', class: 'ghost', onclick: show }, t('Retour'))); }
    const silent = o.silent ? h('p', { class: 'muted' }, t('{0} station(s) n’ont pas répondu (hors ligne) : réessayez plus tard pour les voir.', o.silent)) : null;
    if (!o.stations.length) {
      return say(h('p', {}, t('Aucune station amie ne propose la lecture pour l’instant.')),
        h('p', { class: 'muted' }, t('Pour en avoir une, votre Capitaine doit être ami avec un Capitaine dont la station la propose.')), silent, keeps(),
        h('div', { class: 'bar' }, h('button', { type: 'button', onclick: () => choose(v) }, t('Réessayer')), h('button', { type: 'button', class: 'ghost', onclick: show }, t('Retour'))));
    }
    // Celles qui lisent déjà pour le membre ne sont pas reproposées.
    const chosen = new Set((v.stations || []).map(s => s.peer));
    const free = o.stations.filter(s => !chosen.has(s.peer));
    if (!free.length) {
      return say(h('p', {}, t('Toutes les stations amies disponibles lisent déjà pour vous.')), silent, h('button', { type: 'button', class: 'ghost', onclick: show }, t('Retour')));
    }
    const radios = free.map((s, i) => {
      const id = 'rd-st-' + i;
      const input = h('input', { type: 'checkbox', name: 'rd-station', id, value: s.peer, checked: i === 0 });
      const near = s.distance_km != null ? t('à {0} km', Math.round(s.distance_km)) : null;
      return h('label', { class: 'rd-choice', for: id }, input, h('span', {}, h('strong', {}, s.name), near ? h('small', {}, near) : null));
    });
    const consent = h('input', { type: 'checkbox', id: 'rd-ok' });
    const go = h('button', { type: 'submit', disabled: true }, t('Confier la lecture'));
    const err = h('p', { class: 'err', role: 'alert', hidden: true });
    const picked = () => [...form.querySelectorAll('input[name=rd-station]:checked')].map(i => i.value);
    const sync = () => { go.disabled = !(consent.checked && picked().length); };
    consent.onchange = sync;
    const form = h('form', { class: 'rd-form' },
      h('fieldset', { onchange: () => sync() }, h('legend', {}, t('Choisissez une ou plusieurs stations amies')),
        free.length > 1 ? h('p', { class: 'muted' }, t('À plusieurs, elles se partagent le travail : la lecture va plus vite et continue si l’une est occupée ou éteinte.')) : null, radios),
      h('div', { class: 'rd-what' }, h('h3', {}, t('Ce qui se passe exactement')), h('ul', {},
        h('li', {}, t('Vos scans, PDF et photos de documents partent un par un vers la station choisie, chiffrés pendant le trajet.')),
        h('li', {}, t('Elle les lit en mémoire et ne renvoie que le texte lu. Rien n’est enregistré chez elle.')),
        h('li', {}, t('Ce texte est rangé chez vous : vos recherches se font ici, même si la station amie est éteinte.')),
        h('li', {}, t('Vous pouvez arrêter à tout moment ; ce qui a déjà été lu reste dans votre recherche.')))),
      h('label', { class: 'rd-choice rd-consent', for: 'rd-ok' }, consent, h('span', {}, t('J’ai compris, et j’accepte que les stations cochées lisent mes documents.'))),
      err, silent,
      h('div', { class: 'bar' }, go, h('button', { type: 'button', class: 'ghost', onclick: show }, t('Annuler'))));
    form.onsubmit = async ev => {
      ev.preventDefault();
      const peers = picked();
      if (!peers.length) { err.textContent = t('Choisissez au moins une station.'); err.hidden = false; return; }
      go.disabled = true; err.hidden = true;
      try {
        await K.api(A, { method: 'POST', body: { accept: { peers, tasks: ['ocr'] } } });
        K.toast(t('C’est fait : la lecture de vos documents commence.'), 1); show();
      } catch (e) { err.textContent = e.message; err.hidden = false; go.disabled = false; }
    };
    say(form);
    form.querySelector('input[name=rd-station]:checked')?.focus();
  }

  show();
}
