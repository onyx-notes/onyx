// The M2 shell: vault open, file list, editor with autosave, quick
// switcher, live external-change refresh. Splits/tabs arrive in M3 with
// the workspace tree.

import { For, Show, createSignal, onCleanup, onMount } from "solid-js";

import { type NoteInfo, api } from "./api";
import Editor from "./components/Editor";
import QuickSwitcher from "./components/QuickSwitcher";
import { t } from "./i18n";

export default function App() {
  const [vaultRoot, setVaultRoot] = createSignal<string | null>(null);
  const [notes, setNotes] = createSignal<NoteInfo[]>([]);
  const [openPath, setOpenPath] = createSignal<string | null>(null);
  const [content, setContent] = createSignal("");
  const [reloadSignal, setReloadSignal] = createSignal(0);
  const [quickOpen, setQuickOpen] = createSignal(false);
  const [status, setStatus] = createSignal("");

  const report = (error: unknown) =>
    setStatus(t("error.generic", { message: String(error) }));

  const refreshNotes = async () => {
    try {
      setNotes(await api.listNotes());
    } catch (error) {
      report(error);
    }
  };

  const openNote = async (path: string) => {
    try {
      const body = await api.readNote(path);
      setOpenPath(path);
      setContent(body);
      setReloadSignal((n) => n + 1);
      setQuickOpen(false);
    } catch (error) {
      report(error);
    }
  };

  const saveNote = async (body: string) => {
    const path = openPath();
    if (path === null) return;
    try {
      setContent(body);
      await api.writeNote(path, body);
      setStatus(t("editor.saved"));
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
      setOpenPath(null);
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
      await openNote(path);
    } catch (error) {
      report(error);
    }
  };

  onMount(async () => {
    const unlisten = await api.onVaultEvent(async (event) => {
      await refreshNotes();
      // The open note changed on disk (another app, sync): reload it.
      if (event.kind === "modified" && event.path === openPath()) {
        try {
          setContent(await api.readNote(event.path));
          setReloadSignal((n) => n + 1);
        } catch {
          // Note vanished between event and read; list refresh handles it.
        }
      }
      if (event.kind === "removed" && event.path === openPath()) {
        setOpenPath(null);
      }
    });
    onCleanup(() => void unlisten());

    const onKey = (event: KeyboardEvent) => {
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "p") {
        event.preventDefault();
        if (vaultRoot() !== null) setQuickOpen(true);
      }
      if (event.key === "Escape") setQuickOpen(false);
    };
    window.addEventListener("keydown", onKey);
    onCleanup(() => window.removeEventListener("keydown", onKey));
  });

  const wordCount = () =>
    content()
      .split(/\s+/)
      .filter((token) => /[\p{L}\p{N}]/u.test(token)).length;

  return (
    <div class="app">
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
                  classList={{ active: note.path === openPath() }}
                  onClick={() => void openNote(note.path)}
                  title={note.path}
                >
                  {note.title}
                </button>
              )}
            </For>
          </Show>
        </div>
      </aside>

      <main class="main">
        <Show
          when={openPath()}
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
            />
          )}
        </Show>
        <div class="statusbar">
          <span>{openPath() ?? ""}</span>
          <Show when={openPath()}>
            <span>{t("status.words", { count: wordCount() })}</span>
          </Show>
          <span style={{ "margin-left": "auto" }}>{status()}</span>
        </div>
      </main>

      <Show when={quickOpen()}>
        <QuickSwitcher
          onPick={(path) => void openNote(path)}
          onClose={() => setQuickOpen(false)}
        />
      </Show>
    </div>
  );
}
