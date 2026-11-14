import { ENGLISH_MODE, type Mode, type Settings } from "./settings";

/// One hotkey of the app and what it does.
export type Shortcut =
  /// Dictation with the main mode; it cannot be removed.
  | { kind: "main"; hotkey: string; mode: Mode }
  /// Dictation with the mode set for the program in focus, else the main mode.
  | { kind: "app"; hotkey: string | null }
  /// Dictation with a mode of its own, the translation to English among them.
  | { kind: "mode"; hotkey: string | null; mode: Mode }
  /// The selected text rewritten by a mode with a language model.
  | { kind: "selection"; hotkey: string | null; mode: Mode };

/// Every hotkey, in the order of the Keys tab: dictation (main, by application, to English when
/// it is on, then each own mode), then the selected text of the modes with a language model.
/// Plain dictation has no hotkey of its own: it runs on the main one.
export function shortcuts(settings: Pick<Settings, "hotkey" | "app_hotkey" | "modes" | "main_mode_id">): Shortcut[] {
  const main = settings.modes.find((mode) => mode.id === settings.main_mode_id) ?? settings.modes[0];
  const english = settings.modes.find((mode) => mode.id === ENGLISH_MODE.id);
  const own = settings.modes.filter((mode) => mode.action.kind === "process");
  return [
    { kind: "main", hotkey: settings.hotkey, mode: main },
    { kind: "app", hotkey: settings.app_hotkey },
    ...(english ? [{ kind: "mode" as const, hotkey: english.hotkey, mode: english }] : []),
    ...own.map((mode) => ({ kind: "mode" as const, hotkey: mode.hotkey, mode })),
    ...own.map((mode) => ({ kind: "selection" as const, hotkey: mode.selection_hotkey, mode })),
  ];
}

/// How a mode is started, for its row in the list of modes: its hotkey, its selected-text
/// hotkey and the number of programs where the "by application" hotkey takes it.
export function modeLaunch(settings: Pick<Settings, "app_rules">, mode: Mode): { hotkey: string | null; selection: string | null; apps: number } {
  return {
    hotkey: mode.hotkey,
    selection: mode.action.kind === "process" ? mode.selection_hotkey : null,
    apps: settings.app_rules.filter((rule) => rule.mode_id === mode.id).length,
  };
}
