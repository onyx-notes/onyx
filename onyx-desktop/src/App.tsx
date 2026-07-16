// The M3 shell: tabs (horizontal + vertical rail), per-tab history,
// live-preview editor, wikilink following, backlinks panel, quick
// switcher, live external-change refresh.

import { For, Show, createEffect, createSignal, on, onCleanup, onMount } from "solid-js";

import { type NoteInfo, type Settings, api } from "./api";
import Editor from "./components/Editor";
import QuickSwitcher from "./components/QuickSwitcher";
import RightPanel from "./components/RightPanel";
import SettingsModal from "./components/SettingsModal";
import TabBar from "./components/TabBar";
import { t } from "./i18n";
import { createWorkspace } from "./workspace";

/** Push settings into the DOM: theme attribute + CSS variables. */
function applySettings(settings: Settings) {
  const root = document.documentElement;
  const theme =
    settings.theme === "system"
      ? window.matchMedia("(prefers-color-scheme: light)").matches
        ? "light"
        : "dark"
      : settings.theme;
  root.dataset["theme"] = theme;
  root.style.setProperty("--onyx-editor-font-size", `${settings.baseFontSize}px`);
  document.body.classList.toggle("full-width", !settings.readableLineLength);
}

/** Local date as YYYY-MM-DD (daily notes follow the user's timezone). */
function localDate(): string {
  const now = new Date();
  const month = String(now.getMonth() + 1).padStart(2, "0");
  const day = String(now.getDate()).padStart(2, "0");
  return `${now.getFullYear()}-${month}-${day}`;
}

export default function App() {
  const workspace = createWorkspace();
  const [vaultRoot, setVaultRoot] = createSignal<string | null>(null);
  const [notes, setNotes] = createSignal<NoteInfo[]>([]);
  const [content, setContent] = createSignal("");
  const [reloadSignal, setReloadSignal] = createSignal(0);
  const [quickOpen, setQuickOpen] = createSignal(false);
  const [showBacklinks, setShowBacklinks] = createSignal(false);
  const [showSettings, setShowSettings] = createSignal(false);
  const [settings, setSettings] = createSignal<Settings | null>(null);
  const [vaultEpoch, setVaultEpoch] = createSignal(0);
  const [scrollTarget, setScrollTarget] = createSignal<{ offset: number; epoch: number } | null>(
    null,
  );
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

  const [vaultEncrypted, setVaultEncrypted] = createSignal(false);

  const finishOpen = async (info: Awaited<ReturnType<typeof api.openVault>>) => {
    setVaultRoot(info.root);
    setVaultEncrypted(info.encrypted);
    const loaded = await api.getSettings();
    setSettings(loaded);
    applySettings(loaded);
    await refreshNotes();
    setStatus(t("vault.noteCount", { count: info.noteCount }));
  };

  const openVault = async () => {
    const path = window.prompt(t("vault.openPrompt"));
    if (!path) return;
    try {
      const status = await api.vaultStatus(path);
      if (status === "encrypted") {
        const passphrase = window.prompt(t("vault.passphrasePrompt"));
        if (!passphrase) return;
        await finishOpen(await api.openVault(path, passphrase));
      } else {
        await finishOpen(await api.openVault(path));
      }
    } catch (error) {
      report(error);
    }
  };

  const createEncryptedVault = async () => {
    const path = window.prompt(t("vault.createEncryptedPrompt"));
    if (!path) return;
    const passphrase = window.prompt(t("vault.newPassphrasePrompt"));
    if (!passphrase) return;
    const confirmed = window.prompt(t("vault.confirmPassphrasePrompt"));
    if (confirmed !== passphrase) {
      setStatus(t("vault.passphraseMismatch"));
      return;
    }
    try {
      await finishOpen(await api.createEncryptedVault(path, passphrase));
    } catch (error) {
      report(error);
    }
  };

  const lockVault = async () => {
    try {
      await api.lockVault();
      setVaultRoot(null);
      setVaultEncrypted(false);
      setNotes([]);
      workspace.evictPath(activePath() ?? "");
      workspace.closeTab(workspace.state.active);
      setStatus(t("vault.locked"));
    } catch (error) {
      report(error);
    }
  };

  const saveSettings = async (updated: Settings) => {
    try {
      await api.updateSettings(updated);
      setSettings(updated);
      applySettings(updated);
      setShowSettings(false);
    } catch (error) {
      report(error);
    }
  };

  const openDailyNote = async () => {
    try {
      const path = await api.dailyNote(localDate());
      await refreshNotes();
      openNote(path);
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
      } else if (mod && event.key === ",") {
        event.preventDefault();
        if (vaultRoot() !== null) setShowSettings(true);
      } else if (mod && event.shiftKey && key === "d") {
        event.preventDefault();
        if (vaultRoot() !== null) void openDailyNote();
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
              <>
                <button class="file-item" onClick={openVault}>
                  {t("vault.open")}
                </button>
                <button class="file-item" onClick={() => void createEncryptedVault()}>
                  {t("vault.createEncrypted")}
                </button>
              </>
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
                scrollTarget={scrollTarget()}
              />
            )}
          </Show>
          <div class="statusbar">
            <span>{activePath() ?? ""}</span>
            <Show when={activePath()}>
              <span>{t("status.words", { count: wordCount() })}</span>
            </Show>
            <span style={{ "margin-left": "auto" }}>{status()}</span>
            <Show when={vaultRoot()}>
              <button onClick={() => void openDailyNote()} title={t("daily.open")}>
                ☀
              </button>
            </Show>
            <Show when={vaultEncrypted()}>
              <button onClick={() => void lockVault()} title={t("vault.lock")}>
                🔒
              </button>
            </Show>
            <button onClick={() => workspace.toggleVertical()} title={t("tabs.vertical")}>
              ⊟
            </button>
            <button
              onClick={() => setShowBacklinks((value) => !value)}
              title={t("backlinks.title")}
            >
              ⇤
            </button>
            <Show when={vaultRoot()}>
              <button onClick={() => setShowSettings(true)} title={t("settings.title")}>
                ⚙
              </button>
            </Show>
          </div>
        </main>
      </div>

      <Show when={showSettings() && settings()}>
        {(current) => (
          <SettingsModal
            settings={current()}
            onSave={(updated) => void saveSettings(updated)}
            onClose={() => setShowSettings(false)}
          />
        )}
      </Show>

      <Show when={showBacklinks() && vaultRoot()}>
        <RightPanel
          path={activePath()}
          epoch={vaultEpoch()}
          onOpen={(path) => openNote(path)}
          onJump={(offset) =>
            setScrollTarget((previous) => ({
              offset,
              epoch: (previous?.epoch ?? 0) + 1,
            }))
          }
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
