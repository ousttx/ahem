import { describe, expect, it } from "vitest";
import { addRule, appKey, appRulesView, focusAfterRule, offeredApps, removeRule, ruleFor, rulesOf, typedApp, type AppId } from "./appRules";
import type { Settings } from "./settings";

const telegram: AppId = { id: "ru.keepcoder.Telegram", name: "Telegram" };
const notepad: AppId = { id: "Notepad.exe", name: "Notepad" };

function settings(app_rules: Settings["app_rules"] = []): Settings {
  return { app_rules } as Settings;
}

describe("app rules", () => {
  it("compares program names as the backend does", () => {
    expect(appKey(" Telegram.EXE ")).toBe("telegram");
    expect(appKey(".exe")).toBe(".exe");
    expect(ruleFor(settings([{ app_id: "notepad", app_name: "notepad", mode_id: "email" }]), notepad)?.mode_id).toBe("email");
  });

  it("ties a program to one mode only", () => {
    let current = settings();
    current = { ...current, ...addRule(current, telegram, "short") };
    current = { ...current, ...addRule(current, notepad, "short") };
    expect(rulesOf(current, "short").map((rule) => rule.app_name)).toEqual(["Telegram", "Notepad"]);
    current = { ...current, ...addRule(current, typedApp(" telegram "), "email") };
    expect(rulesOf(current, "short").map((rule) => rule.app_name)).toEqual(["Notepad"]);
    expect(rulesOf(current, "email")).toEqual([{ app_id: "telegram", app_name: "telegram", mode_id: "email" }]);
    expect(addRule(current, typedApp("  "), "email").app_rules).toBe(current.app_rules);
  });

  it("removes a rule", () => {
    const current = settings([{ app_id: "Notepad.exe", app_name: "Notepad", mode_id: "email" }]);
    expect(removeRule(current, current.app_rules[0]).app_rules).toEqual([]);
  });

  it("offers the recent programs the mode does not have", () => {
    const current = settings([{ app_id: telegram.id, app_name: "Telegram", mode_id: "short" }]);
    expect(offeredApps(current, [telegram, notepad], "short")).toEqual([notepad]);
    expect(offeredApps(current, [telegram, notepad], "email")).toEqual([telegram, notepad]);
  });
});

describe("the programs in a mode's editor", () => {
  const rules: Settings["app_rules"] = [
    { app_id: telegram.id, app_name: "Telegram", mode_id: "short" },
    { app_id: notepad.id, app_name: "Notepad", mode_id: "email" },
  ];
  const view = (main_mode_id: string, modeId: string, app_rules = rules) => appRulesView({ app_rules, main_mode_id } as Settings, modeId);

  it("shows the main mode's programs from before, to be removed, with a note that they wait", () => {
    // The mode became the main one with its programs: they stay, nothing removes them.
    expect(view("short", "short")).toEqual({ rules: [rules[0]], main: true, waiting: true });
  });

  it("shows only the main mode's note without programs", () => {
    expect(view("short", "short", [rules[1]])).toEqual({ rules: [], main: true, waiting: false });
  });

  it("shows another mode's programs, to add to and remove", () => {
    expect(view("plain", "short")).toEqual({ rules: [rules[0]], main: false, waiting: false });
  });
});

describe("focusAfterRule", () => {
  it("moves the focus to another program, else to the field for a name", () => {
    expect(focusAfterRule(["telegram", "notepad"], "telegram", false)).toBe("rule");
    expect(focusAfterRule(["telegram", "notepad"], "notepad", true)).toBe("rule");
    expect(focusAfterRule(["telegram"], "telegram", false)).toBe("field");
  });

  it("moves it to the list of modes once the main mode's last program is gone: it has no field", () => {
    // The editor's first control would take it, and an arrow there turns on cleaning by a model.
    expect(focusAfterRule(["telegram"], "telegram", true)).toBe("list");
  });
});
