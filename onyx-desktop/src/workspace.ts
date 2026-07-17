// Workspace state: a row of panes, each a tab group with browser-like
// per-tab history. One level of horizontal splitting — the most-used
// arrangement (edit here, reference there) — kept deliberately flat rather
// than a recursive tree; the operation surface stays small and testable.

import { createStore, produce } from "solid-js/store";

export interface Tab {
  id: number;
  /** null = empty tab (new-tab page). */
  path: string | null;
  history: string[];
  historyIndex: number;
}

export interface Pane {
  id: number;
  tabs: Tab[];
  active: number;
}

export interface WorkspaceState {
  panes: Pane[];
  activePane: number;
  vertical: boolean;
}

let nextId = 1;

function emptyTab(): Tab {
  return { id: nextId++, path: null, history: [], historyIndex: -1 };
}

function pathTab(path: string): Tab {
  return { id: nextId++, path, history: [path], historyIndex: 0 };
}

function newPane(tabs: Tab[]): Pane {
  return { id: nextId++, tabs, active: 0 };
}

export function createWorkspace() {
  const [state, setState] = createStore<WorkspaceState>({
    panes: [newPane([emptyTab()])],
    activePane: 0,
    vertical: false,
  });

  const pane = (): Pane | undefined => state.panes[state.activePane];
  const activeTab = (): Tab | undefined => {
    const current = pane();
    return current?.tabs[current.active];
  };
  const activePath = (): string | null => activeTab()?.path ?? null;

  /** Run a mutation against the active pane. */
  const withPane = (mutate: (pane: Pane) => void) =>
    setState(
      produce((workspace) => {
        const current = workspace.panes[workspace.activePane];
        if (current) mutate(current);
      }),
    );

  const openInActive = (path: string) =>
    withPane((current) => {
      const tab = current.tabs[current.active];
      if (!tab || tab.path === path) return;
      tab.history = tab.history.slice(0, tab.historyIndex + 1);
      tab.history.push(path);
      tab.historyIndex = tab.history.length - 1;
      tab.path = path;
    });

  const openInNewTab = (path: string, background = false) =>
    withPane((current) => {
      current.tabs.push(pathTab(path));
      if (!background) current.active = current.tabs.length - 1;
    });

  const newTab = () =>
    withPane((current) => {
      current.tabs.push(emptyTab());
      current.active = current.tabs.length - 1;
    });

  const closeTab = (index: number) =>
    setState(
      produce((workspace) => {
        const current = workspace.panes[workspace.activePane];
        if (!current) return;
        if (current.tabs.length === 1) {
          // Closing a pane's last tab closes the pane — unless it's the
          // only pane, which resets instead (app always has a workspace).
          if (workspace.panes.length === 1) {
            current.tabs = [emptyTab()];
            current.active = 0;
          } else {
            workspace.panes.splice(workspace.activePane, 1);
            workspace.activePane = Math.min(
              workspace.activePane,
              workspace.panes.length - 1,
            );
          }
          return;
        }
        current.tabs.splice(index, 1);
        if (current.active >= current.tabs.length) {
          current.active = current.tabs.length - 1;
        } else if (index < current.active) {
          current.active -= 1;
        }
      }),
    );

  const setActive = (index: number) =>
    withPane((current) => {
      if (index >= 0 && index < current.tabs.length) current.active = index;
    });

  const cycleTab = (delta: number) =>
    withPane((current) => {
      const count = current.tabs.length;
      current.active = (current.active + delta + count) % count;
    });

  const navigate = (delta: number) =>
    withPane((current) => {
      const tab = current.tabs[current.active];
      if (!tab) return;
      const target = tab.historyIndex + delta;
      if (target < 0 || target >= tab.history.length) return;
      tab.historyIndex = target;
      tab.path = tab.history[target] ?? null;
    });

  /** Move the active tab into a new pane to the right (or open an empty
   * pane if this pane has only one tab). */
  const splitRight = () =>
    setState(
      produce((workspace) => {
        const current = workspace.panes[workspace.activePane];
        if (!current) return;
        const moved =
          current.tabs.length > 1
            ? current.tabs.splice(current.active, 1)[0]!
            : emptyTab();
        if (current.tabs.length > 0 && current.active >= current.tabs.length) {
          current.active = current.tabs.length - 1;
        }
        workspace.panes.splice(workspace.activePane + 1, 0, newPane([moved]));
        workspace.activePane += 1;
      }),
    );

  const focusPane = (index: number) => {
    if (index >= 0 && index < state.panes.length) setState("activePane", index);
  };

  const cyclePane = (delta: number) => {
    const count = state.panes.length;
    setState("activePane", (state.activePane + delta + count) % count);
  };

  /** A note was deleted/renamed away: point every affected tab at nothing. */
  const evictPath = (path: string) =>
    setState(
      produce((workspace) => {
        for (const current of workspace.panes) {
          for (const tab of current.tabs) {
            if (tab.path === path) tab.path = null;
          }
        }
      }),
    );

  const toggleVertical = () => setState("vertical", (value) => !value);

  return {
    state,
    pane,
    activeTab,
    activePath,
    openInActive,
    openInNewTab,
    newTab,
    closeTab,
    setActive,
    cycleTab,
    navigate,
    splitRight,
    focusPane,
    cyclePane,
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
