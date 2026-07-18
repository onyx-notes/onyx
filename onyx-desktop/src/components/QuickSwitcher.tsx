// Ctrl+P quick switcher: fuzzy jump-to-note, keyboard-first.

import { For, Show, createResource, createSignal } from "solid-js";

import { api } from "../api";
import { t } from "../i18n";

export interface QuickSwitcherProps {
  onPick: (path: string) => void;
  onClose: () => void;
}

export default function QuickSwitcher(props: QuickSwitcherProps) {
  const [query, setQuery] = createSignal("");
  const [selected, setSelected] = createSignal(0);
  // Title fuzzy-match first, then full-text hits for the same query so
  // content-only matches surface too (deduped by path, title hits win).
  const [hits] = createResource(
    query,
    async (q) => {
      const [titleHits, contentHits] = await Promise.all([
        api.quickOpen(q),
        q.trim().length > 0 ? api.searchNotes(q).catch(() => []) : Promise.resolve([]),
      ]);
      const seen = new Set(titleHits.map((hit) => hit.path));
      return [...titleHits, ...contentHits.filter((hit) => !seen.has(hit.path))];
    },
    { initialValue: [] },
  );

  const move = (delta: number) => {
    const count = hits().length;
    if (count === 0) return;
    setSelected((current) => (current + delta + count) % count);
  };

  const pick = () => {
    const hit = hits()[selected()];
    if (hit) props.onPick(hit.path);
  };

  const onKeyDown = (event: KeyboardEvent) => {
    switch (event.key) {
      case "ArrowDown":
        event.preventDefault();
        move(1);
        break;
      case "ArrowUp":
        event.preventDefault();
        move(-1);
        break;
      case "Enter":
        event.preventDefault();
        pick();
        break;
      case "Escape":
        event.preventDefault();
        props.onClose();
        break;
    }
  };

  return (
    <div class="overlay" onClick={() => props.onClose()}>
      <div class="palette" onClick={(event) => event.stopPropagation()}>
        <input
          placeholder={t("quick.placeholder")}
          value={query()}
          onInput={(event) => {
            setQuery(event.currentTarget.value);
            setSelected(0);
          }}
          onKeyDown={onKeyDown}
          ref={(element) => queueMicrotask(() => element.focus())}
        />
        <div class="palette-results">
          <Show
            when={hits().length > 0}
            fallback={<div class="palette-empty">{t("search.noResults")}</div>}
          >
            <For each={hits()}>
              {(hit, index) => (
                <button
                  class="palette-item"
                  classList={{ selected: index() === selected() }}
                  onMouseEnter={() => setSelected(index())}
                  onClick={pick}
                >
                  {hit.path}
                </button>
              )}
            </For>
          </Show>
        </div>
      </div>
    </div>
  );
}
