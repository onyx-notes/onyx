// Pure inline scanner for Obsidian syntax the Lezer markdown parser doesn't
// know: [[wikilinks]] and #tags. Mirrors the Rust-side onyx-md semantics
// (the semantic authority) for the subset the editor renders live.
//
// Code masking is the caller's job (it has the syntax tree); this scanner
// is pure text → ranges, which is what makes it unit-testable.

export interface WikilinkMatch {
  /** Byte offsets are code-unit offsets into `text`, plus `offset`. */
  start: number;
  end: number;
  embed: boolean;
  /** Full inner text between the brackets. */
  inner: string;
  /** Link target (before # and |), trimmed. */
  target: string;
  /** Display alias if `|alias` present. */
  alias: string | null;
  /** Offset where the display text (alias or inner) starts. */
  displayStart: number;
  displayEnd: number;
}

export interface TagMatch {
  start: number;
  end: number;
  tag: string;
}

const TAG_CHAR = /[\p{L}\p{N}_\-/]/u;

export function scanInline(
  text: string,
  offset = 0,
): { links: WikilinkMatch[]; tags: TagMatch[] } {
  const links: WikilinkMatch[] = [];
  const tags: TagMatch[] = [];

  let i = 0;
  while (i < text.length) {
    const c = text[i];

    if (c === "\\") {
      i += 2;
      continue;
    }

    if (text.startsWith("[[", i) || text.startsWith("![[", i)) {
      const embed = c === "!";
      const open = embed ? i + 3 : i + 2;
      const close = text.indexOf("]]", open);
      const inner = close === -1 ? null : text.slice(open, close);
      if (
        inner !== null &&
        inner.length > 0 &&
        !inner.includes("[[") &&
        !inner.includes("\n")
      ) {
        const pipe = inner.indexOf("|");
        const targetPart = pipe === -1 ? inner : inner.slice(0, pipe);
        const alias = pipe === -1 ? null : inner.slice(pipe + 1);
        const hash = targetPart.indexOf("#");
        const target = (hash === -1 ? targetPart : targetPart.slice(0, hash)).trim();
        if (target.length > 0 || hash !== -1) {
          const displayStart = pipe === -1 ? open : open + pipe + 1;
          links.push({
            start: offset + i,
            end: offset + close + 2,
            embed,
            inner,
            target,
            alias,
            displayStart: offset + displayStart,
            displayEnd: offset + close,
          });
          i = close + 2;
          continue;
        }
      }
      // Not a valid link: skip just the opener so a nested `[[` can match.
      i += embed ? 3 : 2;
      continue;
    }

    if (c === "#") {
      const before = text[i - 1];
      const precededOk = i === 0 || before === undefined || /\s/.test(before);
      if (precededOk) {
        let end = i + 1;
        while (end < text.length && TAG_CHAR.test(text[end] ?? "")) end += 1;
        let raw = text.slice(i + 1, end);
        while (raw.endsWith("/")) raw = raw.slice(0, -1);
        const isValid =
          raw.length > 0 && !raw.startsWith("/") && /[^\d]/.test(raw);
        if (isValid) {
          tags.push({ start: offset + i, end: offset + i + 1 + raw.length, tag: raw });
          i = end;
          continue;
        }
      }
    }

    i += 1;
  }

  return { links, tags };
}
