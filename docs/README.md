# Onyx documentation

Developer- and operator-facing documentation for the Onyx monorepo. End-user
guides (installing the app, using the editor) live on the docs site; this
folder is the source of truth for how Onyx is built and run.

| Doc | What it covers |
|-----|----------------|
| [ARCHITECTURE.md](ARCHITECTURE.md) | The crate layout, the zero-knowledge boundary, the sync engine, and the security primitives. |
| [SELF-HOSTING.md](SELF-HOSTING.md) | Running your own `onyx-server` for live sync — Docker, TLS, backups. |
| [SECURITY.md](SECURITY.md) | The threat model, what the server can and cannot see, and how to report a vulnerability. |
| [CONTRIBUTING](../CONTRIBUTING.md) | Building the workspace, running the test suite, and the CI gates. |

## Repository map

Onyx is a single Cargo workspace. The client (desktop **and** mobile — Tauri
unifies them) and the server share the wire protocol crate, so protocol
changes land atomically in one commit.

```
onyx/
├─ crates/
│  ├─ onyx-md        markdown parsing / rendering (Obsidian-compatible)
│  ├─ onyx-core      vault, filesystem, index, search, watcher, history
│  ├─ onyx-crypto    encryption at rest, device pairing, share links
│  ├─ onyx-proto     the wire protocol (shared by client and server)
│  ├─ onyx-sync      Loro CRDT documents + the sidecar sync store
│  └─ onyx-testkit   shared test fixtures and property-test helpers
├─ onyx-desktop/     the Tauri app (desktop + mobile) + SolidJS frontend
├─ onyx-server/      the zero-knowledge sync server (axum)
├─ extensions/       browser web-clipper
├─ packaging/        AUR, Homebrew, winget, store manifests
├─ xtask/            repo automation (corpus generation, perf gates)
└─ docs/             you are here
```

The **community plugin registry** and the **Homebrew tap** live in their own
repositories in the [`onyx-notes`](https://github.com/onyx-notes) org, because
they have a different contributor audience and release cadence.
