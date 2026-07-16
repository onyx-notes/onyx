// String externalization from day one — retrofitting i18n is misery.
// Message files are plain JSON; community translations drop in beside en.

import en from "./locales/en.json";

type Messages = typeof en;
export type MessageKey = keyof Messages;

const locales: Record<string, Messages> = { en };
let active: Messages = locales["en"] ?? en;

export function setLocale(tag: string): void {
  active = locales[tag] ?? en;
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
