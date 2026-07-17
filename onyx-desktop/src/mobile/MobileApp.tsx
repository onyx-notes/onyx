// The mobile shell: single-document navigation, drawer + bottom bar +
// sheets. Shares the editor, search, settings, and AI components with the
// desktop shell — only the chrome differs (see the mobile plan §4).

import { For, Show, createEffect, createSignal, on, onCleanup, onMount } from "solid-js";

import { type ManagedVault, type NoteInfo, type Settings, api } from "../api";
import ChatPanel from "../components/ChatPanel";
import Editor, { type EditorControls } from "../components/Editor";
import QuickSwitcher from "../components/QuickSwitcher";
import RightPanel from "../components/RightPanel";
import SettingsModal from "../components/SettingsModal";
import { t } from "../i18n";
import MobileToolbar from "./MobileToolbar";

/** Push settings into the DOM (same contract as the desktop shell). */
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
}

function localDate(): string {
  const now = new Date();
  const month = String(now.getMonth() + 1).padStart(2, "0");
  const day = String(now.getDate()).padStart(2, "0");
  return `${now.getFullYear()}-${month}-${day}`;
}

export default function MobileApp() {
  // Vault state
  const [vaults, setVaults] = createSignal<ManagedVault[]>([]);
  const [vaultOpen, setVaultOpen] = createSignal(false);
  const [vaultName, setVaultName] = createSignal("");

  // Document state (single-note navigation with a history stack)
  const [notes, setNotes] = createSignal<NoteInfo[]>([]);
  const [navStack, setNavStack] = createSignal<string[]>([]);
  const [content, setContent] = createSignal("");
  const [reloadSignal, setReloadSignal] = createSignal(0);
  const [vaultEpoch, setVaultEpoch] = createSignal(0);

  // UI state
  const [drawerOpen, setDrawerOpen] = createSignal(false);
  const [sheetOpen, setSheetOpen] = createSignal(false);
  const [quickOpen, setQuickOpen] = createSignal(false);
  const [chatOpen, setChatOpen] = createSignal(false);
  const [settingsOpen, setSettingsOpen] = createSignal(false);
  const [settings, setSettings] = createSignal<Settings | null>(null);
  const [syncState, setSyncState] = createSignal<string | null>(null);
  const [status, setStatus] = createSignal("");
  const [editorControls, setEditorControls] = createSignal<EditorControls | null>(null);
  const [keyboardOpen, setKeyboardOpen] = createSignal(false);

  const activePath = () => navStack().at(-1) ?? null;
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

  const openNote = (path: string) => {
    setNavStack((stack) => (stack.at(-1) === path ? stack : [...stack, path]));
    setDrawerOpen(false);
    setQuickOpen(false);
  };

  const goBack = () => {
    if (sheetOpen()) return setSheetOpen(false);
    if (drawerOpen()) return setDrawerOpen(false);
    setNavStack((stack) => (stack.length > 1 ? stack.slice(0, -1) : stack));
  };

  const saveNote = async (body: string) => {
    const path = activePath();
    if (path === null) return;
    try {
      setContent(body);
      await api.writeNote(path, body);
    } catch (error) {
      report(error);
    }
  };

  const followLink = async (target: string) => {
    if (target.length === 0) return;
    try {
      const existing = await api.resolveTarget(target);
      if (existing !== null) return openNote(existing);
      const path = `${target}.md`;
      await api.writeNote(path, "");
      await refreshNotes();
      openNote(path);
    } catch (error) {
      report(error);
    }
  };

  const newNote = async () => {
    const stamp = new Date();
    const name = `Capture ${localDate()} ${String(stamp.getHours()).padStart(2, "0")}${String(
      stamp.getMinutes(),
    ).padStart(2, "0")}${String(stamp.getSeconds()).padStart(2, "0")}`;
    try {
      const path = `${name}.md`;
      await api.writeNote(path, "");
      await refreshNotes();
      openNote(path);
    } catch (error) {
      report(error);
    }
  };

  const openDaily = async () => {
    try {
      const path = await api.dailyNote(localDate());
      await refreshNotes();
      openNote(path);
    } catch (error) {
      report(error);
    }
  };

  const enterVault = async (vault: ManagedVault) => {
    try {
      let passphrase: string | undefined;
      if (vault.encrypted) {
        passphrase = window.prompt(t("vault.passphrasePrompt")) ?? undefined;
        if (!passphrase) return;
      }
      await api.openVault(vault.path, passphrase);
      const loaded = await api.getSettings();
      setSettings(loaded);
      applySettings(loaded);
      await refreshNotes();
      setVaultName(vault.name);
      setVaultOpen(true);
      void openDaily();
    } catch (error) {
      report(error);
    }
  };

  const createVault = async () => {
    const name = window.prompt(t("mobile.vaultNamePrompt"));
    if (!name) return;
    const encrypt = window.confirm(t("mobile.vaultEncryptPrompt"));
    let passphrase: string | undefined;
    if (encrypt) {
      passphrase = window.prompt(t("vault.newPassphrasePrompt")) ?? undefined;
      if (!passphrase) return;
    }
    try {
      await api.createManagedVault(name, passphrase ?? null);
      const loaded = await api.getSettings();
      setSettings(loaded);
      applySettings(loaded);
      await refreshNotes();
      setVaultName(name);
      setVaultOpen(true);
    } catch (error) {
      report(error);
    }
  };

  const closeVault = async () => {
    await api.lockVault();
    setVaultOpen(false);
    setNavStack([]);
    setNotes([]);
    setDrawerOpen(false);
    setVaults(await api.listManagedVaults().catch(() => []));
  };

  onMount(async () => {
    document.body.classList.add("mobile");
    onCleanup(() => document.body.classList.remove("mobile"));

    setVaults(await api.listManagedVaults().catch(() => []));

    const unlisten = await api.onVaultEvent(async (event) => {
      await refreshNotes();
      if (event.kind === "modified" && event.path === activePath()) {
        try {
          setContent(await api.readNote(event.path));
          setReloadSignal((n) => n + 1);
        } catch {
          /* handled by refresh */
        }
      }
    });
    onCleanup(() => void unlisten());

    // Sync lifecycle: pause in background, resume fresh in foreground.
    const onVisibility = () => {
      if (!vaultOpen()) return;
      if (document.visibilityState === "hidden") void api.appPause();
      else void api.appResume();
    };
    document.addEventListener("visibilitychange", onVisibility);
    onCleanup(() => document.removeEventListener("visibilitychange", onVisibility));

    const poll = setInterval(async () => {
      if (!vaultOpen()) return;
      try {
        const info = await api.syncStatus();
        setSyncState(info.enabled ? info.state : null);
      } catch {
        setSyncState(null);
      }
    }, 5000);
    onCleanup(() => clearInterval(poll));

    // Android back gesture arrives as history popstate in the webview.
    history.pushState(null, "");
    const onPop = () => {
      history.pushState(null, "");
      goBack();
    };
    window.addEventListener("popstate", onPop);
    onCleanup(() => window.removeEventListener("popstate", onPop));

    // Keyboard-aware layout: size the app to the visual viewport so the
    // bottom bar and editor stay visible above the keyboard.
    const viewport = window.visualViewport;
    if (viewport) {
      const applyHeight = () => {
        document.documentElement.style.setProperty(
          "--onyx-viewport-height",
          `${viewport.height}px`,
        );
        // A large height loss means the on-screen keyboard is up — swap the
        // bottom bar for the formatting toolbar.
        setKeyboardOpen(window.innerHeight - viewport.height > 120);
      };
      applyHeight();
      viewport.addEventListener("resize", applyHeight);
      onCleanup(() => viewport.removeEventListener("resize", applyHeight));
    }
  });

  const syncDot = () => {
    switch (syncState()) {
      case "idle":
        return "●";
      case "syncing":
        return "◐";
      case "offline":
        return "○";
      case "paused":
        return "◌";
      case "error":
        return "⚠";
      default:
        return "";
    }
  };

  return (
    <Show when={vaultOpen()} fallback={<VaultManager vaults={vaults()} onOpen={enterVault} onCreate={createVault} status={status()} />}>
      <div class="mobile-app">
        <header class="mobile-topbar">
          <button class="mobile-icon" onClick={() => setDrawerOpen(true)}>
            ☰
          </button>
          <span class="mobile-title">
            {activePath()?.split("/").at(-1)?.replace(/\.(md|markdown)$/i, "") ?? vaultName()}
          </span>
          <span class="mobile-sync" title={syncState() ?? ""}>
            {syncDot()}
          </span>
          <button class="mobile-icon" onClick={() => setSheetOpen(true)}>
            ⋯
          </button>
        </header>

        <main class="mobile-editor">
          <Show
            when={activePath()}
            fallback={<div class="empty-state">{t("editor.placeholder")}</div>}
          >
            {(path) => (
              <Editor
                path={path()}
                content={content()}
                reloadSignal={reloadSignal()}
                onChange={(body) => void saveNote(body)}
                onFollowLink={(target) => void followLink(target)}
                scrollTarget={null}
                insert={null}
                mobile
                onReady={setEditorControls}
              />
            )}
          </Show>
        </main>

        <Show when={keyboardOpen() && activePath() !== null ? editorControls() : null}>
          {(controls) => <MobileToolbar controls={controls()} />}
        </Show>

        <nav class="mobile-bottombar" classList={{ hidden: keyboardOpen() }}>
          <button class="mobile-icon" onClick={() => setQuickOpen(true)}>
            ⌕
          </button>
          <button class="mobile-icon" onClick={() => void openDaily()}>
            ☀
          </button>
          <button class="mobile-new" onClick={() => void newNote()}>
            +
          </button>
          <button class="mobile-icon" onClick={() => setChatOpen(true)}>
            ✦
          </button>
          <button class="mobile-icon" onClick={() => setSettingsOpen(true)}>
            ⚙
          </button>
        </nav>

        <Show when={drawerOpen()}>
          <div class="mobile-scrim" onClick={() => setDrawerOpen(false)}>
            <aside class="mobile-drawer" onClick={(event) => event.stopPropagation()}>
              <div class="sidebar-header">
                <span>{vaultName()}</span>
                <button onClick={() => void closeVault()}>{t("mobile.switchVault")}</button>
              </div>
              <div class="file-list">
                <For each={notes()}>
                  {(note) => (
                    <button
                      class="file-item mobile-file"
                      classList={{ active: note.path === activePath() }}
                      onClick={() => openNote(note.path)}
                    >
                      {note.title}
                    </button>
                  )}
                </For>
              </div>
            </aside>
          </div>
        </Show>

        <Show when={sheetOpen() && activePath()}>
          <div class="mobile-scrim" onClick={() => setSheetOpen(false)}>
            <div class="mobile-sheet" onClick={(event) => event.stopPropagation()}>
              <div class="mobile-sheet-grip" />
              <RightPanel
                path={activePath()}
                epoch={vaultEpoch()}
                onOpen={(path) => {
                  setSheetOpen(false);
                  openNote(path);
                }}
                onJump={() => setSheetOpen(false)}
              />
            </div>
          </div>
        </Show>

        <Show when={quickOpen()}>
          <QuickSwitcher onPick={openNote} onClose={() => setQuickOpen(false)} />
        </Show>

        <Show when={chatOpen()}>
          <ChatPanel contextPath={activePath()} onClose={() => setChatOpen(false)} />
        </Show>

        <Show when={settingsOpen() && settings()}>
          {(current) => (
            <SettingsModal
              settings={current()}
              onSave={(updated) => {
                void api.updateSettings(updated).then(() => {
                  setSettings(updated);
                  applySettings(updated);
                  setSettingsOpen(false);
                });
              }}
              onClose={() => setSettingsOpen(false)}
            />
          )}
        </Show>

        <Show when={status()}>
          <div class="mobile-status">{status()}</div>
        </Show>
      </div>
    </Show>
  );
}

function VaultManager(props: {
  vaults: ManagedVault[];
  onOpen: (vault: ManagedVault) => void;
  onCreate: () => void;
  status: string;
}) {
  return (
    <div class="mobile-app mobile-vaults">
      <h1>{t("app.title")}</h1>
      <p class="mobile-tagline">{t("mobile.tagline")}</p>
      <div class="mobile-vault-list">
        <For each={props.vaults}>
          {(vault) => (
            <button class="mobile-vault" onClick={() => props.onOpen(vault)}>
              <b>{vault.name}</b>
              <span>{vault.encrypted ? t("mobile.encrypted") : t("mobile.plain")}</span>
            </button>
          )}
        </For>
      </div>
      <button class="settings-button primary mobile-create" onClick={() => props.onCreate()}>
        {t("mobile.createVault")}
      </button>
      <Show when={props.status}>
        <div class="mobile-status">{props.status}</div>
      </Show>
    </div>
  );
}
