// The M3 shell: tabs (horizontal + vertical rail), per-tab history,
// live-preview editor, wikilink following, backlinks panel, quick
// switcher, live external-change refresh.

import { For, Show, createEffect, createSignal, on, onCleanup, onMount } from "solid-js";

import { type NoteInfo, api } from "./api";
import Backlinks from "./components/Backlinks";
import Editor from "./components/Editor";
import QuickSwitcher from "./components/QuickSwitcher";
import TabBar from "./components/TabBar";
import { t } from "./i18n";
import { createWorkspace } from "./workspace";

export default function App() {
  const workspace = createWorkspace();
  const [vaultRoot, setVaultRoot] = createSignal<string | null>(null);
  const [notes, setNotes] = createSignal<NoteInfo[]>([]);
  const [content, setContent] = createSignal("");
  const [reloadSignal, setReloadSignal] = createSignal(0);
  const [quickOpen, setQuickOpen] = createSignal(false);
  const [showBacklinks, setShowBacklinks] = createSignal(false);
  const [vaultEpoch, setVaultEpoch] = createSignal(0);
  const [status, setStatus] = createSignal("");

  const activePath = workspace.activePath;

  const report = (error: unknown) =>
    setStatus(t("error.generic", { message: String(error) }));

  const refreshNotes = async () => {
    try {
      setNotes(await api.listNotes());
      setVaultEpoch((epoch) => epoch + 1);
    } catch (error) {
      report(error);
    }
  };

  // Whatever the active tab points at, that's what the editor shows.
  createEffect(
    on(activePath, async (path) => {
      if (path === null) return;
      try {
        setContent(await api.readNote(path));
        setReloadSignal((n) => n + 1);
      } catch (error) {
        report(error);
      }
    }),
  );

  const openNote = (path: string, opts?: { newTab?: boolean; background?: boolean }) => {
    if (opts?.newTab) {
      workspace.openInNewTab(path, opts.background ?? false);
    } else {
      workspace.openInActive(path);
    }
    setQuickOpen(false);
  };

  const saveNote = async (body: string) => {
    const path = activePath();
    if (path === null) return;
    try {
      setContent(body);
      await api.writeNote(path, body);
      setStatus(t("editor.saved"));
    } catch (error) {
      report(error);
    }
  };

  /** Follow a wikilink target; create the note if it doesn't exist. */
  const followLink = async (target: string) => {
    if (target.length === 0) return; // same-file heading links: M4 outline work
    try {
      const existing = await api.resolveTarget(target);
      if (existing !== null) {
        openNote(existing);
        return;
      }
      const path = `${target}.md`;
      await api.writeNote(path, "");
      await refreshNotes();
      openNote(path);
      setStatus(t("note.created", { path }));
    } catch (error) {
      report(error);
    }
  };

  const openVault = async () => {
    const path = window.prompt(t("vault.openPrompt"));
    if (!path) return;
    try {
      const info = await api.openVault(path);
      setVaultRoot(info.root);
      await refreshNotes();
      setStatus(t("vault.noteCount", { count: info.noteCount }));
    } catch (error) {
      report(error);
    }
  };

  const createNote = async () => {
    const path = window.prompt(t("sidebar.newNotePrompt"));
    if (!path) return;
    try {
      await api.writeNote(path, "");
      await refreshNotes();
      openNote(path);
    } catch (error) {
      report(error);
    }
  };

  onMount(async () => {
    const unlisten = await api.onVaultEvent(async (event) => {
      await refreshNotes();
      if (event.kind === "modified" && event.path === activePath()) {
        try {
          setContent(await api.readNote(event.path));
          setReloadSignal((n) => n + 1);
        } catch {
          // Vanished between event and read; the list refresh handles it.
        }
      }
      if (event.kind === "removed") {
        workspace.evictPath(event.path);
      }
    });
    onCleanup(() => void unlisten());

    const onKey = (event: KeyboardEvent) => {
      const mod = event.ctrlKey || event.metaKey;
      const key = event.key.toLowerCase();
      if (mod && key === "p") {
        event.preventDefault();
        if (vaultRoot() !== null) setQuickOpen(true);
      } else if (mod && key === "t") {
        event.preventDefault();
        workspace.newTab();
      } else if (mod && key === "w") {
        event.preventDefault();
        workspace.closeTab(workspace.state.active);
      } else if (mod && event.key === "Tab") {
        event.preventDefault();
        workspace.cycleTab(event.shiftKey ? -1 : 1);
      } else if (event.altKey && event.key === "ArrowLeft") {
        event.preventDefault();
        workspace.navigate(-1);
      } else if (event.altKey && event.key === "ArrowRight") {
        event.preventDefault();
        workspace.navigate(1);
      } else if (event.key === "Escape") {
        setQuickOpen(false);
      }
    };
    window.addEventListener("keydown", onKey);
    onCleanup(() => window.removeEventListener("keydown", onKey));
  });

  const wordCount = () =>
    content()
      .split(/\s+/)
      .filter((token) => /[\p{L}\p{N}]/u.test(token)).length;

  return (
    <div class="app" classList={{ "vertical-tabs": workspace.state.vertical }}>
      <aside class="sidebar">
        <div class="sidebar-header">
          <span>{t("sidebar.files")}</span>
          <Show when={vaultRoot()}>
            <button onClick={createNote} title={t("sidebar.newNote")}>
              +
            </button>
          </Show>
        </div>
        <div class="file-list">
          <Show
            when={vaultRoot()}
            fallback={
              <button class="file-item" onClick={openVault}>
                {t("vault.open")}
              </button>
            }
          >
            <For each={notes()}>
              {(note) => (
                <button
                  class="file-item"
                  classList={{ active: note.path === activePath() }}
                  onClick={(event) =>
                    openNote(note.path, {
                      newTab: event.ctrlKey || event.metaKey,
                      background: event.ctrlKey || event.metaKey,
                    })
                  }
                  title={note.path}
                >
                  {note.title}
                </button>
              )}
            </For>
          </Show>
        </div>
      </aside>

      <div class="workspace">
        <TabBar workspace={workspace} />
        <main class="main">
          <Show
            when={activePath()}
            fallback={
              <div class="empty-state">
                {vaultRoot() ? t("editor.placeholder") : t("vault.empty")}
              </div>
            }
          >
            {(path) => (
              <Editor
                path={path()}
                content={content()}
                reloadSignal={reloadSignal()}
                onChange={(body) => void saveNote(body)}
                onFollowLink={(target) => void followLink(target)}
              />
            )}
          </Show>
          <div class="statusbar">
            <span>{activePath() ?? ""}</span>
            <Show when={activePath()}>
              <span>{t("status.words", { count: wordCount() })}</span>
            </Show>
            <span style={{ "margin-left": "auto" }}>{status()}</span>
            <button onClick={() => workspace.toggleVertical()} title={t("tabs.vertical")}>
              ⊟
            </button>
            <button
              onClick={() => setShowBacklinks((value) => !value)}
              title={t("backlinks.title")}
            >
              ⇤
            </button>
          </div>
        </main>
      </div>

      <Show when={showBacklinks() && vaultRoot()}>
        <Backlinks
          path={activePath()}
          epoch={vaultEpoch()}
          onOpen={(path) => openNote(path)}
        />
      </Show>

      <Show when={quickOpen()}>
        <QuickSwitcher
          onPick={(path) => openNote(path)}
          onClose={() => setQuickOpen(false)}
        />
      </Show>
    </div>
  );
}
