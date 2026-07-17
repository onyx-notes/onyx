# Contributing to Onyx

Thanks for helping build a private, self-hosted home for people's notes.

## Ground rules

- **License**: Onyx is AGPL-3.0-or-later with an App Store additional
  permission — read [LICENSE-EXCEPTIONS.md](LICENSE-EXCEPTIONS.md) before
  your first PR. By contributing you agree your contribution carries that
  same license + exception.
- **DCO**: every commit must be signed off (`git commit -s`), certifying
  the [Developer Certificate of Origin](https://developercertificate.org/).
  You keep your copyright; there is no CLA.
- **Zero-knowledge discipline**: nothing that runs on the server may see
  plaintext note content, note names, or key material. PRs that widen the
  server's knowledge get declined regardless of how convenient they are.

## Development

```sh
# Everything (desktop + server + crates)
cargo test --workspace
cargo clippy --workspace --all-targets

# Frontend
cd onyx-desktop && pnpm install && pnpm test && pnpm build

# Android cross-check (needs NDK; see .cargo/config.toml env)
cargo check -p onyx-desktop --target aarch64-linux-android
```

CI runs all of the above plus a perf gate; keep it green.

## Style

- Rust: rustfmt defaults, clippy clean, comments explain *why* not *what*.
- Frontend: SolidJS + strict TypeScript; i18n keys must exist in **all**
  locales (`src/locales/*.json`) — the type system enforces parity.
- Commits: imperative subject, body explains the why, `-s` sign-off.
