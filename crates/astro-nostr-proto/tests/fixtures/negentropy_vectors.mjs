// Vecteurs d'interopérabilité Negentropy V1, produits par l'implémentation de référence (Doug Hoyte, MIT).
import crypto from 'node:crypto';
globalThis.window = globalThis; // branche « navigateur » de la référence : crypto.subtle (présent dans Node 20)
// Référence : hoytech/negentropy (JS), telle qu'embarquée par @welshman/net — copier son negentropy.js à côté sous le nom ref.mjs.
import { Negentropy, NegentropyStorageVector } from './ref.mjs';
const id = s => new Uint8Array(crypto.createHash('sha256').update(s).digest());
function storage(items) { const s = new NegentropyStorageVector(); for (const [t, k] of items) s.insert(t, id(k)); s.seal(); return s; }
// A : 600 éléments ; B : 450 en commun, 40 à elle, des horodatages égaux pour exercer les préfixes.
const A = [], B = [];
for (let n = 0; n < 600; n++) { const t = 1700000000 + Math.floor(n / 3); A.push([t, 'e' + n]); if (n % 4 !== 0) B.push([t, 'e' + n]); }
for (let n = 0; n < 40; n++) B.push([1700000100 + n, 'b' + n]);
// La référence rend ses messages en hexadécimal (chaînes), et les identifiants en octets.
const hex = b => (typeof b === 'string' ? b : Buffer.from(b).toString('hex'));
async function run(limit) {
  const a = new Negentropy(storage(A), limit), b = new Negentropy(storage(B), limit);
  let msg = await a.initiate(); const msgs = [hex(msg)]; const have = [], need = [];
  for (let i = 0; i < 100; i++) {
    const [answer] = await b.reconcile(msg); msgs.push(hex(answer));
    const [next, h, nd] = await a.reconcile(answer); have.push(...h.map(hex)); need.push(...nd.map(hex));
    if (next === null || next === undefined) break; msg = next; msgs.push(hex(msg));
  }
  return { limit, msgs, have: have.sort(), need: need.sort() };
}
console.log(JSON.stringify({ a: A, b: B, runs: [await run(0), await run(4096)] }));
