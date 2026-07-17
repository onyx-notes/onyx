# Store listings (Google Play + Apple App Store)

Everything the store consoles ask for, versioned here so listings are
reviewable like code. `fastlane supply` (Play) reads from this layout via
the release workflow; App Store Connect fields are pasted from here until
`fastlane deliver` is wired.

## Listing copy

**Name**: Onyx — Private Notes
**Subtitle / short description** (30/80 chars):
> Your notes, your server, your AI.

**Full description**:
> Onyx is an open-source, end-to-end encrypted home for your notes.
> Markdown files in a vault you own, synced through a server you run —
> nobody else can read a word, not even the server.
>
> • Obsidian-compatible: opens your existing vault of .md files
> • Live-preview markdown editor built for touch, with a formatting bar
> • End-to-end encrypted sync between all your devices (CRDT — no
>   conflicts, offline-first)
> • Unlock with Face ID / fingerprint
> • Share text into Onyx from any app; capture from a home-screen action
> • Pair a new device by scanning a QR code and comparing a short code
> • Full-text search, backlinks, graph, version history, daily notes
> • Optional AI chat over your notes — your API key, or a local model
> • AGPL open source, no accounts, no telemetry, no ads
>
> Onyx syncs through a server you self-host (or none at all — it works
> fully offline). Nothing about your notes ever touches our
> infrastructure, because there isn't any.

**Category**: Productivity. **Tags**: notes, markdown, encrypted, self-hosted.

## Privacy forms

Both stores: **no data collected** — and the reasoning, stated for review:

- The app makes network connections only to (a) the sync/backup server the
  *user* configures (their own infrastructure) and (b) the AI endpoint the
  user configures with their own key. The developer operates no server and
  receives nothing: no analytics, no crash reporting, no identifiers.
- All note content leaving the device is end-to-end encrypted; the user's
  own server stores ciphertext only.
- Camera permission: used solely to scan device-pairing QR codes,
  processed on-device. Biometric permission: used solely to gate the
  vault key held in the OS keystore.

Play Data Safety: "No data collected, no data shared" + the security
practices boxes (data encrypted in transit, user can request deletion =
n/a, no data). App Store: Privacy Nutrition Label "Data Not Collected".

## Export compliance (iOS)

`ITSAppUsesNonExemptEncryption`: the app uses standard, published
encryption (XChaCha20-Poly1305, argon2id, X25519 — all in open-source
crates) for its E2EE function. This falls under US EAR 5A992.c / mass
market note 4 — **exempt, but file the annual self-classification report
with BIS/ENC**. Answer "Yes, uses encryption; qualifies for exemption".
Have counsel confirm before the first public release. France: standard
declaration for mass-market crypto may apply.

## Licensing

Store distribution is covered by the AGPL §7 additional permission in
`LICENSE-EXCEPTIONS.md`. Keep the source link (GitHub) in each listing's
description or support URL — condition (1) of the exception.

## Remaining manual steps (accounts required)

- Google Play Console: create app `dev.onyx.app`, upload keystore
  fingerprint, complete Data Safety with the answers above, add listing
  assets (icon 512px, feature graphic 1024×500, ≥2 phone screenshots).
- App Store Connect: create app record (bundle id `dev.onyx.app`),
  Privacy Nutrition Label, export-compliance answer, screenshots
  (6.7" + 5.5"), App Review notes: test vault + self-hosted server
  docker one-liner for the reviewer.
- Screenshots: generate from emulator/simulator once available (blocked
  on device access in the dev environment).
