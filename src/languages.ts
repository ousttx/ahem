// Spoken languages for the engine, named in the interface language by the browser's Intl data.
import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";

export interface LanguageInfo {
  /// Whisper's code, e.g. "ru".
  code: string;
  /// English name from the engine, the fallback label.
  name: string;
}

export interface LanguageOption {
  code: string;
  label: string;
}

/// The engine's languages are fixed: read once for every page that lists them.
let languageList: Promise<LanguageInfo[]> | null = null;

/// The spoken languages the engine knows; empty until they are read.
export function useLanguageList(): LanguageInfo[] {
  const [list, setList] = useState<LanguageInfo[]>([]);
  useEffect(() => {
    const read = (languageList ??= invoke<LanguageInfo[]>("list_languages"));
    read.then(setList, () => {
      // A list that could not be read is asked for again by the next page, unless a newer
      // reading has already started.
      if (languageList === read) languageList = null;
    });
  }, []);
  return list;
}

/// The spoken languages named in the interface language, by name.
export function useLanguageOptions(): LanguageOption[] {
  const { i18n } = useTranslation();
  const all = useLanguageList();
  return useMemo(() => localizedLanguages(all, i18n.language), [all, i18n.language]);
}

/// Whisper still uses the retired code for Javanese.
const DISPLAY_CODES: Record<string, string> = { jw: "jv" };

function capitalize(text: string, locale: string): string {
  return text.charAt(0).toLocaleUpperCase(locale) + text.slice(1);
}

/// The spoken languages to offer: those of the dictation model (all without one), and the chosen
/// language even if the model does not know it, so the list can show it.
export function spokenLanguageChoices(
  all: LanguageOption[],
  model: { languages: string[] } | null,
  current: string | null,
): LanguageOption[] {
  if (!model) return all;
  return all.filter((language) => model.languages.includes(language.code) || language.code === current);
}

export function localizedLanguages(list: LanguageInfo[], uiLanguage: string): LanguageOption[] {
  const names = new Intl.DisplayNames([uiLanguage], { type: "language" });
  return list
    .map(({ code, name }) => {
      let label: string | undefined;
      try {
        label = names.of(DISPLAY_CODES[code] ?? code);
      } catch {
        label = undefined;
      }
      if (!label || label === code || label === DISPLAY_CODES[code]) label = name;
      return { code, label: capitalize(label, uiLanguage) };
    })
    .sort((a, b) => a.label.localeCompare(b.label, uiLanguage));
}
