// Règles de l'écran Membres (m-members.js) : tris (activité, disque, ancienneté), part du disque, filtre des inactifs.
//   node crates/astro-ui/tests/members_rules.mjs
import assert from 'node:assert/strict';
import { sorted, usage, ago, FILTERS, SORTS } from '../web/m-members.js';

const D = 86400, now = 1000 * D;
const a = { hex: 'a', email: 'anne@x', last_activity: now - 40 * D, since: now - 300 * D, used_bytes: 9, quota_bytes: 10, removable: true };
const b = { hex: 'b', email: 'bob@x', last_activity: now - D, since: now - 10 * D, used_bytes: 1, quota_bytes: 100 };
const c = { hex: 'c', email: 'cap@x', last_activity: null, since: null, used_bytes: 5000, unlimited: true, quota_bytes: 0 };
const ids = (l, k) => sorted(l, k).map(x => x.hex).join('');

// Par défaut, les moins actifs d'abord (sans trace = le plus ancien de tous) : ce sont eux que le Capitaine regarde.
assert.equal(ids([a, b, c], 'idle'), 'cab');
assert.equal(ids([a, b, c], undefined), 'cab', 'tri inconnu : les moins actifs');
assert.equal(ids([a, b, c], 'recent'), 'bac');
// Disque : la part du quota, pas les octets ; le Capitaine (illimité) ne pèse rien.
assert.equal(usage(a), 0.9); assert.equal(usage(c), 0); assert.equal(usage({ used_bytes: 50, quota_bytes: 10 }), 1, 'plafonné à 100 %');
assert.equal(ids([a, b, c], 'disk'), 'abc');
// Ancienneté : les plus anciens d'abord, sans date à la fin.
assert.equal(ids([c, b, a], 'since'), 'abc');
assert.equal(ids([b, c, a], 'name'), 'abc');
// Le tri ne modifie pas la liste reçue.
const l = [b, a]; sorted(l, 'idle'); assert.equal(l[0], b);
// Jours écoulés.
assert.equal(ago(now - 3 * D - 5, now), 3); assert.equal(ago(null, now), null); assert.equal(ago(now + 50, now), 0, 'horloge en avance');
// Filtre des inactifs : ce que le serveur déclare retirable.
const idle = FILTERS.find(f => f[0] === 'idle')[2];
assert.deepEqual([a, b, c].filter(idle), [a]);
assert.ok(Object.keys(SORTS).includes('disk') && Object.keys(SORTS).includes('since'));
console.log('membres : tris et filtres OK');
