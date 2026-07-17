// One pane: its tab strip + the editor/reading view for its active tab.
// Pane content is self-contained so panes edit independently.

import { Show, createEffect, createSignal, on } from "solid-js";

import { api } from "../api";
import { t } from "../i18n";
import { type Workspace } from "../workspace";
import Editor from "./Editor";
import ReadingView from "./ReadingView";
import TabBar from "./TabBar";

export default function Pane(props: {
  workspace: Workspace;
  paneIndex: number;
  reading: boolean;
  onFollowLink: (target: string) => void;
  onSave: (path: string, body: string) => void;
  externalReload: number;
  scrollTarget: { offset: number; epoch: number } | null;
  insert: { text: string; epoch: number } | null;
}) {
  const pane = () => props.workspace.state.panes[props.paneIndex];
  const activePath = () => {
    const current = pane();
    return current?.tabs[current.active]?.path ?? null;
  };

  const [content, setContent] = createSignal("");
  const [reloadSignal, setReloadSignal] = createSignal(0);

  // Load whatever this pane's active tab points at.
  createEffect(
    on(
      () => [activePath(), props.externalReload] as const,
      async ([path]) => {
        if (path === null) return;
        try {
          setContent(await api.readNote(path));
          setReloadSignal((n) => n + 1);
        } catch {
          setContent("");
        }
      },
    ),
  );

  const focus = () => props.workspace.focusPane(props.paneIndex);

  return (
    <div
      class="pane"
      classList={{ "pane-active": props.paneIndex === props.workspace.state.activePane }}
      onMouseDown={focus}
    >
      <TabBar workspace={props.workspace} paneIndex={props.paneIndex} />
      <main class="pane-main">
        <Show
          when={activePath()}
          fallback={<div class="empty-state">{t("editor.placeholder")}</div>}
        >
          {(path) => (
            <Show
              when={!props.reading}
              fallback={
                <ReadingView
                  path={path()}
                  reloadSignal={reloadSignal()}
                  onFollowLink={props.onFollowLink}
                />
              }
            >
              <Editor
                path={path()}
                content={content()}
                reloadSignal={reloadSignal()}
                onChange={(body) => props.onSave(path(), body)}
                onFollowLink={props.onFollowLink}
                scrollTarget={
                  props.paneIndex === props.workspace.state.activePane
                    ? props.scrollTarget
                    : null
                }
                insert={
                  props.paneIndex === props.workspace.state.activePane
                    ? props.insert
                    : null
                }
              />
            </Show>
          )}
        </Show>
      </main>
    </div>
  );
}
