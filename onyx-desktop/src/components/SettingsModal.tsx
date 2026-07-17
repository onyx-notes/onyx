// Settings dialog: the seed of the full settings surface. Includes the
// `.obsidian` importer with its review flow (imported keys are listed,
// nothing saves without confirmation).

import { For, Show, createResource, createSignal } from "solid-js";

import { type AiConfig, type Settings, api } from "../api";
import { t } from "../i18n";

export default function SettingsModal(props: {
  settings: Settings;
  onSave: (settings: Settings) => void;
  onClose: () => void;
}) {
  const [draft, setDraft] = createSignal<Settings>({ ...props.settings });
  const [importedKeys, setImportedKeys] = createSignal<string[] | null>(null);
  const [syncUrl, setSyncUrl] = createSignal("");
  const [syncCodeInput, setSyncCodeInput] = createSignal("");
  const [syncResult, setSyncResult] = createSignal<string | null>(null);
  const [backupResult, setBackupResult] = createSignal<string | null>(null);
  const [backupConfig] = createResource(() => api.getBackupConfig());
  const [plugins, { refetch: refetchPlugins }] = createResource(() => api.listPlugins());
  const [aiDraft, setAiDraft] = createSignal<AiConfig | null>(null);
  void api.getAiConfig().then(setAiDraft);
  const [aiSaved, setAiSaved] = createSignal(false);
  const [keychain, setKeychain] = createSignal(false);
  void api.keychainAvailable().then(setKeychain);
  const updateAi = <K extends keyof AiConfig>(key: K, value: AiConfig[K]) =>
    setAiDraft((current) => (current ? { ...current, [key]: value } : current));
  const saveAi = async () => {
    const draft = aiDraft();
    if (!draft) return;
    await api.setAiConfig(draft);
    setAiSaved(true);
  };
  const [ragResult, setRagResult] = createSignal<string | null>(null);
  const reindexRag = async () => {
    setRagResult(t("rag.indexing"));
    try {
      const status = await api.ragReindex();
      setRagResult(t("rag.done", { chunks: status.indexedChunks }));
    } catch (error) {
      setRagResult(String(error));
    }
  };

  const togglePlugin = async (id: string, enabled: boolean) => {
    await api.setPluginEnabled(id, enabled);
    await refetchPlugins();
  };
  const [registry, setRegistry] = createSignal<Awaited<
    ReturnType<typeof api.pluginRegistry>
  > | null>(null);
  const [registryUrl, setRegistryUrl] = createSignal(
    "https://raw.githubusercontent.com/onyx-notes/plugins/main/registry.json",
  );
  const [pluginMsg, setPluginMsg] = createSignal<string | null>(null);
  const browse = async () => {
    setPluginMsg(null);
    try {
      setRegistry(await api.pluginRegistry(registryUrl().trim()));
    } catch (error) {
      setPluginMsg(String(error));
    }
  };
  const install = async (source: string) => {
    try {
      const info = await api.installPlugin(source);
      setPluginMsg(t("plugins.installed") + ` (${info.name})`);
      await refetchPlugins();
    } catch (error) {
      setPluginMsg(String(error));
    }
  };
  const uninstall = async (id: string) => {
    await api.uninstallPlugin(id);
    await refetchPlugins();
  };

  const runBackup = async (name: string) => {
    setBackupResult(t("backup.running"));
    try {
      const report = await api.backupNow(name);
      setBackupResult(
        t("backup.done", {
          uploaded: report.uploaded,
          skipped: report.skipped,
        }),
      );
    } catch (error) {
      setBackupResult(String(error));
    }
  };

  const enableSync = async () => {
    try {
      const result = await api.syncEnable(syncUrl().trim());
      setSyncResult(
        result.code === null
          ? t("sync.enabledEncrypted")
          : t("sync.enabledCode", { code: result.code }),
      );
    } catch (error) {
      setSyncResult(String(error));
    }
  };

  const joinSync = async () => {
    try {
      await api.syncJoin(syncUrl().trim(), syncCodeInput().trim());
      setSyncResult(t("sync.joined"));
    } catch (error) {
      setSyncResult(String(error));
    }
  };

  // Device pairing (enrollment): approve side + receive side.
  const [pairResult, setPairResult] = createSignal<string | null>(null);
  const [pairCodeInput, setPairCodeInput] = createSignal("");

  const approveDevice = async () => {
    try {
      const result = await api.enrollApproveDevice(pairCodeInput().trim());
      setPairResult(t("pair.approveSas", { sas: result.sas }));
    } catch (error) {
      setPairResult(String(error));
    }
  };

  const receivePairing = async () => {
    try {
      const started = await api.enrollStart(syncUrl().trim());
      setPairResult(t("pair.showCode", { code: started.code }));
      const waited = await api.enrollWait();
      setPairResult(t("pair.confirmSas", { sas: waited.sas }));
    } catch (error) {
      setPairResult(String(error));
      await api.enrollCancel();
    }
  };

  const confirmPairing = async () => {
    try {
      await api.enrollConfirm();
      setPairResult(t("pair.done"));
    } catch (error) {
      setPairResult(String(error));
    }
  };

  const cancelPairing = async () => {
    await api.enrollCancel();
    setPairResult(null);
  };

  const update = <K extends keyof Settings>(key: K, value: Settings[K]) =>
    setDraft((current) => ({ ...current, [key]: value }));

  const runImport = async () => {
    const result = await api.importObsidianSettings();
    setDraft(result.settings);
    setImportedKeys(result.imported);
  };

  return (
    <div class="overlay" onClick={() => props.onClose()}>
      <div class="palette settings" onClick={(event) => event.stopPropagation()}>
        <div class="settings-header">{t("settings.title")}</div>

        <div class="settings-body">
          <label class="settings-row">
            <span>{t("settings.readableLineLength")}</span>
            <input
              type="checkbox"
              checked={draft().readableLineLength}
              onChange={(event) =>
                update("readableLineLength", event.currentTarget.checked)
              }
            />
          </label>

          <label class="settings-row">
            <span>{t("settings.baseFontSize")}</span>
            <input
              type="number"
              min="8"
              max="40"
              value={draft().baseFontSize}
              onChange={(event) =>
                update("baseFontSize", Number(event.currentTarget.value) || 15)
              }
            />
          </label>

          <label class="settings-row">
            <span>{t("settings.theme")}</span>
            <select
              value={draft().theme}
              onChange={(event) =>
                update("theme", event.currentTarget.value as Settings["theme"])
              }
            >
              <option value="dark">{t("settings.theme.dark")}</option>
              <option value="light">{t("settings.theme.light")}</option>
              <option value="system">{t("settings.theme.system")}</option>
            </select>
          </label>

          <label class="settings-row">
            <span>{t("settings.newFileFolder")}</span>
            <input
              type="text"
              value={draft().newFileFolder}
              onChange={(event) => update("newFileFolder", event.currentTarget.value)}
            />
          </label>

          <label class="settings-row">
            <span>{t("settings.dailyNoteFolder")}</span>
            <input
              type="text"
              value={draft().dailyNoteFolder}
              onChange={(event) =>
                update("dailyNoteFolder", event.currentTarget.value)
              }
            />
          </label>

          <div class="settings-import">
            <div class="settings-row">
              <span>{t("sync.serverUrl")}</span>
              <input
                type="text"
                placeholder="https://sync.example.com"
                value={syncUrl()}
                onInput={(event) => setSyncUrl(event.currentTarget.value)}
              />
            </div>
            <div class="settings-row">
              <span>{t("sync.code")}</span>
              <input
                type="text"
                value={syncCodeInput()}
                onInput={(event) => setSyncCodeInput(event.currentTarget.value)}
              />
            </div>
            <div class="settings-row">
              <button class="settings-button" onClick={() => void enableSync()}>
                {t("sync.enable")}
              </button>
              <button class="settings-button" onClick={() => void joinSync()}>
                {t("sync.join")}
              </button>
            </div>
            <Show when={syncResult()}>
              {(message) => <div class="settings-imported">{message()}</div>}
            </Show>

            <div class="settings-row">
              <input
                type="text"
                placeholder={t("pair.codePlaceholder")}
                value={pairCodeInput()}
                onInput={(event) => setPairCodeInput(event.currentTarget.value)}
              />
              <button class="settings-button" onClick={() => void approveDevice()}>
                {t("pair.approve")}
              </button>
            </div>
            <div class="settings-row">
              <button class="settings-button" onClick={() => void receivePairing()}>
                {t("pair.receive")}
              </button>
              <span>
                <button class="settings-button" onClick={() => void confirmPairing()}>
                  {t("pair.sasMatches")}
                </button>{" "}
                <button class="settings-button" onClick={() => void cancelPairing()}>
                  {t("settings.cancel")}
                </button>
              </span>
            </div>
            <Show when={pairResult()}>
              {(message) => <div class="settings-imported">{message()}</div>}
            </Show>
          </div>

          <div class="settings-import">
            <Show when={aiDraft()}>
              {(ai) => (
                <>
                  <div class="settings-row">
                    <span>{t("ai.provider")}</span>
                    <select
                      value={ai().provider}
                      onChange={(event) => updateAi("provider", event.currentTarget.value)}
                    >
                      <option value="openai">{t("ai.provider.openai")}</option>
                      <option value="anthropic">Anthropic</option>
                    </select>
                  </div>
                  <div class="settings-row">
                    <span>{t("ai.baseUrl")}</span>
                    <input
                      type="text"
                      placeholder="https://api.openai.com/v1"
                      value={ai().baseUrl}
                      onInput={(event) => updateAi("baseUrl", event.currentTarget.value)}
                    />
                  </div>
                  <div class="settings-row">
                    <span>{t("ai.apiKey")}</span>
                    <input
                      type="password"
                      value={ai().apiKey}
                      onInput={(event) => updateAi("apiKey", event.currentTarget.value)}
                    />
                  </div>
                  <div class="settings-imported settings-caps">
                    {keychain() ? t("ai.keychainOn") : t("ai.keychainOff")}
                  </div>
                  <div class="settings-row">
                    <span>{t("ai.model")}</span>
                    <input
                      type="text"
                      value={ai().model}
                      onInput={(event) => updateAi("model", event.currentTarget.value)}
                    />
                  </div>
                  <div class="settings-row">
                    <span>{t("ai.embedModel")}</span>
                    <input
                      type="text"
                      placeholder="text-embedding-3-small / nomic-embed-text"
                      value={ai().embedModel}
                      onInput={(event) => updateAi("embedModel", event.currentTarget.value)}
                    />
                  </div>
                  <div class="settings-row">
                    <span class="settings-caps">{t("rag.hint")}</span>
                    <button class="settings-button" onClick={() => void reindexRag()}>
                      {t("rag.reindex")}
                    </button>
                  </div>
                  <Show when={ragResult()}>
                    {(message) => <div class="settings-imported">{message()}</div>}
                  </Show>
                  <div class="settings-row">
                    <span class="settings-caps">{t("ai.storageNote")}</span>
                    <button class="settings-button" onClick={() => void saveAi()}>
                      {aiSaved() ? t("ai.saved") : t("ai.save")}
                    </button>
                  </div>
                </>
              )}
            </Show>
          </div>

          <div class="settings-import">
            <Show
              when={(plugins()?.length ?? 0) > 0}
              fallback={<div class="settings-imported">{t("plugins.none")}</div>}
            >
              <For each={plugins() ?? []}>
                {(plugin) => (
                  <div class="settings-row">
                    <span title={plugin.capabilities.join(", ")}>
                      {plugin.name}{" "}
                      <span class="settings-caps">
                        [{plugin.capabilities.join(", ")}]
                      </span>
                    </span>
                    <span>
                      <input
                        type="checkbox"
                        checked={plugin.enabled}
                        onChange={(event) =>
                          void togglePlugin(plugin.id, event.currentTarget.checked)
                        }
                      />
                      <button
                        class="settings-button"
                        onClick={() => void uninstall(plugin.id)}
                      >
                        {t("plugins.uninstall")}
                      </button>
                    </span>
                  </div>
                )}
              </For>
            </Show>
            <div class="settings-imported">{t("plugins.restartHint")}</div>

            <div class="settings-row">
              <input
                type="text"
                value={registryUrl()}
                onInput={(event) => setRegistryUrl(event.currentTarget.value)}
              />
              <button class="settings-button" onClick={() => void browse()}>
                {t("plugins.browse")}
              </button>
            </div>
            <Show when={registry()}>
              {(entries) => (
                <For each={entries()}>
                  {(entry) => (
                    <div class="settings-row">
                      <span title={entry.capabilities.join(", ")}>
                        {entry.name}{" "}
                        <span class="settings-caps">[{entry.capabilities.join(", ")}]</span>
                      </span>
                      <button
                        class="settings-button"
                        onClick={() => void install(entry.source)}
                      >
                        {t("plugins.install")}
                      </button>
                    </div>
                  )}
                </For>
              )}
            </Show>
            <Show when={pluginMsg()}>
              {(message) => <div class="settings-imported">{message()}</div>}
            </Show>
          </div>

          <div class="settings-import">
            <Show
              when={(backupConfig()?.destinations.length ?? 0) > 0}
              fallback={<div class="settings-imported">{t("backup.none")}</div>}
            >
              <For each={backupConfig()?.destinations ?? []}>
                {(destination) => (
                  <div class="settings-row">
                    <span>
                      {destination.name} ({destination.kind})
                    </span>
                    <button
                      class="settings-button"
                      onClick={() => void runBackup(destination.name)}
                    >
                      {t("backup.now")}
                    </button>
                  </div>
                )}
              </For>
            </Show>
            <Show when={backupResult()}>
              {(message) => <div class="settings-imported">{message()}</div>}
            </Show>
          </div>

          <div class="settings-import">
            <button class="settings-button" onClick={() => void runImport()}>
              {t("settings.importObsidian")}
            </button>
            <Show when={importedKeys()}>
              {(keys) => (
                <div class="settings-imported">
                  <Show
                    when={keys().length > 0}
                    fallback={<span>{t("settings.importNothing")}</span>}
                  >
                    <span>{t("settings.importFound", { count: keys().length })}</span>
                    <ul>
                      <For each={keys()}>{(key) => <li>{key}</li>}</For>
                    </ul>
                  </Show>
                </div>
              )}
            </Show>
          </div>
        </div>

        <div class="settings-footer">
          <button class="settings-button" onClick={() => props.onClose()}>
            {t("settings.cancel")}
          </button>
          <button
            class="settings-button primary"
            onClick={() => props.onSave(draft())}
          >
            {t("settings.save")}
          </button>
        </div>
      </div>
    </div>
  );
}
