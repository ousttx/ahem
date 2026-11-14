import { beforeAll, describe, expect, it } from "vitest";
import type { TFunction } from "i18next";
import { forgetEngineFailure, initialView, phase, reduce, type View } from "./dictation";
import type { CoreEvent, DictationResult, FailureKind, SelectionProblem, SessionState } from "./events";
import { initI18n } from "./i18n";
import type { CloudEngineInfo } from "./components/CloudPanel";
import type { ModelInfo } from "./components/ModelsPanel";
import {
  canRecognize,
  capsule,
  capsuleWarning,
  clock,
  commandFailure,
  dictionaryNotes,
  errorKind,
  failureText,
  latestOnly,
  microphoneLostText,
  microphoneOptions,
  meter,
  downloadBadge,
  engineStatus,
  gigabytes,
  hero,
  modeCaption,
  newCapsuleVisit,
  noticeText,
  ownMessages,
  plainModeNote,
  quote,
  recognitionNote,
  runningPercent,
  saveErrorText,
  splitClock,
  STATUS_EVENTS,
  statusReader,
  translationWarning,
  type Message,
  type ModelTraits,
  type Status,
} from "./present";
import { HOTKEYS_EVENT, SETTINGS_EVENT, type Mode, type SaveError, type Settings } from "./settings";
import whisperServerSource from "../src-tauri/src/engines/whisper_server.rs?raw";

let t: TFunction;

beforeAll(async () => {
  t = (await initI18n("en")).t;
});

const settings: Settings = {
  ui_language: "en",
  hotkey: "control+alt+KeyD",
  recording_mode: "hold",
  microphone_id: null,
  model_id: "whisper-large-v3-turbo",
  unload_policy: { mode: "immediate" },
  mute_while_recording: false,
  show_indicator: true,
  spoken_language: null,
  dictionary: "",
  replacements: [],
  clean_text: { level: "fillers" },
  fillers: "э-э",
  connections: [
    {
      id: "connection-1",
      name: "Claude",
      provider: "anthropic",
      base_url: null,
      key_hint: "••••abcd",
      added_at_ms: 0,
      models: ["claude-sonnet-5-5"],
    },
  ],
  modes: [
    { id: "plain", name: "", action: { kind: "plain" }, hotkey: null, selection_hotkey: null },
    { id: "english", name: "", action: { kind: "translate_to_english" }, hotkey: null, selection_hotkey: null },
  ],
  main_mode_id: "plain",
  app_rules: [],
  app_hotkey: null,
  history_enabled: false,
  history_limit: 10,
  launch_at_login: false,
  onboarding_done: false,
  theme: "system",
  files_by_speaker: false,
  auto_update: true,
  beta_updates: false,
};

const status: Status = { hotkey_registered: true, unregistered_hotkeys: [], engine_available: true, settings_error: null };

const result: DictationResult = {
  mode_id: "plain",
  transcript: "Hello   world",
  language: "en",
  processed: null,
  inserted: true,
};

function apply(view: View, ...events: CoreEvent[]): View {
  return events.reduce(reduce, view);
}

function session(state: SessionState): CoreEvent {
  return { source: "session", event: { type: "state_changed", session: 1, state } };
}

function card(
  view: View,
  overrides: { settings?: Settings; status?: Status; usable?: boolean; fresh?: boolean; elapsed?: number; copyFailed?: boolean } = {},
) {
  return hero(t, {
    view,
    settings: overrides.settings ?? settings,
    status: overrides.status ?? status,
    modelName: "Large v3 Turbo",
    usable: overrides.usable ?? true,
    fresh: overrides.fresh ?? false,
    elapsed: overrides.elapsed ?? 0,
    copyFailed: overrides.copyFailed,
  });
}

