import { invoke } from "@tauri-apps/api/core";
import type { AppRule } from "./appRules";
import type { Language } from "./i18n";

// Mirrors `storage::settings::Settings` in Rust.
export type UiLanguage = "system" | Language;

export type RecordingMode = "auto" | "hold" | "toggle";

/// `system` follows the OS light or dark mode.
export type Theme = "system" | "light" | "dark";

export type UnloadPolicy = { mode: "immediate" } | { mode: "after_idle"; minutes: number } | { mode: "never" };

/// A phrase as the engine writes it and the text that goes in instead (`domain::replacements`).
export interface Replacement {
  from: string;
  to: string;
}

/// How plain dictation cleans the recognized text (`domain::clean_text::CleanText`).
export type CleanText = { level: "off" } | { level: "fillers" } | { level: "model"; connection_id: string; model: string };

export interface Settings {
  ui_language: UiLanguage;
  hotkey: string;
  recording_mode: RecordingMode;
  microphone_id: string | null;
  model_id: string | null;
  unload_policy: UnloadPolicy;
  mute_while_recording: boolean;
  /// The capsule at the bottom of the screen while dictating and for its notices.
  show_indicator: boolean;
  /// `null` lets the engine detect the language.
  spoken_language: string | null;
  dictionary: string;
  replacements: Replacement[];
  clean_text: CleanText;
  /// The words "clean text" removes, separated by commas or new lines.
  fillers: string;
  connections: Connection[];
  modes: Mode[];
  main_mode_id: string;
  /// The modes the "by application" hotkey takes in particular programs.
  app_rules: AppRule[];
  /// Dictates with the mode of the program in focus; `null` until set.
  app_hotkey: string | null;
  history_enabled: boolean;
  /// 1 to `HISTORY_LIMIT_MAX`; `null` keeps every dictation.
  history_limit: number | null;
  launch_at_login: boolean;
  onboarding_done: boolean;
  theme: Theme;
  /// Transcribing an audio or video file tells its voices apart; off until the user turns it on.
  files_by_speaker: boolean;
  /// New versions download in the background and install at a restart.
  auto_update: boolean;
  /// New versions as betas first.
  beta_updates: boolean;
}

/// Mirrors `storage::settings::MAX_DICTIONARY_CHARS`: a longer dictionary is not saved.
export const MAX_DICTIONARY_CHARS = 2000;

/// Mirrors `storage::settings::HISTORY_LIMIT_MAX`.
export const HISTORY_LIMIT_MAX = 10_000;
/// What "Last" starts with.
export const HISTORY_LIMIT_DEFAULT = 10;

/// The number typed for the history: a whole number from 1 to `HISTORY_LIMIT_MAX`, else `null`.
export function parseHistoryLimit(text: string): number | null {
  const trimmed = text.trim();
  if (!/^\d+$/.test(trimmed)) return null;
  const limit = Number(trimmed);
  return limit >= 1 && limit <= HISTORY_LIMIT_MAX ? limit : null;
}

/// Mirrors `domain::clean_text::INSTRUCTION`: what the model of "clean text" is asked to do.
export const CLEAN_INSTRUCTION = `Clean up this dictated text so it reads as the speaker meant to write it.

Remove:
- filler sounds, and filler words that carry no meaning in the sentence (uh, um, э-э, ну, типа, like, you know); keep such words where they carry meaning;
- false starts, stutters and accidental repetitions; keep repetitions made for emphasis;
- what the speaker took back when correcting themselves ("at two, no, at three", "I mean", "scratch that", "нет, точнее", "вернее", "то есть"): keep only the final version. Such words stay when they do not correct anything.

Fix punctuation, capitalization and sentence breaks. Make only the smallest grammar fixes that the removals require.

Keep everything else as spoken: the words and their order, the language or mix of languages, names, numbers, terms, tone and formality. Do not summarize, shorten, rephrase, translate or add anything.

Keep the text in one paragraph, unless the speaker clearly dictates separate paragraphs or a list ("first… second…", "the list is…"): then put each item on its own line, starting with "- " or the number the speaker used. A sentence that names several things is not a list.`;

/// Mirrors `domain::clean_text::DEFAULT_FILLERS`: the list "clean text" starts with.
export const DEFAULT_FILLERS = "uh, uhh, uhm, umm, erm, hmm, hm, э, ээ, эээ, э-э, э-э-э, эм, эмм, мм, ммм, м-м";

/// Sent by the backend after every save, also one made from the tray menu.
export const SETTINGS_EVENT = "settings-changed";
/// Sent by the backend, with no payload, when the registered hotkeys changed without a save (a
/// hotkey field stopped recording and the paused hotkeys came back): the status is read again.
export const HOTKEYS_EVENT = "hotkeys-changed";

/// `deepgram` recognizes speech only.
/// `chatgpt_account`: ChatGPT through the user's account, on their Plus or Pro plan, no key.
export type ProviderKind = "anthropic" | "open_ai" | "open_ai_compatible" | "deepgram" | "chatgpt_account";

/// A service for language models, speech recognition or both; its key never comes back from
/// the backend.
export interface Connection {
  id: string;
  name: string;
  provider: ProviderKind;
  base_url: string | null;
  key_hint: string | null;
  added_at_ms: number;
  models: string[];
}

export type ModeAction =
  | { kind: "plain" }
  | { kind: "translate_to_english" }
  | {
      kind: "process";
      connection_id: string;
      model: string;
      instruction: string;
      output_language: string | null;
      /// What the model sees besides the dictation; the backend fills it in when missing.
      context?: ContextSources;
    };

