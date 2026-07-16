// Obsidian-style live preview for CodeMirror 6.
//
// The contract: formatting marks (`**`, `#`, `[[ ]]`, `>` …) are hidden and
// content is styled in place — until the cursor touches the construct's
// line(s), which reveals the raw markdown for editing. Decorations are
// computed for the viewport only, on doc/selection/viewport changes, so
// cost is bounded by what's on screen, never by document size.

import { syntaxTree } from "@codemirror/language";
import type { EditorState, Range } from "@codemirror/state";
import {
  Decoration,
  type DecorationSet,
  EditorView,
  ViewPlugin,
  type ViewUpdate,
  WidgetType,
} from "@codemirror/view";

import { scanInline } from "./inline-scan";

export interface LivePreviewOptions {
  /** Follow a wikilink target (rendered-link click or Ctrl+click). */
  followLink: (target: string) => void;
}

// ---------------------------------------------------------------------------
// Widgets
// ---------------------------------------------------------------------------

class BulletWidget extends WidgetType {
  toDOM(): HTMLElement {
    const span = document.createElement("span");
    span.className = "cm-onyx-bullet";
    span.textContent = "•";
    return span;
  }
}

class RuleWidget extends WidgetType {
  toDOM(): HTMLElement {
    const span = document.createElement("span");
    span.className = "cm-onyx-hr";
    return span;
  }
}

class TaskWidget extends WidgetType {
  constructor(private checked: boolean) {
    super();
  }

  override eq(other: TaskWidget): boolean {
    return other.checked === this.checked;
  }

  toDOM(): HTMLElement {
    const box = document.createElement("input");
    box.type = "checkbox";
    box.checked = this.checked;
    box.className = "cm-onyx-taskbox";
    return box;
  }

  override ignoreEvent(): boolean {
    // Let our mousedown handler process checkbox clicks.
    return false;
  }
}

const bulletWidget = new BulletWidget();
const ruleWidget = new RuleWidget();

// ---------------------------------------------------------------------------
// Reveal rule
// ---------------------------------------------------------------------------

/** Does any selection touch the lines spanned by [from, to]? */
function selectionTouches(state: EditorState, from: number, to: number): boolean {
  const first = state.doc.lineAt(from).from;
  const last = state.doc.lineAt(Math.min(to, state.doc.length)).to;
  return state.selection.ranges.some((range) => range.from <= last && range.to >= first);
}

/** Is this position inside code (inline or fenced)? Links/tags don't render there. */
function insideCode(state: EditorState, pos: number): boolean {
  let node = syntaxTree(state).resolveInner(pos, 1);
  while (node.parent) {
    if (node.name === "InlineCode" || node.name === "FencedCode" || node.name === "CodeBlock") {
      return true;
    }
    node = node.parent;
  }
  return false;
}

// ---------------------------------------------------------------------------
// Decoration build
// ---------------------------------------------------------------------------

const HEADING_LEVELS: Record<string, number> = {
  ATXHeading1: 1,
  ATXHeading2: 2,
  ATXHeading3: 3,
  ATXHeading4: 4,
  ATXHeading5: 5,
  ATXHeading6: 6,
};

const INLINE_STYLES: Record<string, string> = {
  Emphasis: "cm-onyx-em",
  StrongEmphasis: "cm-onyx-strong",
  Strikethrough: "cm-onyx-strike",
  InlineCode: "cm-onyx-code",
};

