// The quick-capture window: a minimal textarea that appends to today's
// daily note and closes. Rendered when the window URL has ?capture=1.

import { createSignal } from "solid-js";

import { api } from "./api";
import { t } from "./i18n";

function localDate(): string {
  const now = new Date();
  const m = String(now.getMonth() + 1).padStart(2, "0");
  const d = String(now.getDate()).padStart(2, "0");
  return `${now.getFullYear()}-${m}-${d}`;
}

export default function CaptureApp() {
  const [text, setText] = createSignal("");
  const [status, setStatus] = createSignal("");

  const save = async () => {
    const value = text().trim();
    if (value.length === 0) return closeWindow();
    try {
      await api.quickCapture(value, localDate());
      closeWindow();
    } catch (error) {
      setStatus(String(error));
    }
  };

  const closeWindow = async () => {
    const { getCurrentWindow } = await import("@tauri-apps/api/window");
    await getCurrentWindow().close();
  };

  return (
    <div class="capture">
      <textarea
        class="capture-input"
        placeholder={t("capture.placeholder")}
        value={text()}
        onInput={(event) => setText(event.currentTarget.value)}
        onKeyDown={(event) => {
          if (event.key === "Enter" && (event.ctrlKey || event.metaKey)) {
            event.preventDefault();
            void save();
          } else if (event.key === "Escape") {
            void closeWindow();
          }
        }}
        ref={(element) => queueMicrotask(() => element.focus())}
      />
      <div class="capture-footer">
        <span>{status() || t("capture.hint")}</span>
        <button onClick={() => void save()}>{t("capture.save")}</button>
      </div>
    </div>
  );
}
