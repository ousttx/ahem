import { describe, expect, it } from "vitest";
import { ENGLISH_MODE, type Mode, type Settings } from "./settings";
import { modeLaunch, shortcuts } from "./shortcuts";

const plain: Mode = { id: "plain", name: "", action: { kind: "plain" }, hotkey: null, selection_hotkey: null };
const business: Mode = {
  id: "mode-business",
  name: "Business",
  action: { kind: "process", connection_id: "c", model: "m", instruction: "Rewrite", output_language: null },
  hotkey: "control+alt+KeyB",
  selection_hotkey: "control+alt+KeyR",
};
const letter: Mode = { ...business, id: "mode-letter", name: "Letter", hotkey: null, selection_hotkey: null };

const settings: Pick<Settings, "hotkey" | "app_hotkey" | "modes" | "main_mode_id" | "app_rules"> = {
  hotkey: "control+alt+KeyD",
  app_hotkey: null,
  modes: [plain, business, letter],
  main_mode_id: "plain",
  app_rules: [
    { app_id: "telegram", app_name: "Telegram", mode_id: "mode-business" },
    { app_id: "slack", app_name: "Slack", mode_id: "mode-business" },
  ],
};

describe("shortcuts", () => {
  it("lists every hotkey: dictation first, then the selected text", () => {
    expect(shortcuts(settings).map((item) => [item.kind, "mode" in item ? item.mode.id : null, item.hotkey])).toEqual([
      ["main", "plain", "control+alt+KeyD"],
      ["app", null, null],
      ["mode", "mode-business", "control+alt+KeyB"],
      ["mode", "mode-letter", null],
      ["selection", "mode-business", "control+alt+KeyR"],
      ["selection", "mode-letter", null],
    ]);
  });

  it("names the main mode and lists the translation to English only while it is on", () => {
    const english = { ...ENGLISH_MODE, hotkey: "control+alt+KeyE" };
    const list = shortcuts({ ...settings, modes: [plain, english, business], main_mode_id: "mode-business" });
    expect(list[0]).toMatchObject({ kind: "main", mode: { id: "mode-business" } });
    expect(list[2]).toMatchObject({ kind: "mode", hotkey: "control+alt+KeyE", mode: { id: "english" } });
    // Plain dictation runs on the main hotkey and has no row of its own.
    expect(list.some((item) => item.kind !== "main" && "mode" in item && item.mode.id === "plain")).toBe(false);
  });
});

describe("modeLaunch", () => {
  it("tells how a mode is started", () => {
    expect(modeLaunch(settings, business)).toEqual({ hotkey: "control+alt+KeyB", selection: "control+alt+KeyR", apps: 2 });
    expect(modeLaunch(settings, plain)).toEqual({ hotkey: null, selection: null, apps: 0 });
  });
});
