# Vault Word Count — Onyx sample plugin

The minimal Onyx plugin: one command, two capabilities.

## Install

Copy this folder to `<your vault>/.onyx/plugins/word-count/` and reopen the
vault. Run "Count words in vault" from the command palette (Ctrl+Shift+P).

## Anatomy

- `manifest.json` — identity + **declared capabilities**. Calls outside the
  declared set are rejected by the host; users see exactly what a plugin
  can touch before enabling it.
- `main.js` — runs in a sandboxed, separate-origin iframe with no DOM, no
  network, and no filesystem. The global `onyx` object is the entire API.

## API (v1)

```js
onyx.vault.list()                    // capability: vault:read
onyx.vault.read(path)                // capability: vault:read
onyx.vault.write(path, content)      // capability: vault:write
onyx.commands.register({id,name,run}) // capability: ui:commands
onyx.notice(message)                 // always available
```
