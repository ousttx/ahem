import { beforeAll, describe, expect, it } from "vitest";
import type { TFunction } from "i18next";
import type { CloudEngineInfo } from "./components/CloudPanel";
import type { ModelInfo } from "./components/ModelsPanel";
import { initialView, reduce, type View } from "./dictation";
import type { CoreEvent, SessionState } from "./events";
import { initI18n } from "./i18n";
import type { Status } from "./present";
import { meetingFailureText, quickHead, quickModels, recentTexts, restingState, retryLine, panelRisesIn } from "./quick";
import type { Connection } from "./settings";
import trayCss from "./tray.css?raw";
import appCss from "./App.css?raw";
import traySource from "./tray.tsx?raw";
import indicatorCss from "./indicator.css?raw";

let t: TFunction;

beforeAll(async () => {
  t = (await initI18n("en")).t;
});

const model = (id: string, kind: "ready" | "missing", translate = false): ModelInfo => ({
  id,
  name: id.toUpperCase(),
  author: "OpenAI",
  license: "MIT",
  url: "https://huggingface.co/owner/repo/resolve/0000/file.bin",
  download_size: 1,
  translate,
  hardware: { kind: "cpu" },
  state: kind === "ready" ? { kind: "ready" } : { kind: "missing" },
  languages: [],
  takes_dictionary: false,
  takes_language: false,
});

const engine = (id: string, service: CloudEngineInfo["service"]): CloudEngineInfo => ({
  id,
  name: id,
  service,
  translate: true,
  files: true,
  languages: [],
  takes_dictionary: true,
  takes_language: true,
});

const groq: Connection = {
  id: "c1",
  name: "Groq",
  provider: "open_ai_compatible",
  base_url: "https://api.groq.com/openai/v1",
  key_hint: null,
  added_at_ms: 0,
  models: [],
};

describe("quickModels", () => {
  it("offers downloaded models and engines of connected services", () => {
    const list = quickModels(
      [model("large", "ready", true), model("turbo", "missing"), model("parakeet", "ready")],
      [engine("groq-whisper", "groq"), engine("deepgram-nova", "deepgram")],
      [groq],
      "large",
    );
    expect(list.map((item) => item.id)).toEqual(["large", "parakeet", "groq-whisper"]);
    expect(list[0]).toEqual({ id: "large", name: "LARGE" });
    expect(list[2]).toEqual({ id: "groq-whisper", name: "groq-whisper" });
  });

  it("keeps the chosen model even when it cannot be used now", () => {
    const list = quickModels([model("turbo", "missing")], [engine("deepgram-nova", "deepgram")], [], "turbo");
    expect(list.map((item) => item.id)).toEqual(["turbo"]);
  });
});

describe("recentTexts", () => {
  it("takes the newest entries with the inserted text", () => {
    const entry = (id: number, transcript: string, processed: string | null) => ({
      id,
      created_at_ms: id * 1000,
      mode_id: "plain",
      language: null,
      transcript,
      processed,
      inserted: true,
    });
    const texts = recentTexts([entry(3, "raw", "Polished"), entry(2, "two", null), entry(1, "one", null)], 2);
    expect(texts).toEqual([
      { id: 3, time: 3000, text: "Polished" },
      { id: 2, time: 2000, text: "two" },
    ]);
  });
});

