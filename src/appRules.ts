// Modes by application (`domain::app_rules`): the user ties a mode to a program, and the "by
// application" hotkey (`app_hotkey`) dictates with that mode there; the main hotkey keeps the
// main mode everywhere. One program has one rule; only the user adds them.
import type { Settings } from "./settings";

/// A program as the OS names it (`domain::app_rules::AppId`).
export interface AppId {
  id: string;
  name: string;
}

export interface AppRule {
  /// The program's id, or a name the user typed.
  app_id: string;
  app_name: string;
  mode_id: string;
}

/// Mirrors `app_rules::key`: names compare without the case, the spaces around and a `.exe`.
export function appKey(text: string): string {
  const key = text.trim().toLowerCase();
  return key.endsWith(".exe") && key.length > 4 ? key.slice(0, -4) : key;
}

/// Whether the rule stands for `app`: its id or its name is the program's id or name. Wider than
/// the backend's match (the rule's id only), so a typed name and a picked program are one program.
function sameApp(rule: AppRule, app: AppId): boolean {
  const names = [appKey(app.id), appKey(app.name)].filter((key) => key !== "");
  return [appKey(rule.app_id), appKey(rule.app_name)].some((key) => key !== "" && names.includes(key));
}

export function rulesOf(settings: Settings, modeId: string): AppRule[] {
  return settings.app_rules.filter((rule) => rule.mode_id === modeId);
}

/// The rule that already names `app`, of any mode.
export function ruleFor(settings: Settings, app: AppId): AppRule | undefined {
  return settings.app_rules.find((rule) => sameApp(rule, app));
}

/// Ties `app` to the mode; a rule of another mode for the same program moves here.
export function addRule(settings: Settings, app: AppId, modeId: string): Pick<Settings, "app_rules"> {
  const id = app.id.trim();
  if (!appKey(id)) return { app_rules: settings.app_rules };
  const others = settings.app_rules.filter((rule) => !sameApp(rule, app));
  return { app_rules: [...others, { app_id: id, app_name: app.name.trim() || id, mode_id: modeId }] };
}

export function removeRule(settings: Settings, rule: AppRule): Pick<Settings, "app_rules"> {
  return { app_rules: settings.app_rules.filter((item) => appKey(item.app_id) !== appKey(rule.app_id)) };
}

/// A name the user typed stands for the program: it matches its id or its name.
export function typedApp(name: string): AppId {
  return { id: name.trim(), name: name.trim() };
}

/// What a mode's editor shows of its programs. The main mode takes none (`main`): the per-app
/// shortcut uses it wherever no rule applies. Programs it had before it became the main mode stay
/// as they are, shown to be removed, with a note (`waiting`): there the per-app shortcut keeps
/// this mode once another mode is the main one. Nothing removes them by itself.
export function appRulesView(settings: Settings, modeId: string): { rules: AppRule[]; main: boolean; waiting: boolean } {
  const rules = rulesOf(settings, modeId);
  const main = settings.main_mode_id === modeId;
  return { rules, main, waiting: main && rules.length > 0 };
}

/// Where the focus goes as the program `removed` is taken off a mode's programs (`rules`, by
/// id): to the program in its place or the one before (`"rule"`), else to the field for a
/// program's name; the main mode has no field (`main`): to the list of modes (`"list"`), where no
/// key changes a setting by itself.
export function focusAfterRule(rules: readonly string[], removed: string, main: boolean): "rule" | "field" | "list" {
  if (rules.some((id) => id !== removed)) return "rule";
  return main ? "list" : "field";
}

/// The recent programs this mode does not have yet.
export function offeredApps(settings: Settings, recent: AppId[], modeId: string): AppId[] {
  return recent.filter((app) => ruleFor(settings, app)?.mode_id !== modeId);
}
