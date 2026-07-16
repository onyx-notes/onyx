// Tab strip: horizontal by default, vertical rail (Arc/Edge-style) on
// toggle. Middle-click closes; the + button opens an empty tab.

import { For } from "solid-js";

import { t } from "../i18n";
import { type Workspace, tabTitle } from "../workspace";

export default function TabBar(props: { workspace: Workspace }) {
  const { state, setActive, closeTab, newTab } = props.workspace;

  return (
    <div class="tabbar" classList={{ vertical: state.vertical }}>
      <For each={state.tabs}>
        {(tab, index) => (
          <div
            class="tab"
            classList={{ active: index() === state.active }}
            onMouseDown={(event) => {
              if (event.button === 1) {
                event.preventDefault();
                closeTab(index());
              } else if (event.button === 0) {
                setActive(index());
              }
            }}
            title={tab.path ?? ""}
          >
            <span class="tab-title">{tabTitle(tab)}</span>
            <button
              class="tab-close"
              onMouseDown={(event) => event.stopPropagation()}
              onClick={() => closeTab(index())}
              aria-label={t("tabs.close")}
            >
              ×
            </button>
          </div>
        )}
      </For>
      <button class="tab-new" onClick={newTab} aria-label={t("tabs.new")}>
        +
      </button>
    </div>
  );
}