describe("the dictation card", () => {
  it("offers the ChatGPT usage settings when the plan's limit is reached", () => {
    const view = apply(initialView(null), session({ kind: "failed", failure: { kind: "plan_limit", detail: "" } }));
    expect(card(view)).toMatchObject({ title: "Inserted without processing", manageUsage: true, configure: false });
  });

  it("is ready only with a model and the engine", () => {
    const view = initialView(null);
    expect(card(view)).toMatchObject({ title: "Ready to dictate", subtitle: "", icon: "microphone", tone: "ok", configure: false });
    expect(card(view, { settings: { ...settings, model_id: null } })).toMatchObject({
      title: "Dictation unavailable",
      subtitle: "Download and choose a speech model.",
      configure: true,
    });
    expect(card(view, { status: { ...status, engine_available: false } })).toMatchObject({
      title: "Dictation unavailable",
      configure: false,
    });
  });

  it("names the keys of the recording mode while ready", () => {
    const view = initialView(null);
    expect(card(view).keyNote).toBe("Hold");
    expect(card(view, { settings: { ...settings, recording_mode: "toggle" } }).keyNote).toBe("Start and stop");
    expect(card(view, { settings: { ...settings, recording_mode: "auto" } }).keyNote).toBe("Tap or hold");
  });

  it("counts the recording time and explains how to finish", () => {
    const view = apply(initialView(null), session({ kind: "recording" }));
    expect(card(view, { elapsed: 65_400 })).toMatchObject({
      title: "Listening · 01:05",
      clock: "01:05",
      tone: "danger",
      ring: true,
      wave: true,
      keyNote: "Release to stop",
    });
    const toggle = { ...settings, recording_mode: "toggle" as const };
    expect(card(view, { settings: toggle }).keyNote).toBe("Press again");
    const auto = { ...settings, recording_mode: "auto" as const };
    expect(card(view, { settings: auto })).toMatchObject({ keyNote: "Release or press again", subtitle: "" });
  });

  it("follows recognition and processing", () => {
    const recognizing = apply(initialView(null), session({ kind: "transcribing" }));
    expect(card(recognizing)).toMatchObject({ title: "Recognizing…", subtitle: "Large v3 Turbo" });
    const loading = apply(recognizing, { source: "engine", event: { type: "state_changed", state: "loading" } });
    expect(card(loading).title).toBe("Loading the model…");
    const processing = apply(initialView(null), session({ kind: "post_processing" }));
    expect(card(processing)).toMatchObject({ title: "Processing…", icon: "sparkle" });
  });

  it("says that the text is being inserted, as the capsule and the quick panel do", () => {
    const inserting = apply(initialView(null), session({ kind: "inserting" }));
    expect(card(inserting)).toMatchObject({ title: "Inserting…", subtitle: "" });
    expect(card(inserting).title).toBe(t(`status.${phase(inserting)}`));
    // Also while the model loads again for another dictation.
    const loading = apply(inserting, { source: "engine", event: { type: "state_changed", state: "loading" } });
    expect(card(loading).title).toBe("Inserting…");
  });

  it("shows an insertion for a moment, then is ready again", () => {
    const view = apply(
      initialView(null),
      session({ kind: "recording" }),
      { source: "session", event: { type: "finished", session: 1, result } },
      session({ kind: "inserted" }),
    );
    expect(card(view, { fresh: true })).toMatchObject({ title: "Text inserted", subtitle: "“Hello world”", tone: "ok" });
    expect(card(view, { fresh: false }).title).toBe("Ready to dictate");
  });

  it("keeps a failure until the next dictation", () => {
    const failed = (kind: "focus_changed" | "processing" | "transcription") =>
      apply(initialView(null), session({ kind: "failed", failure: { kind, detail: "HTTP 500" } }));
    expect(card(failed("focus_changed"))).toMatchObject({ title: "Text not inserted", icon: "warning", tone: "danger" });
    expect(card(failed("processing")).title).toBe("Inserted without processing");
    expect(card(failed("transcription")).subtitle).toBe("Recognition failed: HTTP 500");
    const blocked = apply(initialView(null), {
      source: "dictation",
      event: { type: "blocked", reason: "no_model", detail: "" },
    });
    expect(card(blocked, { usable: false })).toMatchObject({ title: "Dictation unavailable", configure: true });
    expect(card(apply(failed("transcription"), session({ kind: "recording" }))).title).toBe("Listening · 00:00");
  });

  it("lets a refused press go once its reason has gone", () => {
    const blocked = (reason: "no_model" | "no_engine" | "microphone") =>
      apply(initialView(null), { source: "dictation", event: { type: "blocked", reason, detail: "busy" } });
    // No engine to recognize with: said until a downloaded model or a connected cloud engine is chosen.
    expect(card(blocked("no_model"), { usable: false })).toMatchObject({ title: "Dictation unavailable", configure: true });
    expect(card(blocked("no_model"), { usable: true })).toMatchObject({ title: "Ready to dictate", configure: false });
    // No program for the model: said until it is there.
    const missing = { ...status, engine_available: false };
    expect(card(blocked("no_engine"), { status: missing })).toMatchObject({
      title: "Dictation unavailable",
      subtitle: t("notice.no_engine", { detail: "busy" }),
    });
    expect(card(blocked("no_engine")).title).toBe("Ready to dictate");
    // A busy microphone shows nowhere else: it is said until the next dictation.
    expect(card(blocked("microphone")).title).toBe("Dictation unavailable");
  });

  it("lets an engine failure go once the engine loads again or another model is chosen", () => {
    const failed = apply(initialView(null), { source: "engine", event: { type: "failed", detail: "exit 3" } });
    expect(card(failed)).toMatchObject({ title: "Dictation unavailable", subtitle: t("notice.engine_failed", { detail: "exit 3" }) });
    expect(card(apply(failed, { source: "engine", event: { type: "state_changed", state: "loading" } })).title).toBe("Ready to dictate");
    expect(card(forgetEngineFailure(failed)).title).toBe("Ready to dictate");
  });

  it("offers to recognize again a dictation that was not recognized, while nothing runs", () => {
    const available = (on: boolean): CoreEvent => ({ source: "dictation", event: { type: "retry_available", available: on } });
    const failed = apply(initialView(null), session({ kind: "failed", failure: { kind: "transcription", detail: "HTTP 503" } }));
    expect(card(failed).retry).toBe(false);
    const kept = apply(failed, available(true));
    expect(card(kept)).toMatchObject({ title: "Dictation failed", subtitle: "Recognition failed: HTTP 503", retry: true });
    // No model or engine for it now: the press is blocked and the recording stays.
    const blocked = apply(kept, { source: "dictation", event: { type: "blocked", reason: "no_model", detail: "" } });
    expect(card(blocked, { usable: false })).toMatchObject({ title: "Dictation unavailable", configure: true, retry: true });
    // A window that opened after the failure knows it only from the command.
    expect(card(initialView(null, true))).toMatchObject({
      title: "Dictation failed",
      subtitle: "The last dictation was not recognized",
      tone: "danger",
      retry: true,
    });
    expect(card(apply(initialView(null, true), session({ kind: "transcribing" }))).retry).toBe(false);
    expect(card(apply(kept, available(false), session({ kind: "recording" }))).retry).toBe(false);
  });

  it("keeps a text recognized again in the card to copy, as a success", () => {
    const last = { ...result, inserted: false };
    const view = apply(
      initialView(null),
      session({ kind: "transcribing" }),
      session({ kind: "kept" }),
      { source: "session", event: { type: "finished", session: 1, result: last } },
    );
    const shown = { title: "Text recognized", subtitle: "“Hello world”", icon: "check", tone: "ok", copy: true, retry: false };
    expect(card(view, { fresh: true })).toMatchObject(shown);
    // Not a moment's notice: the text is nowhere else on the page.
    expect(card(view, { fresh: false })).toMatchObject(shown);
    // True in both windows: each has a button to copy it.
    expect(noticeText(t, { kind: "kept" })).toBe("Text recognized but not inserted: you can copy it.");
  });

  it("says that a text recognized again was not processed, and still offers to copy it", () => {
    const last = { ...result, inserted: false };
    const unprocessed = (kind: "processing" | "empty_processing" | "plan_limit", detail: string) =>
      apply(
        initialView(null),
        session({ kind: "post_processing" }),
        { source: "session", event: { type: "processing_failed", session: 1, failure: { kind, detail } } },
        session({ kind: "kept" }),
        { source: "session", event: { type: "finished", session: 1, result: last } },
      );
    expect(card(unprocessed("processing", "HTTP 401"))).toMatchObject({
      title: "Recognized without processing",
      subtitle: "Processing failed: HTTP 401",
      icon: "warning",
      tone: "danger",
      copy: true,
      manageUsage: false,
    });
    expect(card(unprocessed("empty_processing", "")).subtitle).toBe("The model returned nothing.");
    expect(card(unprocessed("plan_limit", ""))).toMatchObject({
      subtitle: "ChatGPT plan limit reached.",
      manageUsage: true,
    });
    for (const kind of ["processing", "empty_processing", "plan_limit"] as const) {
      const shown = card(unprocessed(kind, "HTTP 401"));
      // The line below says why, not the title again; and not a word about insertion: nothing
      // was typed.
      expect(shown.subtitle.toLowerCase()).not.toContain(shown.title.toLowerCase());
      expect(shown.subtitle).not.toContain("insert");
    }
  });

  it("warns that the microphone went away midway, beside the outcome, until the next recording", () => {
    const lost = apply(
      initialView(null),
      session({ kind: "recording" }),
      { source: "dictation", event: { type: "microphone_lost", session: 1, detail: "device disconnected" } },
      session({ kind: "inserted" }),
    );
    const warnings = ["The microphone stopped sending audio during recording. Only the part recorded before that was recognized: device disconnected"];
    expect(card(lost, { fresh: true })).toMatchObject({ title: "Text inserted", warnings });
    expect(card(lost, { fresh: false })).toMatchObject({ title: "Ready to dictate", warnings });
    expect(card(apply(lost, session({ kind: "recording" }))).warnings).toEqual([]);
    expect(card(initialView(null)).warnings).toEqual([]);
  });

  it("says that the history was not saved beside the outcome, without hiding it", () => {
    const historyFailed: CoreEvent = { source: "dictation", event: { type: "history_failed", detail: "disk full" } };
    const warning = "Could not save to history: disk full";
    const notInserted = apply(
      initialView(null),
      session({ kind: "failed", failure: { kind: "focus_changed", detail: "" } }),
      { source: "session", event: { type: "finished", session: 1, result: { ...result, inserted: false } } },
      historyFailed,
    );
    expect(card(notInserted)).toMatchObject({ title: "Text not inserted", copy: true, warnings: [warning] });
    const unprocessed = apply(
      initialView(null),
      session({ kind: "post_processing" }),
      { source: "session", event: { type: "processing_failed", session: 1, failure: { kind: "processing", detail: "HTTP 401" } } },
      session({ kind: "inserted" }),
      historyFailed,
    );
    expect(card(unprocessed)).toMatchObject({ title: "Inserted without processing", warnings: [warning] });
    // "Recognize again": the text is kept to copy.
    const recognizedAgain = apply(
      initialView(null),
      session({ kind: "transcribing" }),
      session({ kind: "kept" }),
      { source: "session", event: { type: "finished", session: 1, result: { ...result, inserted: false } } },
      historyFailed,
    );
    expect(card(recognizedAgain)).toMatchObject({ title: "Text recognized", copy: true, warnings: [warning] });
    // Both lines when both happened.
    const both = apply(
      initialView(null),
      session({ kind: "recording" }),
      { source: "dictation", event: { type: "microphone_lost", session: 1, detail: "gone" } },
      session({ kind: "inserted" }),
      historyFailed,
    );
    expect(card(both, { fresh: true }).warnings).toHaveLength(2);
  });

  it("never repeats the title in the line under it, only says why", async () => {
    const failures: FailureKind[] = [
      "recording",
      "transcription",
      "processing",
      "empty_processing",
      "plan_limit",
      "no_focus",
      "focus_changed",
      "insertion_blocked",
      "no_permission",
      "insertion",
    ];
    const problems: SelectionProblem[] = [
      "no_focus",
      "focus_changed",
      "password",
      "not_supported",
      "no_selection",
      "several_ranges",
      "read_only",
      "changed",
      "processing",
      "insertion_blocked",
      "insertion",
      "no_permission",
      "unsupported",
      "failed",
    ];
    const views = [
      ...failures.map((kind) => apply(initialView(null), session({ kind: "failed", failure: { kind, detail: "boom" } }))),
      ...(["processing", "empty_processing", "plan_limit"] as const).map((kind) =>
        apply(
          initialView(null),
          session({ kind: "post_processing" }),
          { source: "session", event: { type: "processing_failed", session: 1, failure: { kind, detail: "boom" } } },
          session({ kind: "kept" }),
        ),
      ),
      ...problems.map((problem) =>
        apply(initialView(null), { source: "dictation", event: { type: "selection_failed", problem, detail: "boom" } }),
      ),
    ];
    // What the titles say: not inserted, not replaced, without processing.
    const outcomes = {
      en: ["inserted", "replaced", "without processing", "as recognized"],
      ru: ["вставлен", "заменен", "заменён", "замены", "без обработки", "как распознан"],
    };
    for (const language of ["en", "ru"] as const) {
      const fixed = (await initI18n("en")).getFixedT(language);
      for (const view of views) {
        const shown = hero(fixed, { view, settings, status, modelName: null, usable: false, fresh: false, elapsed: 0 });
        const subtitle = shown.subtitle.toLowerCase();
        const where = `${language} ${JSON.stringify(view.notice)}`;
        expect(subtitle, where).not.toContain(shown.title.toLowerCase());
        for (const word of outcomes[language]) expect(subtitle, where).not.toContain(word);
      }
    }
  });

  it("says only the detail under a text or a selection that the system did not take", () => {
    const notInserted = apply(initialView(null), session({ kind: "failed", failure: { kind: "insertion", detail: "error 5" } }));
    expect(card(notInserted)).toMatchObject({ title: "Text not inserted", subtitle: "error 5" });
    const notReplaced = (problem: SelectionProblem) =>
      card(apply(initialView(null), { source: "dictation", event: { type: "selection_failed", problem, detail: "error 5" } }));
    expect(notReplaced("insertion")).toMatchObject({ title: "Selection not replaced", subtitle: "error 5" });
    expect(notReplaced("focus_changed").subtitle).toBe("You switched windows.");
    expect(notReplaced("changed").subtitle).toBe("The text changed during processing.");
    expect(notReplaced("password").subtitle).toBe("ahem does not read password fields.");
    const unprocessed = apply(initialView(null), session({ kind: "failed", failure: { kind: "processing", detail: "HTTP 401" } }));
    expect(card(unprocessed)).toMatchObject({ title: "Inserted without processing", subtitle: "Processing failed: HTTP 401" });
    // The capsule has no title: it keeps the whole sentence.
    expect(noticeText(t, unprocessed.notice!)).toBe("Processing failed, inserted as recognized: HTTP 401");
  });

  it("keeps the warnings of a dictation away from what came after it that is not its own", () => {
    const lost: CoreEvent = { source: "dictation", event: { type: "microphone_lost", session: 1, detail: "gone" } };
    const historyFailed: CoreEvent = { source: "dictation", event: { type: "history_failed", detail: "disk full" } };
    const recording = apply(initialView(null), session({ kind: "recording" }), lost);
    // While the dictation runs, its own warning shows.
    expect(card(recording).warnings).toHaveLength(1);
    expect(microphoneLostText(t, recording)).not.toBeNull();
    const done = apply(recording, session({ kind: "inserted" }), historyFailed);
    expect(card(done).warnings).toHaveLength(2);
    const after: Record<string, CoreEvent[]> = {
      rewriting: [{ source: "dictation", event: { type: "selection_started", mode_id: "mode-1" } }],
      replaced: [
        { source: "dictation", event: { type: "selection_started", mode_id: "mode-1" } },
        { source: "dictation", event: { type: "selection_done" } },
      ],
      "not replaced": [{ source: "dictation", event: { type: "selection_failed", problem: "no_selection", detail: "" } }],
      blocked: [{ source: "dictation", event: { type: "blocked", reason: "microphone", detail: "busy" } }],
      "engine failed": [{ source: "engine", event: { type: "failed", detail: "crashed" } }],
    };
    for (const [name, events] of Object.entries(after)) {
      const view = apply(done, ...events);
      expect(card(view).warnings, name).toEqual([]);
      // The quick panel says it no more either.
      expect(microphoneLostText(t, view), name).toBeNull();
    }
  });

  it("says where to allow typing when the OS forbids it", () => {
    const view = apply(initialView(null), session({ kind: "failed", failure: { kind: "no_permission", detail: "" } }));
    expect(card(view)).toMatchObject({ title: "Text not inserted", tone: "danger" });
    expect(card(view).subtitle).toBe("Allow ahem in System Settings → Privacy & Security → Accessibility.");
    expect(card(view).copy).toBe(false);
    const last = { mode_id: "plain", transcript: "привет", language: null, processed: null, inserted: false };
    // The text is offered to copy, not quoted in place of where to allow typing.
    expect(card({ ...view, last })).toMatchObject({ copy: true, subtitle: card(view).subtitle });
    expect(card(apply(initialView(null), session({ kind: "recording" }))).copy).toBe(false);
  });

  it("says beside the copy button that the clipboard refused the text, while the button is offered", () => {
    const last = { ...result, inserted: false };
    const notInserted = apply(
      initialView(null),
      { source: "session", event: { type: "finished", session: 1, result: last } },
      session({ kind: "failed", failure: { kind: "focus_changed", detail: "" } }),
    );
    expect(card(notInserted).copyFailure).toBeNull();
    expect(card(notInserted, { copyFailed: true })).toMatchObject({ copy: true, copyFailure: "Could not copy. Try again." });
    const kept = apply(initialView(null), { source: "session", event: { type: "finished", session: 1, result: last } }, session({ kind: "kept" }));
    expect(card(kept, { copyFailed: true }).copyFailure).toBe(t("last.copy_failed"));
    // No copy button: nothing to say of a copy.
    const inserted = apply(initialView(null), { source: "session", event: { type: "finished", session: 1, result } }, session({ kind: "inserted" }));
    expect(card(inserted, { fresh: true, copyFailed: true }).copyFailure).toBeNull();
    expect(card(apply(notInserted, session({ kind: "recording" })), { copyFailed: true }).copyFailure).toBeNull();
  });
});

