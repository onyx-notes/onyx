// The M3 shell: tabs (horizontal + vertical rail), per-tab history,
// live-preview editor, wikilink following, backlinks panel, quick
// switcher, live external-change refresh.

import { For, Show, createEffect, createSignal, on, onCleanup, onMount } from "solid-js";

import { type NoteInfo, type Settings, api } from "./api";
import AgentPanel from "./components/AgentPanel";
import ChatPanel from "./components/ChatPanel";
import CommandPalette, { type PaletteCommand } from "./components/CommandPalette";
import { PluginHost, type PluginCommand } from "./plugins/host";
import GraphView from "./components/GraphView";
import HistoryPanel from "./components/HistoryPanel";
import Insights from "./components/Insights";
import Pane from "./components/Pane";
import QuickSwitcher from "./components/QuickSwitcher";
import RightPanel from "./components/RightPanel";
import SettingsModal from "./components/SettingsModal";
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
  const [syncState, setSyncState] = createSignal<string | null>(null);
  const [paletteOpen, setPaletteOpen] = createSignal(false);
  const [graphOpen, setGraphOpen] = createSignal(false);
  const [insightsOpen, setInsightsOpen] = createSignal(false);
  const [chatOpen, setChatOpen] = createSignal(false);
  const [readingMode, setReadingMode] = createSignal(false);
  const [agentOpen, setAgentOpen] = createSignal(false);
  const [historyOpen, setHistoryOpen] = createSignal(false);
  const [shareLink, setShareLink] = createSignal<string | null>(null);

  const shareActive = async () => {
    const path = activePath();
    if (path === null) return;
    try {
      const link = await api.createShare(path);
      setShareLink(link.url);
      try {
        await navigator.clipboard.writeText(link.url);
        setStatus(t("share.copied"));
      } catch {
        setStatus(t("share.created"));
      }
    } catch (error) {
      report(error);
    }
  };
  const [pluginCommands, setPluginCommands] = createSignal<PluginCommand[]>([]);

  const [pluginInsert, setPluginInsert] = createSignal<{ text: string; epoch: number } | null>(
    null,
  );
  const pluginHost = new PluginHost({
    onNotice: (pluginId, message) => setStatus(`[${pluginId}] ${message}`),
    onCommandsChanged: (commands) => setPluginCommands(commands),
    onEditorInsert: (text) =>
      setPluginInsert((previous) => ({ text, epoch: (previous?.epoch ?? 0) + 1 })),
    activePath: () => activePath(),
  });
  onCleanup(() => pluginHost.destroy());

  const loadPlugins = async () => {
    try {
      for (const plugin of await api.listPlugins()) {
        if (plugin.enabled) pluginHost.load(plugin);
      }
    } catch (error) {
      report(error);
    }
  };

  const paletteCommands = (): PaletteCommand[] => [
    { id: "app.daily", name: t("daily.open"), run: () => void openDailyNote() },
    { id: "app.settings", name: t("settings.title"), run: () => setShowSettings(true) },
    { id: "app.newTab", name: t("tabs.new"), run: () => workspace.newTab() },
    {
      id: "app.vertical",
      name: t("tabs.vertical"),
      run: () => workspace.toggleVertical(),
    },
    { id: "app.graph", name: t("graph.title"), run: () => setGraphOpen(true) },
    { id: "app.chat", name: t("chat.title"), run: () => setChatOpen(true) },
    { id: "app.agent", name: t("agent.title"), run: () => setAgentOpen(true) },
    {
      id: "app.history",
      name: t("history.title"),
      run: () => {
        if (activePath() !== null) setHistoryOpen(true);
      },
    },
    {
      id: "app.reading",
      name: t("reading.toggle"),
      run: () => setReadingMode((value) => !value),
    },
    { id: "app.share", name: t("share.title"), run: () => void shareActive() },
    { id: "app.insights", name: t("insights.title"), run: () => setInsightsOpen(true) },
    { id: "app.lock", name: t("vault.lock"), run: () => void lockVault() },
    ...pluginCommands().map((command) => ({
      id: `${command.pluginId}:${command.commandId}`,
      name: `${command.name}`,
      run: () => pluginHost.runCommand(command.pluginId, command.commandId),
    })),
  ];

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

  // Track the active note's content for the status-bar word count. Panes
  // own their own editor content; this is a lightweight mirror.
  createEffect(
    on(activePath, async (path) => {
      if (path === null) {
        setContent("");
        return;
      }
      try {
        setContent(await api.readNote(path));
      } catch {
        setContent("");
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

  const saveNote = async (path: string, body: string) => {
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
    await loadPlugins();
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
      workspace.closeTab(workspace.pane()?.active ?? 0);
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
      if (event.kind === "modified") {
        // Panes showing this note re-read via the reload bump.
        setReloadSignal((n) => n + 1);
        if (event.path === activePath()) {
          try {
            setContent(await api.readNote(event.path));
          } catch {
            // Vanished between event and read; list refresh handles it.
          }
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
      if (mod && event.shiftKey && key === "p") {
        event.preventDefault();
        if (vaultRoot() !== null) setPaletteOpen(true);
      } else if (mod && key === "p") {
        event.preventDefault();
        if (vaultRoot() !== null) setQuickOpen(true);
      } else if (mod && key === "r") {
        event.preventDefault();
        if (activePath() !== null) setReadingMode((value) => !value);
      } else if (mod && key === "g") {
        event.preventDefault();
        if (vaultRoot() !== null) setGraphOpen((value) => !value);
      } else if (mod && key === "t") {
        event.preventDefault();
        workspace.newTab();
      } else if (mod && key === "w") {
        event.preventDefault();
        workspace.closeTab(workspace.pane()?.active ?? 0);
      } else if (mod && key === "\\") {
        event.preventDefault();
        workspace.splitRight();
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

    const poll = setInterval(async () => {
      if (vaultRoot() === null) return;
      try {
        const info = await api.syncStatus();
        setSyncState(info.enabled ? info.state : null);
      } catch {
        setSyncState(null);
      }
    }, 5000);
    onCleanup(() => clearInterval(poll));

    // App lifecycle: pause sync when hidden (mobile background, minimized),
    // resume with a fresh connection when visible again. Complements the
    // native RunEvent handling on platforms that deliver it.
    const onVisibility = () => {
      if (vaultRoot() === null) return;
      if (document.visibilityState === "hidden") {
        void api.appPause();
      } else {
        void api.appResume();
      }
    };
    document.addEventListener("visibilitychange", onVisibility);
    onCleanup(() =>
      document.removeEventListener("visibilitychange", onVisibility),
    );
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
        <div class="panes">
          <Show
            when={vaultRoot()}
            fallback={<div class="empty-state">{t("vault.empty")}</div>}
          >
            <For each={workspace.state.panes}>
              {(_pane, index) => (
                <Pane
                  workspace={workspace}
                  paneIndex={index()}
                  reading={readingMode()}
                  externalReload={reloadSignal()}
                  scrollTarget={scrollTarget()}
                  insert={pluginInsert()}
                  onFollowLink={(target) => void followLink(target)}
                  onSave={(path, body) => void saveNote(path, body)}
                />
              )}
            </For>
          </Show>
        </div>
        <main class="main">
          <div class="statusbar">
            <span>{activePath() ?? ""}</span>
            <Show when={activePath()}>
              <span>{t("status.words", { count: wordCount() })}</span>
            </Show>
            <Show when={syncState()}>
              {(state) => (
                <span title={t("sync.statusTitle")}>
                  {state() === "error"
                    ? "⚠ sync"
                    : state() === "offline"
                      ? "⌁ offline"
                      : state() === "paused"
                        ? "⏸ sync"
                        : "⟳ " + state()}
                </span>
              )}
            </Show>
            <Show when={shareLink()}>
              {(url) => (
                <a
                  class="share-link"
                  href={url()}
                  onClick={(event) => {
                    event.preventDefault();
                    void navigator.clipboard.writeText(url());
                    setStatus(t("share.copied"));
                  }}
                  title={url()}
                >
                  🔗 {t("share.link")}
                </a>
              )}
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

      <Show when={paletteOpen()}>
        <CommandPalette
          commands={paletteCommands()}
          onClose={() => setPaletteOpen(false)}
        />
      </Show>

      <Show when={graphOpen()}>
        <GraphView onOpen={(path) => openNote(path)} onClose={() => setGraphOpen(false)} />
      </Show>

      <Show when={insightsOpen()}>
        <Insights onOpen={(path) => openNote(path)} onClose={() => setInsightsOpen(false)} />
      </Show>

      <Show when={chatOpen()}>
        <ChatPanel contextPath={activePath()} onClose={() => setChatOpen(false)} />
      </Show>

      <Show when={agentOpen()}>
        <AgentPanel
          onClose={() => setAgentOpen(false)}
          onApplied={() => void refreshNotes()}
        />
      </Show>

      <Show when={historyOpen() && activePath()}>
        {(path) => (
          <HistoryPanel
            path={path()}
            onClose={() => setHistoryOpen(false)}
            onRestored={() => setReloadSignal((n) => n + 1)}
          />
        )}
      </Show>
    </div>
  );
}
