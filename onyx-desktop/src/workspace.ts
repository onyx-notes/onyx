// Workspace state: tabs with browser-like per-tab history. The recursive
// split tree from the plan layers on top of this later; tabs are the
// foundation and their semantics (history, MRU, background open) are what
// users feel every minute.

import { createStore, produce } from "solid-js/store";

export interface Tab {
  id: number;
  /** null = empty tab (new-tab page). */
  path: string | null;
  history: string[];
  historyIndex: number;
}

export interface WorkspaceState {
  tabs: Tab[];
  active: number;
  vertical: boolean;
}

let nextTabId = 1;

function emptyTab(): Tab {
  return { id: nextTabId++, path: null, history: [], historyIndex: -1 };
}

export function createWorkspace() {
  const [state, setState] = createStore<WorkspaceState>({
    tabs: [emptyTab()],
    active: 0,
    vertical: false,
  });

  const activeTab = (): Tab | undefined => state.tabs[state.active];
  const activePath = (): string | null => activeTab()?.path ?? null;

  /** Navigate the active tab to `path`, pushing history. */
  const openInActive = (path: string) => {
    setState(
      produce((workspace) => {
        const tab = workspace.tabs[workspace.active];
        if (!tab || tab.path === path) return;
        // A new navigation truncates any forward history (browser rule).
        tab.history = tab.history.slice(0, tab.historyIndex + 1);
        tab.history.push(path);
        tab.historyIndex = tab.history.length - 1;
        tab.path = path;
      }),
    );
  };

  const openInNewTab = (path: string, background = false) => {
    setState(
      produce((workspace) => {
        const tab = emptyTab();
        tab.path = path;
        tab.history = [path];
        tab.historyIndex = 0;
        workspace.tabs.push(tab);
        if (!background) workspace.active = workspace.tabs.length - 1;
      }),
    );
  };

  const newTab = () => {
    setState(
      produce((workspace) => {
        workspace.tabs.push(emptyTab());
        workspace.active = workspace.tabs.length - 1;
      }),
    );
  };

  const closeTab = (index: number) => {
    setState(
      produce((workspace) => {
        if (workspace.tabs.length === 1) {
          // Last tab never closes; it resets (app always has a workspace).
          workspace.tabs = [emptyTab()];
          workspace.active = 0;
          return;
        }
        workspace.tabs.splice(index, 1);
        if (workspace.active >= workspace.tabs.length) {
          workspace.active = workspace.tabs.length - 1;
        } else if (index < workspace.active) {
          workspace.active -= 1;
        }
      }),
    );
  };

  const setActive = (index: number) => {
    if (index >= 0 && index < state.tabs.length) setState("active", index);
  };

  const cycleTab = (delta: number) => {
    const count = state.tabs.length;
    setState("active", (state.active + delta + count) % count);
  };

  /** Browser-style history navigation on the active tab. */
  const navigate = (delta: number) => {
    setState(
      produce((workspace) => {
        const tab = workspace.tabs[workspace.active];
        if (!tab) return;
        const target = tab.historyIndex + delta;
        if (target < 0 || target >= tab.history.length) return;
        tab.historyIndex = target;
        tab.path = tab.history[target] ?? null;
      }),
    );
  };

  /** A note was deleted/renamed away: point affected tabs at nothing. */
  const evictPath = (path: string) => {
    setState(
      produce((workspace) => {
        for (const tab of workspace.tabs) {
          if (tab.path === path) tab.path = null;
        }
      }),
    );
  };

  const toggleVertical = () => setState("vertical", (value) => !value);

  return {
    state,
    activeTab,
    activePath,
    openInActive,
    openInNewTab,
    newTab,
    closeTab,
    setActive,
    cycleTab,
    navigate,
    evictPath,
    toggleVertical,
  };
}

export type Workspace = ReturnType<typeof createWorkspace>;

/** Display title for a tab: the note's filename stem. */
export function tabTitle(tab: Tab): string {
  if (tab.path === null) return "•";
  const name = tab.path.split("/").at(-1) ?? tab.path;
  return name.replace(/\.(md|markdown)$/i, "");
}