describe("the engine status", () => {
  it("names the model and its state", () => {
    expect(engineStatus(t, { engine: "unloaded", modelName: null, available: true })).toMatchObject({ tone: "danger" });
    expect(engineStatus(t, { engine: "ready", modelName: "Turbo", available: false })).toMatchObject({
      title: "Turbo",
      tone: "danger",
    });
    expect(engineStatus(t, { engine: "loading", modelName: "Turbo", available: true })).toEqual({
      title: "Turbo",
      note: "Loading…",
      tone: "accent",
    });
    expect(engineStatus(t, { engine: "ready", modelName: "Turbo", available: true }).tone).toBe("ok");
    expect(engineStatus(t, { engine: "unloaded", modelName: "Turbo", available: true }).note).toBe(
      "Loads on recording",
    );
  });

  it("a cloud engine loads nothing here", () => {
    const groq = "Groq · whisper-large-v3";
    expect(engineStatus(t, { engine: "unloaded", modelName: groq, available: true, cloud: true })).toEqual({
      title: groq,
      note: "In the cloud",
      tone: "ok",
    });
  });
});

describe("microphoneOptions", () => {
  const devices = [
    { id: "usb", name: "USB microphone", default: false },
    { id: "built-in", name: "MacBook microphone", default: true },
  ];

  it("offers the system's microphone, then the connected ones", () => {
    expect(microphoneOptions(t, devices, null)).toEqual([
      { value: "", label: "System microphone" },
      { value: "usb", label: "USB microphone" },
      { value: "built-in", label: "MacBook microphone" },
    ]);
    expect(microphoneOptions(t, devices, "usb")).toHaveLength(3);
  });

  it("keeps a chosen microphone that is disconnected, so the field is not empty", () => {
    const options = microphoneOptions(t, devices, "headset");
    expect(options).toHaveLength(4);
    expect(options[3]).toEqual({ value: "headset", label: "Disconnected microphone" });
    expect(microphoneOptions(t, [], "headset").map((option) => option.value)).toEqual(["", "headset"]);
  });
});

