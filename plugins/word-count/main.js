// Onyx sample plugin. Install: copy this folder into <vault>/.onyx/plugins/.
// Runs sandboxed: only the capabilities declared in manifest.json work.

onyx.commands.register({
  id: "count-vault-words",
  name: "Count words in vault",
  run: async () => {
    const notes = await onyx.vault.list();
    let total = 0;
    let counted = 0;
    for (const note of notes) {
      if (!note.isMarkdown) continue;
      const body = await onyx.vault.read(note.path);
      total += body.split(/\s+/).filter((token) => /\w/.test(token)).length;
      counted += 1;
    }
    await onyx.notice(`${total} words across ${counted} notes`);
  },
});
