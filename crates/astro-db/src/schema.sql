-- astro-db : schéma de uplanet.db
--
-- RÈGLE FONDATRICE : Nostr et IPFS sont les seules sources de vérité.
-- Ce fichier est un CACHE ET UN INDEX DE RECHERCHE reconstruisible par `astroportd replay`.
--   * Section 1 « PROJECTION » : tout est rejouable depuis des événements Nostr et des CID IPFS.
--     `src_event` = id de l'événement Nostr d'origine, `src_cid` = CID IPFS du blob d'origine.
--   * Section 2 « LOCAL » : état volontairement non rejouable (anti-rejeu, files, journaux).
--     Sa perte est sans conséquence au-delà de la fenêtre en cours.
-- Aucune clé racine (nsec, dunikey, SSSS) n'est stockée ici.

PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA foreign_keys = ON;
PRAGMA busy_timeout = 30000;

CREATE TABLE IF NOT EXISTS schema_info (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
) STRICT, WITHOUT ROWID;
INSERT OR IGNORE INTO schema_info(key, value) VALUES ('schema_version', '1');

-- ═══════════════════════════ 1. PROJECTION (rejouable) ═══════════════════════════

-- Curseur de rejeu par source (relai, filtre, backup CID) : permet de reprendre `replay`.
CREATE TABLE IF NOT EXISTS replay_state (
  source     TEXT PRIMARY KEY,         -- ex. 'relay:wss://…', 'backup:<hex>'
  cursor     TEXT,                     -- since=<ts> ou dernier event id
  updated_at INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

-- Événements du relai local (copie de travail ; la vérité reste le relai/la constellation).
-- Table à `rowid` (pas WITHOUT ROWID) : le contenu, la signature et les tags sont gros, et un `rowid` entier sert de clé aux
-- tags et à l'index plein texte (mesuré sur 200 000 événements : base 28 % plus petite, insertion 2× plus rapide, jointure
-- par tag ≈ 9× plus rapide qu'avec l'identifiant de 64 caractères comme clé).
CREATE TABLE IF NOT EXISTS nostr_events (
  seq        INTEGER PRIMARY KEY,      -- identifiant interne : jamais exposé, change si l'événement est réinséré
  id         TEXT NOT NULL UNIQUE,
  pubkey     TEXT NOT NULL,
  kind       INTEGER NOT NULL,
  created_at INTEGER NOT NULL,
  content    TEXT NOT NULL,
  sig        TEXT NOT NULL,
  tags       TEXT NOT NULL,            -- JSON
  d_tag      TEXT,                     -- kinds remplaçables paramétrés (30000-39999)
  expires_at INTEGER                   -- NIP-40
) STRICT;
CREATE INDEX IF NOT EXISTS ne_kind_ts ON nostr_events(kind, created_at DESC);
CREATE INDEX IF NOT EXISTS ne_author  ON nostr_events(pubkey, kind, created_at DESC);
CREATE UNIQUE INDEX IF NOT EXISTS ne_replaceable
  ON nostr_events(pubkey, kind, d_tag) WHERE d_tag IS NOT NULL;

-- Recherche plein texte NIP-50 : index FTS5 « sans contenu » (le texte reste dans `nostr_events`), dont le rowid EST le `seq` de
-- l'événement, tenu à jour par triggers pour que toute insertion, remplacement, suppression NIP-09 ou purge d'expiration le
-- suive. Les messages chiffrés (kinds 4 et 1059) ne sont pas indexés. Reconstructible : `astro_db::events::rebuild_search_index`.
CREATE VIRTUAL TABLE IF NOT EXISTS nostr_fts USING fts5(content, content='', contentless_delete=1, tokenize='unicode61 remove_diacritics 2');
CREATE TRIGGER IF NOT EXISTS nostr_fts_ai AFTER INSERT ON nostr_events WHEN new.kind NOT IN (4, 1059) BEGIN
  INSERT INTO nostr_fts(rowid, content) VALUES (new.seq, new.content);
END;
CREATE TRIGGER IF NOT EXISTS nostr_fts_ad AFTER DELETE ON nostr_events WHEN old.kind NOT IN (4, 1059) BEGIN
  DELETE FROM nostr_fts WHERE rowid = old.seq;
END;

CREATE TABLE IF NOT EXISTS nostr_tags (
  event_id INTEGER NOT NULL REFERENCES nostr_events(seq) ON DELETE CASCADE,   -- `seq` de l'événement
  name     TEXT NOT NULL,              -- d, p, e, t, g
  value    TEXT NOT NULL,
  PRIMARY KEY (event_id, name, value)
) STRICT, WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS nt_lookup ON nostr_tags(name, value);

