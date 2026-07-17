# Security model

Onyx is designed so that the people who run infrastructure for you — including
whoever hosts your sync server — cannot read your notes.

## What each party can see

| Party | Can see |
|-------|---------|
| Your device (unlocked) | Everything — this is where plaintext lives. |
| Your sync server | Opaque ciphertext ops and content-addressed encrypted blobs, per vault. A monotonic sequence number and rough sizes/timing. **No** note content, filenames, tags, or keys. |
| A backup destination (S3/WebDAV/folder) | Convergently-encrypted blobs. No plaintext. |
| Someone with a share link | Only the single note that link points to, and only if they also have the key embedded in the URL fragment. |
| A network observer | TLS-protected traffic (you terminate TLS at your proxy). |

## Encryption

- **At rest** (optional, per vault): XChaCha20-Poly1305 chunked containers,
  keys derived with argon2id, filenames encrypted deterministically so the
  folder layout survives without leaking names.
- **In transit / on the server**: every operation and attachment is encrypted
  on the client before it is sent. The wire protocol carries no plaintext
  fields.
- **Device pairing**: an X25519 sealed-box handoff verified by a short
  authentication string you compare on both devices, so a malicious relay
  cannot substitute keys undetected.
- **Device authentication**: Ed25519 challenge–response; no password crosses
  the wire.

## What Onyx does *not* protect against

- A compromised **unlocked device** — that's where your plaintext is.
- A weak vault passphrase — the at-rest KDF is strong, but choose a good one.
- Metadata inherent to sync — the server learns *that* a vault changed and
  roughly *how much*, just not *what*.

## Reporting a vulnerability

**Please do not open a public issue for security reports.**

Email **security@onyx.md** (or, until that is live, open a
[GitHub security advisory](https://github.com/onyx-notes/onyx/security/advisories/new)).
Include a description, reproduction steps, and impact. We aim to acknowledge
within 72 hours and will coordinate a fix and disclosure timeline with you.

Because Onyx is end-to-end encrypted, cryptographic and sync-protocol issues
are treated as highest severity — they are the core promise of the project.
