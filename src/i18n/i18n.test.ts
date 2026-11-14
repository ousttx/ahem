import { afterEach, describe, expect, it, vi } from "vitest";
import { initI18n, pickLanguage, resources, startFailure } from "./index";

function keys(value: unknown, prefix = ""): string[] {
  if (typeof value !== "object" || value === null) return [prefix];
  return Object.entries(value).flatMap(([key, child]) => keys(child, prefix ? `${prefix}.${key}` : key));
}

describe("pickLanguage", () => {
  it("matches the primary language of OS locales", () => {
    expect(pickLanguage(["ru-RU"])).toBe("ru");
    expect(pickLanguage(["ru_RU.UTF-8"])).toBe("ru");
    expect(pickLanguage(["EN-us"])).toBe("en");
  });

  it("takes the first supported locale in preference order", () => {
    expect(pickLanguage(["de-DE", "ru-RU", "en-US"])).toBe("ru");
  });

  it("falls back to English", () => {
    expect(pickLanguage(["de-DE", "zh-CN"])).toBe("en");
    expect(pickLanguage([])).toBe("en");
  });
});

describe("translations", () => {
  it("have the same keys in every language", () => {
    expect(keys(resources.ru.translation).sort()).toEqual(keys(resources.en.translation).sort());
  });

  it("render the same key in English and Russian", async () => {
    const i18n = await initI18n("en");
    expect(i18n.t("language.label")).toBe("Language");
    await i18n.changeLanguage("ru");
    expect(i18n.t("language.label")).toBe("Язык");
  });
});

function texts(value: unknown, prefix = ""): [string, string][] {
  if (typeof value === "string") return [[prefix, value]];
  return Object.entries(value as object).flatMap(([key, child]) => texts(child, prefix ? `${prefix}.${key}` : key));
}

describe("keys", () => {
  it("never end like the plural forms of i18next, which would pick them by a count", () => {
    const plural = /_(zero|one|two|few|many|other)$/;
    expect(keys(resources.en.translation).filter((key) => plural.test(key))).toEqual([]);
  });
});

describe("texts", () => {
  it("do not put a period after a detail, which may end with its own", () => {
    for (const language of ["en", "ru"] as const) {
      expect(texts(resources[language].translation).filter(([, text]) => text.includes("{{detail}}."))).toEqual([]);
    }
  });

  it("call other apps apps, as everywhere in the interface", () => {
    expect(texts(resources.en.translation).filter(([, text]) => /\bprograms?\b/i.test(text))).toEqual([]);
    expect(texts(resources.ru.translation).filter(([, text]) => /программ/i.test(text))).toEqual([]);
  });

  it("need no plural forms in English: a count of one reads well", async () => {
    const i18n = await initI18n("en");
    expect(i18n.t("models.languages", { number: 1 })).toBe("Languages: 1");
    expect(i18n.t("connections.model_count", { number: 1 })).toBe("models: 1");
    expect(i18n.t("modes.clean.words_count", { count: 1 })).toBe("Words: 1");
  });
});

describe("the page's language", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("follows the language shown, so screen readers read it right", async () => {
    const root = { lang: "en" };
    vi.stubGlobal("document", { documentElement: root });
    const i18n = await initI18n("ru");
    expect(root.lang).toBe("ru");
    await i18n.changeLanguage("en");
    expect(root.lang).toBe("en");
  });
});

describe("startFailure", () => {
  it("says in the OS language why a window did not start, before i18next runs", () => {
    expect(startFailure(new Error("no settings"), ["ru-RU"])).toBe("ahem не запустился: Error: no settings");
    expect(startFailure("$& gone", ["de-DE"])).toBe("ahem could not start: $& gone");
  });
});
