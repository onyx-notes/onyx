// Typed wrappers over the Tauri IPC surface. Everything the UI knows about
// the backend lives here.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export interface VaultInfo {
  root: string;
  noteCount: number;
  encrypted: boolean;
}

export interface NoteInfo {
  path: string;
  title: string;
  isMarkdown: boolean;
  wordCount: number | null;
}

export interface Hit {
  path: string;
  score: number;
}

export interface TagInfo {
  tag: string;
  count: number;
}

export interface HeadingInfo {
  level: number;
  text: string;
  offset: number;
}

export type VaultEvent =
  | { kind: "created"; path: string }
  | { kind: "modified"; path: string }
  | { kind: "removed"; path: string }
  | { kind: "bulk" };

export interface Settings {
  readableLineLength: boolean;
  baseFontSize: number;
  theme: "dark" | "light" | "system";
  useMarkdownLinks: boolean;
  newFileFolder: string;
  attachmentFolder: string;
  dailyNoteFolder: string;
}

export interface ObsidianImport {
  settings: Settings;
  imported: string[];
}

export interface SyncStatus {
  enabled: boolean;
  state: string;
  lastError: string | null;
  lastSyncedEpochSecs: number | null;
}

export interface SyncEnabled {
  code: string | null;
}

export interface BackupDestination {
  name: string;
  kind: string;
  config: Record<string, string>;
}

export interface BackupConfig {
  destinations: BackupDestination[];
  autoIntervalHours: number;
}

export interface AiConfig {
  provider: string;
  baseUrl: string;
  apiKey: string;
  model: string;
  embedModel: string;
}

export interface RagStatus {
  configured: boolean;
  indexedChunks: number;
}

export interface PublishReport {
  pages: number;
  attachments: number;
  outputDir: string;
}

export interface ShareLink {
  id: string;
  url: string;
}

export interface ShareEntry {
  id: string;
  path: string;
}

export interface QueryOutput {
  columns: string[];
  rows: string[][];
  error: string | null;
}

export interface NoteVersion {
  createdMs: number;
  hash: string;
}

export type Proposal =
  | { kind: "write"; path: string; content: string }
  | { kind: "delete"; path: string };

export interface Changeset {
  proposals: Proposal[];
  log: string[];
  finished: string | null;
}

export interface ChatMessage {
  role: "user" | "assistant";
  content: string;
}

export interface AiLogEntry {
  atEpochSecs: number;
  endpoint: string;
  model: string;
  requestBody: string;
  responseChars: number;
}

export interface VaultStats {
  noteCount: number;
  attachmentCount: number;
  totalWords: number;
  linkCount: number;
  orphanCount: number;
  unresolvedCount: number;
  mostLinked: { path: string; count: number }[];
  topTags: { tag: string; count: number }[];
  longestNotes: { path: string; count: number }[];
}

export interface GraphPayload {
  nodes: { path: string; title: string; degree: number }[];
  edges: [number, number][];
}

export interface PluginInfo {
  id: string;
  name: string;
  version: string;
  description: string;
  capabilities: string[];
  enabled: boolean;
}

export interface RegistryEntry {
  id: string;
  name: string;
  description: string;
  author: string;
  capabilities: string[];
  source: string;
}

export interface BackupReport {
  snapshotId: number;
  files: number;
  uploaded: number;
  skipped: number;
  bytesUploaded: number;
}

