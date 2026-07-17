// Ctrl+Shift+P command palette: built-in app commands + plugin commands.

import { For, Show, createMemo, createSignal } from "solid-js";

import { t } from "../i18n";

export interface PaletteCommand {
  id: string;
  name: string;
  run: () => void;
}

export default function CommandPalette(props: {
  commands: PaletteCommand[];
  onClose: () => void;
}) {
  const [query, setQuery] = createSignal("");
  const [selected, setSelected] = createSignal(0);

  const matches = createMemo(() => {
    const needle = query().toLowerCase();
    if (needle.length === 0) return props.commands;
    return props.commands.filter((command) =>
      command.name.toLowerCase().includes(needle),
    );
  });

  const move = (delta: number) => {
    const count = matches().length;
    if (count > 0) setSelected((current) => (current + delta + count) % count);
  };

  const run = () => {
    const command = matches()[selected()];
    if (command) {
      props.onClose();
      command.run();
    }
  };

  return (
    <div class="overlay" onClick={() => props.onClose()}>
      <div class="palette" onClick={(event) => event.stopPropagation()}>
        <input
          placeholder={t("palette.placeholder")}
          value={query()}
          onInput={(event) => {
            setQuery(event.currentTarget.value);
            setSelected(0);
          }}
          onKeyDown={(event) => {
            if (event.key === "ArrowDown") {
              event.preventDefault();
              move(1);
            } else if (event.key === "ArrowUp") {
              event.preventDefault();
              move(-1);
            } else if (event.key === "Enter") {
              event.preventDefault();
              run();
            } else if (event.key === "Escape") {
              props.onClose();
            }
          }}
          ref={(element) => queueMicrotask(() => element.focus())}
        />
        <div class="palette-results">
          <Show
            when={matches().length > 0}
            fallback={<div class="palette-empty">{t("search.noResults")}</div>}
          >
            <For each={matches()}>
              {(command, index) => (
                <button
                  class="palette-item"
                  classList={{ selected: index() === selected() }}
                  onMouseEnter={() => setSelected(index())}
                  onClick={run}
                >
                  {command.name}
                </button>
              )}
            </For>
          </Show>
        </div>
      </div>
    </div>
  );
}
