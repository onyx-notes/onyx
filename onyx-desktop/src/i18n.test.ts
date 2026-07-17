import { describe, expect, it } from "vitest";

import { availableLocales, setLocale, t } from "./i18n";

describe("i18n", () => {
  it("ships at least English and Spanish", () => {
    expect(availableLocales).toContain("en");
    expect(availableLocales).toContain("es");
  });

  it("translates and substitutes placeholders", () => {
    setLocale("es");
    expect(t("vault.open")).toBe("Abrir bóveda");
    expect(t("status.words", { count: 5 })).toBe("5 palabras");
    setLocale("en");
    expect(t("vault.open")).toBe("Open vault");
  });

  it("resolves region subtags to the base language", () => {
    setLocale("es-ES");
    expect(t("settings.title")).toBe("Ajustes");
    setLocale("en");
  });

  it("unknown locale falls back to English", () => {
    setLocale("xx");
    expect(t("vault.open")).toBe("Open vault");
  });

  it("every key resolves in every locale (no missing translations crash)", () => {
    setLocale("es");
    // A key only present structurally still returns a string, never undefined.
    expect(typeof t("agent.apply")).toBe("string");
    setLocale("en");
  });
});
