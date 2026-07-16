// Typed wrappers over the Tauri IPC surface. Everything the UI knows about
// the backend lives here.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export interface VaultInfo {
  root: string;
  noteCount: number;
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

export const api = {
  openVault: (path: string) => invoke<VaultInfo>("open_vault", { path }),
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

  onVaultEvent: (handler: (event: VaultEvent) => void): Promise<UnlistenFn> =>
    listen<VaultEvent>("onyx://vault-event", (event) => handler(event.payload)),
};
