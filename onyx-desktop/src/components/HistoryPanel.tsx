// Time machine: scrub a note's saved versions, preview a diff against the
// current content, and restore. All local, all offline.

import { For, Show, createResource, createSignal } from "solid-js";

import { type NoteVersion, api } from "../api";
import { t } from "../i18n";

export default function HistoryPanel(props: {
  path: string;
  onClose: () => void;
  onRestored: () => void;
}) {
  const [versions, { refetch }] = createResource(
    () => props.path,
    (path) => api.noteHistory(path),
    { initialValue: [] },
  );
  const [selected, setSelected] = createSignal<NoteVersion | null>(null);
  const [preview] = createResource(selected, async (version) => {
    const [past, current] = await Promise.all([
      api.noteVersionContent(props.path, version.createdMs),
      api.readNote(props.path).catch(() => ""),
    ]);
    return diffLines(past, current);
  });

  const restore = async (version: NoteVersion) => {
    await api.restoreNoteVersion(props.path, version.createdMs);
    await refetch();
    props.onRestored();
  };

  const when = (ms: number) => new Date(ms).toLocaleString();

  return (
    <div class="overlay" onClick={() => props.onClose()}>
      <div class="palette history" onClick={(event) => event.stopPropagation()}>
        <div class="settings-header">
          {t("history.title")} · {props.path}
        </div>
        <div class="history-body">
          <div class="history-list">
            <Show
              when={versions().length > 0}
              fallback={<div class="palette-empty">{t("history.empty")}</div>}
            >
              <For each={versions()}>
                {(version) => (
                  <button
                    class="file-item"
                    classList={{ active: version === selected() }}
                    onClick={() => setSelected(version)}
                  >
                    {when(version.createdMs)}
                  </button>
                )}
              </For>
            </Show>
          </div>
          <div class="history-preview">
            <Show
              when={selected()}
              fallback={<div class="palette-empty">{t("history.pick")}</div>}
            >
              {(version) => (
                <>
                  <div class="history-preview-head">
                    <span class="settings-caps">{t("history.diffNote")}</span>
                    <button class="settings-button" onClick={() => void restore(version())}>
                      {t("history.restore")}
                    </button>
                  </div>
                  <pre class="agent-diff">
                    <For each={preview() ?? []}>
                      {(line) => (
                        <div class={`diff-${line.kind}`}>
                          {line.prefix}
                          {line.text}
                        </div>
                      )}
                    </For>
                  </pre>
                </>
              )}
            </Show>
          </div>
        </div>
      </div>
    </div>
  );
}

interface DiffLine {
  kind: "add" | "del" | "same";
  prefix: string;
  text: string;
}

/** Diff a past version (left) against current (right). */
function diffLines(past: string, current: string): DiffLine[] {
  const a = past.split("\n");
  const b = current.split("\n");
  const bset = new Set(b);
  const aset = new Set(a);
  const out: DiffLine[] = [];
  for (const line of a) {
    if (!bset.has(line)) out.push({ kind: "del", prefix: "- ", text: line });
  }
  for (const line of b) {
    out.push(
      aset.has(line)
        ? { kind: "same", prefix: "  ", text: line }
        : { kind: "add", prefix: "+ ", text: line },
    );
  }
  return out;
}
