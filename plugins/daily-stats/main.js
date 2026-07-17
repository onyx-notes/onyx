// Report total words across all markdown notes.
onyx.commands.register({
  id: "vault-word-total",
  name: "Show total vault word count",
  run: async () => {
    const notes = await onyx.vault.list();
    let total = 0;
    for (const note of notes) {
      if (!note.isMarkdown) continue;
      const body = await onyx.vault.read(note.path);
      total += body.split(/\s+/).filter((w) => /[\p{L}\p{N}]/u.test(w)).length;
    }
    await onyx.notice(`${total.toLocaleString()} words in your vault`);
  },
});
