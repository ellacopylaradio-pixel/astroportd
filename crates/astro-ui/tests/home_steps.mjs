// Étapes « Bien démarrer » de l'accueil d'un membre (web/m-home.js) : fonction pure, rejouée sans navigateur.
//   node crates/astro-ui/tests/home_steps.mjs
import { readFileSync, writeFileSync, mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import assert from 'node:assert/strict';

const src = readFileSync(join(dirname(fileURLToPath(import.meta.url)), '../web/m-home.js'), 'utf8');
const tmp = join(mkdtempSync(join(tmpdir(), 'home-')), 'm-home.mjs');
writeFileSync(tmp, src);
const { steps, news } = await import(pathToFileURL(tmp));

// Compte neuf : rien de fait, tout est à proposer.
let r = steps({ dash: { sovereignty: { encrypted_files: 0 } }, faces: { enabled: true, people: [] }, friends: { friends: [] }, backup: { mine: null }, me: { ai: false } });
assert.equal(r.list.length, 6); assert.equal(r.left, 6); assert.equal(r.files, 0);
// Sans assistant, l'étape smartphone vient tout de suite après les photos (quitter Google et Apple).
assert.equal(r.list[1].title, 'Synchroniser votre smartphone');

// Tout fait : plus rien à proposer.
r = steps({ dash: { sovereignty: { encrypted_files: 12 } }, faces: { enabled: true, people: [{}] }, friends: { friends: [{ hex: 'a' }] }, backup: { mine: { status: 'accepted' } }, me: { ai: true }, devices: { devices: [{}] } });
assert.equal(r.left, 0); assert.equal(r.files, 12);

// Sauvegarde seulement demandée (pas acceptée) : pas cochée.
r = steps({ dash: {}, faces: { enabled: true, people: [] }, friends: {}, backup: { mine: { status: 'pending' } }, me: {} });
assert.equal(r.list.find(s => s.title.startsWith('Mettre')).done, false);

// Reconnaissance des visages absente de la station : l'étape n'existe pas.
r = steps({ dash: {}, faces: { enabled: false }, friends: {}, backup: null, me: {} });
assert.equal(r.list.length, 5); assert.ok(!r.list.some(s => s.title.includes('visage')));

// Appels qui échouent (null) : jamais d'exception.
r = steps({}); assert.equal(r.list.length, 5); assert.equal(r.left, 5);
// Lecture des documents : l'étape n'existe que si la station sait lire ; cochée quand elle lit (ici ou chez un ami).
r = steps({ reading: { state: 'off' } }); assert.equal(r.list.at(-1).href, '#/devices?card=reading'); assert.equal(r.list.at(-1).done, false);
assert.equal(steps({ reading: { state: 'friend' } }).list.at(-1).done, true);
console.log('accueil : étapes OK');

// « Pour vous » : rien à faire = encart vide ; partages en attente d'abord, puis ressemblances (avant les visages inconnus).
assert.deepEqual(news({ faces: null, shares: null }), []);
let n = news({ faces: { suggestions: [{}, {}], unknown: 9 }, shares: { pending: [{ count: 12, name: 'Fred' }] } });
assert.equal(n.length, 2); assert.deepEqual(n[0].text.slice(1), [12, 'Fred']); assert.equal(n[1].href, '#/people/tri?grand'); assert.deepEqual(n[1].text, ['{0} visage(s) à confirmer', 2]);
assert.deepEqual(news({ faces: { suggestions: [], unknown: 4 } })[0].text, ['{0} visage(s) à nommer', 4]);
// Un ami en secours passe avant tout le reste.
n = news({ faces: { suggestions: [{}], unknown: 0 }, shares: { pending: [] }, backup: { hosting: [{ label: 'Léa', rescue: { station: 'x' } }, { label: 'Max' }] } });
assert.deepEqual(n.map(x => x.icon), ['🆘', '🫂']); assert.equal(n[0].href, '#/backup'); assert.deepEqual(n[0].text[1], 'Léa');
// Vigie de Public : un fichier qui en dit trop passe avant les photos et les visages ; rien à signaler = rien d'affiché.
// Une photo HEIC non servie n'en dit pas trop (les autres photos partent nettoyées) : elle n'alarme pas l'accueil.
n = news({ faces: { suggestions: [{}] }, shares: { pending: [{ count: 1, name: 'Fred' }] }, fp: { privacy: { unserved: 2, sensitive: 3 } } });
assert.deepEqual(n.map(x => x.icon), ['🔒', '📸', '🫂']); assert.equal(n[0].text[1], 3); assert.equal(n[0].href, '#/cloud?p=%2FPublic');
assert.deepEqual(news({ fp: { privacy: { unserved: 2, sensitive: 0 } } }), []);
console.log('accueil : « Pour vous » OK');

// Gardiens : aucun hors de cette station = alerte tant qu'il n'y en a pas ; un gardien d'ailleurs la fait taire.
n = news({ backup: { hosting: [], guardians: [] } });
assert.deepEqual(n.map(x => x.icon), ['⚠️']); assert.equal(n[0].href, '#/backup');
assert.deepEqual(news({ backup: { guardians: [{ label: 'Bob', same_station: true }] } }).map(x => x.icon), ['⚠️'], 'un gardien sur la même station ne protège pas');
assert.deepEqual(news({ backup: { guardians: [{ label: 'Léa', same_station: false }] } }), []);
assert.deepEqual(news({ backup: { mine: null } }), [], 'ancienne réponse sans gardiens : pas d\'alerte inventée');
// Restauration en cours : le suivi du retour des fichiers, après un secours éventuel ; terminée = plus rien.
n = news({ backup: { hosting: [{ label: 'Léa', rescue: {} }], guardians: [{ same_station: false }], restore: { state: 'late', returned: ['Léa'], waiting: [{ label: 'Paul', seen_days: 4 }] } } });
assert.deepEqual(n.map(x => x.icon), ['🆘', '📦']); assert.deepEqual(n[1].text.slice(1), [1, 1]);
assert.deepEqual(news({ backup: { guardians: [{ same_station: false }], restore: { state: 'done', returned: ['Léa'], waiting: [] } } }), []);
console.log('accueil : gardiens et retour des fichiers OK');
