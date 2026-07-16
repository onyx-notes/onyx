// CodeMirror 6 markdown editor pane with Obsidian-style live preview,
// wikilink autocomplete, and autosave. Ctrl+E toggles source mode via a
// compartment — instant, no remount.

import { markdown, markdownLanguage } from "@codemirror/lang-markdown";
import { Compartment, EditorState } from "@codemirror/state";
import { EditorView, keymap } from "@codemirror/view";
import { basicSetup } from "codemirror";
import { createEffect, on, onCleanup, onMount } from "solid-js";

import { api } from "../api";
import { livePreview } from "../editor/live-preview";
import { wikilinkCompletion } from "../editor/wikilink-complete";

const AUTOSAVE_MS = 400;

export interface EditorProps {
  /** Path of the open note (identity — content reloads when it changes). */
  path: string;
  /** Initial content for the current path. */
  content: string;
  /** Called with the full document, debounced, after edits. */
  onChange: (content: string) => void;
  /** Follow a wikilink target (open or create the note). */
  onFollowLink: (target: string) => void;
  /** Bump this counter to force a reload from `content` (external edits). */
  reloadSignal: number;
}

export default function Editor(props: EditorProps) {
  let host!: HTMLDivElement;
  let view: EditorView | undefined;
  let saveTimer: ReturnType<typeof setTimeout> | undefined;
  let suppressChange = false;
  let sourceMode = false;
  const previewCompartment = new Compartment();

  const flushPending = () => {
    if (saveTimer !== undefined) {
      clearTimeout(saveTimer);
      saveTimer = undefined;
    }
  };

  const previewExtension = () =>
    sourceMode ? [] : livePreview({ followLink: (target) => props.onFollowLink(target) });

  const toggleSourceMode = () => {
    sourceMode = !sourceMode;
    view?.dispatch({
      effects: previewCompartment.reconfigure(previewExtension()),
    });
    return true;
  };

  const buildState = (content: string) =>
    EditorState.create({
      doc: content,
      extensions: [
        basicSetup,
        markdown(),
        markdownLanguage.data.of({
          autocomplete: wikilinkCompletion(async (query) =>
            (await api.quickOpen(query)).map((hit) => ({ path: hit.path })),
          ),
        }),
        previewCompartment.of(previewExtension()),
        keymap.of([{ key: "Mod-e", run: toggleSourceMode }]),
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
    view.focus();
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
        view.focus();
      },
    ),
  );

  onCleanup(() => {
    flushPending();
    view?.destroy();
  });

  return <div class="editor-host" ref={host} />;
}
