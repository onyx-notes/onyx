# Onyx Web Clipper

A browser extension that clips the current page into your Onyx vault as
clean markdown. Extraction (Mozilla Readability) and HTML→markdown
(Turndown) run in the browser; only the finished markdown is sent to Onyx
over loopback.

## Setup

1. In Onyx: Settings → Web clipper → copy the token.
2. Load this folder as an unpacked extension (chrome://extensions →
   Developer mode → Load unpacked), after placing the two libraries in
   `lib/` (see `lib/README.md`).
3. Paste the token into the extension popup.

## Security

The clipper endpoint listens only on `127.0.0.1:47600` and requires the
token in the `X-Onyx-Token` header, so no other local page can write to
your vault. Onyx must be open with a vault unlocked.
