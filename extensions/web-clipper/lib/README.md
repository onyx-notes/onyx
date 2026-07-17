Place `Readability.js` (from @mozilla/readability) and `turndown.js` (from
the turndown package) here. They are MIT/Apache-licensed vendored deps kept
out of the repo; the build/release step fetches pinned versions. The popup
loads them locally so no code is fetched at runtime.