describe("texts", () => {
  it("say plain dictation runs on the main hotkey while it is the main mode, else nothing", () => {
    expect(plainModeNote(t, settings)).toBe(
      "Runs on Ctrl + Alt + D, the main shortcut from the settings.",
    );
    // Not the main mode: its editor offers "Make main" instead.
    expect(plainModeNote(t, { ...settings, main_mode_id: "english" })).toBeNull();
  });

  it("describe what a mode does", () => {
    const [plain, english] = settings.modes;
    const email: Mode = {
      id: "mode-1",
      name: "Email",
      action: { kind: "process", connection_id: "connection-1", model: "claude-sonnet-5-5", instruction: "x", output_language: null },
      hotkey: null,
      selection_hotkey: null,
    };
    expect(modeCaption(t, plain, settings.connections)).toBe("As recognized, no processing");
    expect(modeCaption(t, english, settings.connections)).toBe("Whisper translates as it recognizes");
    expect(modeCaption(t, email, settings.connections)).toBe("Claude · claude-sonnet-5-5");
    expect(modeCaption(t, email, [])).toBe("service not connected");
    expect(modeCaption(t, english, [], true)).toBe("Whisper translates as it recognizes");
    expect(modeCaption(t, english, [], false)).toBe("The model does not translate");
  });

  it("say how plain dictation cleans its text, not in files", () => {
    const [plain] = settings.modes;
    const model = { level: "model", connection_id: "connection-1", model: "claude-sonnet-5-5" } as const;
    expect(modeCaption(t, plain, settings.connections, null, { level: "off" })).toBe("As recognized, no processing");
    expect(modeCaption(t, plain, settings.connections, null, { level: "fillers" })).toBe("No fillers");
    expect(modeCaption(t, plain, settings.connections, null, model)).toBe("Clean text · Claude · claude-sonnet-5-5");
    expect(modeCaption(t, plain, [], null, model)).toBe("No fillers");
    expect(modeCaption(t, plain, settings.connections)).toBe("As recognized, no processing");
  });

  const large: ModelTraits = { name: "Large v3", translate: true, languages: ["en", "ru"], takes_dictionary: true, takes_language: true };
  const turbo: ModelTraits = { ...large, name: "Turbo", translate: false };
  const parakeet: ModelTraits = { ...turbo, name: "Parakeet", takes_dictionary: false, takes_language: false };
  const nemotron: ModelTraits = { ...turbo, name: "Nemotron", languages: ["en"], takes_dictionary: false };

  it("warn when the dictation model does not translate", () => {
    expect(translationWarning(t, large, [large, turbo])).toBeNull();
    expect(translationWarning(t, null, [large, turbo])).toBeNull();
    expect(translationWarning(t, turbo, [large, turbo, { ...large, name: "Medium" }])).toBe(
      "Turbo does not translate to English; Large v3, Medium do. Change the model in “Models”.",
    );
  });

  it("say which recognition settings the model ignores", () => {
    expect(recognitionNote(t, large, "language", "ru")).toBeNull();
    expect(recognitionNote(t, large, "dictionary", null)).toBeNull();
    expect(recognitionNote(t, null, "dictionary", null)).toBeNull();
    expect(recognitionNote(t, parakeet, "language", null)).toBe("Parakeet detects the language itself.");
    expect(recognitionNote(t, parakeet, "dictionary", null)).toBe("Parakeet ignores the dictionary: only Whisper models and cloud services read it.");
    const gigaam: ModelTraits = { ...parakeet, name: "GigaAM", languages: ["ru"] };
    expect(recognitionNote(t, gigaam, "language", "en")).toBe("GigaAM supports one language only.");
    expect(recognitionNote(t, nemotron, "language", "ru")).toBe("Nemotron does not support the chosen language.");
    expect(recognitionNote(t, nemotron, "language", null)).toBeNull();
  });

  it("tell how much of the dictionary the engine takes only for a model that reads it", () => {
    const hint = "Names, terms and abbreviations, one per line or comma-separated.";
    const limit = t("settings.dictionary_limit");
    expect(dictionaryNotes(t, large)).toEqual([hint, limit]);
    expect(dictionaryNotes(t, null)).toEqual([hint, limit]);
    expect(dictionaryNotes(t, parakeet)).toEqual([hint, "Parakeet ignores the dictionary: only Whisper models and cloud services read it."]);
  });

  it("promise as many dictionary terms as the engines' budgets hold", async () => {
    // A character of ordinary text is one unit, the ", " between terms two: an ordinary term of
    // ten characters takes twelve.
    const budget = (name: string) => Number(new RegExp(`const ${name}: usize = (\\d+);`).exec(whisperServerSource)?.[1]);
    const terms = (name: string) => budget(name) / 12;
    expect(terms("MAX_PROMPT_UNITS")).toBe(50);
    expect(terms("WHISPER_PROMPT_UNITS")).toBe(25);
    expect(t("settings.dictionary_limit")).toContain("about 50 terms in the cloud and 25 with Whisper");
    const russian = (await initI18n("en")).getFixedT("ru");
    expect(russian("settings.dictionary_limit")).toContain("примерно 50 терминов в облаке и 25 у Whisper");
  });

  it("name a command's failure by its kind, and anything else as it is, never a key", () => {
    const text = (failure: unknown) => failureText(t, failure, "files.start_error", "files.start_error.other");
    expect(text({ kind: "text_too_long" })).toBe("The text file is over 1 MB.");
    expect(text({ kind: "read", detail: "denied" })).toBe("Could not read the file: denied");
    expect(text({ kind: "new_reason" })).toBe("Could not start: new_reason");
    expect(text({ kind: "new_reason", detail: "why" })).toBe("Could not start: why");
    expect(text({ kind: "read", detail: { nested: true } })).toBe("Could not read the file: ");
    expect(text("command not found")).toBe("Could not start: command not found");
    expect(text(null)).toBe("Could not start: null");
  });

  it("take the kind of a command's failure only from a failure that has one", () => {
    expect(errorKind({ kind: "store", detail: "disk full" })).toBe("store");
    expect(errorKind({ kind: 3 })).toBeNull();
    expect(errorKind({ detail: "why" })).toBeNull();
    expect(errorKind("command not found")).toBeNull();
    expect(errorKind(null)).toBeNull();
    expect(errorKind(undefined)).toBeNull();
  });

  it("give a command's failure as text, and nothing once it worked", async () => {
    const text = (detail: string) => t("stats.reset_failed", { detail });
    expect(await commandFailure(Promise.resolve(), text)).toBeNull();
    expect(await commandFailure(Promise.reject("disk full"), text)).toBe("Could not reset the statistics: disk full");
    expect(await commandFailure(Promise.reject("denied"), (detail) => t("onboarding.allow_failed", { detail }))).toBe(
      "Could not open the system settings: denied",
    );
  });

  it("quote on one line and cut long text, in the quotation marks of the language", async () => {
    expect(quote(t, "  two\n\nlines ", 100)).toBe("“two lines”");
    expect(quote(t, "one two three", 7)).toBe("“one two…”");
    // Cut by characters: an emoji is two UTF-16 units and stays whole.
    expect(quote(t, "ok 👍👍 fine", 4)).toBe("“ok 👍…”");
    expect(quote(t, "👍👍", 2)).toBe("“👍👍”");
    // As a reader sees them: a flag, a family, a skin tone and a letter with its mark apart.
    expect(quote(t, "a 🇷🇺🇷🇺", 3)).toBe("“a 🇷🇺…”");
    expect(quote(t, "a 👨‍👩‍👧 b", 3)).toBe("“a 👨‍👩‍👧…”");
    expect(quote(t, "a 👍🏽 b", 3)).toBe("“a 👍🏽…”");
    const short = "и\u0306";
    expect(quote(t, `${short}${short}${short}`, 2)).toBe(`“${short}${short}…”`);
    expect(quote(t, "👨‍👩‍👧", 1)).toBe("“👨‍👩‍👧”");
    const russian = (await initI18n("en")).getFixedT("ru");
    expect(quote(russian, "два слова", 100)).toBe("«два слова»");
  });

  it("name save errors", () => {
    expect(saveErrorText(t, { kind: "hotkey_taken", detail: "x" })).toBe(
      "This shortcut is already taken.",
    );
    expect(saveErrorText(t, { kind: "io", detail: "disk full" })).toBe("Not saved: disk full");
    expect(saveErrorText(t, { kind: "read_only" })).toBe("Settings file unreadable, changes are not saved.");
  });

  it("say that the dictionary is too long, not the name of the setting", () => {
    expect(saveErrorText(t, { kind: "invalid", detail: "dictionary" })).toBe("The dictionary is too long: up to 2000 characters.");
    expect(saveErrorText(t, { kind: "invalid", detail: "history_limit" })).toBe("Not saved: history_limit");
  });

  it("name a failure that is not a save error", () => {
    expect(saveErrorText(t, "command save_settings not found" as unknown as SaveError)).toBe(
      "Not saved: command save_settings not found",
    );
  });

  it("name a ChatGPT account by the service, not by the account's email", () => {
    const account = { ...settings.connections[0], id: "chatgpt", name: "me@example.com", provider: "chatgpt_account" as const };
    const mode: Mode = {
      id: "mode-1",
      name: "Email",
      action: { kind: "process", connection_id: "chatgpt", model: "gpt-5.5", instruction: "x", output_language: null },
      hotkey: null,
      selection_hotkey: null,
    };
    expect(modeCaption(t, mode, [account])).toBe("ChatGPT · gpt-5.5");
    const clean = { level: "model", connection_id: "chatgpt", model: "gpt-5.5" } as const;
    expect(modeCaption(t, settings.modes[0], [account], null, clean)).toBe("Clean text · ChatGPT · gpt-5.5");
  });
});

