# Onyx

> **Your notes, your server, your AI.** Fast like a browser, private like a vault.

Onyx is an open-source, self-hosted, markdown-based knowledge base — a faster,
privacy-first alternative to Obsidian that opens your existing Obsidian vault
**in place** (run both side by side; uninstall-safe).

> ⚠️ **Working name.** "Onyx" is a development codename; the final brand is
> decided before the first public release.

## Why Onyx

- **Truly open source** — AGPL-3.0 app + server. Every feature is free and
  self-hostable, forever. Future revenue comes only from optional managed
  hosting — never from paywalling features.
- **Self-hosted E2EE sync** — a zero-knowledge Rust server that never sees your
  plaintext. Live incremental sync (CRDT-based: concurrent edits merge, text is
  never silently lost), plus encrypted incremental backups to S3 / Google Drive /
  WebDAV / local disk — multiple destinations at once.
- **Fast** — Rust core, Tauri v2 shell, tantivy full-text search. Perf budgets
  are CI-enforced on a 100k-note corpus and published as a reproducible
  benchmark suite.
- **Private AI** — bring your own key (Anthropic, OpenAI-compatible, Ollama).
  Client-side RAG. An agent that proposes changes as reviewable diffs and a
  request log that shows every byte that leaves your machine.
- **Plugins you can trust** — capability-based permissions, sandboxed by
  default, signed releases.
- **Browser-grade UX** — vertical tabs, tab groups, per-tab history,
  VS Code / browser keybindings, global quick capture.

## Repository layout

| Path | What |
|---|---|
| `crates/onyx-core` | Vault engine (file watcher, atomic writes) + index layer (SQLite, tantivy, link graph) |
| `crates/onyx-md` | Markdown extraction: wikilinks, tags, frontmatter, headings + HTML export |
| `crates/onyx-crypto` | Key hierarchy, chunked AEAD container, filename encryption |
| `crates/onyx-sync` | CRDT sync engine (Loro), chunker, sync state machine |
| `crates/onyx-proto` | Wire protocol types shared by clients and server |
| `crates/onyx-testkit` | Test fixtures + deterministic 100k-note corpus generator |
| `onyx-desktop` | Tauri v2 desktop app (SolidJS + CodeMirror 6) |
| `onyx-server` | Self-hosted zero-knowledge sync + backup server (axum) |
| `xtask` | Bench harness, corpus generation, CI perf gates |

## Status

Working today (all covered by tests):

- **Editor**: CodeMirror 6 live preview (Obsidian-style mark concealing),
  wikilinks with autocomplete + click-to-follow, embeds/transclusions,
  tags, tasks, tabs (horizontal + vertical rail) with per-tab history,
  outline + backlinks panels, quick switcher (Ctrl+P), command palette
  (Ctrl+Shift+P), daily notes, settings with `.obsidian` import
- **Encryption at rest**: per-vault passphrase (argon2id), encrypted
  content AND filenames, in-RAM indexes — nothing legible on disk
- **Sync**: self-hosted zero-knowledge server; CRDT text merge (concurrent
  edits never lose text), delete propagation with edits-beat-deletes,
  attachment sync, WebSocket live push (sub-second), device pairing codes
- **Backups**: encrypted, deduplicated snapshots to local/S3/WebDAV via
  OpenDAL, standalone disaster restore
- **Plugins**: sandboxed (separate-origin iframe + capability broker),
  `.onyx/plugins/`, sample plugin in `plugins/word-count/`
- **Graph view** (Ctrl+G), **Vault Insights** (all-local analytics),
  **AI chat** (BYOK: OpenAI-compatible/Anthropic/Ollama, request log)

## Quickstart

```sh
# Desktop app (needs Rust, Node 20+, pnpm; Linux: webkit2gtk + gtk3 dev)
cd onyx-desktop && pnpm install && pnpm tauri dev

# Sync server — one command, automatic HTTPS via Caddy
cd onyx-server && ONYX_DOMAIN=notes.example.com docker compose up -d
# …or the single static binary:
cargo run --release -p onyx-server -- --data-dir ./data --listen 0.0.0.0:7677
```

Installers (macOS/Windows/Linux), the server Docker image, and package-
manager manifests are built by `.github/workflows/release.yml` on a
version tag; code signing and notarization activate when the maintainer
adds the documented secrets. See [packaging/](packaging/).

Open any folder of markdown — an existing Obsidian vault works as-is and
is never modified outside `.onyx/`.

## Development

```sh
cargo test --workspace          # all Rust tests
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
cd onyx-desktop && pnpm test    # frontend tests
cargo run --release -p xtask -- ci-perf /tmp/corpus 10000  # perf budgets
```

## License

- Application, server, and core crates: [AGPL-3.0-or-later](LICENSE).
- The public plugin API types (`@onyx/api`) and plugin template are MIT so
  plugin authors may license their plugins however they wish.

Contributions are accepted under the
[Developer Certificate of Origin](https://developercertificate.org/)
(`git commit -s`).
