// Shared post-processing for rendered note HTML (reading view + embeds):
// KaTeX math, mermaid diagrams, vault-image sources, wikilink clicks.
// Heavy renderers load lazily so the base bundle stays lean.

import { convertFileSrc } from "@tauri-apps/api/core";

import { api } from "../api";

function fileUrl(path: string): string {
  return convertFileSrc(`file/${encodeURIComponent(path)}`, "onyx");
}

let mermaidReady: Promise<typeof import("mermaid")["default"]> | null = null;
function loadMermaid() {
  mermaidReady ??= import("mermaid").then((module) => {
    const mermaid = module.default;
    mermaid.initialize({
      startOnLoad: false,
      securityLevel: "strict",
      theme: document.documentElement.dataset["theme"] === "light" ? "default" : "dark",
    });
    return mermaid;
  });
  return mermaidReady;
}

let katexReady: Promise<
  (element: HTMLElement, options?: Record<string, unknown>) => void
> | null = null;
function loadKatexAutoRender() {
  katexReady ??= Promise.all([
    import("katex/contrib/auto-render"),
    // CSS side effect (fonts bundle as assets).
    import("katex/dist/katex.min.css"),
  ]).then(([autoRender]) => autoRender.default);
  return katexReady;
}

let mermaidCounter = 0;

/** Enhance a container holding renderer output. Idempotent per node. */
export async function enhanceRendered(
  container: HTMLElement,
  callbacks: { followLink: (target: string) => void },
): Promise<void> {
  // 1. Vault images → protocol URLs.
  for (const image of container.querySelectorAll<HTMLImageElement>("img[data-vault-target]")) {
    const target = image.dataset["vaultTarget"];
    if (target) {
      const path = await api.resolveTarget(target).catch(() => null);
      if (path) image.src = fileUrl(path);
    }
  }

  // 2. Wikilink navigation (delegated once per container).
  if (!container.dataset["onyxLinksBound"]) {
    container.dataset["onyxLinksBound"] = "1";
    container.addEventListener("click", (event) => {
      const anchor = (event.target as HTMLElement).closest<HTMLElement>("a.onyx-wikilink");
      const target = anchor?.dataset["target"];
      if (target) {
        event.preventDefault();
        callbacks.followLink(target);
      }
    });
  }

  // 3a. onyx-query blocks → rendered tables (link column clickable).
  for (const block of container.querySelectorAll<HTMLElement>(
    "pre code.language-onyx-query",
  )) {
    const source = block.textContent ?? "";
    const host = document.createElement("div");
    host.className = "onyx-query";
    try {
      const result = await api.runQueryBlock(source);
      if (result.error) {
        host.classList.add("is-error");
        host.textContent = result.error;
      } else if (result.rows.length === 0) {
        host.classList.add("is-empty");
        host.textContent = "No results";
      } else {
        renderQueryTable(host, result, callbacks.followLink);
      }
    } catch (error) {
      host.classList.add("is-error");
      host.textContent = String(error);
    }
    block.closest("pre")?.replaceWith(host);
  }

  // 3. Mermaid diagrams: ```mermaid fences arrive as language-mermaid code.
  const mermaidBlocks = [
    ...container.querySelectorAll<HTMLElement>("pre code.language-mermaid"),
  ];
  if (mermaidBlocks.length > 0) {
    const mermaid = await loadMermaid();
    for (const block of mermaidBlocks) {
      const source = block.textContent ?? "";
      const host = document.createElement("div");
      host.className = "onyx-mermaid";
      try {
        const { svg } = await mermaid.render(`onyx-mermaid-${mermaidCounter++}`, source);
        host.innerHTML = svg;
      } catch {
        host.textContent = source; // invalid diagram: show the source
        host.classList.add("is-error");
      }
      block.closest("pre")?.replaceWith(host);
    }
  }

  // 4. Math ($…$ and $$…$$), skipping code and already-rendered nodes.
  if (/\$[^$]/.test(container.textContent ?? "")) {
    const autoRender = await loadKatexAutoRender();
    autoRender(container, {
      delimiters: [
        { left: "$$", right: "$$", display: true },
        { left: "$", right: "$", display: false },
      ],
      ignoredTags: ["script", "noscript", "style", "textarea", "pre", "code"],
      throwOnError: false,
    });
  }
}

/** Render a query result as a table; the first column links to the note. */
function renderQueryTable(
  host: HTMLElement,
  result: { columns: string[]; rows: string[][] },
  followLink: (target: string) => void,
): void {
  const table = document.createElement("table");
  const thead = document.createElement("thead");
  const headRow = document.createElement("tr");
  for (const col of result.columns) {
    const th = document.createElement("th");
    th.textContent = col;
    headRow.appendChild(th);
  }
  thead.appendChild(headRow);
  table.appendChild(thead);

  const tbody = document.createElement("tbody");
  for (const row of result.rows) {
    const tr = document.createElement("tr");
    row.forEach((cell, index) => {
      const td = document.createElement("td");
      if (index === 0) {
        const link = document.createElement("a");
        link.className = "onyx-wikilink";
        link.textContent = cell.replace(/\.(md|markdown)$/i, "");
        link.addEventListener("click", (event) => {
          event.preventDefault();
          followLink(cell);
        });
        td.appendChild(link);
      } else {
        td.textContent = cell;
      }
      tr.appendChild(td);
    });
    tbody.appendChild(tr);
  }
  table.appendChild(tbody);
  host.replaceChildren(table);
}
