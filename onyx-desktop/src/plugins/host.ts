// The plugin host: loads each enabled plugin into a sandboxed iframe
// (served over onyx:// → different origin from the app; `sandbox`
// attribute on top) and brokers every API call against the plugin's
// DECLARED capabilities. The iframe realm has no Tauri IPC, no app DOM,
// and no network (CSP) — the broker is the only door, and it checks
// capabilities on every knock.

import { convertFileSrc } from "@tauri-apps/api/core";

import { api } from "../api";

export interface PluginManifest {
  id: string;
  name: string;
  version: string;
  description: string;
  capabilities: string[];
  enabled: boolean;
}

export interface PluginCommand {
  pluginId: string;
  commandId: string;
  name: string;
}

/** Capability required for each broker method (null = always allowed). */
export function requiredCapability(method: string): string | null | undefined {
  switch (method) {
    case "vault.read":
    case "vault.list":
      return "vault:read";
    case "vault.write":
      return "vault:write";
    case "commands.register":
      return "ui:commands";
    case "editor.insert":
    case "editor.getActivePath":
      return "editor:write";
    case "notice":
      return null;
    default:
      return undefined; // unknown method: always rejected
  }
}

export class PluginHost {
  private frames = new Map<string, { frame: HTMLIFrameElement; capabilities: Set<string> }>();
  private commands = new Map<string, PluginCommand>();

  constructor(
    private callbacks: {
      onNotice: (pluginId: string, message: string) => void;
      onCommandsChanged: (commands: PluginCommand[]) => void;
      onEditorInsert: (text: string) => void;
      activePath: () => string | null;
    },
  ) {
    window.addEventListener("message", this.onMessage);
  }

  destroy(): void {
    window.removeEventListener("message", this.onMessage);
    for (const { frame } of this.frames.values()) frame.remove();
    this.frames.clear();
    this.commands.clear();
    this.callbacks.onCommandsChanged([]);
  }

  load(manifest: PluginManifest): void {
    if (this.frames.has(manifest.id)) return;
    const frame = document.createElement("iframe");
    frame.sandbox.add("allow-scripts");
    frame.style.display = "none";
    frame.src = convertFileSrc(`plugin/${manifest.id}`, "onyx");
    document.body.appendChild(frame);
    this.frames.set(manifest.id, {
      frame,
      capabilities: new Set(manifest.capabilities),
    });
  }

  unload(pluginId: string): void {
    const entry = this.frames.get(pluginId);
    if (!entry) return;
    entry.frame.remove();
    this.frames.delete(pluginId);
    for (const [key, command] of this.commands) {
      if (command.pluginId === pluginId) this.commands.delete(key);
    }
    this.callbacks.onCommandsChanged([...this.commands.values()]);
  }

  runCommand(pluginId: string, commandId: string): void {
    const entry = this.frames.get(pluginId);
    entry?.frame.contentWindow?.postMessage(
      { onyxRunCommand: true, commandId },
      "*",
    );
  }

  listCommands(): PluginCommand[] {
    return [...this.commands.values()];
  }

  private onMessage = (event: MessageEvent) => {
    const message = event.data as
      | { onyxPlugin?: string; id?: number; method?: string; params?: Record<string, unknown> }
      | undefined;
    if (!message?.onyxPlugin || typeof message.id !== "number" || !message.method) return;

    const entry = this.frames.get(message.onyxPlugin);
    // The sender must be the iframe we created for that exact plugin id —
    // a plugin cannot speak in another plugin's name.
    if (!entry || event.source !== entry.frame.contentWindow) return;

    void this.dispatch(message.onyxPlugin, entry.capabilities, message.method, message.params ?? {})
      .then((value) => {
        entry.frame.contentWindow?.postMessage(
          { onyxReply: true, id: message.id, ok: true, value },
          "*",
        );
      })
      .catch((error: unknown) => {
        entry.frame.contentWindow?.postMessage(
          { onyxReply: true, id: message.id, ok: false, error: String(error) },
          "*",
        );
      });
  };

  private async dispatch(
    pluginId: string,
    capabilities: Set<string>,
    method: string,
    params: Record<string, unknown>,
  ): Promise<unknown> {
    const required = requiredCapability(method);
    if (required === undefined) {
      throw new Error(`unknown method: ${method}`);
    }
    if (required !== null && !capabilities.has(required)) {
      throw new Error(`plugin lacks capability ${required} for ${method}`);
    }

    switch (method) {
      case "vault.read":
        return api.readNote(String(params["path"]));
      case "vault.write":
        await api.writeNote(String(params["path"]), String(params["content"]));
        return null;
      case "vault.list":
        return api.listNotes();
      case "commands.register": {
        const commandId = String(params["id"]);
        const name = String(params["name"]);
        this.commands.set(`${pluginId}:${commandId}`, { pluginId, commandId, name });
        this.callbacks.onCommandsChanged([...this.commands.values()]);
        return null;
      }
      case "editor.insert":
        this.callbacks.onEditorInsert(String(params["text"]));
        return null;
      case "editor.getActivePath":
        return this.callbacks.activePath();
      case "notice":
        this.callbacks.onNotice(pluginId, String(params["message"]));
        return null;
      default:
        throw new Error(`unknown method: ${method}`);
    }
  }
}