describe("meter", () => {
  it("keeps silence flat and lets ordinary speech fill the bars well", () => {
    expect(meter(0)).toBe(0);
    expect(meter(0.002)).toBe(0); // room noise, −54 dBFS
    expect(meter(0.03)).toBeGreaterThan(0.45); // quiet speech, −30 dBFS
    expect(meter(0.03)).toBeLessThan(0.6);
    expect(meter(0.1)).toBeGreaterThan(0.75); // ordinary speech, −20 dBFS
    expect(meter(0.5)).toBe(1); // loud
  });
});

describe("clock", () => {
  it("shows times of meetings and files", () => {
    expect(clock(0)).toBe("0:00");
    expect(clock(5_400)).toBe("0:05");
    expect(clock(754_000)).toBe("12:34");
    expect(clock(3_723_000)).toBe("1:02:03");
    expect(clock(-5)).toBe("0:00");
  });

  it("gives the minutes of a recording two digits", () => {
    expect(clock(0, true)).toBe("00:00");
    expect(clock(7_900, true)).toBe("00:07");
    expect(clock(67_000, true)).toBe("01:07");
    expect(clock(-3_000, true)).toBe("00:00");
  });
});

describe("splitClock", () => {
  it("finds the clock in the title, so it can be kept from screen readers", () => {
    expect(splitClock("Listening · 01:05", "01:05")).toEqual(["Listening · ", "01:05", ""]);
    expect(splitClock("01:05 · Слушаю", "01:05")).toEqual(["", "01:05", " · Слушаю"]);
    expect(splitClock("Ready to dictate", null)).toEqual(["Ready to dictate", "", ""]);
    expect(splitClock("Listening", "01:05")).toEqual(["Listening", "", ""]);
  });
});

