// Backlinks panel: who links to the active note. Refreshes on vault
// changes via the epoch counter.

import { For, Show, createResource } from "solid-js";

import { api } from "../api";
import { t } from "../i18n";

export default function Backlinks(props: {
  path: string | null;
  epoch: number;
  onOpen: (path: string) => void;
}) {
  const [links] = createResource(
    () => (props.path === null ? null : ([props.path, props.epoch] as const)),
    ([path]) => api.backlinks(path),
    { initialValue: [] },
  );

  return (
    <aside class="backlinks">
      <div class="sidebar-header">
        <span>{t("backlinks.title")}</span>
      </div>
      <div class="file-list">
        <Show
          when={links().length > 0}
          fallback={<div class="palette-empty">{t("backlinks.empty")}</div>}
        >
          <For each={links()}>
            {(source) => (
              <button class="file-item" onClick={() => props.onOpen(source)}>
                {source}
              </button>
            )}
          </For>
        </Show>
      </div>
    </aside>
  );
}