export const api = {
  vaultStatus: (path: string) => invoke<string>("vault_status", { path }),
  openVault: (path: string, passphrase?: string) =>
    invoke<VaultInfo>("open_vault", { path, passphrase: passphrase ?? null }),
  createEncryptedVault: (path: string, passphrase: string) =>
    invoke<VaultInfo>("create_encrypted_vault", { path, passphrase }),
  lockVault: () => invoke<void>("lock_vault"),
  listNotes: () => invoke<NoteInfo[]>("list_notes"),
  readNote: (path: string) => invoke<string>("read_note", { path }),
  writeNote: (path: string, content: string) =>
    invoke<void>("write_note", { path, content }),
  deleteNote: (path: string) => invoke<void>("delete_note", { path }),
  renameNote: (from: string, to: string) =>
    invoke<void>("rename_note", { from, to }),
  searchNotes: (query: string) => invoke<Hit[]>("search_notes", { query }),
  quickOpen: (query: string) => invoke<Hit[]>("quick_open", { query }),
  backlinks: (path: string) => invoke<string[]>("backlinks", { path }),
  resolveTarget: (target: string) =>
    invoke<string | null>("resolve_target", { target }),
  vaultTags: () => invoke<TagInfo[]>("vault_tags"),
  renderNote: (path: string) => invoke<string>("render_note", { path }),
  noteHeadings: (path: string) =>
    invoke<HeadingInfo[]>("note_headings", { path }),
  getSettings: () => invoke<Settings>("get_settings"),
  updateSettings: (settings: Settings) =>
    invoke<void>("update_settings", { settings }),
  importObsidianSettings: () =>
    invoke<ObsidianImport>("import_obsidian_settings"),
  dailyNote: (date: string) => invoke<string>("daily_note", { date }),
  syncEnable: (serverUrl: string) =>
    invoke<SyncEnabled>("sync_enable", { serverUrl }),
  syncJoin: (serverUrl: string, code: string) =>
    invoke<void>("sync_join", { serverUrl, code }),
  syncStatus: () => invoke<SyncStatus>("sync_status"),
  enrollStart: (serverUrl: string) =>
    invoke<{ code: string }>("enroll_start", { serverUrl }),
  enrollWait: () => invoke<{ sas: string }>("enroll_wait"),
  enrollConfirm: () => invoke<void>("enroll_confirm"),
  enrollCancel: () => invoke<void>("enroll_cancel"),
  enrollApproveDevice: (code: string) =>
    invoke<{ sas: string }>("enroll_approve_device", { code }),
  getBackupConfig: () => invoke<BackupConfig>("get_backup_config"),
  setBackupConfig: (config: BackupConfig) =>
    invoke<void>("set_backup_config", { config }),
  backupNow: (destination: string) =>
    invoke<BackupReport>("backup_now", { destination }),
  listBackupSnapshots: (destination: string) =>
    invoke<number[]>("list_backup_snapshots", { destination }),
  listPlugins: () => invoke<PluginInfo[]>("list_plugins"),
  vaultStats: () => invoke<VaultStats>("vault_stats"),
  graphPayload: () => invoke<GraphPayload>("graph_payload"),
  keychainAvailable: () => invoke<boolean>("keychain_available"),
  getAiConfig: () => invoke<AiConfig>("get_ai_config"),
  setAiConfig: (config: AiConfig) => invoke<void>("set_ai_config", { config }),
  aiChat: (messages: ChatMessage[], contextPath: string | null) =>
    invoke<string>("ai_chat", { messages, contextPath }),
  aiRequestLog: () => invoke<AiLogEntry[]>("ai_request_log"),
  ragReindex: () => invoke<RagStatus>("rag_reindex"),
  ragStatus: () => invoke<RagStatus>("rag_status"),
  agentRun: (goal: string) => invoke<Changeset>("agent_run", { goal }),
  agentApply: (proposals: Proposal[]) =>
    invoke<number>("agent_apply", { proposals }),
  runQueryBlock: (source: string) =>
    invoke<QueryOutput>("run_query_block", { source }),
  createShare: (path: string) => invoke<ShareLink>("create_share", { path }),
  publishSite: (folder: string, outputDir: string, siteTitle: string) =>
    invoke<PublishReport>("publish_site", { folder, outputDir, siteTitle }),
  listShares: () => invoke<ShareEntry[]>("list_shares"),
  revokeShare: (id: string) => invoke<void>("revoke_share", { id }),
  noteHistory: (path: string) => invoke<NoteVersion[]>("note_history", { path }),
  noteVersionContent: (path: string, createdMs: number) =>
    invoke<string>("note_version_content", { path, createdMs }),
  restoreNoteVersion: (path: string, createdMs: number) =>
    invoke<void>("restore_note_version", { path, createdMs }),
  setPluginEnabled: (id: string, enabled: boolean) =>
    invoke<void>("set_plugin_enabled", { id, enabled }),
  pluginRegistry: (registryUrl: string) =>
    invoke<RegistryEntry[]>("plugin_registry", { registryUrl }),
  installPlugin: (source: string) =>
    invoke<PluginInfo>("install_plugin", { source }),
  uninstallPlugin: (id: string) => invoke<void>("uninstall_plugin", { id }),

  onVaultEvent: (handler: (event: VaultEvent) => void): Promise<UnlistenFn> =>
    listen<VaultEvent>("onyx://vault-event", (event) => handler(event.payload)),
};
