// Règles de l'écran Écluses (m-locks.js) : état des cinq portes et occupants des six sas selon le mode.
//   node crates/astro-ui/tests/locks_rules.mjs
import assert from 'node:assert/strict';
import { gates, rooms, MODES, ROOMS } from '../web/m-locks.js';

const v = { gates: { crawl: true }, people: { hosted: 3, n1: 5, n1_mutual: 2, n2_known: 40 }, plane: { origin: true, kubo: true }, public: { files: 7 } };
const states = m => gates(v, m).map(g => g.state);

assert.equal(ROOMS.length, 6); assert.equal(gates(v, 'village').length, 5);
assert.deepEqual(MODES.map(m => m.id), ['forteresse', 'village', 'place']);
// Le coffre n'est jamais partagé, quel que soit le mode.
for (const m of ['forteresse', 'village', 'place']) assert.equal(states(m)[0], 'shut', m);
// Village : photos seul à seul, cercle ouvert, copie des amis d'amis, public en lecture seule.
assert.deepEqual(states('village'), ['shut', 'open', 'open', 'open', 'filter']);
// Forteresse : photos sur accord, cercle fermé aux non-réciproques, copie des seuls amis.
assert.deepEqual(states('forteresse'), ['shut', 'filter', 'shut', 'filter', 'filter']);
// Place publique : tout le monde publie.
assert.deepEqual(states('place'), ['shut', 'open', 'open', 'open', 'open']);
// Crawl coupé au lancement : la porte de la copie est fermée, quel que soit le mode.
assert.equal(gates({ gates: { crawl: false } }, 'village')[3].state, 'shut');

// Occupants : chiffres de la vue ; en Forteresse, le cercle montre qui reste dehors.
assert.deepEqual(rooms(v, 'village')[3].who, ["{0} suivi(s) + {1} ami(s) d'amis", 5, 40]);
assert.deepEqual(rooms(v, 'forteresse')[3].who, ['{0} suivi(s) à sens unique laissé(s) dehors', 3]);
assert.deepEqual(rooms(v, 'village')[4].who, ['Réseau ouvert, avec Kubo']);
assert.equal(rooms({ plane: { origin: false } }, 'village')[4].name, 'Constellation ẐEN');
assert.deepEqual(rooms({}, 'village')[1].who, ['{0} membre(s) hébergé(s)', 0], 'vue vide : zéro, jamais undefined');
console.log('écluses : portes et sas OK');