describe("runningPercent", () => {
  it("rounds down and stays below 100 while running", () => {
    expect(runningPercent(0)).toBe(0);
    expect(runningPercent(0.256)).toBe(25);
    expect(runningPercent(0.996)).toBe(99);
    expect(runningPercent(1)).toBe(99);
    expect(runningPercent(-0.1)).toBe(0);
  });
});

describe("gigabytes", () => {
  it("write two decimals as the interface language does", () => {
    expect(gigabytes(1_624_000_000, "en")).toBe("1.62");
    expect(gigabytes(1_624_000_000, "ru")).toBe("1,62");
    expect(gigabytes(0, "en")).toBe("0.00");
  });
});

describe("the indicator's capsule", () => {
  const notice = (view: View) => capsule(t, "idle", view.notice);

  it("says what is done after rewriting the selection, not that text was inserted", () => {
    const rewritten = apply(initialView(null), { source: "dictation", event: { type: "selection_done" } });
    expect(notice(rewritten)).toEqual({ state: "success", text: null, label: "Selection replaced." });
    const inserted = apply(initialView(null), session({ kind: "inserted" }));
    expect(notice(inserted)).toEqual({ state: "success", text: null, label: "Text inserted." });
  });

  it("shows a text recognized again as a short success, and as a problem when it was not processed", () => {
    const kept = apply(initialView(null), session({ kind: "kept" }));
    expect(notice(kept)).toEqual({ state: "success", text: null, label: "Text recognized" });
    const unprocessed = apply(
      initialView(null),
      session({ kind: "post_processing" }),
      { source: "session", event: { type: "processing_failed", session: 1, failure: { kind: "processing", detail: "HTTP 401" } } },
      session({ kind: "kept" }),
    );
    const text = "Recognized without processing: HTTP 401";
    expect(notice(unprocessed)).toEqual({ state: "problem", text, label: text });
  });

  it("says when a language model processes the text", () => {
    expect(capsule(t, "processing", null)).toEqual({ state: "busy", text: null, label: "Processing…" });
    expect(capsule(t, "transcribing", null)?.label).toBe("Recognizing…");
    expect(capsule(t, "recording", null)?.state).toBe("recording");
  });

  it("warns about a microphone that went away midway instead of the success, longer", () => {
    const recorded = (id: number, ...events: CoreEvent[]) =>
      apply(
        initialView(null),
        { source: "session", event: { type: "state_changed", session: id, state: { kind: "recording" } } },
        ...events,
        { source: "session", event: { type: "state_changed", session: id, state: { kind: "inserted" } } },
      );
    const lost = recorded(1, { source: "dictation", event: { type: "microphone_lost", session: 1, detail: "gone" } });
    const text = "The microphone stopped sending audio. Only the part recorded before that was recognized.";
    expect(capsule(t, "idle", lost.notice, capsuleWarning(t, lost))).toEqual({ state: "problem", text, label: text });
    // Only for the dictation that lost it.
    const other = apply(lost, { source: "session", event: { type: "state_changed", session: 2, state: { kind: "inserted" } } });
    expect(capsuleWarning(t, other)).toBeNull();
    const rewritten = apply(lost, { source: "dictation", event: { type: "selection_done" } });
    expect(capsule(t, "idle", rewritten.notice, capsuleWarning(t, rewritten))?.state).toBe("success");
  });

  it("warns about a lost microphone at its own dictation's outcome when the next dictation began before it", () => {
    const state = (id: number, kind: "recording" | "transcribing" | "inserted"): CoreEvent => ({
      source: "session",
      event: { type: "state_changed", session: id, state: { kind } },
    });
    const lost: CoreEvent = { source: "dictation", event: { type: "microphone_lost", session: 1, detail: "gone" } };
    const text = "The microphone stopped sending audio. Only the part recorded before that was recognized.";
    const shown = (view: View) => capsule(t, phase(view), view.notice, capsuleWarning(t, view));
    const overlap = apply(initialView(null), state(1, "recording"), lost, state(1, "transcribing"), state(2, "recording"));
    // The card of the new recording does not take the first one's warning.
    expect(card(overlap).warnings).toEqual([]);
    // The second dictation ends first: its outcome is its own.
    const second = apply(overlap, state(2, "transcribing"), state(2, "inserted"));
    expect(card(second, { fresh: true }).warnings).toEqual([]);
    expect(capsuleWarning(t, second)).toBeNull();
    // The first one's outcome comes with its warning, in the card and in the capsule.
    const first = apply(second, state(1, "inserted"));
    expect(card(first, { fresh: true })).toMatchObject({ title: "Text inserted", warnings: [t("dictation.microphone_lost", { detail: "gone" })] });
    expect(shown(first)).toEqual({ state: "problem", text, label: text });
    expect(microphoneLostText(t, first)).not.toBeNull();
  });

  it("keeps the warnings of a dictation whose outcome came while the next one records until that recording ends", () => {
    const state = (id: number, kind: "recording" | "transcribing" | "inserted"): CoreEvent => ({
      source: "session",
      event: { type: "state_changed", session: id, state: { kind } },
    });
    const lost: CoreEvent = { source: "dictation", event: { type: "microphone_lost", session: 1, detail: "gone" } };
    const historyFailed: CoreEvent = { source: "dictation", event: { type: "history_failed", detail: "disk full" } };
    const overlap = apply(initialView(null), state(1, "recording"), lost, state(1, "transcribing"), state(2, "recording"));
    // The first one's outcome comes while the second one records: the card is about the recording.
    for (const outcome of [apply(overlap, state(1, "inserted")), apply(overlap, state(1, "inserted"), historyFailed)]) {
      expect(outcome.noticeSession).toBe(1);
      expect(card(outcome, { fresh: true }).warnings).toEqual([]);
      expect(microphoneLostText(t, outcome)).toBeNull();
    }
    // The recording ended without an outcome yet: the outcome shown is the first one's again.
    const ended = apply(overlap, state(1, "inserted"), historyFailed, state(2, "transcribing"));
    expect(card(ended, { fresh: true }).warnings).toEqual([
      t("dictation.microphone_lost", { detail: "gone" }),
      t("history.save_failed", { detail: "disk full" }),
    ]);
    expect(microphoneLostText(t, ended)).not.toBeNull();
  });

  it("says that the history was not saved after a success, and keeps a problem the outcome's own", () => {
    const historyFailed: CoreEvent = { source: "dictation", event: { type: "history_failed", detail: "disk full" } };
    const shown = (view: View) => capsule(t, "idle", view.notice, capsuleWarning(t, view));
    const inserted = apply(initialView(null), session({ kind: "inserted" }), historyFailed);
    expect(shown(inserted)).toEqual({ state: "problem", text: "Could not save to history: disk full", label: "Could not save to history: disk full" });
    const notInserted = apply(initialView(null), session({ kind: "failed", failure: { kind: "no_focus", detail: "" } }), historyFailed);
    expect(shown(notInserted)?.text).toBe("No active window. You can copy the text in the ahem window.");
    const unprocessed = apply(
      initialView(null),
      session({ kind: "post_processing" }),
      { source: "session", event: { type: "processing_failed", session: 1, failure: { kind: "processing", detail: "HTTP 401" } } },
      session({ kind: "inserted" }),
      historyFailed,
    );
    expect(shown(unprocessed)?.text).toBe("Processing failed, inserted as recognized: HTTP 401");
    // A text recognized again that went in later is not blamed for the earlier one.
    const retried = apply(inserted, { source: "session", event: { type: "state_changed", session: 2, state: { kind: "transcribing" } } }, {
      source: "session",
      event: { type: "state_changed", session: 2, state: { kind: "kept" } },
    });
    expect(shown(retried)).toEqual({ state: "success", text: null, label: "Text recognized" });
  });

  it("grows in anew for a notice that comes while idle, not for the outcome of a dictation that ran", () => {
    const shown = (view: View) => ({ idle: phase(view) === "idle", notice: view.notice });
    const visits = (...events: CoreEvent[]) => {
      let view = initialView(null);
      let count = 0;
      for (const event of events) {
        const next = reduce(view, event);
        if (newCapsuleVisit(shown(view), shown(next))) count += 1;
        view = next;
      }
      return count;
    };
    const blocked: CoreEvent = { source: "dictation", event: { type: "blocked", reason: "no_model", detail: "" } };
    const unreadable: CoreEvent = { source: "dictation", event: { type: "selection_failed", problem: "no_focus", detail: "" } };
    const historyFailed: CoreEvent = { source: "dictation", event: { type: "history_failed", detail: "disk full" } };
    const dictation = [session({ kind: "recording" }), session({ kind: "transcribing" }), session({ kind: "inserted" })];
    // One dictation is one visit: its outcome and a warning beside it do not grow in again.
    expect(visits(...dictation)).toBe(1);
    expect(visits(...dictation, historyFailed)).toBe(1);
    // A dictation refused after the last one faded out shows again, and so does the next refusal.
    expect(visits(...dictation, blocked)).toBe(2);
    expect(visits(...dictation, blocked, blocked)).toBe(3);
    expect(visits(...dictation, unreadable)).toBe(2);
    // A selection rewritten after a dictation is a visit of its own; its outcome is not another.
    const rewrite: CoreEvent[] = [
      { source: "dictation", event: { type: "selection_started", mode_id: "mode-1" } },
      { source: "dictation", event: { type: "selection_done" } },
    ];
    expect(visits(...dictation, ...rewrite)).toBe(2);
    // A rewrite that began and then failed is the same visit: the capsule only changes its text.
    const failedRewrite: CoreEvent[] = [
      { source: "dictation", event: { type: "selection_started", mode_id: "mode-1" } },
      { source: "dictation", event: { type: "selection_failed", problem: "changed", detail: "" } },
    ];
    expect(visits(...dictation, ...failedRewrite)).toBe(2);
  });

  it("does not grow in again for an engine that failed while idle: the backend shows no window for it", () => {
    const shown = (view: View) => ({ idle: phase(view) === "idle", notice: view.notice });
    const visits = (...events: CoreEvent[]) => {
      let view = initialView(null);
      let count = 0;
      for (const event of events) {
        const next = reduce(view, event);
        if (newCapsuleVisit(shown(view), shown(next))) count += 1;
        view = next;
      }
      return count;
    };
    const engineFailed: CoreEvent = { source: "engine", event: { type: "failed", detail: "exit code 128" } };
    const failed = [session({ kind: "recording" }), session({ kind: "transcribing" }), session({ kind: "failed", failure: { kind: "transcription", detail: "" } })];
    // The capsule still there from the failure only changes its text.
    expect(visits(...failed, engineFailed)).toBe(1);
    expect(visits(engineFailed)).toBe(0);
    // A refused dictation after a success is still a visit of its own.
    const blocked: CoreEvent = { source: "dictation", event: { type: "blocked", reason: "no_engine", detail: "" } };
    expect(visits(session({ kind: "recording" }), session({ kind: "inserted" }), blocked)).toBe(2);
  });

  it("shows problems and hides when there is nothing to show", () => {
    expect(capsule(t, "idle", null)).toBeNull();
    const focus = apply(initialView(null), session({ kind: "failed", failure: { kind: "focus_changed", detail: "" } }));
    expect(notice(focus)).toEqual({ state: "problem", text: "You switched windows. You can copy the text in the ahem window.", label: "You switched windows. You can copy the text in the ahem window." });
    const short = apply(initialView(null), session({ kind: "discarded", reason: "too_short" }));
    expect(notice(short)?.state).toBe("quiet");
  });
});

