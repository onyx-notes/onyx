// Settings dialog: the seed of the full settings surface. Includes the
// `.obsidian` importer with its review flow (imported keys are listed,
// nothing saves without confirmation).

import { For, Show, createResource, createSignal } from "solid-js";

import { type Settings, api } from "../api";
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
