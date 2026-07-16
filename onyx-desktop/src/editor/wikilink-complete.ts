// `[[` autocomplete: suggests notes from the quick-switcher index while
// typing a wikilink. Registered as markdown language data so the stock
// autocompletion plumbing picks it up.

import type { CompletionContext, CompletionResult } from "@codemirror/autocomplete";

export interface LinkSuggestion {
  path: string;
}

/** Strip the markdown extension the way wikilink targets are written. */
function linkText(path: string): string {
  return path.replace(/\.(md|markdown)$/i, "");
}

export function wikilinkCompletion(
  fetchSuggestions: (query: string) => Promise<LinkSuggestion[]>,
) {
  return async (context: CompletionContext): Promise<CompletionResult | null> => {
    const match = context.matchBefore(/\[\[([^\[\]]*)$/);
    if (!match) return null;

    const query = match.text.slice(2);
    const hits = await fetchSuggestions(query);
    if (context.aborted) return null;

    const closesAhead = context.state.doc
      .sliceString(context.pos, context.pos + 2)
      .startsWith("]]");

    return {
      from: match.from + 2,
      options: hits.map((hit) => {
        const text = linkText(hit.path);
        return {
          label: text,
          type: "text",
          apply: closesAhead ? text : `${text}]]`,
        };
      }),
      // Backend already ranked fuzzily; client-side re-filtering would
      // fight it.
      filter: false,
    };
  };
}