describe("ownMessages", () => {
  // The window's message bar: another part may show its message there too.
  function bar(initial: Message | null) {
    let shown = initial;
    const messages = ownMessages((change) => {
      shown = change(shown);
    });
    return { messages, shown: () => shown, show: (message: Message | null) => (shown = message) };
  }

  it("clear only their own failure after a success", () => {
    const page = bar(null);
    page.messages.fail("Could not start");
    expect(page.shown()).toEqual({ text: "Could not start", error: true });
    page.messages.succeed();
    expect(page.shown()).toBeNull();
  });

  it("leave a message of another part alone", () => {
    const saved = { text: "Saved: notes.md", error: false };
    const page = bar(saved);
    page.messages.succeed();
    expect(page.shown()).toBe(saved);

    page.messages.fail("Could not start");
    const other = { text: "Not saved: disk full", error: true };
    page.show(other);
    page.messages.succeed();
    expect(page.shown()).toBe(other);
  });
});

describe("latestOnly", () => {
  it("takes only the answer to the latest request, however late an earlier one comes", async () => {
    const taken: string[] = [];
    const take = latestOnly<string>((answer) => taken.push(answer));
    let early: (answer: string) => void = () => {};
    const first = take(new Promise<string>((resolve) => (early = resolve)));
    const second = take(Promise.resolve("after the hotkeys came back"));
    await second;
    early("while the hotkeys were paused");
    await first;
    expect(taken).toEqual(["after the hotkeys came back"]);
  });

  it("lets a failed request go", async () => {
    const taken: number[] = [];
    const take = latestOnly<number>((answer) => taken.push(answer));
    await expect(take(Promise.reject(new Error("no window")))).resolves.toBeUndefined();
    await take(Promise.resolve(1));
    expect(taken).toEqual([1]);
  });
});