-- Identités : DID kind 30800 (+ kind 0 pour home_station). `key_formula` est détectée par
-- astro-lifecycle en comparant G1PUBNOSTR ; elle n'entraîne JAMAIS de destruction de compte.
CREATE TABLE IF NOT EXISTS identities (
  hex          TEXT PRIMARY KEY,       -- clé Nostr x-only (64 hex)
  role         TEXT NOT NULL CHECK (role IN ('node','captain','multipass','zencard','love')),
  email        TEXT,
  npub         TEXT,
  g1pub_nostr  TEXT,
  g1pub        TEXT,
  ipns         TEXT,
  lang         TEXT,
  lat          REAL,
  lon          REAL,
  umap         TEXT,
  key_formula  TEXT NOT NULL DEFAULT 'isolated'
               CHECK (key_formula IN ('isolated','raw','concat_swarm','coop_prefix')),
  home_station TEXT,                   -- '<IPFSNODEID>:<NODE_HEX>'
  peer_id      TEXT,                   -- PeerID libp2p d'un NODE (flux P2P → NODE)
  contract     TEXT CHECK (contract IN ('active_rental','cooperative_member') OR contract IS NULL),
  status       TEXT,                   -- contractStatus brut du DID
  hosted       INTEGER NOT NULL DEFAULT 0 CHECK (hosted IN (0,1)),  -- 1 = hébergé ici (état LOCAL, jamais écrasé par un DID)
  quota_bytes  INTEGER,
  created_at   INTEGER,
  todate       TEXT,
  src_event    TEXT                    -- DID 30800
) STRICT, WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS identities_email ON identities(email) WHERE email IS NOT NULL;
-- Un email = un MULTIPASS hébergé ici (l'email tient lieu de npub). Seuls les MULTIPASS : la ZenCard et le Capitaine d'un
-- même membre portent légitimement la même adresse que son MULTIPASS.
CREATE UNIQUE INDEX IF NOT EXISTS identities_email_multipass ON identities(lower(email)) WHERE email IS NOT NULL AND hosted = 1 AND role = 'multipass';
CREATE INDEX IF NOT EXISTS identities_hosted ON identities(hex) WHERE hosted = 1;

-- Graphe social N1/N2, projection des kind 3.
CREATE TABLE IF NOT EXISTS social_graph (
  owner_hex      TEXT NOT NULL,
  pubkey         TEXT NOT NULL,
  level          INTEGER NOT NULL CHECK (level IN (0,1,2)),
  via            TEXT,
  certified_a4l  INTEGER NOT NULL DEFAULT 0,
  updated_at     INTEGER NOT NULL,
  src_event      TEXT,
  PRIMARY KEY (owner_hex, pubkey)
) STRICT, WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS sg_pubkey ON social_graph(pubkey);

-- Marqueurs d'idempotence (paiements, cérémonies) : rejouables depuis les événements économiques.
CREATE TABLE IF NOT EXISTS done (
  key       TEXT PRIMARY KEY,
  value     TEXT,
  at        INTEGER NOT NULL,
  src_event TEXT
) STRICT, WITHOUT ROWID;

-- uCloud : l'index chiffré (manifest) est un blob IPFS référencé par un événement 30078 ;
-- ces lignes en sont la projection.
-- Table à `rowid` : chaque ligne porte un JSON `data` de plusieurs centaines d'octets, et les 6 index secondaires d'une table
-- WITHOUT ROWID recopient la clé `(owner_hex, path)` (≥ 70 octets) dans chaque entrée. Mesuré sur 150 000 entrées : base 26 %
-- plus petite, insertion 1,6× plus rapide, liste d'un dossier ≈ 100× plus rapide. L'unicité de `(owner_hex, path)` reste garantie.
CREATE TABLE IF NOT EXISTS ucloud_entries (
  id            INTEGER PRIMARY KEY,   -- interne
  owner_hex     TEXT NOT NULL,
  path          TEXT NOT NULL,
  parent        TEXT NOT NULL,
  type          TEXT NOT NULL,
  cid           TEXT,
  mime          TEXT,
  size          INTEGER,
  mtime         INTEGER,
  taken_at      INTEGER,               -- HEURE MURALE de la prise de vue (EXIF lu comme UTC, sans fuseau) : la date civile du membre
  sha_orig      TEXT,
  sha_plain     TEXT,
  faceid_status TEXT CHECK (faceid_status IN ('faces','no_face','pending','timeout','unreadable') OR faceid_status IS NULL),
  origin        TEXT,
  phash         INTEGER,               -- dHash 64 bits, entier signé
  lat           REAL,
  lon           REAL,
  desc_text     TEXT,
  data          TEXT NOT NULL,         -- entrée JSON complète du manifest
  src_cid       TEXT                   -- CID du manifest d'où provient la ligne
) STRICT;
CREATE UNIQUE INDEX IF NOT EXISTS ue_owner_path ON ucloud_entries(owner_hex, path);
CREATE INDEX IF NOT EXISTS ue_parent     ON ucloud_entries(owner_hex, parent, path);   -- enfants d'un dossier, déjà triés
CREATE INDEX IF NOT EXISTS ue_cid        ON ucloud_entries(cid)       WHERE cid IS NOT NULL;
-- Miniature d'une entrée : le `CidGate` d'un bassin partagé demande « ce CID est-il à moi ? » à chaque blob servi. Expression
-- gardée par `json_valid` (un JSON abîmé ne doit jamais faire échouer une écriture) ; `astro_dav::pool` l'écrit à l'identique.
CREATE INDEX IF NOT EXISTS ue_thumb_cid  ON ucloud_entries(CASE WHEN json_valid(data) THEN json_extract(data, '$.thumb.cid') END);
CREATE INDEX IF NOT EXISTS ue_sha_orig   ON ucloud_entries(sha_orig)  WHERE sha_orig IS NOT NULL;
CREATE INDEX IF NOT EXISTS ue_sha_plain  ON ucloud_entries(sha_plain) WHERE sha_plain IS NOT NULL;
CREATE INDEX IF NOT EXISTS ue_taken      ON ucloud_entries(owner_hex, COALESCE(taken_at, mtime) DESC)
  WHERE type = 'file' AND cid IS NOT NULL;
CREATE INDEX IF NOT EXISTS ue_phash      ON ucloud_entries(phash)     WHERE phash IS NOT NULL;
CREATE INDEX IF NOT EXISTS ue_geo        ON ucloud_entries(lat, lon)  WHERE lat IS NOT NULL;

-- Trousseau des clés AES de fichier. Projection d'un blob chiffré NIP-44 vers soi-même
-- (publié sur IPFS, CID dans un 30078) : jamais la seule copie de ces clés.
CREATE TABLE IF NOT EXISTS ucloud_keyring (
  owner_hex TEXT NOT NULL,
  cid       TEXT NOT NULL,
  key_hex   TEXT NOT NULL CHECK (length(key_hex) = 64),
  enc       TEXT NOT NULL DEFAULT 'uenc-aes256gcm',
  src_cid   TEXT,
  PRIMARY KEY (owner_hex, cid)
) STRICT, WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS ucloud_embeddings (
  emb_id INTEGER PRIMARY KEY,
  cid    TEXT NOT NULL UNIQUE,
  model  TEXT NOT NULL,
  dsrc   TEXT
) STRICT;

-- Annuaire des contacts : index des fiches vCard de `/Contacts` (le chiffré reste la source de vérité ; table rebuildable
-- par un balayage). `pubkey` = clé Nostr rattachée à la fiche : donnée par la fiche (X-NOSTR-*) ou déduite de l'email d'un
-- MULTIPASS HÉBERGÉ ici (un email auto-déclaré dans un DID n'est jamais une preuve). `cid` = état indexé (étiquette de la fiche).
CREATE TABLE IF NOT EXISTS identity_links (
  owner_hex  TEXT NOT NULL,
  path       TEXT NOT NULL,            -- /Contacts/<fichier>.vcf
  uid        TEXT,
  name       TEXT NOT NULL,            -- FN
  name_key   TEXT NOT NULL,            -- FN comparable (minuscules, sans accents) : rapproche les visages nommés
  emails     TEXT NOT NULL DEFAULT '[]',
  pubkey     TEXT,
  via        TEXT CHECK (via IN ('vcard','email') OR via IS NULL),
  g1pub      TEXT,
  warning    TEXT,                     -- X-NOSTR-* illisible, fiche invalide…
  cid        TEXT NOT NULL,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (owner_hex, path)
) STRICT, WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS identity_links_name ON identity_links(owner_hex, name_key);

-- Carnet d'adresses : les champs lus des fiches de `/Contacts` (téléphones, société, adresses, groupes…) pour la recherche et l'affichage.
-- PROJECTION reconstructible : le fichier chiffré reste la source de vérité, un balayage de `/Contacts` la refait.
CREATE TABLE IF NOT EXISTS contact_cards (
  owner_hex TEXT NOT NULL,
  path      TEXT NOT NULL,
  fields    TEXT NOT NULL,             -- JSON de `astro_dav::vcard::Fields`
  search    TEXT NOT NULL,             -- texte comparable : noms, société, emails, chiffres des téléphones, notes, groupes
  me        INTEGER NOT NULL DEFAULT 0 CHECK (me IN (0,1)),  -- 1 : la fiche du titulaire lui-même (X-ASTRO-ME), liée à son profil Nostr
  PRIMARY KEY (owner_hex, path)
) STRICT, WITHOUT ROWID;

-- Conversations privées 1 à 1 (NIP-17) : messages déchiffrés. ÉTAT LOCAL (les enveloppes restent dans nostr_events ;
-- remettre le repère kv `chat_seq` à zéro les rouvre toutes).
CREATE TABLE IF NOT EXISTS chat_messages (
  owner_hex TEXT NOT NULL,
  id        TEXT NOT NULL,             -- identifiant NIP-01 de la rumeur
  peer      TEXT NOT NULL,             -- l'autre personne (clé Nostr, 64 hex)
  mine      INTEGER NOT NULL CHECK (mine IN (0,1)),
  at        INTEGER NOT NULL,
  text      TEXT NOT NULL,
  state     TEXT NOT NULL DEFAULT 'ok' CHECK (state IN ('ok','failed')),
  read      INTEGER NOT NULL DEFAULT 0 CHECK (read IN (0,1)),
  n         INTEGER NOT NULL DEFAULT 0,   -- ordre d'insertion : départage deux messages de la même seconde
  reply     TEXT,                         -- message cité (identifiant de rumeur, balise `e`)
  meta      TEXT,                         -- pièces jointes (JSON : nom, type, taille ; chemin uCloud côté expéditeur)
  seen      INTEGER NOT NULL DEFAULT 0,   -- message envoyé que l'autre a lu (accusé de lecture reçu)
  author    TEXT,                         -- conversation de groupe : qui a écrit (sinon `peer`, ou le titulaire si `mine`)
  edited    INTEGER NOT NULL DEFAULT 0,   -- texte modifié par son auteur après l'envoi (balise `t` astro-edit)
  PRIMARY KEY (owner_hex, id)
) STRICT, WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS chat_by_peer ON chat_messages(owner_hex, peer, at);

-- Conversations de groupe (NIP-17 : une rumeur, plusieurs `p`) vues par un titulaire. ÉTAT LOCAL : l'identifiant se recalcule
-- des participants (empreinte de leurs clés triées), le nom vient de la balise `subject` ; relire les enveloppes les recrée.
CREATE TABLE IF NOT EXISTS chat_groups (
  owner_hex TEXT NOT NULL,
  id        TEXT NOT NULL,             -- 64 hex : sha256("astro-group:" + clés triées, séparées par des virgules)
  name      TEXT,
  members   TEXT NOT NULL,             -- JSON : clés des participants, titulaire compris
  PRIMARY KEY (owner_hex, id)
) STRICT, WITHOUT ROWID;

-- Réactions aux messages du chat (NIP-25 kind 7 emballé en NIP-59) : une par auteur et par message. ÉTAT LOCAL, recréé en relisant
-- les enveloppes.
CREATE TABLE IF NOT EXISTS chat_reactions (
  owner_hex TEXT NOT NULL,
  msg_id    TEXT NOT NULL,
  author    TEXT NOT NULL,
  emoji     TEXT NOT NULL,
  at        INTEGER NOT NULL,
  PRIMARY KEY (owner_hex, msg_id, author)
) STRICT, WITHOUT ROWID;

-- Préférences du titulaire pour une conversation : épinglée, archivée, en sourdine (pas de notification). PRÉFÉRENCES LOCALES,
-- jamais publiées.
CREATE TABLE IF NOT EXISTS chat_prefs (
  owner_hex TEXT NOT NULL,
  peer      TEXT NOT NULL,
  pinned    INTEGER NOT NULL DEFAULT 0 CHECK (pinned IN (0,1)),
  archived  INTEGER NOT NULL DEFAULT 0 CHECK (archived IN (0,1)),
  muted     INTEGER NOT NULL DEFAULT 0 CHECK (muted IN (0,1)),
  PRIMARY KEY (owner_hex, peer)
) STRICT, WITHOUT ROWID;

-- Partages FaceCloud déjà effectués (un par photo et destinataire) : état LOCAL, évite les doublons au retraitement.
CREATE TABLE IF NOT EXISTS face_shares (
  owner_hex     TEXT NOT NULL,
  cid           TEXT NOT NULL,
  recipient_hex TEXT NOT NULL,
  via           TEXT NOT NULL CHECK (via IN ('local','nip17')),
  at            INTEGER NOT NULL,
  PRIMARY KEY (owner_hex, cid, recipient_hex)
) STRICT, WITHOUT ROWID;

-- Partages FaceCloud en zone de précaution (visage flou/de profil ou score < 0,88) : l'envoi attend un clic de validation
-- groupée par ami dans l'interface. État LOCAL ; une ligne par photo et destinataire.
CREATE TABLE IF NOT EXISTS face_share_pending (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  owner_hex     TEXT NOT NULL,
  recipient_hex TEXT NOT NULL,
  path          TEXT NOT NULL,
  cid           TEXT NOT NULL,
  det_score     REAL NOT NULL,
  score         REAL NOT NULL,
  created_at    INTEGER NOT NULL,
  UNIQUE (owner_hex, recipient_hex, cid)
) STRICT;
CREATE INDEX IF NOT EXISTS fsp_owner ON face_share_pending(owner_hex, recipient_hex);

-- Invitations à rejoindre la constellation en recevant des photos (lien `/i/<jeton>`). Le jeton (128 bits aléatoires) est la
-- seule preuve de possession du lien. `items` : fichiers choisis à la création (chemin, CID, nom, type, taille, empreinte) ;
-- leurs clés restent dans `ucloud_keyring` et ne quittent jamais la station avant la réclamation.
CREATE TABLE IF NOT EXISTS invites (
  token        TEXT PRIMARY KEY,
  owner_hex    TEXT NOT NULL,
  label        TEXT NOT NULL DEFAULT '',   -- nom du destinataire, tel que l'expéditeur le connaît
  email        TEXT,                       -- adresse de la fiche de contact, si connue
  contact_path TEXT,                       -- /Contacts/<fiche>.vcf à relier au MULTIPASS créé
  items        TEXT NOT NULL,
  created_at   INTEGER NOT NULL,
  expires_at   INTEGER NOT NULL,
  opened_at    INTEGER,                    -- première ouverture de la page publique
  claimed_at   INTEGER,                    -- réclamation en cours ou faite (usage unique)
  claimed_by   TEXT,                       -- clé du MULTIPASS créé ; NULL tant que la création n'est pas finie
  revoked_at   INTEGER
) STRICT, WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS invites_owner ON invites(owner_hex, created_at);

-- Économie : projections des kinds 30852 (ledger Ğ1-N²), 30800 (DID Node, capex) et 30850.
CREATE TABLE IF NOT EXISTS ledger_n2 (
  event_id   TEXT PRIMARY KEY,
  author     TEXT NOT NULL,
  dest       TEXT NOT NULL,
  amount     TEXT NOT NULL,            -- décimal exact en texte, jamais flottant
  prev       TEXT NOT NULL,            -- 'genesis' ou hex64
  d          TEXT NOT NULL,
  is_mint    INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL,
  UNIQUE (author, d)
) STRICT, WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS ln2_author ON ledger_n2(author, created_at);
CREATE INDEX IF NOT EXISTS ln2_dest   ON ledger_n2(dest);

CREATE TABLE IF NOT EXISTS capital_assets (
  id                    TEXT PRIMARY KEY,
  label                 TEXT,
  asset_type            TEXT,
  email                 TEXT,
  value_zen             REAL,
  amortized_zen         REAL,
  depreciable           INTEGER,
  depreciation_weeks    INTEGER,
  date                  TEXT,
  created_at            INTEGER,
  last_depreciated_week TEXT,
  status                TEXT,
  src_event             TEXT,          -- DID Node (kind 30800, d-tag = IPFSNODEID)
  node_hex              TEXT           -- auteur du DID : un instantané par NODE
) STRICT, WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS love_ledger (
  id         INTEGER PRIMARY KEY,
  at         INTEGER NOT NULL,
  kind       TEXT,
  amount_cents INTEGER NOT NULL CHECK (amount_cents >= 0),  -- centimes de ẐEN : jamais de flottant
  detail     TEXT,
  src_event  TEXT
) STRICT;

-- Loyers constatés (projection du kind 30851 `type=rent`, attesté par un NODE ou le Capitaine hébergés) : le dernier de chaque
-- compte reconstruit son statut de cycle de vie lors d'un replay (voir `astro_lifecycle::rebuild`). `src_kind` admet encore
-- 30852 pour les bases existantes, mais un virement Ğ1-N² n'est plus compté comme loyer (voir les déclencheurs ci-dessous).
CREATE TABLE IF NOT EXISTS rent_payments (
  event_id     TEXT PRIMARY KEY,
  owner_hex    TEXT NOT NULL,           -- MULTIPASS qui paie
  at           INTEGER NOT NULL,
  amount_cents INTEGER NOT NULL DEFAULT 0,
  src_kind     INTEGER NOT NULL CHECK (src_kind IN (30851, 30852))
) STRICT, WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS rent_owner ON rent_payments(owner_hex, at DESC);
-- Ordre d'arrivée quelconque : un loyer 30851 `type=rent` arrivé AVANT le DID qui fait connaître son payeur (email, rôle)
-- ou avant l'hébergement d'un des comptes n'a rien pu constater ; il est rattrapé ici dès que l'identité apparaît ou change
-- (même règle que `astro_sync::projector`, montant en centimes entiers).
-- Un virement Ğ1-N² (30852) n'est PAS un loyer : il est signé par le payeur seul, sans contrôle de solde ni de chaîne `prev`
-- (le relai ne valide pas encore le registre). Les déclencheurs sont recréés à chaque ouverture (DROP puis CREATE) pour qu'une
-- base d'avant cette règle perde les anciens rattrapages 30852 ; les loyers 30852 déjà comptés sont retirés.
DELETE FROM rent_payments WHERE src_kind = 30852;
DROP TRIGGER IF EXISTS identities_rent_catch_up_ai;
CREATE TRIGGER identities_rent_catch_up_ai AFTER INSERT ON identities BEGIN
  INSERT OR IGNORE INTO rent_payments(event_id, owner_hex, at, amount_cents, src_kind)
    SELECT json_extract(d.value, '$.event_id'), m.hex, d.at, (CASE WHEN instr(COALESCE(json_extract(d.value, '$.tags.amount'), '0'), '.') = 0 THEN CAST(COALESCE(json_extract(d.value, '$.tags.amount'), '0') AS INTEGER) * 100 ELSE CAST(substr(COALESCE(json_extract(d.value, '$.tags.amount'), '0'), 1, instr(COALESCE(json_extract(d.value, '$.tags.amount'), '0'), '.') - 1) AS INTEGER) * 100 + CAST(substr(substr(COALESCE(json_extract(d.value, '$.tags.amount'), '0'), instr(COALESCE(json_extract(d.value, '$.tags.amount'), '0'), '.') + 1) || '00', 1, 2) AS INTEGER) END), 30851 FROM identities m
    JOIN done d ON d.key >= 'zen_issuance:' AND d.key < 'zen_issuance;' AND json_valid(d.value)
      AND json_extract(d.value, '$.tags.type') = 'rent' AND json_extract(d.value, '$.tags.s') = 'OK'
      AND json_extract(d.value, '$.tags.email') = m.email
    WHERE m.hex = NEW.hex AND m.hosted = 1 AND m.role = 'multipass' AND json_extract(d.value, '$.event_id') IS NOT NULL
      AND EXISTS (SELECT 1 FROM identities a WHERE a.hex = substr(d.key, 14, 64) AND a.hosted = 1 AND a.role IN ('node', 'captain'));
END;
DROP TRIGGER IF EXISTS identities_rent_catch_up_au;
CREATE TRIGGER identities_rent_catch_up_au AFTER UPDATE OF email, role, hosted ON identities BEGIN
  INSERT OR IGNORE INTO rent_payments(event_id, owner_hex, at, amount_cents, src_kind)
    SELECT json_extract(d.value, '$.event_id'), m.hex, d.at, (CASE WHEN instr(COALESCE(json_extract(d.value, '$.tags.amount'), '0'), '.') = 0 THEN CAST(COALESCE(json_extract(d.value, '$.tags.amount'), '0') AS INTEGER) * 100 ELSE CAST(substr(COALESCE(json_extract(d.value, '$.tags.amount'), '0'), 1, instr(COALESCE(json_extract(d.value, '$.tags.amount'), '0'), '.') - 1) AS INTEGER) * 100 + CAST(substr(substr(COALESCE(json_extract(d.value, '$.tags.amount'), '0'), instr(COALESCE(json_extract(d.value, '$.tags.amount'), '0'), '.') + 1) || '00', 1, 2) AS INTEGER) END), 30851 FROM identities m
    JOIN done d ON d.key >= 'zen_issuance:' AND d.key < 'zen_issuance;' AND json_valid(d.value)
      AND json_extract(d.value, '$.tags.type') = 'rent' AND json_extract(d.value, '$.tags.s') = 'OK'
      AND json_extract(d.value, '$.tags.email') = m.email
    WHERE m.hex = NEW.hex AND m.hosted = 1 AND m.role = 'multipass' AND json_extract(d.value, '$.event_id') IS NOT NULL
      AND EXISTS (SELECT 1 FROM identities a WHERE a.hex = substr(d.key, 14, 64) AND a.hosted = 1 AND a.role IN ('node', 'captain'));
END;

-- Tout ce que le cycle de vie et la clôture hebdomadaire comptent comme loyer : les loyers attestés (30851) ci-dessus ET
-- les virements d'hébergement faits par la station sur la Ğ1 (journal `hosting_fee`, montant en centimes de ẑ).
CREATE VIEW IF NOT EXISTS rents(owner_hex, at, amount_cents) AS
  SELECT owner_hex, at, amount_cents FROM rent_payments
  UNION ALL
  SELECT json_extract(data, '$.owner'), at, CAST(json_extract(data, '$.zen') AS INTEGER) * 100 FROM journal WHERE stream = 'hosting_fee';

-- ISLAND-1 « Ğ1 en différé » (`src/island.rs`, `astro_economy::island`). Montants en centimes de Ğ1.
-- État LOCAL : dernier instantané Ğ1 de chaque MULTIPASS hébergé, pris tant que la chaîne répond (base du crédit d'île).
CREATE TABLE IF NOT EXISTS g1_snapshot (
  hex        TEXT PRIMARY KEY,         -- MULTIPASS
  wallet     TEXT NOT NULL,            -- portefeuille Ğ1 (SS58)
  spendable  INTEGER NOT NULL CHECK (spendable >= 0),   -- dépensable, réserve déduite
  ud         INTEGER NOT NULL CHECK (ud >= 0),          -- DU en vigueur si membre, 0 sinon
  ud_period  INTEGER NOT NULL,         -- secondes
  ud_until   INTEGER NOT NULL,         -- fin d'adhésion connue (secondes Unix)
  at         INTEGER NOT NULL
) STRICT, WITHOUT ROWID;
-- Projection (kind 30855) : rencontres attestées (« Se rencontrer »), la plus récente par (auteur, personne rencontrée).
-- `how` : 'qr' (code scanné, preuve de présence jointe) ou 'named' (choisi dans une liste, un lien, un code de paiement).
-- `verified` : preuve revérifiée (signée par la personne rencontrée, datée de l'attestation) ; contre les faux comptes,
-- Ğ1-N² ne compte que les rencontres vérifiées (astro_economy::meet).
CREATE TABLE IF NOT EXISTS meetings (
  owner_hex  TEXT NOT NULL,
  target_hex TEXT NOT NULL,
  how        TEXT NOT NULL CHECK (how IN ('qr', 'named')),
  verified   INTEGER NOT NULL CHECK (verified IN (0, 1)),
  at         INTEGER NOT NULL,
  src_event  TEXT NOT NULL,
  PRIMARY KEY (owner_hex, target_hex)
) STRICT, WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS meetings_target ON meetings(target_hex);
-- Projection (kind 30854) : crédits ouverts dans une île, un par membre.
CREATE TABLE IF NOT EXISTS island_credits (
  island     TEXT NOT NULL,
  member_hex TEXT NOT NULL,
  node_hex   TEXT NOT NULL,
  g1         TEXT NOT NULL,
  opening    INTEGER NOT NULL,
  ud         INTEGER NOT NULL,
  ud_period  INTEGER NOT NULL,
  ud_until   INTEGER NOT NULL,
  at         INTEGER NOT NULL,
  src_event  TEXT NOT NULL,
  PRIMARY KEY (island, member_hex)
) STRICT, WITHOUT ROWID;
-- Projection (kind 30852 `t=island`) : promesses ; `state` est recalculé par `island::revalidate` depuis l'ENSEMBLE des
-- promesses du payeur (jamais selon l'ordre d'arrivée).
CREATE TABLE IF NOT EXISTS island_promises (
  event_id   TEXT PRIMARY KEY,
  island     TEXT NOT NULL,
  author     TEXT NOT NULL,
  dest       TEXT NOT NULL,
  dest_g1    TEXT NOT NULL,
  amount     INTEGER NOT NULL,
  created_at INTEGER NOT NULL,
  state      TEXT NOT NULL DEFAULT 'pending' CHECK (state IN ('pending', 'accepted', 'refused')),
  reason     TEXT
) STRICT, WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS island_promises_author ON island_promises(island, author);
CREATE INDEX IF NOT EXISTS island_promises_dest ON island_promises(dest, created_at DESC);
-- État LOCAL : règlement en vraie Ğ1 des promesses acceptées. Noté AVANT l'envoi ; `unknown` n'est JAMAIS rejoué.
CREATE TABLE IF NOT EXISTS island_settle (
  event_id   TEXT PRIMARY KEY,
  state      TEXT NOT NULL CHECK (state IN ('sending', 'paid', 'unknown', 'short')),
  at         INTEGER NOT NULL,
  detail     TEXT
) STRICT, WITHOUT ROWID;

-- Permis Oracle (kinds 30500-30504).
CREATE TABLE IF NOT EXISTS permit_defs (
  id                  TEXT PRIMARY KEY,
  name                TEXT,
  description         TEXT,
  issuer_did          TEXT,
  min_attestations    INTEGER,
  required_license    TEXT,
  revocable           INTEGER,
  valid_duration_days INTEGER,
  verification_method TEXT,
  metadata            TEXT,
  src_event           TEXT
) STRICT, WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS permit_requests (
  request_id           TEXT PRIMARY KEY,
  applicant_did        TEXT,
  applicant_npub       TEXT,
  permit_definition_id TEXT,
  attestations         TEXT,
  evidence             TEXT,
  statement            TEXT,
  status               TEXT,
  nostr_event_id       TEXT,
  created_at           INTEGER,
  updated_at           INTEGER
) STRICT, WITHOUT ROWID;

-- Recherche vectorielle (sqlite-vec). Cosinus partout ; `owner_hex` est une clé de partition.
-- Visages : InsightFace buffalo_l, 512 dimensions.
CREATE VIRTUAL TABLE IF NOT EXISTS face_vectors USING vec0(
  face_id   INTEGER PRIMARY KEY,
  owner_hex TEXT PARTITION KEY,
  embedding FLOAT[512] DISTANCE_METRIC=cosine
);
-- Mémoire BRO : bge-m3, 1024 dimensions (EMBED_DIM).
CREATE VIRTUAL TABLE IF NOT EXISTS memory_vectors USING vec0(
  mem_id    INTEGER PRIMARY KEY,
  owner_hex TEXT PARTITION KEY,
  embedding FLOAT[1024] DISTANCE_METRIC=cosine
);
-- Descriptions de photos uCloud : bge-m3, 1024 dimensions.
CREATE VIRTUAL TABLE IF NOT EXISTS ucloud_desc_vectors USING vec0(
  emb_id    INTEGER PRIMARY KEY,
  owner_hex TEXT PARTITION KEY,
  embedding FLOAT[1024] DISTANCE_METRIC=cosine
);
-- Tags DREAM (love_dream_vectors) : vecteur de 768 dimensions obtenu par hachage de tags, pas un embedding sémantique.
CREATE VIRTUAL TABLE IF NOT EXISTS dream_vectors USING vec0(
  owner_hex TEXT PARTITION KEY,
  dream_id  INTEGER PRIMARY KEY,
  embedding FLOAT[768] DISTANCE_METRIC=cosine
);

-- ═══════════════════════════ 2. LOCAL (non rejouable) ═══════════════════════════

-- Visages : remplace les collections Qdrant `faces_<hex64>`. ÉTAT LOCAL (non rejouable : les noms donnés par le membre ne sont
-- dans aucun événement) ; les visages nommés sont emportés dans son uCloud par `life_pack` (/_/Visages/faces.astroidx).
CREATE TABLE IF NOT EXISTS faces (
  face_id      INTEGER PRIMARY KEY,
  owner_hex    TEXT NOT NULL,
  cid          TEXT,
  source_path  TEXT,
  name         TEXT,                   -- 'Inconnu_<hash8>' tant que non nommé
  pubkey       TEXT,
  ref          TEXT CHECK (ref IN ('contact','shared') OR ref IS NULL),
  email        TEXT,
  det_score    REAL,
  bbox_x1      REAL, bbox_y1 REAL, bbox_x2 REAL, bbox_y2 REAL,  -- repère de l'image originale
  embed_hash   TEXT NOT NULL,          -- id déterministe (sha256 de l'embedding arrondi à 6 décimales)
  ts           INTEGER NOT NULL,
  src_cid      TEXT,
  ucloud_path  TEXT,                   -- photo d'origine dans le VFS
  suggest_pubkey TEXT,                 -- zone grise [0,55 ; 0,82[ : « est-ce X ? », à valider par l'humain
  suggest_name   TEXT,
  suggest_score  REAL,
  contact_uid    TEXT,                 -- UID vCard de la fiche de la personne (/Contacts) : le lien fait foi, pas le nom
  suggest_contact_uid TEXT,            -- fiche de la personne suggérée
  UNIQUE (owner_hex, embed_hash)
) STRICT;
CREATE INDEX IF NOT EXISTS faces_owner_name ON faces(owner_hex, name);
-- faces_owner_contact et identity_links_uid : créés par la migration (une base ancienne n'a pas encore la colonne ici).

-- Mémoire BRO : remplace les collections Qdrant `memory_<hex16>`. ÉTAT LOCAL (écrite par astro-bro memory.rs et reve.rs, rien
-- ne la rejoue) ; emportée dans l'uCloud du membre par `life_pack` (/_/Memoire/memory.astroidx).
CREATE TABLE IF NOT EXISTS memory (
  mem_id       INTEGER PRIMARY KEY,
  point_id     TEXT NOT NULL UNIQUE,   -- _stable_id(user_id, content_hash)
  owner_hex    TEXT NOT NULL,
  content      TEXT NOT NULL,
  content_hash TEXT NOT NULL,
  ts           INTEGER NOT NULL,
  event_id     TEXT,
  season       TEXT,
  importance   REAL,
  kind         TEXT CHECK (kind IN ('fact','episode','trait','person')),
  source       TEXT,
  visibility   TEXT CHECK (visibility IN ('private','love','public')),
  confidence   REAL,
  status       TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active','superseded','deleted')),
  embed_model  TEXT,
  schema_v     INTEGER,
  origin       TEXT,
  slot         INTEGER,                -- seuls 0, 13, 14, 15 subsistent (SLOT_SCHEMA legacy)
  src_cid      TEXT
) STRICT;
CREATE INDEX IF NOT EXISTS memory_owner ON memory(owner_hex, status, ts DESC);

-- Anti-rejeu NIP-98 : persistant entre redémarrages, utile seulement pendant la fenêtre de 2×120 s.
CREATE TABLE IF NOT EXISTS nip98_seen (
  event_id   TEXT PRIMARY KEY,
  expires_at INTEGER NOT NULL
) STRICT, WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS nip98_expiry ON nip98_seen(expires_at);

-- File de tâches (DM entrants, gift wraps, FaceID) avec backpressure et ré-essais.
CREATE TABLE IF NOT EXISTS tasks (
  id         INTEGER PRIMARY KEY,
  kind       TEXT NOT NULL,
  payload    TEXT NOT NULL,
  status     TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','running','done','failed')),
  not_before INTEGER,
  attempts   INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL,
  ttl        INTEGER
) STRICT;
CREATE INDEX IF NOT EXISTS tasks_ready ON tasks(status, not_before);

-- Déduplication des événements déjà traités (remplace les marqueurs O_EXCL), rétention 7 jours.
CREATE TABLE IF NOT EXISTS claimed (
  owner    TEXT NOT NULL,
  event_id TEXT NOT NULL,
  at       INTEGER NOT NULL,
  PRIMARY KEY (owner, event_id)
) STRICT, WITHOUT ROWID;

-- Suppressions NIP-09 : empêchent de réinsérer un événement supprimé par son auteur. `ref` est un identifiant d'événement
-- (`until` = i64::MAX) ou l'adresse `kind:pubkey:d` d'un événement remplaçable (bloque les versions jusqu'à `until`).
CREATE TABLE IF NOT EXISTS nostr_deleted (
  pubkey TEXT NOT NULL,
  ref    TEXT NOT NULL,
  until  INTEGER NOT NULL,
  PRIMARY KEY (pubkey, ref)
) STRICT, WITHOUT ROWID;

-- Demandes d'assistance entre Capitaines amis (état LOCAL). `direction` : `out` (je demande de l'aide, une ligne par ami
-- sollicité) ou `in` (un ami me la demande). `status` : pending, accepted, declined, closed (déjà aidé ou annulée), expired.
CREATE TABLE IF NOT EXISTS assist_requests (
  id         TEXT NOT NULL,
  direction  TEXT NOT NULL CHECK (direction IN ('out','in')),
  captain    TEXT NOT NULL,          -- mon Capitaine (clé Nostr)
  other      TEXT NOT NULL,          -- l'ami (clé Nostr)
  service    TEXT NOT NULL,
  hours      INTEGER NOT NULL,
  note       TEXT NOT NULL DEFAULT '',
  exclusive  INTEGER NOT NULL DEFAULT 0,
  station    TEXT,                   -- PeerID de la station de celui qui demande (sens `in`)
  status     TEXT NOT NULL CHECK (status IN ('pending','accepted','declined','closed','expired')),
  created_at INTEGER NOT NULL,
  decided_at INTEGER,
  PRIMARY KEY (id, direction, other)
) STRICT, WITHOUT ROWID;

-- Niveau de partage des fichiers du uCloud (état LOCAL) : un fichier absent de cette table, des invitations, des partages
-- FaceCloud et du dossier /Partages n'a jamais quitté son propriétaire : il est « privé » et sauvegardable chez un ami.
CREATE TABLE IF NOT EXISTS ucloud_sharing (
  owner_hex TEXT NOT NULL,
  cid       TEXT NOT NULL,
  kind      TEXT NOT NULL CHECK (kind IN ('friend','link','invite')),
  at        INTEGER NOT NULL,
  PRIMARY KEY (owner_hex, cid, kind)
) STRICT, WITHOUT ROWID;

-- Sauvegarde privée chez un ami (état LOCAL, une ligne par couple propriétaire/hébergeur, tenue par chaque station concernée).
-- `status` : décision de l'hébergeur, qui fixe aussi le volume (`quota_bytes`) ; `enabled` : choix du propriétaire.
CREATE TABLE IF NOT EXISTS backup_pacts (
  owner_hex   TEXT NOT NULL,
  host_hex    TEXT NOT NULL,
  status      TEXT NOT NULL CHECK (status IN ('pending','accepted','declined','revoked')),
  quota_bytes INTEGER NOT NULL DEFAULT 0,
  enabled     INTEGER NOT NULL DEFAULT 0,
  asked_at    INTEGER NOT NULL,
  decided_at  INTEGER,
  PRIMARY KEY (owner_hex, host_hex)
) STRICT, WITHOUT ROWID;

-- Présence des gardiens (état LOCAL, non rejouable : c'est ce que CETTE station a vu) : un jour (`day` = secondes UNIX / 86 400)
-- où le gardien `hex` s'est montré (message de sauvegarde reçu, événement publié, balise 30850 de sa station). Fenêtre
-- glissante de 30 jours pour estimer sa disponibilité (`guardians`) ; au-delà de 60 jours, purgé.
CREATE TABLE IF NOT EXISTS guardian_presence (
  hex TEXT NOT NULL,
  day INTEGER NOT NULL,
  PRIMARY KEY (hex, day)
) STRICT, WITHOUT ROWID;

-- Blobs chiffrés déjà envoyés (côté propriétaire) ou rangés (côté hébergeur) ; `size` en octets de clair. Un blob dont le
-- fichier a disparu chez le propriétaire est retiré de chez l'ami après 7 jours (nettoyage de 20h12).
CREATE TABLE IF NOT EXISTS backup_items (
  owner_hex TEXT NOT NULL,
  host_hex  TEXT NOT NULL,
  cid       TEXT NOT NULL,
  size      INTEGER NOT NULL,
  at        INTEGER NOT NULL,
  missing_since INTEGER,                    -- côté propriétaire : depuis quand le fichier n'existe plus chez lui
  PRIMARY KEY (owner_hex, host_hex, cid)
) STRICT, WITHOUT ROWID;

-- Sauvegarde k-sur-n (codage à effacement, docs/design/SAUVEGARDE_RS.md). Toutes ces tables sont de l'état LOCAL, des
-- caches de l'index chiffré des paquets (copié chez chaque gardien, clé dérivée du secret du compte) : une station qui
-- restaure le compte les reconstruit depuis l'index (`import_index`), rien n'y vit seul.
-- Paquets scellés du propriétaire : `seq` = rang dans le flux, `key_hex` = clé UENC du paquet, `len` = longueur chiffrée.
CREATE TABLE IF NOT EXISTS backup_packets (
  owner_hex TEXT NOT NULL,
  packet_id TEXT NOT NULL,                  -- 32 hex : packet_id ARSF
  seq       INTEGER NOT NULL,
  key_hex   TEXT NOT NULL,
  len       INTEGER NOT NULL,
  k         INTEGER NOT NULL,
  at        INTEGER NOT NULL,
  head_hex  TEXT,                           -- en-tête UENC du paquet chiffré (28 hex) : recalcul d'un fragment (lot 5) ; jamais dans l'index
  staged    INTEGER NOT NULL DEFAULT 0,     -- 1 : paquet neuf d'un compactage en cours (lot 6), ses blobs sont dans `backup_compact`
  PRIMARY KEY (owner_hex, packet_id),
  UNIQUE (owner_hex, seq)
) STRICT, WITHOUT ROWID;

-- Blobs entrés dans le flux de paquets (un CID n'y entre qu'une fois, tous gardiens confondus) : premier paquet (`seq`),
-- décalage dans son clair, longueur.
CREATE TABLE IF NOT EXISTS backup_blobs (
  owner_hex TEXT NOT NULL,
  cid       TEXT NOT NULL,
  seq       INTEGER NOT NULL,
  off       INTEGER NOT NULL,
  len       INTEGER NOT NULL,
  at        INTEGER NOT NULL,
  gone_at   INTEGER,                        -- depuis quand le fichier n'existe plus chez le propriétaire (place « morte » après 7 jours)
  PRIMARY KEY (owner_hex, cid)
) STRICT, WITHOUT ROWID;

-- Compactage en cours (lot 6, état LOCAL) : position de chaque blob vivant dans les paquets neufs (`staged = 1`). Les blobs
-- restent rattachés à leur ancien paquet (`backup_blobs`) tant que les paquets neufs ne sont pas acquittés par assez de
-- gardiens : la restauration reste possible à chaque instant. Vidée, en une transaction, à la bascule.
CREATE TABLE IF NOT EXISTS backup_compact (
  owner_hex TEXT NOT NULL,
  cid       TEXT NOT NULL,
  seq       INTEGER NOT NULL,
  off       INTEGER NOT NULL,
  len       INTEGER NOT NULL,
  PRIMARY KEY (owner_hex, cid)
) STRICT, WITHOUT ROWID;

-- Fragments ARSF, tenue des deux côtés comme `backup_items` : chez le propriétaire, ce qui est parti (`sent_at`) et acquitté
-- (`acked_at`, après quoi le fragment local est effacé) ; chez le gardien, ce qu'il garde (`acked_at` = rangé). La clé
-- impose un seul fragment par paquet et par gardien (Q9). `size` = octets du fragment, en-tête compris (quota).
CREATE TABLE IF NOT EXISTS backup_frags (
  owner_hex TEXT NOT NULL,
  host_hex  TEXT NOT NULL,
  packet_id TEXT NOT NULL,
  i         INTEGER NOT NULL,
  cid       TEXT NOT NULL,
  size      INTEGER NOT NULL,
  sent_at   INTEGER,
  acked_at  INTEGER,
  seen_at   INTEGER,                        -- propriétaire : dernière sonde `HASN` qui l'a trouvé chez le gardien (lot 5)
  lost_at   INTEGER,                        -- propriétaire : tenu pour perdu (réponse fausse, absent de chez un gardien joignable,
                                            -- gardien muet depuis 14 jours) ; hors de l'index, remplacé ailleurs par un indice neuf
  PRIMARY KEY (owner_hex, host_hex, packet_id)
) STRICT, WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS backup_frags_cid ON backup_frags(cid);

-- Copies VÉRIFIÉES d'un blob partagé (état LOCAL, non rejouable : c'est ce que CETTE station a sondé) : la station
-- `station_peer` d'un ami réciproque a répondu « oui » à une sonde `HASN` sur NOTRE CID à `verified_at`. Une station = une
-- copie, quel que soit le nombre d'amis qu'elle héberge. Un fichier partagé ne reste hors des paquets k-sur-n que si ces
-- copies, vérifiées depuis moins de 7 jours, suffisent à la cible de sûreté ; réévalué chaque nuit.
CREATE TABLE IF NOT EXISTS blob_copies (
  owner_hex    TEXT NOT NULL,
  cid          TEXT NOT NULL,
  station_peer TEXT NOT NULL,
  verified_at  INTEGER NOT NULL,
  PRIMARY KEY (owner_hex, cid, station_peer)
) STRICT, WITHOUT ROWID;

-- Index courant des paquets d'un compte hébergé ici (blob UENC v2, `index_cid` : le `CidGate` le laisse sortir) et sa
-- révision ; reporté dans le manifeste (`Manifest::backup_index`).
CREATE TABLE IF NOT EXISTS backup_index (
  owner_hex TEXT PRIMARY KEY,
  rev       INTEGER NOT NULL,
  index_cid TEXT NOT NULL,
  at        INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

-- Journaux bornés (activité, audit ARBOR, requêtes d'outils).
CREATE TABLE IF NOT EXISTS journal (
  id     INTEGER PRIMARY KEY,
  stream TEXT NOT NULL,
  at     INTEGER NOT NULL,
  data   TEXT NOT NULL
) STRICT;
CREATE INDEX IF NOT EXISTS journal_stream ON journal(stream, at);

-- Liens entre les clés d'une même personne dans des UPlanet différentes (kind 31903 `d = plane-link`). PROJECTION rejouable.
-- Un lien n'existe que si LES DEUX clés l'ont signé : `a` signe l'événement, `b` signe la preuve qu'il porte.
CREATE TABLE IF NOT EXISTS key_links (
  a         TEXT NOT NULL,             -- clé qui annonce (signataire de l'événement)
  b         TEXT NOT NULL,             -- l'autre clé de la personne (signataire de la preuve)
  plane     TEXT NOT NULL DEFAULT '',  -- UPlanet où vit `b`, selon `a` (indication d'affichage, non prouvée)
  at        INTEGER NOT NULL,
  src_event TEXT NOT NULL,
  PRIMARY KEY (a, b)
) STRICT, WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS key_links_b ON key_links(b);

-- Médiathèque (CACHE LOCAL, rejouable) : résumé de chaque fiche `<nom>.media.json` du uCloud (plugin `media`), relu quand le CID
-- de la fiche change (`cid`) ; `item` NULL = fiche illisible. Une base rejouée le reconstruit à la première ouverture de l'écran.
CREATE TABLE IF NOT EXISTS media_index (
  owner_hex TEXT NOT NULL,
  path      TEXT NOT NULL,
  cid       TEXT NOT NULL,
  item      TEXT,
  PRIMARY KEY (owner_hex, path)
) STRICT, WITHOUT ROWID;

-- Médiathèque des amis (CACHE LOCAL) : vidéos et morceaux qu'un proche réciproque partage avec son cercle N¹ (kinds 21/22),
-- lus chez sa station au nom du membre (`src/friends_media.rs`) ; `x` : empreinte du fichier en clair. Relu au plus toutes les
-- 10 minutes ; `friend_media_sync` garde la date et l'erreur de la dernière lecture (un ami hors ligne garde sa dernière liste).
CREATE TABLE IF NOT EXISTS friend_media (
  owner_hex  TEXT NOT NULL,
  author     TEXT NOT NULL,
  event_id   TEXT NOT NULL,
  x          TEXT NOT NULL,
  cid        TEXT,                    -- blob chiffré désigné (`url /ipfs/<cid>`) : les copies d'un même blob se servent entre elles
  item       TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (owner_hex, event_id)
) STRICT, WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS friend_media_sync (
  owner_hex TEXT NOT NULL,
  author    TEXT NOT NULL,
  at        INTEGER NOT NULL,
  error     TEXT,
  PRIMARY KEY (owner_hex, author)
) STRICT, WITHOUT ROWID;
-- Copies de médias entre amis (ÉTAT LOCAL) : `asked` = ce qu'un membre d'ici a demandé (et le refus éventuel) ; `served` = ce
-- qu'un membre d'ici a envoyé, pour ne pas renvoyer un film à chaque clic (6 h).
CREATE TABLE IF NOT EXISTS media_requests (
  side      TEXT NOT NULL CHECK (side IN ('asked', 'served')),
  owner_hex TEXT NOT NULL,
  peer      TEXT NOT NULL,
  x         TEXT NOT NULL,
  at        INTEGER NOT NULL,
  error     TEXT,
  PRIMARY KEY (side, owner_hex, peer, x)
) STRICT, WITHOUT ROWID;

-- Caches et états mutables divers (jamais purgés par la maintenance nocturne).
CREATE TABLE IF NOT EXISTS kv (
  scope      TEXT NOT NULL,
  key        TEXT NOT NULL,
  value      TEXT NOT NULL,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (scope, key)
) STRICT, WITHOUT ROWID;

-- Consommation de l'IA par membre et par semaine ISO (état LOCAL, `src/ai_rights.rs`) : sert le quota que le Capitaine fixe
-- (jetons par semaine) et l'affichage au membre. Jetons tels que le fournisseur les annonce (0 s'il ne le dit pas).
CREATE TABLE IF NOT EXISTS ai_usage (
  owner_hex  TEXT NOT NULL,
  week       TEXT NOT NULL,
  requests   INTEGER NOT NULL DEFAULT 0,
  tokens_in  INTEGER NOT NULL DEFAULT 0,
  tokens_out INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (owner_hex, week)
) STRICT, WITHOUT ROWID;

-- Journal des changements du carnet d'adresses (`/Contacts/*.vcf`) et de l'agenda (`/Calendrier/*.ics`) (état LOCAL) : sert le rapport
-- `sync-collection` (RFC 6578) de CardDAV et de CalDAV ; chaque collection filtre le journal par son dossier.
-- Alimenté par des triggers sur `ucloud_entries`, donc quel que soit le chemin d'écriture (WebDAV, restauration, déplacement).
-- Une ligne = « ce chemin a changé » ; l'état courant (présent ou supprimé) est relu dans `ucloud_entries` au moment du rapport.
-- Le jeton de synchronisation est le numéro `seq` ; les lignes de plus de 30 jours sont purgées la nuit (`schema_info`
-- `dav_changes_floor` retient le dernier `seq` purgé : un jeton plus ancien oblige le client à resynchroniser).
CREATE TABLE IF NOT EXISTS dav_changes (
  seq       INTEGER PRIMARY KEY AUTOINCREMENT,
  owner_hex TEXT NOT NULL,
  path      TEXT NOT NULL,
  at        INTEGER NOT NULL DEFAULT (unixepoch())
) STRICT;
CREATE INDEX IF NOT EXISTS dav_changes_owner ON dav_changes(owner_hex, seq);
-- Carnet et agenda vivent dans le tiroir système `/_` : les déclencheurs sont recréés à chaque ouverture (DROP puis CREATE),
-- pour qu'une base d'avant le tiroir suive les nouvelles places.
DROP TRIGGER IF EXISTS dav_changes_ai;
CREATE TRIGGER dav_changes_ai AFTER INSERT ON ucloud_entries
  WHEN new.type = 'file' AND new.parent = '/_/Contacts' AND new.path LIKE '%.vcf' BEGIN
  INSERT INTO dav_changes(owner_hex, path) VALUES (new.owner_hex, new.path);
END;
DROP TRIGGER IF EXISTS dav_changes_au;
CREATE TRIGGER dav_changes_au AFTER UPDATE OF path, cid ON ucloud_entries
  WHEN (old.parent = '/_/Contacts' AND old.path LIKE '%.vcf') OR (new.parent = '/_/Contacts' AND new.path LIKE '%.vcf') BEGIN
  INSERT INTO dav_changes(owner_hex, path) SELECT old.owner_hex, old.path WHERE old.path <> new.path AND old.parent = '/_/Contacts' AND old.path LIKE '%.vcf';
  INSERT INTO dav_changes(owner_hex, path) SELECT new.owner_hex, new.path WHERE new.type = 'file' AND new.parent = '/_/Contacts' AND new.path LIKE '%.vcf';
END;
DROP TRIGGER IF EXISTS dav_changes_ad;
CREATE TRIGGER dav_changes_ad AFTER DELETE ON ucloud_entries
  WHEN old.type = 'file' AND old.parent = '/_/Contacts' AND old.path LIKE '%.vcf' BEGIN
  INSERT INTO dav_changes(owner_hex, path) VALUES (old.owner_hex, old.path);
END;
DROP TRIGGER IF EXISTS dav_changes_ics_ai;
CREATE TRIGGER dav_changes_ics_ai AFTER INSERT ON ucloud_entries
  WHEN new.type = 'file' AND new.parent = '/_/Calendrier' AND new.path LIKE '%.ics' BEGIN
  INSERT INTO dav_changes(owner_hex, path) VALUES (new.owner_hex, new.path);
END;
DROP TRIGGER IF EXISTS dav_changes_ics_au;
CREATE TRIGGER dav_changes_ics_au AFTER UPDATE OF path, cid ON ucloud_entries
  WHEN (old.parent = '/_/Calendrier' AND old.path LIKE '%.ics') OR (new.parent = '/_/Calendrier' AND new.path LIKE '%.ics') BEGIN
  INSERT INTO dav_changes(owner_hex, path) SELECT old.owner_hex, old.path WHERE old.path <> new.path AND old.parent = '/_/Calendrier' AND old.path LIKE '%.ics';
  INSERT INTO dav_changes(owner_hex, path) SELECT new.owner_hex, new.path WHERE new.type = 'file' AND new.parent = '/_/Calendrier' AND new.path LIKE '%.ics';
END;
DROP TRIGGER IF EXISTS dav_changes_ics_ad;
CREATE TRIGGER dav_changes_ics_ad AFTER DELETE ON ucloud_entries
  WHEN old.type = 'file' AND old.parent = '/_/Calendrier' AND old.path LIKE '%.ics' BEGIN
  INSERT INTO dav_changes(owner_hex, path) VALUES (old.owner_hex, old.path);
END;

-- Reprise depuis un autre serveur WebDAV (mode « pull ») : ce qui a été repris, de quelle source et d'où. ÉTAT LOCAL (pas une
-- projection). Indépendant de l'endroit où le fichier se trouve ensuite : le rangement du uCloud peut le déplacer sans que le
-- passage suivant le reprenne en double. `local_path` : où il a été rangé à la reprise (indicatif).
CREATE TABLE IF NOT EXISTS pull_imported (
  owner_hex   TEXT NOT NULL,
  source_id   TEXT NOT NULL,
  remote_path TEXT NOT NULL,
  size        INTEGER NOT NULL,
  local_path  TEXT NOT NULL,
  at          INTEGER NOT NULL,
  PRIMARY KEY (owner_hex, source_id, remote_path)
) STRICT, WITHOUT ROWID;

-- Rangement du uCloud par type (Documents, Images, Musique, Vidéos, Archives) : journal des passages et de chaque déplacement,
-- pour pouvoir annuler. ÉTAT LOCAL (pas une projection). Un rangement ne déplace que des entrées : aucun blob n'est touché.
CREATE TABLE IF NOT EXISTS organize_runs (
  run_id    TEXT NOT NULL,
  owner_hex TEXT NOT NULL,
  at        INTEGER NOT NULL,
  options   TEXT NOT NULL,
  moved     INTEGER NOT NULL DEFAULT 0,
  undone_at INTEGER,
  PRIMARY KEY (owner_hex, run_id)
) STRICT, WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS organize_moves (
  run_id    TEXT NOT NULL,
  owner_hex TEXT NOT NULL,
  seq       INTEGER NOT NULL,
  from_path TEXT NOT NULL,
  to_path   TEXT NOT NULL,
  PRIMARY KEY (owner_hex, run_id, seq)
) STRICT, WITHOUT ROWID;

-- Jetons WebDAV d'appareil : un par téléphone ou ordinateur, chacun confiné à un dossier du uCloud et à un mode (envoi seul,
-- lecture et écriture, lecture seule). On ne garde que l'EMPREINTE du jeton (montré une seule fois). ÉTAT LOCAL.
CREATE TABLE IF NOT EXISTS dav_tokens (
  owner_hex    TEXT NOT NULL,
  id           TEXT NOT NULL,
  label        TEXT NOT NULL,
  token_sha256 TEXT NOT NULL,
  root         TEXT NOT NULL,
  mode         TEXT NOT NULL CHECK (mode IN ('write','readwrite','read')),
  created_at   INTEGER NOT NULL,
  last_used    INTEGER,
  revoked_at   INTEGER,
  PRIMARY KEY (owner_hex, id)
) STRICT, WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS dav_tokens_hash ON dav_tokens(owner_hex, token_sha256);

-- Déplacer un fichier (WebDAV, rangement) met à jour les visages et les analyses en attente qui le désignent par son chemin :
-- ces index évitent de parcourir toute la table à chaque déplacement (30 000 photos rangées d'un coup).
CREATE INDEX IF NOT EXISTS faces_ucloud_path ON faces(owner_hex, ucloud_path);
CREATE INDEX IF NOT EXISTS faces_source_path ON faces(owner_hex, source_path);
CREATE INDEX IF NOT EXISTS tasks_facecloud_path ON tasks(json_extract(payload, '$.path')) WHERE kind = 'facecloud';

-- Virements des portefeuilles de la coopérative, lus auprès de l'indexeur Ğ1 (squid) : l'historique d'un portefeuille n'est pas
-- dans le nœud. CACHE reconstructible (la chaîne reste la vérité) ; alimente la carte animée de la constellation.
CREATE TABLE IF NOT EXISTS coop_ledger (
  id      TEXT PRIMARY KEY,   -- identifiant du virement chez l'indexeur
  block   INTEGER NOT NULL,
  at      INTEGER NOT NULL,   -- secondes Unix du bloc
  from_id TEXT,
  to_id   TEXT,
  cents   INTEGER NOT NULL    -- centimes de Ğ1
) STRICT, WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS coop_ledger_at ON coop_ledger(at, block);

-- Blobs publics en clair (avatars, bandeaux…) servis à `GET /ipfs/<cid>` : liste explicite des CID (CIDv1 raw sha2-256) que la station
-- accepte de servir sans authentification. ÉTAT LOCAL : l'octet vit dans le magasin de blobs, le profil Nostr (kind 0) garde le CID.
-- Une station qui rapatrie un blob chez ses pairs ne l'inscrit que s'il est une image (octets magiques vérifiés).
CREATE TABLE IF NOT EXISTS public_blobs (
  cid      TEXT PRIMARY KEY,
  mime     TEXT NOT NULL CHECK (mime IN ('image/jpeg','image/png','image/webp')),
  size     INTEGER NOT NULL,
  owner    TEXT,              -- compte (hex) qui l'a déposé ici : ses octets comptent dans son quota (vfs::used_bytes) ; NULL pour un blob rapatrié
  added_at INTEGER NOT NULL
) STRICT, WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS public_blobs_owner ON public_blobs(owner) WHERE owner IS NOT NULL;

-- Visites du site public d'un compte (`/Public`) : un compteur par jour et par page, **sans adresse IP ni identifiant de visiteur**. ÉTAT LOCAL,
-- gardé 90 jours (purgé à l'écriture), au plus 200 pages distinctes par site et par jour (le reste est compté dans « (autres) »).
CREATE TABLE IF NOT EXISTS site_visits (
  hex  TEXT NOT NULL,
  day  INTEGER NOT NULL,       -- jours depuis 1970 (UTC)
  path TEXT NOT NULL,
  hits INTEGER NOT NULL,
  PRIMARY KEY (hex, day, path)
) STRICT, WITHOUT ROWID;

-- ─── Recherche multimédia (astro-index) : ÉTAT LOCAL recalculable ───
-- Tiré de contenus DÉCHIFFRÉS : jamais publié en clair (les paquets d'index sauvegardés chez les amis sont chiffrés). Clé : l'empreinte
-- du clair (`sha_plain` d'ucloud_entries), qui survit aux renommages. Un modèle différent (`model_sha`) rend ses vecteurs incomparables :
-- la ligne est alors recalculée. Les vecteurs de texte (384 ou 1024 dimensions selon E5 ou bge-m3) arriveront avec leur lot.
CREATE TABLE IF NOT EXISTS search_index (
  owner_hex  TEXT NOT NULL,
  sha        TEXT NOT NULL,
  task       TEXT NOT NULL CHECK (task IN ('ocr','vision','text','speech')),
  state      TEXT NOT NULL CHECK (state IN ('pending','done','failed','skipped')),
  model_id   TEXT,
  model_sha  TEXT,
  via        TEXT,                      -- NULL = calculé ici ; PeerID = station amie (délégation acceptée par le membre)
  tries      INTEGER NOT NULL DEFAULT 0,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (owner_hex, sha, task)
) STRICT, WITHOUT ROWID;

-- Images vues par SigLIP : une photo (t_ms = 0) ou les images clés d'une vidéo (t_ms = instant). `vis_id` = clé de search_vision_vectors.
CREATE TABLE IF NOT EXISTS search_frames (
  vis_id    INTEGER PRIMARY KEY,
  owner_hex TEXT NOT NULL,
  sha       TEXT NOT NULL,
  t_ms      INTEGER NOT NULL DEFAULT 0,
  UNIQUE (owner_hex, sha, t_ms)
) STRICT;
CREATE VIRTUAL TABLE IF NOT EXISTS search_vision_vectors USING vec0(
  vis_id    INTEGER PRIMARY KEY,
  owner_hex TEXT PARTITION KEY,
  embedding FLOAT[768] DISTANCE_METRIC=cosine
);

-- Texte cherchable mot à mot. Toute requête filtre sur owner_hex. `src` : 'ocr' (images, PDF), 'speech' (transcription, t_ms = instant)
-- ou 'fiche' (fiche de la Médiathèque, `media_index` : `ref` = chemin de la fiche, `sha` = empreinte du fichier décrit, tenue dans la
-- même transaction que media_index). Le `sha` relie les trois au même fichier : un film a sa fiche, ses images clés, sa parole.
-- Les compagnons d'une fiche (affiche, pochette, .vtt, la fiche elle-même : `media::NOT_COMPANION_SQL`) ne sont pas indexés à part.
CREATE VIRTUAL TABLE IF NOT EXISTS search_text USING fts5(
  body, owner_hex UNINDEXED, sha UNINDEXED, src UNINDEXED, t_ms UNINDEXED, ref UNINDEXED,
  tokenize='unicode61 remove_diacritics 2'
);

-- Lecture des documents décidée par le Capitaine (état LOCAL) : une ligne par MULTIPASS dont il a changé le réglage ; sans ligne,
-- le réglage par défaut de la station (`kv` options/search_index : { enabled, default }) s'applique. 0 = rien de ce compte n'est lu.
CREATE TABLE IF NOT EXISTS index_members (
  owner_hex  TEXT PRIMARY KEY,
  enabled    INTEGER NOT NULL CHECK (enabled IN (0, 1)),
  updated_at INTEGER NOT NULL
) STRICT;

-- Cerveau partagé : le membre accepte (Mon compte › Lecture de vos documents) que la station confie ses fichiers, en mémoire, à
-- des stations de Capitaines amis pour les tâches listées ; plusieurs stations se partagent la charge (la moins chargée d'abord).
-- Sans ligne ici, rien ne quitte la station.
CREATE TABLE IF NOT EXISTS index_delegation (
  owner_hex   TEXT NOT NULL,
  peer        TEXT NOT NULL,            -- PeerID de la station amie
  label       TEXT NOT NULL,            -- nom montré au membre
  tasks       TEXT NOT NULL,            -- 'ocr,vision' : ce qu'il accepte d'envoyer
  accepted_at INTEGER NOT NULL,
  PRIMARY KEY (owner_hex, peer)
) STRICT, WITHOUT ROWID;

-- Miroir Kubo (ORIGIN seulement) : « le disque d'abord, Kubo en miroir ». Chaque blob que la base référence vit sur le disque
-- de la station ; il est aussi publié dans Kubo, par référence au fichier (filestore, `nocopy` = 1) ou par copie. `root` est
-- la racine que Kubo lui donne (égale à `blob` pour un fichier d'un seul bloc, une racine `dag-pb` au-delà). ÉTAT LOCAL : sa
-- perte fait seulement republier. Colonnes nommées hors de la règle `cid` / `*_cid` : une ligne ici ne retient aucun blob
-- (le bassin et le ménage ne la comptent pas comme une référence).
CREATE TABLE IF NOT EXISTS kubo_mirror (
  blob   TEXT PRIMARY KEY,
  root   TEXT NOT NULL,
  nocopy INTEGER NOT NULL CHECK (nocopy IN (0, 1)),
  at     INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

-- Boîte d'alertes du Capitaine (ÉTAT LOCAL, non rejouable : ce sont des événements de cette station, pas des faits Nostr).
-- Une alerte reste ouverte (`done_at` nul) jusqu'à ce que le Capitaine la marque traitée ; une alerte identique encore
-- ouverte n'ajoute pas de ligne (`count`, `last_at`). Voir `src/captain_alerts.rs`.
CREATE TABLE IF NOT EXISTS captain_alerts (
  id       INTEGER PRIMARY KEY,
  source   TEXT NOT NULL,
  message  TEXT NOT NULL,
  data     TEXT,
  first_at INTEGER NOT NULL,
  last_at  INTEGER NOT NULL,
  count    INTEGER NOT NULL DEFAULT 1,
  done_at  INTEGER,
  done_by  TEXT
) STRICT;
CREATE INDEX IF NOT EXISTS captain_alerts_open ON captain_alerts(source, message) WHERE done_at IS NULL;
