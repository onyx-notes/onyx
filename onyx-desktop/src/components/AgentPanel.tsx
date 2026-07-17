// Vault Assistant: state a goal, the agent gathers context and PROPOSES
// changes; you review each as a diff and apply only what you approve.
// Nothing touches disk until you press Apply.

import { For, Show, createSignal } from "solid-js";

import { type Proposal, api } from "../api";
import { t } from "../i18n";

interface Row {
  proposal: Proposal;
  before: string;
  approved: boolean;
}

export default function AgentPanel(props: { onClose: () => void; onApplied: () => void }) {
  const [goal, setGoal] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [summary, setSummary] = createSignal<string | null>(null);
  const [rows, setRows] = createSignal<Row[]>([]);
  const [error, setError] = createSignal<string | null>(null);

  const run = async () => {
    if (goal().trim().length === 0 || busy()) return;
    setBusy(true);
    setError(null);
    setRows([]);
    setSummary(null);
    try {
      const changeset = await api.agentRun(goal().trim());
      setSummary(changeset.finished);
      const built: Row[] = [];
      for (const proposal of changeset.proposals) {
        const before = await api.readNote(proposal.path).catch(() => "");
        built.push({ proposal, before, approved: true });
      }
      setRows(built);
    } catch (failure) {
      setError(String(failure));
    } finally {
      setBusy(false);
    }
  };

  const toggle = (index: number) =>
    setRows((current) =>
      current.map((row, i) => (i === index ? { ...row, approved: !row.approved } : row)),
    );

  const apply = async () => {
    const approved = rows()
      .filter((row) => row.approved)
      .map((row) => row.proposal);
    if (approved.length === 0) return;
    try {
      const count = await api.agentApply(approved);
      setSummary(t("agent.applied", { count }));
      setRows([]);
      props.onApplied();
    } catch (failure) {
      setError(String(failure));
    }
  };

  return (
    <div class="overlay" onClick={() => props.onClose()}>
      <div class="palette agent" onClick={(event) => event.stopPropagation()}>
        <div class="settings-header">{t("agent.title")}</div>
        <div class="agent-body">
          <div class="agent-goal">
            <input
              type="text"
              placeholder={t("agent.placeholder")}
              value={goal()}
              onInput={(event) => setGoal(event.currentTarget.value)}
              onKeyDown={(event) => {
                if (event.key === "Enter") void run();
              }}
              ref={(element) => queueMicrotask(() => element.focus())}
            />
            <button class="settings-button" disabled={busy()} onClick={() => void run()}>
              {busy() ? t("agent.working") : t("agent.run")}
            </button>
          </div>

          <Show when={error()}>{(message) => <div class="chat-error">{message()}</div>}</Show>
          <Show when={summary()}>{(text) => <div class="settings-imported">{text()}</div>}</Show>

          <For each={rows()}>
            {(row, index) => (
              <div class="agent-proposal">
                <label class="agent-proposal-head">
                  <input
                    type="checkbox"
                    checked={row.approved}
                    onChange={() => toggle(index())}
                  />
                  <b>{row.proposal.kind === "delete" ? "🗑 " : "✎ "}{row.proposal.path}</b>
                </label>
                <Show when={row.proposal.kind === "write"}>
                  <Diff
                    before={row.before}
                    after={row.proposal.kind === "write" ? row.proposal.content : ""}
                  />
                </Show>
              </div>
            )}
          </For>

          <Show when={rows().length > 0}>
            <div class="agent-actions">
              <button class="settings-button primary" onClick={() => void apply()}>
                {t("agent.apply")}
              </button>
            </div>
          </Show>
        </div>
      </div>
    </div>
  );
}

/** Minimal line diff — added/removed/context, enough to review a proposal. */
function Diff(props: { before: string; after: string }) {
  const lines = () => diffLines(props.before, props.after);
  return (
    <pre class="agent-diff">
      <For each={lines()}>
        {(line) => <div class={`diff-${line.kind}`}>{line.prefix}{line.text}</div>}
      </For>
    </pre>
  );
}

interface DiffLine {
  kind: "add" | "del" | "same";
  prefix: string;
  text: string;
}

function diffLines(before: string, after: string): DiffLine[] {
  const a = before.split("\n");
  const b = after.split("\n");
  const set = new Set(a);
  const bset = new Set(b);
  const out: DiffLine[] = [];
  // Simple set-based diff: good enough for review (not a minimal-edit LCS).
  for (const line of a) {
    if (!bset.has(line)) out.push({ kind: "del", prefix: "- ", text: line });
  }
  for (const line of b) {
    out.push(
      set.has(line)
        ? { kind: "same", prefix: "  ", text: line }
        : { kind: "add", prefix: "+ ", text: line },
    );
  }
  return out;
}
