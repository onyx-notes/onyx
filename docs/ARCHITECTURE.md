# Architecture

Onyx is a fast, private, self-hosted markdown notes app that opens an Obsidian
vault in place. This document explains how it is built.

## Principles

1. **Your notes are plain files.** A vault is a directory of markdown and
   attachments. Onyx never locks your data inside a database; the index and
   sync state are derived caches that can be deleted and rebuilt at any time.
2. **The server is zero-knowledge.** Live sync runs through a server that only
   ever stores opaque ciphertext. It cannot read a note, a filename, or a tag.
   This is enforced by the dependency graph, not just by convention.
3. **Local-first, offline-always.** Every feature works with no network. Sync
   is an additive layer over the local vault, built on CRDTs so concurrent
   edits merge without a central authority.

## The workspace

Onyx is one Cargo workspace (edition 2024, Rust 1.85). Crates are layered so
that the security boundary is a compile-time fact.

```
                 ┌────────────────────────────┐
   client only   │  onyx-desktop (Tauri app)  │   desktop + mobile
                 │  · SolidJS + CodeMirror 6  │
                 └───────────┬────────────────┘
                             │
        ┌──────────┬─────────┼──────────┬───────────┐
        │          │         │          │           │
   ┌────▼───┐ ┌────▼───┐ ┌───▼────┐ ┌───▼────┐ ┌────▼─────┐
   │onyx-md │ │onyx-   │ │onyx-   │ │onyx-   │ │onyx-proto│
   │        │ │core    │ │crypto  │ │sync    │ │          │
   └────────┘ └────────┘ └────────┘ └───┬────┘ └────┬─────┘
                                        │           │  shared
                                        │           │  wire types
                              ┌─────────▼───────────▼─────────┐
   server only                │        onyx-server (axum)     │
                              │  encrypted oplog, no plaintext │
                              └───────────────────────────────┘
```

### Crates

- **onyx-md** — Obsidian-compatible markdown: wikilinks, embeds, tags,
  frontmatter, callouts. Pure parsing/rendering, no I/O.
- **onyx-core** — the vault engine. A `Vault` over a `VaultFs` (with a
  `CryptoFs` decorator for encryption at rest), a rebuildable index
  (SQLite metadata + tantivy full-text), a filesystem watcher, note history
  (a content-addressed time machine), and a Dataview-lite query engine.
- **onyx-crypto** — every cryptographic primitive (see below). No vault or
  markdown knowledge.
- **onyx-proto** — the versioned wire protocol (postcard binary). Shared by
  the client and the server, which is why a protocol change is a single
  atomic commit across both.
- **onyx-sync** — a `SyncDoc` (a Loro text CRDT with attachment-pointer and
  manifest documents) and the sidecar `SyncStore` that persists CRDT
  snapshots and the server cursor.
- **onyx-testkit** — shared fixtures and property-test helpers.

### The zero-knowledge boundary

`onyx-server` depends on `onyx-proto` and `onyx-crypto` **but not** on
`onyx-core` or `onyx-md`. It has no code that could parse a note or a vault
even if it wanted to. This is checked in CI by `cargo-deny` (see
[`deny.toml`](../deny.toml)) — a build that makes the server link the vault
engine fails the `deny` gate. The security claim is therefore verifiable from
the dependency graph.

## The desktop/mobile app

The app is a single Tauri v2 project. Tauri unifies desktop and mobile, so the
same Rust core and the same SolidJS frontend target macOS, Windows, Linux,
Android, and iOS. The editor is CodeMirror 6 with live-preview decorations
(WYSIWYG-style rendering inline).

The frontend talks to the Rust core over three IPC lanes:

| Lane | Shape | Used for |
|------|-------|----------|
| Commands | JSON request/response | ordinary actions (open note, run query) |
| `onyx://` protocol | binary | bulk reads (file bytes, attachments) |
| Events | pushed deltas | vault changes (the watcher → frontend) |

## The sync engine

Sync is operation-based, not last-write-wins.

- **CRDT merge.** Each note is a Loro text CRDT. A device exports only the
  operations a peer hasn't seen (version-vector deltas) and imports remote
  ops. Concurrent edits — even to the same paragraph — merge without loss.
  This is property-tested for convergence across random interleavings.
- **The server is an oplog.** `onyx-server` stores encrypted ops per vault
  under a monotonically increasing sequence number — a *delivery cursor*,
  never an ordering authority (ordering is causal, inside the ciphertext).
  Each device persists its cursor and pulls "ops since N."
- **Live push.** A WebSocket delivers a tiny "the head advanced" nudge; the
  actual ops always travel over the HTTP pull lane, so there is exactly one
  op-delivery code path and a dropped WebSocket frame can never lose data.
  The client heartbeats the socket and reconnects on a half-open link.
- **Idempotent, bounded.** Every op carries an `op_id` derived from its
  plaintext, so a resend after a dropped ack is deduplicated. When a
  document's history grows long the server asks a client to upload a
  full-state **checkpoint**, then prunes the superseded ops — bounding oplog
  growth while still letting a brand-new device converge from the checkpoint
  alone.
- **Attachments.** Binary attachments are content-addressed, convergently
  encrypted (so equal content dedupes and re-uploads resume), and transferred
  in resumable chunks with HTTP range requests, so a poor connection never
  restarts a large file from zero. Concurrent binary edits keep-both.

See [SELF-HOSTING.md](SELF-HOSTING.md) for running the server, and the
`sync_e2e` / `flaky_net_e2e` integration tests for the behavior exercised
end to end.

## Cryptography

All in `onyx-crypto`; the server never holds a key.

- **At rest (optional per vault).** An XChaCha20-Poly1305 chunked container,
  keys derived with argon2id, filenames encrypted deterministically (SIV) so
  the directory structure is preserved without leaking names. Encrypted
  vaults keep their index and search in RAM.
- **Sync ops & attachments.** Encrypted client-side before they ever reach
  the server. Attachments use convergent encryption for dedup + resumable
  upload.
- **Device pairing.** New devices enroll via an X25519 sealed-box handoff
  verified by a short authentication string (SAS) the user compares on both
  screens — the server relays only opaque blobs and cannot MITM undetected.
- **Device auth.** Ed25519 challenge–response; no password ever crosses the
  wire.
- **Share links.** A single note can be published as an AES-256-GCM blob whose
  key lives only in the URL fragment; the built-in viewer decrypts in the
  browser with WebCrypto, so even the share server is zero-knowledge.

## Testing & CI

- **Property tests** for the invariants that matter: CRDT convergence,
  `incremental-index == full-rebuild`, crypto round-trips, path-identity.
- **End-to-end** sync over a real server and real sockets, including a
  fault-injection harness that cuts and restores the network to exercise
  reconnect, half-open sockets, and resumable transfer.
- **Golden snapshots** against an Obsidian-compatibility corpus.
- **CI gates:** the full cross-platform test matrix, the `cargo-deny`
  zero-knowledge dependency ban, and a performance budget (`cargo xtask
  ci-perf`) that fails if indexing/search/reconcile regress.

## License

AGPL-3.0-or-later, with an additional App Store distribution permission (see
[LICENSE-EXCEPTIONS.md](../LICENSE-EXCEPTIONS.md)). The copyleft keeps hosted
forks open; the exception lets Onyx ship on the Apple App Store and Google
Play without relicensing.
