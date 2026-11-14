import { describe, expect, it } from "vitest";
import { localizedLanguages, spokenLanguageChoices } from "./languages";

const list = [
  { code: "ru", name: "russian" },
  { code: "jw", name: "javanese" },
  { code: "en", name: "english" },
];

describe("localizedLanguages", () => {
  it("names languages in the interface language, sorted", () => {
    expect(localizedLanguages(list, "en").map((option) => option.label)).toEqual([
      "English",
      "Javanese",
      "Russian",
    ]);
    const russian = localizedLanguages(list, "ru");
    expect(russian.find((option) => option.code === "ru")?.label).toBe("Русский");
    expect(russian.find((option) => option.code === "en")?.label).toBe("Английский");
  });

  it("falls back to the engine's English name", () => {
    const [unknown] = localizedLanguages([{ code: "zz", name: "klingon" }], "en");
    expect(unknown.label).toBe("Klingon");
  });
});

describe("spokenLanguageChoices", () => {
  const all = localizedLanguages(list, "en");
  const codes = (options: { code: string }[]) => options.map((option) => option.code);

  it("offers the languages of the dictation model", () => {
    expect(codes(spokenLanguageChoices(all, { languages: ["en", "ru"] }, null))).toEqual(["en", "ru"]);
    expect(codes(spokenLanguageChoices(all, null, null))).toEqual(["en", "jw", "ru"]);
  });

  it("keeps the chosen language the model does not know", () => {
    expect(codes(spokenLanguageChoices(all, { languages: ["en"] }, "jw"))).toEqual(["en", "jw"]);
  });
});
