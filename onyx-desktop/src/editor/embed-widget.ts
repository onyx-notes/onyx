// Transclusion widgets: `![[note]]` renders the target inline (read-only,
// depth 1), `![[image.png]]` renders the image. Content loads async and
// fills a placeholder so typing never blocks; a small cache keeps
// re-renders (every keystroke rebuilds decorations) from re-fetching.

import { WidgetType } from "@codemirror/view";
import { convertFileSrc } from "@tauri-apps/api/core";

import { api } from "../api";
import { t } from "../i18n";

const IMAGE_RE = /\.(png|jpe?g|gif|webp|svg|avif|bmp)$/i;

/** target → rendered HTML (or resolved image path). Cleared per note open
 * by bumping `cacheEpoch` from the editor rebuild. */
const htmlCache = new Map<string, string>();
const pathCache = new Map<string, string | null>();

export function clearEmbedCache(): void {
  htmlCache.clear();
  pathCache.clear();
}

function fileUrl(path: string): string {
  return convertFileSrc(`file/${encodeURIComponent(path)}`, "onyx");
}

async function resolve(target: string): Promise<string | null> {
  if (pathCache.has(target)) return pathCache.get(target) ?? null;
  const path = await api.resolveTarget(target).catch(() => null);
  pathCache.set(target, path);
  return path;
}

async function renderInto(container: HTMLElement, target: string): Promise<void> {
  const path = await resolve(target);
  if (path === null) {
    container.textContent = t("embed.missing", { target });
    container.classList.add("is-missing");
    return;
  }

  if (IMAGE_RE.test(path)) {
    const image = document.createElement("img");
    image.src = fileUrl(path);
    image.className = "onyx-embed-image";
    container.replaceChildren(image);
    return;
  }

  let html = htmlCache.get(path);
  if (html === undefined) {
    try {
      html = await api.renderNote(path);
      htmlCache.set(path, html);
    } catch {
      container.textContent = t("embed.missing", { target });
      container.classList.add("is-missing");
      return;
    }
  }
  container.innerHTML = html;
  // Nested image embeds inside the rendered HTML get real sources; nested
  // note embeds stay links (depth limit 1 by construction).
  for (const image of container.querySelectorAll<HTMLElement>("img[data-vault-target]")) {
    const nested = image.dataset["vaultTarget"];
    if (nested) {
      void resolve(nested).then((nestedPath) => {
        if (nestedPath) (image as HTMLImageElement).src = fileUrl(nestedPath);
      });
    }
  }
}

export class EmbedWidget extends WidgetType {
  constructor(private target: string) {
    super();
  }

  override eq(other: EmbedWidget): boolean {
    return other.target === this.target;
  }

  toDOM(): HTMLElement {
    const container = document.createElement("div");
    container.className = "onyx-embed";
    container.textContent = "…";
    void renderInto(container, this.target);
    return container;
  }

  override ignoreEvent(event: Event): boolean {
    // Clicks on links inside the embed are handled by the editor's
    // delegation; everything else stays inert.
    return event.type !== "mousedown";
  }
}
