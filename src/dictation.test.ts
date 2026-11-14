import { afterEach, describe, expect, it, vi } from "vitest";
import {
  fileRunning,
  forgetEngineFailure,
  formatHotkey,
  hotkeyFromKey,
  hotkeyKeys,
  levelAfter,
  shortcutModifierNames,
  initialView,
  phase,
  reduce,
  type View,
} from "./dictation";
import type { CoreEvent, DictationResult } from "./events";
import hotkeysSource from "../src-tauri/src/hotkeys.rs?raw";

const result: DictationResult = {
  mode_id: "plain",
  transcript: "hello",
  language: "en",
  processed: null,
  inserted: true,
};

function apply(events: CoreEvent[], view: View = initialView(null)): View {
  return events.reduce(reduce, view);
}

const state = (session: number, kind: "recording" | "transcribing" | "post_processing" | "inserting"): CoreEvent => ({
  source: "session",
  event: { type: "state_changed", session, state: { kind } },
});

describe("reduce", () => {
  it("follows one dictation to the end", () => {
    let view = apply([state(1, "recording")]);
    expect(phase(view)).toBe("recording");
    // The level does not change the view: the windows would draw anew twenty times a second.
    expect(apply([{ source: "dictation", event: { type: "level", permille: 250 } }], view)).toBe(view);
    view = apply([state(1, "transcribing"), state(1, "inserting")], view);
    expect(phase(view)).toBe("inserting");
    view = apply(
      [
        { source: "session", event: { type: "state_changed", session: 1, state: { kind: "inserted" } } },
        { source: "session", event: { type: "finished", session: 1, result } },
      ],
      view,
    );
    expect(phase(view)).toBe("idle");
    expect(view.notice).toEqual({ kind: "inserted" });
    expect(view.last).toEqual(result);
  });

  it("marks a recording that takes context, until the next one", () => {
    const taken: CoreEvent = { source: "dictation", event: { type: "context_taken", session: 1 } };
    let view = apply([state(1, "recording"), taken]);
    expect(view.contextTaken).toBe(true);
    view = apply([state(1, "transcribing"), state(2, "recording")], view);
    expect(view.contextTaken).toBe(false);
    view = apply([taken], view);
    expect(view.contextTaken).toBe(false);
  });

  it("remembers the mode a program's rule chose for the recording", () => {
    const chosen: CoreEvent = { source: "dictation", event: { type: "mode_by_app", session: 1, mode_id: "short" } };
    let view = apply([state(1, "recording"), chosen]);
    expect(view.modeByApp).toBe("short");
    view = apply([state(1, "transcribing"), state(2, "recording")], view);
    expect(view.modeByApp).toBeNull();
    // A late event of a recording that ended does not label the next one.
    view = apply([chosen], view);
    expect(view.modeByApp).toBeNull();
  });

  it("tells processing by a language model from recognition", () => {
    let view = apply([state(1, "recording"), state(1, "transcribing")]);
    expect(phase(view)).toBe("transcribing");
    view = apply([state(1, "post_processing")], view);
    expect(phase(view)).toBe("processing");
    view = apply([state(1, "inserting")], view);
    expect(phase(view)).toBe("inserting");
  });

  it("shows recording while an earlier dictation is still transcribed", () => {
    const view = apply([state(1, "recording"), state(1, "transcribing"), state(2, "recording")]);
    expect(phase(view)).toBe("recording");
  });

  it("keeps a failure until the next recording starts", () => {
    const failed: CoreEvent = {
      source: "session",
      event: {
        type: "state_changed",
        session: 1,
        state: { kind: "failed", failure: { kind: "focus_changed", detail: "" } },
      },
    };
    let view = apply([state(1, "recording"), failed]);
    expect(view.notice).toEqual({ kind: "failed", failure: { kind: "focus_changed", detail: "" } });
    view = apply([state(2, "recording")], view);
    expect(view.notice).toBeNull();
  });

  it("reports blocked dictations and engine failures", () => {
    let view = apply([{ source: "dictation", event: { type: "blocked", reason: "no_model", detail: "" } }]);
    expect(view.notice).toEqual({ kind: "blocked", reason: "no_model", detail: "" });
    view = apply([{ source: "engine", event: { type: "failed", detail: "no GPU" } }], view);
    expect(view.notice).toEqual({ kind: "engine_failed", detail: "no GPU" });
    view = apply([{ source: "engine", event: { type: "state_changed", state: "ready" } }], view);
    expect(view.engine).toBe("ready");
  });

  it("lets an engine failure go once the engine loads again, with this model or another", () => {
    const failed = apply([
      { source: "engine", event: { type: "state_changed", state: "unloaded" } },
      { source: "engine", event: { type: "failed", detail: "exit 3" } },
    ]);
    expect(failed.notice).toEqual({ kind: "engine_failed", detail: "exit 3" });
    for (const state of ["loading", "ready"] as const) {
      const again = apply([{ source: "engine", event: { type: "state_changed", state } }], failed);
      expect(again.notice, state).toBeNull();
      expect(again.engine, state).toBe(state);
    }
    // Stopping says nothing of the failure.
    expect(apply([{ source: "engine", event: { type: "state_changed", state: "unloading" } }], failed).notice).toEqual(failed.notice);
    // Another notice stays as the engine loads.
    const blocked = apply([{ source: "dictation", event: { type: "blocked", reason: "microphone", detail: "busy" } }]);
    expect(apply([{ source: "engine", event: { type: "state_changed", state: "loading" } }], blocked).notice).toEqual(blocked.notice);
  });

  it("lets an engine failure go when another model is chosen, and nothing else", () => {
    const failed = apply([{ source: "engine", event: { type: "failed", detail: "exit 3" } }]);
    expect(forgetEngineFailure(failed).notice).toBeNull();
    const inserted = apply([state(1, "recording"), { source: "session", event: { type: "state_changed", session: 1, state: { kind: "inserted" } } }]);
    // The same view: the window does not draw anew.
    expect(forgetEngineFailure(inserted)).toBe(inserted);
  });

  it("keeps a language-model failure visible after the transcript is inserted", () => {
    const failure = { kind: "processing" as const, detail: "401" };
    const view = apply([
      state(1, "recording"),
      { source: "session", event: { type: "processing_failed", session: 1, failure } },
      state(1, "inserting"),
      { source: "session", event: { type: "state_changed", session: 1, state: { kind: "inserted" } } },
    ]);
    expect(view.notice).toEqual({ kind: "failed", failure });
    expect(view.processingFailures).toEqual({});
  });

  it("keeps a text recognized again as a success, not a failure", () => {
    const kept = { ...result, inserted: false };
    let view = apply([
      { source: "dictation", event: { type: "retry_available", available: false } },
      state(2, "recording"),
      state(2, "transcribing"),
    ]);
    expect(phase(view)).toBe("transcribing");
    view = apply(
      [
        { source: "session", event: { type: "state_changed", session: 2, state: { kind: "kept" } } },
        { source: "session", event: { type: "finished", session: 2, result: kept } },
      ],
      view,
    );
    expect(phase(view)).toBe("idle");
    expect(view.sessions).toEqual({});
    expect(view.notice).toEqual({ kind: "kept" });
    expect(view.last).toEqual(kept);
  });

  it("says that a text recognized again was not processed when its language model failed", () => {
    const failure = { kind: "processing" as const, detail: "401" };
    const view = apply([
      state(3, "recording"),
      state(3, "post_processing"),
      { source: "session", event: { type: "processing_failed", session: 3, failure } },
      { source: "session", event: { type: "state_changed", session: 3, state: { kind: "kept" } } },
    ]);
    expect(view.notice).toEqual({ kind: "kept_unprocessed", failure });
    expect(view.processingFailures).toEqual({});
    // The failure belongs to its own dictation only.
    const next = apply([state(4, "recording"), { source: "session", event: { type: "state_changed", session: 4, state: { kind: "kept" } } }], view);
    expect(next.notice).toEqual({ kind: "kept" });
  });

  it("keeps a microphone that went away midway until the next recording", () => {
    const lost: CoreEvent = { source: "dictation", event: { type: "microphone_lost", session: 1, detail: "device disconnected" } };
    let view = apply([state(1, "recording"), lost, state(1, "transcribing")]);
    expect(view.microphoneLost).toEqual({ session: 1, detail: "device disconnected" });
    // The text of what came before still goes in.
    view = apply([{ source: "session", event: { type: "state_changed", session: 1, state: { kind: "inserted" } } }], view);
    expect(view.notice).toEqual({ kind: "inserted" });
    expect(view.noticeSession).toBe(1);
    expect(view.microphoneLost).toEqual({ session: 1, detail: "device disconnected" });
    view = apply([state(2, "recording")], view);
    expect(view.microphoneLost).toBeNull();
  });

  it("keeps a microphone that went away for its dictation's outcome when the next one begins first", () => {
    const lost: CoreEvent = { source: "dictation", event: { type: "microphone_lost", session: 1, detail: "gone" } };
    const inserted = (session: number): CoreEvent => ({ source: "session", event: { type: "state_changed", session, state: { kind: "inserted" } } });
    // The second dictation begins while the first one is still recognized.
    let view = apply([state(1, "recording"), lost, state(1, "transcribing"), state(2, "recording")]);
    expect(view.microphoneLost).toEqual({ session: 1, detail: "gone" });
    view = apply([state(2, "transcribing"), inserted(2), inserted(1)], view);
    expect(view.noticeSession).toBe(1);
    expect(view.microphoneLost).toEqual({ session: 1, detail: "gone" });
    // A dictation begun after its outcome takes it away.
    view = apply([state(3, "recording")], view);
    expect(view.microphoneLost).toBeNull();
  });

  it("ties a notice to its dictation, and to nothing when it is not a dictation's outcome", () => {
    let view = apply([state(3, "recording"), { source: "session", event: { type: "state_changed", session: 3, state: { kind: "inserted" } } }]);
    expect(view.noticeSession).toBe(3);
    view = apply([{ source: "dictation", event: { type: "selection_done" } }], view);
    expect(view.noticeSession).toBeNull();
    view = apply([state(4, "recording")], view);
    expect(view.noticeSession).toBeNull();
  });

  it("follows whether a recording that was not recognized waits to go again", () => {
    expect(initialView(null).retry).toBe(false);
    expect(initialView(null, true).retry).toBe(true);
    const available = (on: boolean): CoreEvent => ({ source: "dictation", event: { type: "retry_available", available: on } });
    let view = apply([
      state(1, "recording"),
      state(1, "transcribing"),
      {
        source: "session",
        event: { type: "state_changed", session: 1, state: { kind: "failed", failure: { kind: "transcription", detail: "HTTP 503" } } },
      },
      available(true),
    ]);
    expect(view.retry).toBe(true);
    // Other events leave it, a blocked press too: the recording is still there.
    view = apply(
      [
        { source: "dictation", event: { type: "blocked", reason: "no_engine", detail: "" } },
        { source: "dictation", event: { type: "level", permille: 10 } },
      ],
      view,
    );
    expect(view.retry).toBe(true);
    view = apply([available(false), state(2, "recording")], view);
    expect(view.retry).toBe(false);
  });

  it("follows a selected-text rewrite", () => {
    let view = apply([{ source: "dictation", event: { type: "selection_started", mode_id: "email" } }]);
    expect(phase(view)).toBe("rewriting");
    view = apply(
      [{ source: "dictation", event: { type: "selection_failed", problem: "read_only", detail: "" } }],
      view,
    );
    expect(phase(view)).toBe("idle");
    expect(view.notice).toEqual({ kind: "selection_failed", problem: "read_only", detail: "" });
  });

  it("reports a dictation that did not reach the history beside its outcome, until the next dictation", () => {
    let view = apply([{ source: "dictation", event: { type: "history_saved" } }]);
    expect(view.notice).toBeNull();
    expect(view.historyFailed).toBeNull();
    // A text that was not inserted stays the outcome: it waits in the window to be copied.
    const failure = { kind: "focus_changed" as const, detail: "" };
    view = apply(
      [
        state(1, "recording"),
        state(1, "inserting"),
        { source: "session", event: { type: "state_changed", session: 1, state: { kind: "failed", failure } } },
        { source: "session", event: { type: "finished", session: 1, result: { ...result, inserted: false } } },
        { source: "dictation", event: { type: "history_failed", detail: "disk" } },
      ],
      view,
    );
    expect(view.notice).toEqual({ kind: "failed", failure });
    expect(view.historyFailed).toEqual({ session: 1, detail: "disk" });
    // Transcribing a kept recording again is a new dictation too.
    const retried = apply([state(2, "transcribing")], view);
    expect(retried.historyFailed).toBeNull();
    expect(apply([state(2, "recording")], view).historyFailed).toBeNull();
  });

  it("follows a file from start to the saved result", () => {
    let view = apply([
      { source: "file", event: { type: "started", file: 1, name: "talk.mp3" } },
      { source: "file", event: { type: "progress", file: 1, done_ms: 27_000, total_ms: 60_000 } },
    ]);
    expect(view.file).toEqual({ kind: "transcribing", file: 1, name: "talk.mp3", done_ms: 27_000, total_ms: 60_000 });
    expect(fileRunning(view.file)).toBe(true);
    // Telling the voices apart still counts as running.
    view = apply([{ source: "file", event: { type: "speakers", file: 1, step: "segments", done_ms: 0, total_ms: 60_000 } }], view);
    expect(view.file).toEqual({ kind: "speakers", file: 1, name: "talk.mp3", step: "segments", done_ms: 0, total_ms: 60_000, since: expect.any(Number) });
    expect(fileRunning(view.file)).toBe(true);
    // The second model: how far it is.
    view = apply([{ source: "file", event: { type: "speakers", file: 1, step: "voices", done_ms: 15_000, total_ms: 60_000 } }], view);
    expect(view.file).toEqual({ kind: "speakers", file: 1, name: "talk.mp3", step: "voices", done_ms: 15_000, total_ms: 60_000, since: expect.any(Number) });
    // A second file is refused without hiding the first one.
    view = apply([{ source: "file", event: { type: "failed", file: 2, problem: "busy", detail: "", transcript: null, held: false } }], view);
    expect(view.file?.file).toBe(1);
    view = apply(
      [{ source: "file", event: { type: "finished", file: 1, path: "C:\\talk.md", processing_error: null, transcript: 7 } }],
      view,
    );
    // The entry kept in the list of transcripts comes with it.
    expect(view.file).toEqual({
      kind: "finished",
      file: 1,
      name: "talk.mp3",
      path: "C:\\talk.md",
      processing_error: null,
      transcript: 7,
    });
    expect(fileRunning(view.file)).toBe(false);
  });

  it("keeps the entry of a transcript whose result could not be written, and whether the result is held to be saved elsewhere", () => {
    const view = apply([
      { source: "file", event: { type: "started", file: 3, name: "talk.mp3" } },
      { source: "file", event: { type: "failed", file: 3, problem: "save", detail: "disk full", transcript: 9, held: true } },
    ]);
    expect(view.file).toEqual({ kind: "failed", file: 3, name: "talk.mp3", problem: "save", detail: "disk full", transcript: 9, held: true });
    const text = apply([
      { source: "file", event: { type: "started", file: 4, name: "notes.txt" } },
      { source: "file", event: { type: "failed", file: 4, problem: "save", detail: "disk full", transcript: null, held: true } },
    ]);
    expect(text.file).toMatchObject({ kind: "failed", transcript: null, held: true });
  });

  describe("the time of telling a file's voices apart", () => {
    afterEach(() => vi.useRealTimers());
    const speakers = (file: number, step: "segments" | "voices"): CoreEvent => ({
      source: "file",
      event: { type: "speakers", file, step, done_ms: 0, total_ms: null },
    });
    const since = (view: View) => (view.file?.kind === "speakers" ? view.file.since : null);

    it("counts from the start of the step, not from when it is shown", () => {
      vi.useFakeTimers();
      vi.setSystemTime(10_000);
      let view = apply([speakers(1, "segments")]);
      expect(since(view)).toBe(10_000);
      vi.setSystemTime(25_000);
      view = apply([speakers(1, "segments")], view);
      expect(since(view)).toBe(10_000);
      // A new step, or the same step of another file, starts again.
      expect(since(apply([speakers(1, "voices")], view))).toBe(25_000);
      expect(since(apply([speakers(2, "segments")], view))).toBe(25_000);
    });
  });

  it("tracks downloads", () => {
    let view = apply([
      { source: "model", event: { type: "download_progress", model: "m", downloaded: 5, total: 10 } },
    ]);
    expect(view.downloads.m).toEqual({ downloaded: 5, total: 10 });
    view = apply([{ source: "model", event: { type: "unpacking", model: "m" } }], view);
    expect(view.downloads.m).toEqual({ downloaded: 5, total: 10, unpacking: true });
    view = apply([{ source: "model", event: { type: "download_failed", model: "m", failure: "network", detail: "" } }], view);
    expect(view.downloads.m.failure).toBe("network");
    view = apply([{ source: "model", event: { type: "download_finished", model: "m" } }], view);
    expect(view.downloads.m).toBeUndefined();
  });

  it("drops the last failure when an archive is unpacked again", () => {
    const failed = apply([
      { source: "model", event: { type: "download_failed", model: "m", failure: "no_space", detail: "" } },
    ]);
    const view = apply([{ source: "model", event: { type: "unpacking", model: "m" } }], failed);
    expect(view.downloads.m.failure).toBeUndefined();
    expect(view.downloads.m.unpacking).toBe(true);
  });
});