function build(view: EditorView): DecorationSet {
  const { state } = view;
  const decorations: Range<Decoration>[] = [];
  const line = (at: number, className: string) =>
    decorations.push(Decoration.line({ class: className }).range(at));
  const mark = (from: number, to: number, spec: Parameters<typeof Decoration.mark>[0]) => {
    if (from < to) decorations.push(Decoration.mark(spec).range(from, to));
  };
  const conceal = (from: number, to: number) => {
    if (from < to) decorations.push(Decoration.replace({}).range(from, to));
  };

  for (const range of view.visibleRanges) {
    syntaxTree(state).iterate({
      from: range.from,
      to: range.to,
      enter(node) {
        const revealed = selectionTouches(state, node.from, node.to);
        const name = node.name;

        const headingLevel = HEADING_LEVELS[name];
        if (headingLevel !== undefined) {
          line(state.doc.lineAt(node.from).from, `cm-onyx-h${headingLevel}`);
          if (!revealed) {
            const markNode = node.node.getChild("HeaderMark");
            if (markNode) {
              // Conceal the `#…# ` including its trailing space.
              conceal(markNode.from, Math.min(markNode.to + 1, node.to));
            }
          }
          return;
        }

        const inlineClass = INLINE_STYLES[name];
        if (inlineClass !== undefined) {
          mark(node.from, node.to, { class: inlineClass });
          if (!revealed) {
            for (const child of node.node.getChildren(
              name === "InlineCode" ? "CodeMark" : `${name === "Strikethrough" ? "Strikethrough" : "Emphasis"}Mark`,
            )) {
              conceal(child.from, child.to);
            }
          }
          return;
        }

        switch (name) {
          case "Blockquote": {
            const firstLine = state.doc.lineAt(node.from).number;
            const lastLine = state.doc.lineAt(node.to).number;
            for (let n = firstLine; n <= lastLine; n += 1) {
              line(state.doc.line(n).from, "cm-onyx-quote");
            }
            if (!revealed) {
              for (const quoteMark of node.node.getChildren("QuoteMark")) {
                // Conceal `>` plus one following space if present.
                const after = state.doc.sliceString(quoteMark.to, quoteMark.to + 1);
                conceal(quoteMark.from, quoteMark.to + (after === " " ? 1 : 0));
              }
            }
            break;
          }
          case "FencedCode": {
            const firstLine = state.doc.lineAt(node.from).number;
            const lastLine = state.doc.lineAt(node.to).number;
            for (let n = firstLine; n <= lastLine; n += 1) {
              line(state.doc.line(n).from, "cm-onyx-codeblock");
            }
            break;
          }
          case "Link": {
            mark(node.from, node.to, { class: "cm-onyx-link" });
            if (!revealed) {
              // `[text](url)` → show only text.
              const marks = node.node.getChildren("LinkMark");
              const url = node.node.getChild("URL");
              const opening = marks[0];
              const textEnd = marks[1];
              if (opening) conceal(opening.from, opening.to);
              if (textEnd && url) conceal(textEnd.from, node.to);
            }
            break;
          }
          case "HorizontalRule": {
            if (!revealed) {
              decorations.push(
                Decoration.replace({ widget: ruleWidget }).range(node.from, node.to),
              );
            }
            break;
          }
          case "ListMark": {
            const parent = node.node.parent;
            const inBullet = parent?.parent?.name === "BulletList";
            const text = state.doc.sliceString(node.from, node.to);
            if (inBullet && !revealed && /^[-*+]$/.test(text)) {
              decorations.push(
                Decoration.replace({ widget: bulletWidget }).range(node.from, node.to),
              );
            }
            break;
          }
          case "TaskMarker": {
            const checked = /x/i.test(state.doc.sliceString(node.from, node.to));
            if (!revealed) {
              decorations.push(
                Decoration.replace({ widget: new TaskWidget(checked) }).range(
                  node.from,
                  node.to,
                ),
              );
            }
            if (checked) {
              const taskLine = state.doc.lineAt(node.from);
              line(taskLine.from, "cm-onyx-task-done");
            }
            break;
          }
        }
      },
    });

    // Wikilinks and tags: not in the Lezer grammar, scanned per line so
    // offsets stay cheap; matches inside code are dropped.
    const firstLine = state.doc.lineAt(range.from).number;
    const lastLine = state.doc.lineAt(range.to).number;
    for (let n = firstLine; n <= lastLine; n += 1) {
      const docLine = state.doc.line(n);
      const { links, tags } = scanInline(docLine.text, docLine.from);
      for (const link of links) {
        if (insideCode(state, link.start)) continue;
        const revealed = selectionTouches(state, link.start, link.end);
        const attributes = { "data-onyx-target": link.target };
        if (revealed) {
          mark(link.start, link.end, { class: "cm-onyx-wikilink", attributes });
        } else {
          conceal(link.start, link.displayStart);
          mark(link.displayStart, link.displayEnd, {
            class: "cm-onyx-wikilink is-preview",
            attributes,
          });
          conceal(link.displayEnd, link.end);
        }
      }
      for (const tag of tags) {
        if (insideCode(state, tag.start)) continue;
        mark(tag.start, tag.end, { class: "cm-onyx-tag" });
      }
    }
  }

  return Decoration.set(decorations, true);
}

// ---------------------------------------------------------------------------
// Interaction
// ---------------------------------------------------------------------------

function toggleTaskAt(view: EditorView, pos: number): boolean {
  let node = syntaxTree(view.state).resolveInner(pos, 1);
  while (node.parent && node.name !== "TaskMarker") node = node.parent;
  if (node.name !== "TaskMarker") return false;
  const checked = /x/i.test(view.state.doc.sliceString(node.from, node.to));
  view.dispatch({
    changes: { from: node.from, to: node.to, insert: checked ? "[ ]" : "[x]" },
  });
  return true;
}

function interactions(options: LivePreviewOptions) {
  return EditorView.domEventHandlers({
    mousedown(event, view) {
      const target = event.target as HTMLElement;

      if (target.classList.contains("cm-onyx-taskbox")) {
        const pos = view.posAtDOM(target);
        if (toggleTaskAt(view, pos)) {
          event.preventDefault();
          return true;
        }
      }

      const linkEl = target.closest<HTMLElement>(".cm-onyx-wikilink");
      if (linkEl) {
        const linkTarget = linkEl.dataset["onyxTarget"];
        const follow =
          event.ctrlKey || event.metaKey || linkEl.classList.contains("is-preview");
        if (linkTarget && follow) {
          event.preventDefault();
          options.followLink(linkTarget);
          return true;
        }
      }
      return false;
    },
  });
}

// ---------------------------------------------------------------------------
// Extension entry point
// ---------------------------------------------------------------------------

export function livePreview(options: LivePreviewOptions) {
  const plugin = ViewPlugin.fromClass(
    class {
      decorations: DecorationSet;

      constructor(view: EditorView) {
        this.decorations = build(view);
      }

      update(update: ViewUpdate) {
        if (update.docChanged || update.selectionSet || update.viewportChanged) {
          this.decorations = build(update.view);
        }
      }
    },
    { decorations: (instance) => instance.decorations },
  );

  return [plugin, interactions(options)];
}
