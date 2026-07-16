// Right side panel: outline (headings, click to jump) + backlinks.

import { For, Show, createResource } from "solid-js";

import { api } from "../api";
import { t } from "../i18n";

export default function RightPanel(props: {
  path: string | null;
  epoch: number;
  onOpen: (path: string) => void;
  onJump: (offset: number) => void;
}) {
  const source = () =>
    props.path === null ? null : ([props.path, props.epoch] as const);

  const [headings] = createResource(source, ([path]) => api.noteHeadings(path), {
    initialValue: [],
  });
  const [links] = createResource(source, ([path]) => api.backlinks(path), {
    initialValue: [],
  });

  return (
    <aside class="right-panel">
      <div class="sidebar-header">
        <span>{t("outline.title")}</span>
      </div>
      <div class="file-list outline">
        <Show
          when={headings().length > 0}
          fallback={<div class="palette-empty">{t("outline.empty")}</div>}
        >
          <For each={headings()}>
            {(heading) => (
              <button
                class="file-item outline-item"
                style={{ "padding-left": `${8 + (heading.level - 1) * 14}px` }}
                onClick={() => props.onJump(heading.offset)}
              >
                {heading.text}
              </button>
            )}
          </For>
        </Show>
      </div>

      <div class="sidebar-header">
        <span>{t("backlinks.title")}</span>
      </div>
      <div class="file-list">
        <Show
          when={links().length > 0}
          fallback={<div class="palette-empty">{t("backlinks.empty")}</div>}
        >
          <For each={links()}>
            {(sourcePath) => (
              <button class="file-item" onClick={() => props.onOpen(sourcePath)}>
                {sourcePath}
              </button>
            )}
          </For>
        </Show>
      </div>
    </aside>
  );
}
