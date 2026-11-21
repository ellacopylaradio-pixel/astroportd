#!/usr/bin/env python3
"""Recoupement indépendant du vecteur 6 (docs/design/SAUVEGARDE_RS.md § 4.5).

Relit les constantes V6_* de backup_pack.rs, recalcule la clé HKDF-SHA256 (info « astro-backup-index-v1 », sans sel)
avec `cryptography`, puis déchiffre le blob UENC v2 (STREAM : nonce = préfixe[7] | compteur u32 BE | drapeau
« dernier », AAD = en-tête de 14 octets) sans passer par le code Rust. Dépendance : `pip install cryptography`.
Usage : python3 crates/astro-dav/tests/vector6.py
"""
import pathlib
import re
import struct
import sys

from cryptography.hazmat.primitives import hashes
from cryptography.hazmat.primitives.ciphers.aead import AESGCM, ChaCha20Poly1305
from cryptography.hazmat.primitives.kdf.hkdf import HKDF

src = (pathlib.Path(__file__).parent / "backup_pack.rs").read_text(encoding="utf-8")
key_hex = re.search(r'const V6_KEY: &str = "([0-9a-f]{64})";', src).group(1)
blob = bytes.fromhex(re.search(r'const V6_BLOB: &str = "([0-9a-f]+)";', src).group(1))
expected = re.search(r'const V6_JSON: &str = r#"(.*)"#;', src).group(1).encode()

secret = bytes(range(1, 33))
key = HKDF(algorithm=hashes.SHA256(), length=32, salt=None, info=b"astro-backup-index-v1").derive(secret)
assert key.hex() == key_hex, (key.hex(), key_hex)

head = blob[:14]
assert head[:4] == b"UENC" and head[4] == 2, head
aead = {1: AESGCM, 2: ChaCha20Poly1305}[head[5]](key)
chunk = 1 << head[6]
prefix = head[7:14]
body = blob[14:]
plain = b""
i = 0
while True:
    part = body[: chunk + 16]
    body = body[chunk + 16 :]
    last = len(body) == 0
    nonce = prefix + struct.pack(">I", i) + bytes([1 if last else 0])
    plain += aead.decrypt(nonce, part, head)
    i += 1
    if last:
        break
assert plain == expected, plain
print("vecteur 6 : OK (clé HKDF et déchiffrement UENC v2 recoupés)")
sys.exit(0)
