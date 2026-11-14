import i18n from "i18next";
import { initReactI18next } from "react-i18next";
import en from "./locales/en.json";
import ru from "./locales/ru.json";

export const supportedLanguages = ["en", "ru"] as const;
export type Language = (typeof supportedLanguages)[number];
const defaultLanguage: Language = "en";

export const resources = {
  en: { translation: en },
  ru: { translation: ru },
} satisfies Record<Language, { translation: unknown }>;

// OS locales look like "ru-RU" (Windows, macOS) or "ru_RU.UTF-8" (Linux).
export function pickLanguage(locales: readonly string[]): Language {
  for (const locale of locales) {
    const primary = locale.split(/[-_.]/)[0].toLowerCase();
    const match = supportedLanguages.find((language) => language === primary);
    if (match) return match;
  }
  return defaultLanguage;
}

/// The page's `lang` follows the language shown: screen readers and the browser's text rules
/// read it.
function markPage(language: string) {
  if (typeof document !== "undefined") document.documentElement.lang = language;
}

export async function initI18n(language: Language) {
  i18n.off("languageChanged", markPage);
  i18n.on("languageChanged", markPage);
  await i18n.use(initReactI18next).init({
    resources,
    lng: language,
    fallbackLng: defaultLanguage,
    interpolation: { escapeValue: false },
  });
  return i18n;
}

/// Why a window did not start, in the OS language: i18next may not have started yet.
export function startFailure(error: unknown, locales: readonly string[]): string {
  return resources[pickLanguage(locales)].translation.app.start_failed.replace("{{detail}}", () => String(error));
}
