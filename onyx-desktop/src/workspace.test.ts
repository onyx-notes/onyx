// Workspace model: panes, tabs, history, splits. Pure logic, no DOM.

import { describe, expect, it } from "vitest";

import { createWorkspace } from "./workspace";

describe("tabs and history", () => {
  it("opens notes in the active pane with history", () => {
    const ws = createWorkspace();
    ws.openInActive("a.md");
    ws.openInActive("b.md");
    expect(ws.activePath()).toBe("b.md");
    ws.navigate(-1);
    expect(ws.activePath()).toBe("a.md");
    ws.navigate(1);
    expect(ws.activePath()).toBe("b.md");
    // Forward history truncates on a new navigation.
    ws.navigate(-1);
    ws.openInActive("c.md");
    ws.navigate(1);
    expect(ws.activePath()).toBe("c.md");
  });

  it("new tabs and cycling", () => {
    const ws = createWorkspace();
    ws.openInActive("a.md");
    ws.newTab();
    ws.openInActive("b.md");
    expect(ws.pane()!.tabs.length).toBe(2);
    ws.cycleTab(1);
    expect(ws.activePath()).toBe("a.md");
    ws.cycleTab(-1);
    expect(ws.activePath()).toBe("b.md");
  });

  it("background open does not steal focus", () => {
    const ws = createWorkspace();
    ws.openInActive("a.md");
    ws.openInNewTab("b.md", true);
    expect(ws.activePath()).toBe("a.md");
    expect(ws.pane()!.tabs.length).toBe(2);
  });

  it("last tab resets instead of closing", () => {
    const ws = createWorkspace();
    ws.openInActive("a.md");
    ws.closeTab(0);
    expect(ws.state.panes.length).toBe(1);
    expect(ws.activePath()).toBe(null);
  });
});

describe("splits", () => {
  it("splitRight moves the active tab into a new pane", () => {
    const ws = createWorkspace();
    ws.openInActive("a.md");
    ws.newTab();
    ws.openInActive("b.md");
    ws.splitRight();
    expect(ws.state.panes.length).toBe(2);
    // The moved tab (b.md) is focused in the new pane.
    expect(ws.state.activePane).toBe(1);
    expect(ws.activePath()).toBe("b.md");
    // The original pane kept a.md.
    expect(ws.state.panes[0]!.tabs.map((tab) => tab.path)).toEqual(["a.md"]);
  });

  it("single-tab split opens an empty pane, keeping the original", () => {
    const ws = createWorkspace();
    ws.openInActive("a.md");
    ws.splitRight();
    expect(ws.state.panes.length).toBe(2);
    expect(ws.state.panes[0]!.tabs[0]!.path).toBe("a.md");
    expect(ws.activePath()).toBe(null);
  });

  it("closing a pane's last tab removes the pane", () => {
    const ws = createWorkspace();
    ws.openInActive("a.md");
    ws.splitRight(); // pane 1 = empty
    expect(ws.state.panes.length).toBe(2);
    ws.closeTab(0); // closes empty pane 1
    expect(ws.state.panes.length).toBe(1);
    expect(ws.state.activePane).toBe(0);
    expect(ws.activePath()).toBe("a.md");
  });

  it("panes are independent and cyclable", () => {
    const ws = createWorkspace();
    ws.openInActive("left.md");
    ws.newTab();
    ws.openInActive("right.md");
    ws.splitRight();
    ws.focusPane(0);
    expect(ws.activePath()).toBe("left.md");
    ws.cyclePane(1);
    expect(ws.activePath()).toBe("right.md");
  });

  it("evictPath clears the path across all panes", () => {
    const ws = createWorkspace();
    ws.openInActive("shared.md");
    ws.splitRight();
    ws.openInActive("shared.md");
    ws.evictPath("shared.md");
    for (const pane of ws.state.panes) {
      for (const tab of pane.tabs) expect(tab.path).not.toBe("shared.md");
    }
  });
});
