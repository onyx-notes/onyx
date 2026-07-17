// Formatting toolbar shown above the on-screen keyboard while editing.
// Drives the editor through EditorControls (no DOM coupling), and uses
// onPointerDown+preventDefault so taps never steal focus from the editor
// (a focus bounce would dismiss the keyboard).

import { selectionFeedback } from "@tauri-apps/plugin-haptics";
import type { JSX } from "solid-js";

import type { EditorControls } from "../components/Editor";
import { type MessageKey, t } from "../i18n";

export default function MobileToolbar(props: { controls: EditorControls }) {
  const button = (
    label: string,
    titleKey: MessageKey,
    action: () => void,
  ): JSX.Element => (
    <button
      class="mobile-tool"
      title={t(titleKey)}
      aria-label={t(titleKey)}
      onPointerDown={(event) => {
        event.preventDefault();
        // Best-effort tick; absent in the desktop dev override.
        void selectionFeedback().catch(() => undefined);
        action();
      }}
    >
      {label}
    </button>
  );

  return (
    <div class="mobile-toolbar" role="toolbar">
      {button("B", "mobile.toolbar.bold", () => props.controls.wrapSelection("**", "**"))}
      {button("I", "mobile.toolbar.italic", () => props.controls.wrapSelection("*", "*"))}
      {button("H", "mobile.toolbar.heading", () => props.controls.prefixLines("# "))}
      {button("•", "mobile.toolbar.list", () => props.controls.prefixLines("- "))}
      {button("☐", "mobile.toolbar.task", () => props.controls.prefixLines("- [ ] "))}
      {button("[[", "mobile.toolbar.wikilink", () => props.controls.wrapSelection("[[", "]]"))}
      {button("`", "mobile.toolbar.code", () => props.controls.wrapSelection("`", "`"))}
      {button("⇥", "mobile.toolbar.indent", () => props.controls.indent(1))}
      {button("⇤", "mobile.toolbar.outdent", () => props.controls.indent(-1))}
      {button("↶", "mobile.toolbar.undo", () => props.controls.undo())}
      {button("↷", "mobile.toolbar.redo", () => props.controls.redo())}
    </div>
  );
}