describe("canRecognize", () => {
  const model = (id: string, kind: "ready" | "missing"): ModelInfo => ({
    id,
    name: id,
    author: "OpenAI",
    license: "MIT",
    url: "https://huggingface.co/owner/repo/resolve/0000/file.bin",
    download_size: 1,
    translate: false,
    hardware: { kind: "cpu" },
    state: kind === "ready" ? { kind: "ready" } : { kind: "missing" },
    languages: [],
    takes_dictionary: false,
    takes_language: false,
  });
  const engine: CloudEngineInfo = {
    id: "groq-whisper",
    name: "Groq Whisper",
    service: "groq",
    translate: true,
    files: true,
    languages: [],
    takes_dictionary: true,
    takes_language: true,
  };
  const groq = { ...settings.connections[0], id: "c2", name: "Groq", provider: "open_ai_compatible" as const, base_url: "https://api.groq.com/openai/v1" };

  it("is a downloaded model, or a cloud engine whose service is connected, as the backend decides", () => {
    const models = [model("ready", "ready"), model("missing", "missing")];
    expect(canRecognize("ready", models, [engine], [])).toBe(true);
    expect(canRecognize("missing", models, [engine], [])).toBe(false);
    expect(canRecognize(null, models, [engine], [groq])).toBe(false);
    expect(canRecognize("groq-whisper", models, [engine], [])).toBe(false);
    expect(canRecognize("groq-whisper", models, [engine], [groq])).toBe(true);
    // Not known yet, before the lists are read.
    expect(canRecognize("ready", [], [], [])).toBe(false);
  });
});

describe("statusReader", () => {
  it("is read again after every save of the settings and once the paused hotkeys are back", () => {
    expect(STATUS_EVENTS).toEqual([SETTINGS_EVENT, HOTKEYS_EVENT]);
  });

  it("asks at every call and takes only the latest answer; a failed read keeps the status", async () => {
    const status = (engine_available: boolean): Status => ({ hotkey_registered: true, unregistered_hotkeys: [], engine_available, settings_error: null });
    const answers: ((answer: Status) => void)[] = [];
    let failing = false;
    let asked = 0;
    const taken: Status[] = [];
    const read = statusReader(
      () => {
        asked += 1;
        return failing ? Promise.reject(new Error("no window")) : new Promise<Status>((resolve) => answers.push(resolve));
      },
      (answer) => taken.push(answer),
    );
    // A model chosen in the quick panel, then another save: the first answer comes last.
    const first = read();
    const second = read();
    answers[1](status(true));
    answers[0](status(false));
    await Promise.all([first, second]);
    expect(asked).toBe(2);
    expect(taken).toEqual([status(true)]);
    failing = true;
    await read();
    expect(taken).toEqual([status(true)]);
  });
});

describe("downloadBadge", () => {
  it("shows the percent of running downloads", () => {
    expect(downloadBadge({})).toBeNull();
    expect(downloadBadge({ a: { downloaded: 46, total: 100 } })).toBe("46%");
    expect(downloadBadge({ a: { downloaded: 50, total: 100 }, b: { downloaded: 0, total: 100 } })).toBe("25%");
    expect(downloadBadge({ a: { downloaded: 10, total: 100, failure: "network" } })).toBeNull();
  });

  it("stays below 100 until the downloads end", () => {
    expect(downloadBadge({ a: { downloaded: 999, total: 1000 } })).toBe("99%");
  });
});