describe("retryLine", () => {
  const apply = (view: View, ...events: CoreEvent[]) => events.reduce(reduce, view);
  const available = (on: boolean): CoreEvent => ({ source: "dictation", event: { type: "retry_available", available: on } });
  const session = (id: number, state: SessionState): CoreEvent => ({ source: "session", event: { type: "state_changed", session: id, state } });

  it("offers “Recognize again” only while the recording is kept", () => {
    expect(retryLine(t, initialView(null))).toBeNull();
    const failed = apply(initialView(null), session(1, { kind: "failed", failure: { kind: "transcription", detail: "HTTP 503" } }));
    expect(retryLine(t, failed)).toBeNull();
    const kept = apply(failed, available(true));
    expect(retryLine(t, kept)).toEqual({ text: "The last dictation was not recognized", retry: true, warning: false });
    // A panel opened later knows it from the command.
    expect(retryLine(t, initialView(null, true))?.retry).toBe(true);
    // A new dictation lets the recording go.
    expect(retryLine(t, apply(kept, available(false), session(2, { kind: "recording" })))).toBeNull();
  });

  it("says that the text recognized again can be copied, without the button", () => {
    const view = apply(
      initialView(null, true),
      available(false),
      session(2, { kind: "recording" }),
      session(2, { kind: "transcribing" }),
    );
    expect(retryLine(t, view)).toBeNull();
    const done = apply(view, session(2, { kind: "kept" }));
    expect(retryLine(t, done)).toEqual({ text: "Text recognized but not inserted: you can copy it.", retry: false, warning: false });
    // A text that did not reach the history is still there to copy.
    const unsaved = apply(done, { source: "dictation", event: { type: "history_failed", detail: "disk full" } });
    expect(retryLine(t, unsaved)).toEqual(retryLine(t, done));
    expect(retryLine(t, apply(done, session(3, { kind: "recording" })))).toBeNull();
  });

  it("warns that the text recognized again was not processed", () => {
    const failure = { kind: "processing" as const, detail: "HTTP 401" };
    const done = apply(
      initialView(null),
      session(2, { kind: "post_processing" }),
      { source: "session", event: { type: "processing_failed", session: 2, failure } },
      session(2, { kind: "kept" }),
    );
    expect(retryLine(t, done)).toEqual({ text: "Recognized without processing: HTTP 401", retry: false, warning: true });
  });
});

describe("panelRisesIn", () => {
  it("opens at once on macOS, as the menus of the menu bar", () => {
    expect(panelRisesIn(true, false)).toBe(false);
  });
  it("rises in elsewhere, unless motion is reduced", () => {
    expect(panelRisesIn(false, false)).toBe(true);
    expect(panelRisesIn(false, true)).toBe(false);
  });
});

describe("the chosen mode in the quick panel", () => {
  // The chosen look is a copy of the mode buttons, cut to the chosen one by the button's box. The
  // panel spaces its letters tighter (`.quick`), but the web view's defaults for buttons
  // (WebKit, WebView2) give them their own spacing: the copy must space as the buttons, or it is
  // narrower than the chosen button and the button shows at its right end.
  it("spaces its letters as the buttons under it", () => {
    expect(trayCss).toMatch(/\.quick\s*\{[^}]*letter-spacing:\s*-/);
    const copy = trayCss.match(/\.quick-modes-chosen \.chip\s*\{([^}]*)\}/);
    expect(copy?.[1]).toMatch(/letter-spacing:\s*normal;/);
  });
});

describe("a long mode name", () => {
  // A usual name keeps its width: only a name too long for the room left is cut.
  it("ends in an ellipsis inside the capsule", () => {
    expect(indicatorCss).toMatch(/\.part-label\s*\{\s*flex:\s*0 1 auto;\s*\}/);
    expect(indicatorCss).toMatch(/\.part-label\.on\s*\{\s*min-width:\s*0;\s*\}/);
    const name = indicatorCss.match(/\.part-label > \.swap\s*\{([^}]*)\}/);
    expect(name?.[1]).toMatch(/min-width:\s*0;/);
    expect(name?.[1]).toMatch(/overflow:\s*hidden;/);
    expect(name?.[1]).toMatch(/text-overflow:\s*ellipsis;/);
  });

  it("ends in an ellipsis inside the quick panel's chip", () => {
    const chip = trayCss.match(/\.quick-modes \.chip\s*\{([^}]*)\}/);
    expect(chip?.[1]).toMatch(/max-width:\s*100%;/);
    const name = trayCss.match(/\.quick-modes \.chip-name\s*\{([^}]*)\}/);
    expect(name?.[1]).toMatch(/min-width:\s*0;/);
    expect(name?.[1]).toMatch(/overflow:\s*hidden;/);
    expect(name?.[1]).toMatch(/text-overflow:\s*ellipsis;/);
    expect(name?.[1]).toMatch(/white-space:\s*nowrap;/);
    // The chips and their chosen copy hold the name alike, so the copy stays as wide as the chip.
    expect(traySource.match(/<span className="chip-name">\{modeName\(t, mode\)\}<\/span>/g)).toHaveLength(2);
  });
});

