// AI chat overlay: BYOK conversation with optional current-note context.
// The request-log link in settings shows every byte that left the machine.

import { For, Show, createSignal } from "solid-js";

import { type ChatMessage, api } from "../api";
import { t } from "../i18n";

export default function ChatPanel(props: {
  contextPath: string | null;
  onClose: () => void;
}) {
  const [messages, setMessages] = createSignal<ChatMessage[]>([]);
  const [draft, setDraft] = createSignal("");
  const [useContext, setUseContext] = createSignal(true);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  const send = async () => {
    const content = draft().trim();
    if (content.length === 0 || busy()) return;
    setError(null);
    setDraft("");
    const history: ChatMessage[] = [...messages(), { role: "user", content }];
    setMessages(history);
    setBusy(true);
    try {
      const reply = await api.aiChat(
        history,
        useContext() ? props.contextPath : null,
      );
      setMessages([...history, { role: "assistant", content: reply }]);
    } catch (failure) {
      setError(String(failure));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div class="overlay" onClick={() => props.onClose()}>
      <div class="palette chat" onClick={(event) => event.stopPropagation()}>
        <div class="settings-header">
          <span>{t("chat.title")}</span>
          <label class="chat-context">
            <input
              type="checkbox"
              checked={useContext()}
              disabled={props.contextPath === null}
              onChange={(event) => setUseContext(event.currentTarget.checked)}
            />
            {t("chat.includeNote")}
          </label>
        </div>
        <div class="chat-messages">
          <Show
            when={messages().length > 0}
            fallback={<div class="palette-empty">{t("chat.empty")}</div>}
          >
            <For each={messages()}>
              {(message) => (
                <div class="chat-message" classList={{ user: message.role === "user" }}>
                  {message.content}
                </div>
              )}
            </For>
          </Show>
          <Show when={busy()}>
            <div class="chat-message">…</div>
          </Show>
          <Show when={error()}>
            {(message) => <div class="chat-error">{message()}</div>}
          </Show>
        </div>
        <textarea
          class="chat-input"
          placeholder={t("chat.placeholder")}
          value={draft()}
          onInput={(event) => setDraft(event.currentTarget.value)}
          onKeyDown={(event) => {
            if (event.key === "Enter" && !event.shiftKey) {
              event.preventDefault();
              void send();
            }
          }}
          ref={(element) => queueMicrotask(() => element.focus())}
        />
      </div>
    </div>
  );
}
