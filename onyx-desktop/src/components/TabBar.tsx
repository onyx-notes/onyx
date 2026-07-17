// Tab strip: horizontal by default, vertical rail (Arc/Edge-style) on
// toggle. Middle-click closes; the + button opens an empty tab.

import { For } from "solid-js";

import { t } from "../i18n";
import { type Workspace, tabTitle } from "../workspace";

export default function TabBar(props: { workspace: Workspace; paneIndex: number }) {
  const { state, focusPane, setActive, closeTab, newTab, splitRight } = props.workspace;
  const pane = () => state.panes[props.paneIndex];
  const isActivePane = () => props.paneIndex === state.activePane;

  const act = (fn: () => void) => {
    focusPane(props.paneIndex);
    fn();
  };

  return (
    <div class="tabbar" classList={{ vertical: state.vertical }}>
      <For each={pane()?.tabs ?? []}>
        {(tab, index) => (
          <div
            class="tab"
            classList={{ active: index() === pane()?.active && isActivePane() }}
            onMouseDown={(event) => {
              if (event.button === 1) {
                event.preventDefault();
                act(() => closeTab(index()));
              } else if (event.button === 0) {
                act(() => setActive(index()));
              }
            }}
            title={tab.path ?? ""}
          >
            <span class="tab-title">{tabTitle(tab)}</span>
            <button
              class="tab-close"
              onMouseDown={(event) => event.stopPropagation()}
              onClick={() => act(() => closeTab(index()))}
              aria-label={t("tabs.close")}
            >
              ×
            </button>
          </div>
        )}
      </For>
      <button class="tab-new" onClick={() => act(newTab)} aria-label={t("tabs.new")}>
        +
      </button>
      <button
        class="tab-new"
        onClick={() => act(splitRight)}
        aria-label={t("tabs.split")}
        title={t("tabs.split")}
      >
        ⊞
      </button>
    </div>
  );
}
