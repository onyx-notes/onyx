// String externalization from day one — retrofitting i18n is misery.
// Message files are plain JSON; community translations drop in beside en.

import en from "./locales/en.json";
import es from "./locales/es.json";

type Messages = typeof en;
export type MessageKey = keyof Messages;

// Community translations register here. Every locale is a full object (we
// seed non-English ones from en), so any key always resolves; en is the
// final fallback regardless.
const locales: Record<string, Messages> = { en, es };

/** Available locale tags, for a language picker. */
export const availableLocales = Object.keys(locales);

let active: Messages = en;

export function setLocale(tag: string): void {
  // Accept "es-ES" → "es" by taking the base subtag.
  const base = tag.toLowerCase().split("-")[0] ?? "en";
  active = locales[base] ?? locales[tag] ?? en;
}

/** Pick up the OS/browser language on startup. */
export function initLocaleFromEnvironment(): void {
  const preferred = navigator.languages ?? [navigator.language];
  for (const tag of preferred) {
    const base = tag.toLowerCase().split("-")[0] ?? "";
    if (locales[base]) {
      setLocale(base);
      return;
    }
  }
}

/** Translate a key, with `{name}` placeholder substitution. */
export function t(key: MessageKey, vars?: Record<string, string | number>): string {
  let message: string = active[key] ?? en[key] ?? key;
  if (vars) {
    for (const [name, value] of Object.entries(vars)) {
      message = message.replaceAll(`{${name}}`, String(value));
    }
  }
  return message;
}
