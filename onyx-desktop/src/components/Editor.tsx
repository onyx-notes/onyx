// CodeMirror 6 markdown editor pane with Obsidian-style live preview,
// wikilink autocomplete, and autosave. Ctrl+E toggles source mode via a
// compartment — instant, no remount.

import { indentLess, indentMore, redo, undo } from "@codemirror/commands";
import { markdown, markdownLanguage } from "@codemirror/lang-markdown";
import { Compartment, EditorState } from "@codemirror/state";
import { EditorView, keymap } from "@codemirror/view";
import { basicSetup } from "codemirror";
import { createEffect, on, onCleanup, onMount } from "solid-js";

import { api } from "../api";
import { clearEmbedCache } from "../editor/embed-widget";
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
  /** Scroll/cursor jump request (outline clicks). */
  scrollTarget: { offset: number; epoch: number } | null;
  /** Text a plugin asked to insert at the cursor. */
  insert: { text: string; epoch: number } | null;
  /** Mobile shell: enables touch keyboard attributes. */
  mobile?: boolean;
  /** Receives imperative editing controls (mobile formatting toolbar). */
  onReady?: (controls: EditorControls) => void;
}

/** Imperative surface for toolbar-style UI outside the editor DOM. */
export interface EditorControls {
  wrapSelection(prefix: string, suffix: string): void;
  prefixLines(prefix: string): void;
  insertText(text: string): void;
  undo(): void;
  redo(): void;
  indent(delta: 1 | -1): void;
  focus(): void;
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

  const buildState = (content: string) => {
    // Fresh note or external reload: embedded content may have changed.
    clearEmbedCache();
    return EditorState.create({
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
        // Touch keyboards want sentence-casing and autocorrect; desktop
        // keeps them off (spellcheck squiggles are a setting, not a default).
        props.mobile
          ? EditorView.contentAttributes.of({
              autocapitalize: "sentences",
              autocorrect: "on",
              spellcheck: "true",
            })
          : [],
      ],
    });
  };

  /** Toolbar-facing imperative controls (mobile formatting bar). */
  const makeControls = (editor: EditorView): EditorControls => ({
    wrapSelection(prefix, suffix) {
      const { from, to } = editor.state.selection.main;
      const selected = editor.state.sliceDoc(from, to);
      editor.dispatch({
        changes: { from, to, insert: `${prefix}${selected}${suffix}` },
        selection: selected.length
          ? { anchor: from, head: to + prefix.length + suffix.length }
          : { anchor: from + prefix.length },
      });
      editor.focus();
    },
    prefixLines(prefix) {
      const range = editor.state.selection.main;
      const firstLine = editor.state.doc.lineAt(range.from).number;
      const lastLine = editor.state.doc.lineAt(range.to).number;
      const lines = [];
      for (let line = firstLine; line <= lastLine; line += 1) {
        lines.push(editor.state.doc.line(line));
      }
      // Toggle: strip the prefix when every selected line already has it.
      const allPrefixed = lines.every((line) => line.text.startsWith(prefix));
      const changes = lines.map((line) =>
        allPrefixed
          ? { from: line.from, to: line.from + prefix.length, insert: "" }
          : { from: line.from, insert: prefix },
      );
      editor.dispatch({ changes });
      editor.focus();
    },
    insertText(text) {
      const at = editor.state.selection.main.head;
      editor.dispatch({
        changes: { from: at, insert: text },
        selection: { anchor: at + text.length },
      });
      editor.focus();
    },
    undo() {
      undo(editor);
      editor.focus();
    },
    redo() {
      redo(editor);
      editor.focus();
    },
    indent(delta) {
      (delta > 0 ? indentMore : indentLess)(editor);
      editor.focus();
    },
    focus() {
      editor.focus();
    },
  });

  onMount(() => {
    view = new EditorView({ state: buildState(props.content), parent: host });
    props.onReady?.(makeControls(view));
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

  // Plugin editor.insert: drop text at the current selection.
  createEffect(
    on(
      () => props.insert,
      (request) => {
        if (!view || !request) return;
        const at = view.state.selection.main.head;
        view.dispatch({
          changes: { from: at, insert: request.text },
          selection: { anchor: at + request.text.length },
        });
        view.focus();
      },
    ),
  );

  // Outline clicks: place the cursor at the heading and scroll it to top.
  createEffect(
    on(
      () => props.scrollTarget,
      (target) => {
        if (!view || !target) return;
        const offset = Math.min(target.offset, view.state.doc.length);
        view.dispatch({
          selection: { anchor: offset },
          effects: EditorView.scrollIntoView(offset, { y: "start" }),
        });
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
