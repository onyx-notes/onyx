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

## Development

```sh
cargo test --workspace          # all Rust tests
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

## License

- Application, server, and core crates: [AGPL-3.0-or-later](LICENSE).
- The public plugin API types (`@onyx/api`) and plugin template are MIT so
  plugin authors may license their plugins however they wish.

Contributions are accepted under the
[Developer Certificate of Origin](https://developercertificate.org/)
(`git commit -s`).
