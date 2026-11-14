import { describe, expect, it } from "vitest";
import {
  afterRefusal,
  choosableModes,
  ENGLISH_MODE,
  HISTORY_LIMIT_MAX,
  HOTKEYS_EVENT,
  keepSaved,
  parseHistoryLimit,
  resolveLanguage,
  savesInOrder,
  saveText,
  SETTINGS_EVENT,
  toggledContext,
  withEnglish,
  withModeChange,
  withNewMode,
  type Mode,
  type SaveError,
  type Settings,
} from "./settings";
import commandsSource from "../src-tauri/src/commands.rs?raw";

describe("translation to English", () => {
  const plain: Mode = { id: "plain", name: "", action: { kind: "plain" }, hotkey: null, selection_hotkey: null };
  const email: Mode = {
    id: "mode-1",
    name: "Email",
    action: { kind: "process", connection_id: "c1", model: "m", instruction: "x", output_language: null },
    hotkey: null,
    selection_hotkey: null,
  };
  const off = { modes: [plain, email], main_mode_id: "mode-1" } as Settings;

  it("is not a mode to choose", () => {
    expect(choosableModes([plain, ENGLISH_MODE, email])).toEqual([plain, email]);
  });

  it("turns on once and off with its hotkey", () => {
    const on = { ...off, ...withEnglish(off, true) };
    expect(on.modes).toEqual([plain, email, ENGLISH_MODE]);
    expect(withEnglish(on, true).modes).toBe(on.modes);
    const withHotkey = { ...on, modes: [plain, email, { ...ENGLISH_MODE, hotkey: "control+alt+KeyT" }] };
    expect(withEnglish(withHotkey, false)).toEqual({ modes: [plain, email], main_mode_id: "mode-1" });
  });

  it("gives the main hotkey back to plain dictation when it translated", () => {
    const main = { modes: [plain, ENGLISH_MODE], main_mode_id: "english" } as Settings;
    expect(withEnglish(main, false)).toEqual({ modes: [plain], main_mode_id: "plain" });
  });
});

describe("resolveLanguage", () => {
  it("follows the OS language for the system choice", () => {
    expect(resolveLanguage("system", "ru")).toBe("ru");
    expect(resolveLanguage("system", "en")).toBe("en");
  });

  it("keeps an explicit choice", () => {
    expect(resolveLanguage("en", "ru")).toBe("en");
    expect(resolveLanguage("ru", "en")).toBe("ru");
  });
});

describe("parseHistoryLimit", () => {
  it("takes any whole number from 1 to the largest", () => {
    expect(parseHistoryLimit("16")).toBe(16);
    expect(parseHistoryLimit(" 3 ")).toBe(3);
    expect(parseHistoryLimit("1")).toBe(1);
    expect(parseHistoryLimit(String(HISTORY_LIMIT_MAX))).toBe(HISTORY_LIMIT_MAX);
  });

  it("refuses anything else", () => {
    for (const text of ["", "0", "-5", "2.5", "1e3", "abc", "10 000", String(HISTORY_LIMIT_MAX + 1)]) {
      expect(parseHistoryLimit(text)).toBeNull();
    }
  });
});

describe("modes", () => {
  const plain: Mode = { id: "plain", name: "", action: { kind: "plain" }, hotkey: null, selection_hotkey: null };
  const email: Mode = {
    id: "mode-1",
    name: "Email",
    action: { kind: "process", connection_id: "c1", model: "m", instruction: "x", output_language: null },
    hotkey: null,
    selection_hotkey: null,
  };

  it("are created once, however often Create is pressed", () => {
    const before = { modes: [plain] } as Settings;
    const after = { ...before, ...withNewMode(before, email) };
    expect(after.modes).toEqual([plain, email]);
    expect(withNewMode(after, email)).toEqual({});
  });

  it("change only the mode named, from the mode as saved", () => {
    const settings = { modes: [plain, { ...email, name: "Mail" }] } as Settings;
    const changed = withModeChange(settings, "mode-1", (mode) => ({ name: `${mode.name}!`, hotkey: "control+KeyM" }));
    expect(changed.modes).toEqual([plain, { ...email, name: "Mail!", hotkey: "control+KeyM" }]);
    expect(withModeChange(settings, "gone", () => ({ name: "x" })).modes).toEqual(settings.modes);
  });

  it("count every quick press on the context sources, each made from the mode as saved", async () => {
    const latest = { current: { modes: [plain, email] } as Settings };
    const update = savesInOrder(
      async (settings) => {
        await new Promise((resolve) => setTimeout(resolve, 5));
        return settings;
      },
      latest,
      () => {},
      () => {},
    );
    const toggle = (source: "selection" | "clipboard" | "app") =>
      update((current) =>
        withModeChange(current, "mode-1", (mode) =>
          mode.action.kind === "process" ? { action: { ...mode.action, context: toggledContext(mode.action.context, source) } } : {},
        ),
      );
    // Both pressed before the first one is saved.
    await Promise.all([toggle("selection"), toggle("clipboard")]);
    const action = latest.current.modes[1].action;
    expect(action.kind === "process" && action.context).toEqual({ selection: true, clipboard: true, app: false });
    // A second press on the same source turns it off again.
    expect(toggledContext({ selection: true, clipboard: true, app: false }, "selection")).toEqual({ selection: false, clipboard: true, app: false });
  });
});

