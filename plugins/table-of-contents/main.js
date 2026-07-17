// Insert a Markdown table of contents at the cursor, built from the
// active note's headings.
onyx.commands.register({
  id: "insert-toc",
  name: "Insert table of contents",
  run: async () => {
    const path = await onyx.editor.activePath();
    if (!path) return onyx.notice("Open a note first.");
    const body = await onyx.vault.read(path);
    const lines = body.split("\n");
    const toc = [];
    let inCode = false;
    for (const line of lines) {
      if (/^```/.test(line.trim())) { inCode = !inCode; continue; }
      if (inCode) continue;
      const match = /^(#{1,6})\s+(.+?)\s*#*$/.exec(line);
      if (match) {
        const depth = match[1].length - 1;
        toc.push(`${"  ".repeat(depth)}- ${match[2]}`);
      }
    }
    if (toc.length === 0) return onyx.notice("No headings found.");
    await onyx.editor.insert(`\n## Table of contents\n${toc.join("\n")}\n`);
  },
});