/// Mirrors `domain::context::ContextSources`; all off by default.
export interface ContextSources {
  selection: boolean;
  clipboard: boolean;
  app: boolean;
}

export const NO_CONTEXT: ContextSources = { selection: false, clipboard: false, app: false };

/// The sources with `source` turned on or off; made from the sources saved last, so two quick
/// presses on different sources both count.
export function toggledContext(sources: ContextSources | undefined, source: keyof ContextSources): ContextSources {
  const current = sources ?? NO_CONTEXT;
  return { ...current, [source]: !current[source] };
}

export interface Mode {
  id: string;
  /// Empty for the built-in modes.
  name: string;
  action: ModeAction;
  hotkey: string | null;
  selection_hotkey: string | null;
}

export const BUILTIN_MODES = ["plain", "english"];

/// Translation to English: a mode there only when the user turns it on, shown as a hotkey of
/// plain dictation (mirrors `modes::english_mode`).
export const ENGLISH_MODE: Mode = {
  id: "english",
  name: "",
  action: { kind: "translate_to_english" },
  hotkey: null,
  selection_hotkey: null,
};

/// The modes listed on the Modes page and offered for the main hotkey: all but the translation
/// to English.
export function choosableModes(modes: Mode[]): Mode[] {
  return modes.filter((mode) => mode.id !== ENGLISH_MODE.id);
}

/// Turns the translation to English on or off; off, the main hotkey falls back to plain
/// dictation if it translated.
export function withEnglish(settings: Settings, on: boolean): Partial<Settings> {
  const others = settings.modes.filter((mode) => mode.id !== ENGLISH_MODE.id);
  if (on) return { modes: settings.modes.some((mode) => mode.id === ENGLISH_MODE.id) ? settings.modes : [...others, ENGLISH_MODE] };
  return { modes: others, main_mode_id: settings.main_mode_id === ENGLISH_MODE.id ? "plain" : settings.main_mode_id };
}

/// A new mode at the end of the list; once there (a second press of "Create"), nothing changes.
export function withNewMode(settings: Settings, mode: Mode): Partial<Settings> {
  return settings.modes.some((item) => item.id === mode.id) ? {} : { modes: [...settings.modes, mode] };
}

/// The mode `id` changed as `change` says, from the mode as saved last.
export function withModeChange(settings: Settings, id: string, change: (mode: Mode) => Partial<Mode>): Partial<Settings> {
  return { modes: settings.modes.map((item) => (item.id === id ? { ...item, ...change(item) } : item)) };
}

export type SaveError =
  | { kind: "read_only" }
  | { kind: "io"; detail: string }
  | { kind: "invalid_hotkey"; detail: string }
  | { kind: "hotkey_taken"; detail: string }
  | { kind: "invalid"; detail: string };

/// A change of the settings, or a function that makes it from the settings saved last.
export type SettingsChange = Partial<Settings> | ((current: Settings) => Partial<Settings>);

/// Saves one change after another; resolves with the error, or `null` once saved.
export type Update = (change: SettingsChange) => Promise<SaveError | null>;

/// Saves one change after another, each made from the settings saved last (`latest`): two quick
/// edits of a mode must not overwrite each other. `saved` gets the settings of each save that
/// went through, `refused` the error of each that did not; the promise of a change resolves with
/// that error, or `null` once saved. The status is not read here: every save that went through
/// is announced (`SETTINGS_EVENT`), and the window reads it then.
export function savesInOrder(
  save: (settings: Settings) => Promise<Settings>,
  latest: { current: Settings },
  saved: (settings: Settings) => void,
  refused: (error: SaveError) => void,
): Update {
  let queue: Promise<unknown> = Promise.resolve();
  return (change) => {
    const run = queue.then(async (): Promise<SaveError | null> => {
      const patch = typeof change === "function" ? change(latest.current) : change;
      try {
        const next = await save({ ...latest.current, ...patch });
        latest.current = next;
        saved(next);
        return null;
      } catch (error) {
        refused(error as SaveError);
        return error as SaveError;
      }
    });
    queue = run.catch(() => undefined);
    return run;
  };
}

/// Puts the saved value back in a field once its save is refused, so the field never shows what
/// was not saved.
export async function keepSaved(saving: Promise<SaveError | null>, restore: () => void): Promise<void> {
  if (await saving) restore();
}

/// What a field shows once the save of the text it showed (`sent`) was refused: the saved text
/// again, unless the user has typed on since; that text has a save of its own coming.
export function afterRefusal(shown: string, sent: string, saved: string): string {
  return shown === sent ? saved : shown;
}

/// Saves the text a field shows (`sent`); once the save is refused, the field (its state setter
/// `show`) shows the `saved` text again, unless the user has typed on since (`afterRefusal`).
export function saveText(
  saving: Promise<SaveError | null>,
  sent: string,
  saved: string,
  show: (change: (shown: string) => string) => void,
): Promise<void> {
  return keepSaved(saving, () => show((shown) => afterRefusal(shown, sent, saved)));
}

export const loadSettings = () => invoke<Settings>("get_settings");

/// Rejects with a `SaveError`.
export const saveSettings = (settings: Settings) => invoke<Settings>("save_settings", { settings });

export function resolveLanguage(choice: UiLanguage, systemLanguage: Language): Language {
  return choice === "system" ? systemLanguage : choice;
}

/// "As in the system" follows the OS; the stylesheets read the choice from `data-theme`.
export function applyTheme(theme: Theme) {
  const root = document.documentElement;
  if (theme === "system") delete root.dataset.theme;
  else root.dataset.theme = theme;
}