describe("keepSaved", () => {
  it("puts the saved value back only when the save is refused", async () => {
    let restored = 0;
    await keepSaved(Promise.resolve(null), () => restored++);
    expect(restored).toBe(0);
    await keepSaved(Promise.resolve({ kind: "io", detail: "disk full" }), () => restored++);
    expect(restored).toBe(1);
  });
});

describe("the events the windows listen to", () => {
  it("have the backend's names", () => {
    expect(commandsSource).toContain(`pub const SETTINGS_EVENT: &str = "${SETTINGS_EVENT}";`);
    expect(commandsSource).toContain(`pub const HOTKEYS_EVENT: &str = "${HOTKEYS_EVENT}";`);
  });
});

describe("saveText", () => {
  // A field's state: what it shows, changed the way a state setter is.
  function field(text: string) {
    const state = { shown: text };
    return { state, show: (change: (shown: string) => string) => (state.shown = change(state.shown)) };
  }

  it("shows the saved text again once the save is refused", async () => {
    const { state, show } = field("Ivan, um");
    await saveText(Promise.resolve({ kind: "io", detail: "disk full" }), "Ivan, um", "um", show);
    expect(state.shown).toBe("um");
  });

  it("keeps a text that was saved, and what was typed on since a refusal", async () => {
    const saved = field("Ivan");
    await saveText(Promise.resolve(null), "Ivan", "", saved.show);
    expect(saved.state.shown).toBe("Ivan");
    const typed = field("Ivan");
    const saving = saveText(Promise.resolve({ kind: "read_only" }), "Ivan", "", typed.show);
    typed.state.shown = "Ivan Petrov";
    await saving;
    expect(typed.state.shown).toBe("Ivan Petrov");
  });
});

describe("afterRefusal", () => {
  it("shows the saved text again once the text sent is refused", () => {
    expect(afterRefusal("Kubernetes, gRPC", "Kubernetes, gRPC", "Kubernetes")).toBe("Kubernetes");
  });

  it("keeps what the user typed on since: its own save follows", () => {
    expect(afterRefusal("Kubernetes, gRPC, Rust", "Kubernetes, gRPC", "Kubernetes")).toBe("Kubernetes, gRPC, Rust");
  });
});

describe("savesInOrder", () => {
  const base = { theme: "system", history_limit: 10 } as unknown as Settings;

  it("saves one change after another, each made from the settings saved last", async () => {
    const latest = { current: base };
    const sent: Partial<Settings>[] = [];
    const shown: Settings[] = [];
    const update = savesInOrder(
      async (settings) => {
        sent.push(settings);
        await new Promise((resolve) => setTimeout(resolve, 5));
        return settings;
      },
      latest,
      (saved) => shown.push(saved),
      () => {},
    );
    const first = update({ theme: "dark" });
    const second = update((current) => ({ history_limit: current.theme === "dark" ? 20 : 0 }));
    expect(await Promise.all([first, second])).toEqual([null, null]);
    expect(sent[1]).toMatchObject({ theme: "dark", history_limit: 20 });
    expect(shown).toHaveLength(2);
    expect(latest.current).toMatchObject({ theme: "dark", history_limit: 20 });
  });

  it("resolves a refused change with its error, never rejects, and goes on with the next", async () => {
    const latest = { current: base };
    const refused: SaveError[] = [];
    let refuse = true;
    const update = savesInOrder(
      async (settings) => {
        if (refuse) {
          refuse = false;
          throw { kind: "read_only" };
        }
        return settings;
      },
      latest,
      () => {},
      (error) => refused.push(error),
    );
    expect(await update({ theme: "dark" })).toEqual({ kind: "read_only" });
    expect(refused).toEqual([{ kind: "read_only" }]);
    expect(latest.current).toBe(base);
    expect(await update({ theme: "light" })).toBeNull();
    expect(latest.current).toMatchObject({ theme: "light" });
  });
});
