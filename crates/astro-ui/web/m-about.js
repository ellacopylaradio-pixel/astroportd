'use strict';
// À propos : ce qu'est Astroport, qui le fait, sur quoi il repose, et comment y contribuer. Ouvert par le titre « Astroport ».
export default async function about(root, K) {
  const { h, t } = K;
  const out = (href, label) => h('a', { href, target: '_blank', rel: 'noopener' }, label);
  const item = (title, text, ...links) => h('li', {}, h('strong', {}, title), h('br'), h('span', { class: 'muted' }, text), links.length ? h('br') : null, ...links.flatMap((l, i) => (i ? [' · ', l] : [l])));
  const version = h('span', {});
  fetch('/health').then(r => r.json()).then(s => { if (s.version) version.replaceChildren(t('Version {0}', s.version), ' · '); }, () => {});

  root.replaceChildren(
    h('section', { class: 'card wide' }, h('h2', {}, '🛰️ ', t('Astroport, c\'est quoi ?')),
      h('p', {}, t('Astroport est le logiciel de cette station : votre espace à vous (fichiers, photos, proches, messages, Ẑen), hébergé chez un Capitaine que vous connaissez plutôt que chez une grande entreprise.')),
      h('p', {}, t('Les stations s\'entraident : chacune garde une copie chiffrée de ce que ses amis lui confient.')),
      h('p', {}, t('Il fait partie d\'UPlanet, un réseau coopératif. Le Ẑen y est une unité de compte interne, adossée à la Ğ1, la monnaie libre : il n\'est pas fait pour spéculer.')),
      h('p', { class: 'muted' }, version, t('Logiciel libre sous licence'), ' ', out('https://www.gnu.org/licenses/agpl-3.0.html', 'AGPL-3.0'))),
    h('section', { class: 'card wide' }, h('h2', {}, '🙏 ', t('Crédits')),
      h('ul', {},
        item(t('Conception et développement'), t('Fred et les contributeurs du Ğ1FabLab, avec les Capitaines qui testent les stations.'), out('https://qo-op.com', 'qo-op.com')),
        item(t('Construit sur des logiciels libres'), 'Nostr · IPFS · libp2p · Duniter (Ğ1) · Rust · SQLite · ONNX Runtime · wasmtime'),
        item(t('Intelligence locale'), 'PP-OCRv6 (PaddleOCR, Apache-2.0) · SigLIP 2 (Google, Apache-2.0) · Multilingual E5 (Microsoft, MIT) · Whisper (OpenAI, Apache-2.0)'),
        item(t('Données'), t('Villes : GeoNames, licence CC BY 4.0.'), out('https://www.geonames.org', 'geonames.org')))),
    h('section', { class: 'card wide' }, h('h2', {}, '🤝 ', t('Contribuer')),
      h('ul', {},
        item(t('Soutenir la coopérative'), t('Une adhésion ou un don, sur un registre public où chaque dépense est inscrite. Utilisez le courriel de votre MULTIPASS.'),
          out('https://opencollective.com/monnaie-libre/contribute/adhesion-5eur-mois-4726', t('5 € par mois')), out('https://opencollective.com/monnaie-libre', t('Le collectif sur Open Collective'))),
        item(t('Devenir Capitaine'), t('Allumez une station chez vous pour héberger vos proches et renforcer le réseau.'),
          out('https://github.com/papiche/astroportd/blob/main/docs/GUIDE.md', t('Guide du Capitaine')), out('https://map.astroport.one', t('Carte des stations'))),
        item(t('Signaler un souci, proposer une idée'), t('Décrivez ce que vous faisiez et ce que vous avez vu : chaque retour compte.'),
          out('https://github.com/papiche/astroportd/issues', t('Signaler sur GitHub'))),
        item(t('Coder, traduire, documenter'), t('Le code et la documentation sont ouverts : forkez, hébergez, faites-le vôtre.'),
          out('https://github.com/papiche/astroportd', t('Code source'))))));
}