describe("levelAfter", () => {
  it("follows the level and starts each recording at zero", () => {
    expect(levelAfter(0, { source: "dictation", event: { type: "level", permille: 250 } })).toBe(0.25);
    expect(levelAfter(0.25, state(2, "transcribing"))).toBe(0.25);
    expect(levelAfter(0.25, state(2, "recording"))).toBe(0);
    expect(levelAfter(0.4, { source: "dictation", event: { type: "history_saved" } })).toBe(0.4);
  });
});

describe("hotkeys", () => {
  const key = (code: string, mods: Partial<Record<"ctrlKey" | "altKey" | "shiftKey" | "metaKey", boolean>> = {}) => ({
    code,
    ctrlKey: false,
    altKey: false,
    shiftKey: false,
    metaKey: false,
    ...mods,
  });

  it("builds a hotkey from a key press", () => {
    expect(hotkeyFromKey(key("Space", { ctrlKey: true, altKey: true }))).toBe("Control+Alt+Space");
    expect(hotkeyFromKey(key("KeyZ", { shiftKey: true }))).toBe("Shift+KeyZ");
    expect(hotkeyFromKey(key("F9"))).toBe("F9");
  });

  it("takes no key that types or moves the cursor without a modifier, as the backend refuses it", () => {
    for (const code of ["Tab", "Enter", "Space", "Backspace", "Delete", "Escape", "KeyA", "Digit1", "Numpad5", "NumpadEnter", "ArrowUp", "Home", "PageDown", "Comma", "Slash", "Backquote"]) {
      expect(hotkeyFromKey(key(code)), code).toBeNull();
    }
    // With a modifier they are combinations; keys that do not type stand alone.
    expect(hotkeyFromKey(key("Enter", { altKey: true }))).toBe("Alt+Enter");
    expect(hotkeyFromKey(key("KeyA", { shiftKey: true }))).toBe("Shift+KeyA");
    expect(hotkeyFromKey(key("Tab", { ctrlKey: true }))).toBe("Control+Tab");
    expect(hotkeyFromKey(key("CapsLock"))).toBe("CapsLock");
    expect(hotkeyFromKey(key("F12"))).toBe("F12");
  });

  it("refuses alone every key the backend refuses alone (`hotkeys::types`)", () => {
    const body = hotkeysSource.slice(hotkeysSource.indexOf("fn types("), hotkeysSource.indexOf("\n}\n", hotkeysSource.indexOf("fn types(")));
    const codes = [...body.matchAll(/"(\w+)"/g)].map((match) => match[1]);
    expect(codes.length).toBeGreaterThan(20);
    for (const code of codes) expect(hotkeyFromKey(key(code)), code).toBeNull();
  });

  it("waits while only modifiers are pressed", () => {
    expect(hotkeyFromKey(key("ControlLeft", { ctrlKey: true }))).toBeNull();
    expect(hotkeyFromKey(key("ShiftRight", { shiftKey: true }))).toBeNull();
    expect(hotkeyFromKey(key("MetaLeft", { metaKey: true }))).toBeNull();
  });

  it("formats the stored form for people", () => {
    expect(formatHotkey("control+alt+Space")).toBe("Ctrl + Alt + Space");
    expect(formatHotkey("shift+KeyZ")).toBe("Shift + Z");
    expect(formatHotkey("super+Digit1")).toBe("Win + 1");
    expect(formatHotkey("F9")).toBe("F9");
  });

  it("names the keys with signs by their signs, not by their codes", () => {
    expect(formatHotkey("alt+Backquote", false)).toBe("Alt + `");
    const signs: Record<string, string> = {
      Minus: "-",
      Equal: "=",
      BracketLeft: "[",
      BracketRight: "]",
      Semicolon: ";",
      Quote: "'",
      Comma: ",",
      Period: ".",
      Slash: "/",
      Backslash: "\\",
      ArrowUp: "↑",
      ArrowDown: "↓",
      ArrowLeft: "←",
      ArrowRight: "→",
    };
    for (const [code, sign] of Object.entries(signs)) expect(formatHotkey(`control+${code}`, false), code).toBe(`Ctrl + ${sign}`);
    expect(formatHotkey("control+Numpad1", false)).toBe("Ctrl + Num 1");
    expect(formatHotkey("alt+NumpadAdd", true)).toBe("Option + Num +");
    expect(formatHotkey("alt+NumpadEnter", false)).toBe("Alt + Num Enter");
    // Each key apart, also the ones whose sign is a plus or a minus.
    expect(hotkeyKeys("control+alt+KeyD", false)).toEqual(["Ctrl", "Alt", "D"]);
    expect(hotkeyKeys("control+alt+NumpadAdd", false)).toEqual(["Ctrl", "Alt", "Num +"]);
    expect(hotkeyKeys("shift+Minus", false)).toEqual(["Shift", "-"]);
    // Other keys keep their names.
    expect(formatHotkey("control+PageUp", false)).toBe("Ctrl + PageUp");
  });

  it("names the modifiers as a Mac keyboard does", () => {
    expect(formatHotkey("control+alt+Space", true)).toBe("Control + Option + Space");
    expect(formatHotkey("shift+super+KeyD", true)).toBe("Shift + Command + D");
    expect(formatHotkey("super+Digit1", false)).toBe("Win + 1");
    expect(shortcutModifierNames(true)).toEqual(["Control", "Option", "Command"]);
    expect(shortcutModifierNames(false)).toEqual(["Ctrl", "Alt", "Win"]);
  });

  it("names the keys as a Mac keyboard does, and as a PC one elsewhere", () => {
    expect(formatHotkey("control+Backspace", true)).toBe("Control + Delete");
    expect(formatHotkey("super+Enter", true)).toBe("Command + Return");
    expect(formatHotkey("control+Delete", true)).toBe("Control + ⌦");
    expect(formatHotkey("alt+Escape", true)).toBe("Option + Esc");
    expect(hotkeyKeys("control+Backspace", false)).toEqual(["Ctrl", "Backspace"]);
    expect(hotkeyKeys("control+Enter", false)).toEqual(["Ctrl", "Enter"]);
    expect(hotkeyKeys("control+Delete", false)).toEqual(["Ctrl", "Delete"]);
    expect(hotkeyKeys("control+Escape", false)).toEqual(["Ctrl", "Escape"]);
    // The signs stay those of a US keyboard on both.
    expect(formatHotkey("alt+Backquote", true)).toBe("Option + `");
  });
});