describe("the head of the quick panel at rest", () => {
  const ok: Status = { hotkey_registered: true, unregistered_hotkeys: [], engine_available: true, settings_error: null };
  const head = (chosen: boolean, status: Status | null) =>
    restingState(t, { chosen, status, hotkey: "control+alt+Space", recordingMode: "auto" });

  it("is ready with a model, before the status comes and when all is there", () => {
    expect(head(true, null)).toEqual({ title: "Ready", hint: t("quick.hint_auto"), live: true });
    expect(head(true, ok).live).toBe(true);
  });

  it("is not set up without a model", () => {
    expect(head(false, ok)).toEqual({ title: "Not set up", hint: t("quick.set_up"), live: false });
  });

  it("says why it is not ready: the shortcut taken or the engine missing", () => {
    const taken = head(true, { ...ok, hotkey_registered: false, unregistered_hotkeys: ["control+alt+Space"] });
    expect(taken.title).toBe("Not ready");
    expect(taken.hint).toContain("taken by another app");
    expect(taken.live).toBe(false);
    const missing = head(true, { ...ok, engine_available: false });
    expect(missing.hint).toBe(t("status.engine_missing"));
  });
});

describe("the head of the quick panel while a dictation runs", () => {
  const taken: Status = { hotkey_registered: false, unregistered_hotkeys: ["control+alt+Space"], engine_available: true, settings_error: null };
  const head = (current: Parameters<typeof quickHead>[1]) =>
    quickHead(t, current, { chosen: true, status: taken, hotkey: "control+alt+Space", recordingMode: "hold" });

  it("is the resting head at rest", () => {
    expect(head("idle").title).toBe("Not ready");
    expect(head("idle").live).toBe(false);
  });

  it("listens with the dot live and the shortcut's hint, also with the main shortcut taken", () => {
    // Recording by a mode's shortcut: the taken main shortcut is not this dictation's concern.
    expect(head("recording")).toEqual({ title: "Listening", hint: "Release to stop", live: true });
    expect(head("transcribing")).toEqual({ title: t("status.transcribing"), hint: t("quick.hint_hold"), live: true });
  });

  it("says how to stop while recording, as the dictation card does", () => {
    const recording = (recordingMode: "hold" | "toggle" | "auto") =>
      quickHead(t, "recording", { chosen: true, status: null, hotkey: "control+alt+Space", recordingMode }).hint;
    expect(recording("hold")).toBe(t("dictation.keys.release"));
    expect(recording("toggle")).toBe(t("dictation.keys.press_again"));
    expect(recording("auto")).toBe(t("dictation.keys.release_or_press"));
  });
});

describe("meetingFailureText", () => {
  it("says that a stopped meeting was not saved as the main window does", () => {
    expect(meetingFailureText(t, "stop", { kind: "store", detail: "disk full" })).toBe(t("meetings.error.store", { detail: "disk full" }));
    expect(meetingFailureText(t, "stop", { kind: "store", detail: "disk full" })).toBe("Could not save the meeting: disk full");
  });

  it("says that a meeting could not be saved as it starts too", () => {
    expect(meetingFailureText(t, "start", { kind: "store", detail: "read-only folder" })).toBe("Could not save the meeting: read-only folder");
  });

  it("puts the failure on a line of its own, under the label and the button", () => {
    const panel = trayCss.match(/\.quick-meeting\s*\{([^}]*)\}/);
    expect(panel?.[1]).toMatch(/display:\s*grid;/);
    expect(panel?.[1]).toMatch(/grid-template-columns:\s*minmax\(0, 1fr\) auto;/);
    const failure = trayCss.match(/\.quick-meeting-failure\s*\{([^}]*)\}/);
    expect(failure?.[1]).toMatch(/grid-column:\s*1 \/ -1;/);
  });

  it("says that it could not start or stop for anything else", () => {
    expect(meetingFailureText(t, "stop", { kind: "not_recording" })).toBe("Could not stop");
    expect(meetingFailureText(t, "start", { kind: "recording" })).toBe("Could not start");
    expect(meetingFailureText(t, "start", "command not allowed")).toBe("Could not start");
    expect(meetingFailureText(t, "stop", null)).toBe("Could not stop");
  });
});

describe("the copy button of the quick panel", () => {
  it("goes back from \"Copied\" after a moment, as the one in the main window does", () => {
    // The shared hook disarms "Copied" after 2 s; a state of its own would keep it for good.
    expect(traySource).toMatch(/useCopied<string>\(\)/);
    expect(traySource).not.toMatch(/afterCopy|writeClipboard|\bsetCopies\b/);
    expect(traySource).toMatch(/resetCopies\(\);/);
  });

  it("keeps its check visible on the light primary button", () => {
    expect(traySource).toMatch(/btn compact primary copy-button/);
    expect(appCss).toMatch(/\.btn\.primary \.icon\.tone-text \{\s*color: inherit;/);
  });
});
