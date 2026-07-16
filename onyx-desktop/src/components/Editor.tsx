// CodeMirror 6 markdown editor pane. M2 scope: source editing with
// autosave; live-preview decorations are the M3 milestone.

import { markdown } from "@codemirror/lang-markdown";
import { EditorState } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import { basicSetup } from "codemirror";
import { createEffect, on, onCleanup, onMount } from "solid-js";

const AUTOSAVE_MS = 400;

export interface EditorProps {
  /** Path of the open note (identity — content reloads when it changes). */
  path: string;
  /** Initial content for the current path. */
  content: string;
  /** Called with the full document, debounced, after edits. */
  onChange: (content: string) => void;
  /** Bump this counter to force a reload from `content` (external edits). */
  reloadSignal: number;
}

export default function Editor(props: EditorProps) {
  let host!: HTMLDivElement;
  let view: EditorView | undefined;
  let saveTimer: ReturnType<typeof setTimeout> | undefined;
  let suppressChange = false;

  const flushPending = () => {
    if (saveTimer !== undefined) {
      clearTimeout(saveTimer);
      saveTimer = undefined;
    }
  };

  const buildState = (content: string) =>
    EditorState.create({
      doc: content,
      extensions: [
        basicSetup,
        markdown(),
        EditorView.lineWrapping,
        EditorView.updateListener.of((update) => {
          if (!update.docChanged || suppressChange) return;
          flushPending();
          saveTimer = setTimeout(() => {
            saveTimer = undefined;
            props.onChange(update.state.doc.toString());
          }, AUTOSAVE_MS);
        }),
        EditorView.theme({}, { dark: true }),
      ],
    });

  onMount(() => {
    view = new EditorView({ state: buildState(props.content), parent: host });
  });

  // Switching notes (or external reloads) replaces the document wholesale.
  createEffect(
    on(
      () => [props.path, props.reloadSignal] as const,
      (_, previous) => {
        if (!view || previous === undefined) return;
        flushPending();
        suppressChange = true;
        view.setState(buildState(props.content));
        suppressChange = false;
      },
    ),
  );

  onCleanup(() => {
    flushPending();
    view?.destroy();
  });

  return <div class="editor-host" ref={host} />;
}
