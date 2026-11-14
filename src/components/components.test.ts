import { beforeAll, describe, expect, it } from "vitest";
import type { TFunction } from "i18next";
import { initI18n, resources, supportedLanguages } from "../i18n";
import type { CoreEvent, FileProblem, MeetingProblem } from "../events";
import type { SettingsChange } from "../settings";
import { fileStartError, heldDialog } from "./FilesPanel";
import { hotkeyFieldName, oneRecording, pausedRecording, recordedKey } from "./HotkeyField";
import { connectionErrorText, NEW_SERVER } from "./ConnectionsPanel";
import { commandError, problemText, renameKey, reportCopy, stateText, transcriptKept } from "./MeetingsPanel";
import { NO_LIVE, type MeetingSummary } from "../meetings";
import { ownMessages, type Message } from "../present";
import { chooseLanguage, steppedIdle } from "./SettingsPanel";
import { speakerDeleteError, speakerDownloadEnded, speakerModelsRead, speakerViewRead } from "./SpeakerModelsCard";
import { entryModeName, focusAfterDeletion, historyArgs, historyRead, historyStep, limitChoice, limitLeft, limitStep } from "./HistoryPanel";
import { drawnScreen } from "./TranscriptsPanel";
import { formAfterClose, splitCloud, type CloudEngineInfo } from "./CloudPanel";
import {
  deleteModelError,
  deleteThenRead,
  downloadsEnded,
  hardwareBadge,
  heardFrom,
  installedNow,
  listRead,
  modelCredit,
  modelSource,
  rowFocus,
  runningDownloads,
  splitModels,
  type ModelInfo,
} from "./ModelsPanel";
import modelsPanelSource from "./ModelsPanel.tsx?raw";
import modelCatalog from "../../src-tauri/resources/models.json?raw";
import speakerModelsSource from "./SpeakerModelsCard.tsx?raw";
import meetingsPanelSource from "./MeetingsPanel.tsx?raw";
import transcriptsPanelSource from "./TranscriptsPanel.tsx?raw";
import filesPanelSource from "./FilesPanel.tsx?raw";
import historyPanelSource from "./HistoryPanel.tsx?raw";
import connectionsPanelSource from "./ConnectionsPanel.tsx?raw";
import modeEditorsSource from "./ModeEditors.tsx?raw";
import statusPanelSource from "./StatusPanel.tsx?raw";
import appSource from "../App.tsx?raw";
import type { Connection } from "../settings";
import type { KeyboardEvent } from "react";
import commandsSource from "../../src-tauri/src/commands.rs?raw";
import {
  afterCopy,
  arrived,
  focusReturns,
  freshIds,
  NO_COPIES,
  pressGuard,
  rowAfterRemoval,
  rovingStep,
  shownTitle,
  stepOption,
  tabStop,
  trappedTab,
  typedOption,
  useRoving,
  waveBarHeight,
  type SelectOption,
} from "../ui";

describe("the level strip of the dictation card", () => {
  it("uses the capsule's decibel scale: silence and room noise at the bottom, loud speech at the top", () => {
    expect(waveBarHeight(0)).toBe(6);
    expect(waveBarHeight(0.002)).toBe(6);
    expect(waveBarHeight(0.03)).toBeGreaterThan(15);
    expect(waveBarHeight(0.5)).toBe(28);
  });
});

describe("the drop-down's keys", () => {
  const options: SelectOption[] = [
    { value: "system", label: "System (English)" },
    { value: "none", label: "No model", disabled: true },
    { value: "en", label: "English" },
    { value: "es", label: "Español" },
    { value: "ru", label: "Русский" },
  ];

  it("move over the options that can be chosen and stop at the ends", () => {
    expect(stepOption(options, 0, 1)).toBe(2);
    expect(stepOption(options, 2, -1)).toBe(0);
    expect(stepOption(options, 4, 1)).toBe(4);
    expect(stepOption(options, -1, 1)).toBe(0);
    expect(stepOption(options, options.length, -1)).toBe(4);
  });

  it("jump to the typed beginning, in any case and around the list", () => {
    expect(typedOption(options, "e", 0)).toBe(2);
    expect(typedOption(options, "e", 2)).toBe(3);
    expect(typedOption(options, "E", 3)).toBe(2);
    expect(typedOption(options, "р", -1)).toBe(4);
    expect(typedOption(options, "no", 0)).toBe(-1);
    expect(typedOption(options, "x", 0)).toBe(-1);
  });
});

describe("splitModels", () => {
  const model = (id: string, state: ModelInfo["state"]): ModelInfo => ({
    id,
    name: id,
    author: "OpenAI",
    license: "MIT",
    url: "https://huggingface.co/owner/repo/resolve/0000/file.bin",
    download_size: 100,
    translate: false,
    hardware: { kind: "cpu" },
    state,
    languages: [],
    takes_dictionary: false,
    takes_language: false,
  });

  it("puts the models on this computer first and the rest to download after", () => {
    const models = [
      model("a", { kind: "missing" }),
      model("b", { kind: "ready" }),
      model("c", { kind: "missing" }),
      model("d", { kind: "partial", downloaded: 10 }),
      model("e", { kind: "missing" }),
    ];
    const downloads = { c: { downloaded: 5, total: 100 }, e: { downloaded: 0, total: 100, failure: "network" as const } };
    const { installed, available } = splitModels(models, downloads);
    expect(installed.map((item) => item.id)).toEqual(["b", "c", "d"]);
    expect(available.map((item) => item.id)).toEqual(["a", "e"]);
    // None of the models to download can be chosen: their radios need no roving.
    expect(available.every((item) => item.state.kind === "missing")).toBe(true);
  });

  it("keeps a model whose download ended on this computer until the list is read again", () => {
    // The list was read as the download began: the model is missing there.
    const models = [model("a", { kind: "missing" }), model("b", { kind: "missing" })];
    const downloading = { a: { downloaded: 90, total: 100 } };
    expect(splitModels(models, downloading).installed.map((item) => item.id)).toEqual(["a"]);
    // Done, or cancelled: the download is gone, the list not read yet.
    let ended = downloadsEnded({}, runningDownloads(downloading), runningDownloads({}), 4);
    expect(splitModels(models, {}, new Set(Object.keys(ended))).installed.map((item) => item.id)).toEqual(["a"]);
    const cancelled = { a: { downloaded: 90, total: 100, failure: "cancelled" as const } };
    expect(runningDownloads(cancelled)).toEqual([]);
    expect(splitModels(models, cancelled, new Set(Object.keys(ended))).installed.map((item) => item.id)).toEqual(["a"]);
    // A list read before the end changes nothing; the one read after it says where the model is.
    ended = listRead(ended, 3);
    expect(Object.keys(ended)).toEqual(["a"]);
    ended = listRead(ended, 4);
    expect(ended).toEqual({});
    expect(splitModels(models, cancelled, new Set(Object.keys(ended))).available.map((item) => item.id)).toEqual(["a", "b"]);
  });
});

describe("downloadsEnded", () => {
  it("notes the downloads that run no more, with the list read after them", () => {
    expect(runningDownloads({ b: { downloaded: 1, total: 2 }, a: { downloaded: 1, total: 2 }, c: { downloaded: 0, total: 2, failure: "network" } })).toEqual(["a", "b"]);
    const ended = downloadsEnded({ x: 2 }, ["a", "b"], ["b"], 5);
    expect(ended).toEqual({ x: 2, a: 5 });
    // Nothing ended: the same notes, so nothing draws anew.
    expect(downloadsEnded(ended, ["b"], ["b", "c"], 6)).toBe(ended);
  });
});

describe("heardFrom", () => {
  const model = (event: Extract<CoreEvent, { source: "model" }>["event"]): CoreEvent => ({ source: "model", event });
  const asked: ReadonlySet<string> = new Set(["whisper", "speakers"]);

  it("lets a download wait for the server until the first event of its model, whatever it is", () => {
    // The server answered: the first progress moves the row to the models on this computer.
    expect([...heardFrom(asked, model({ type: "download_progress", model: "whisper", downloaded: 0, total: 9 }))]).toEqual(["speakers"]);
    // No network: the failure shows, and the button can be pressed again.
    expect([...heardFrom(asked, model({ type: "download_failed", model: "speakers", failure: "network", detail: "" }))]).toEqual(["whisper"]);
  });

  it("keeps waiting through the events of other models and of anything else", () => {
    expect(heardFrom(asked, model({ type: "download_progress", model: "parakeet", downloaded: 1, total: 9 }))).toBe(asked);
    expect(heardFrom(asked, { source: "dictation", event: { type: "level", permille: 5 } })).toBe(asked);
  });
});

describe("rowFocus", () => {
  it("takes the first button of a row that can be pressed, else the first one to wait for", () => {
    const radio = { name: "radio", disabled: true };
    const resume = { name: "resume", disabled: false };
    const remove = { name: "delete", disabled: false };
    expect(rowFocus([radio, resume, remove])).toBe(resume);
    expect(rowFocus([{ ...radio, disabled: false }, resume])?.name).toBe("radio");
    expect(rowFocus([radio])).toBe(radio);
    expect(rowFocus([])).toBeNull();
  });
});

describe("the models to download", () => {
  // Taken away when empty, the list came back folded, a failed download's reason inside.
  it("stay in the page, hidden while there are none", () => {
    expect(modelsPanelSource).toMatch(/<details className="model-more" hidden=\{available\.length === 0\}>/);
    expect(modelsPanelSource).not.toMatch(/available\.length > 0 && \(\s*<details/);
  });
});

describe("a service whose key ran out", () => {
  it("gets a new key in the same connection, so its modes go on", () => {
    expect(connectionsPanelSource).toMatch(/invoke<Settings>\("replace_connection_key", \{ id: connection\.id, apiKey: key \}\)/);
    expect(connectionsPanelSource).toMatch(/onReplace && connection\.key_hint !== null/);
    expect(connectionsPanelSource).toMatch(/submitLabel=\{t\("connections\.replace"\)\}/);
    // A key removed while its form is open closes the form for good.
    expect(connectionsPanelSource).toMatch(/replacedGone = replacing !== null && named\[replacing\] === undefined/);
  });
});

describe("a download that is connecting", () => {
  it("cannot be deleted meanwhile: its download already holds the files", () => {
    expect(modelsPanelSource).toMatch(/disabled=\{removing\.has\(model\.id\) \|\| waiting\} onClick=\{\(\) => remove\(model\.id\)\}/);
    expect(speakerModelsSource).toMatch(/disabled=\{view\.busy \|\| waiting\}\s*onClick=\{\(\) =>\s*confirmDelete/);
  });
});

describe("buttons that go when the work ends or starts elsewhere", () => {
  it("hand the focus they have to the button in their place", () => {
    // A running file's "Open" gives way to "Choose file…" as the file ends.
    expect(transcriptsPanelSource).toMatch(/onFocus=\{\(\) => follow\(\(\) => chooseButton\.current\)\}\s*onClick=\{\(\) => show\(\{ kind: "file" \}\)\}/);
    expect(transcriptsPanelSource).toMatch(/<button ref=\{chooseButton\}[^>]*onClick=\{pick\}>/);
    // "Start recording", "Stop and transcribe" and "Delete" go as the recording starts or stops
    // in the quick panel.
    expect(meetingsPanelSource.match(/onFocus=\{toOther\}/g)).toHaveLength(3);
  });
});

describe("installedNow", () => {
  const model = (id: string, state: ModelInfo["state"]): ModelInfo => ({
    id,
    name: id,
    author: "OpenAI",
    license: "MIT",
    url: "https://huggingface.co/owner/repo/resolve/0000/file.bin",
    download_size: 100,
    translate: false,
    hardware: { kind: "cpu" },
    state,
    languages: [],
    takes_dictionary: false,
    takes_language: false,
  });

  it("names the followed downloads that ended with the model ready", () => {
    const models = [model("done", { kind: "ready" }), model("going", { kind: "partial", downloaded: 5 }), model("stopped", { kind: "partial", downloaded: 9 })];
    const downloads = { going: { downloaded: 5, total: 100 } };
    expect(installedNow(new Set(["done", "going", "stopped"]), downloads, models)).toEqual(["done"]);
    expect(installedNow(new Set(), downloads, models)).toEqual([]);
  });
});

describe("historyRead", () => {
  it("reads the history again after a saved dictation and after deleting in any window", () => {
    expect(historyRead("history_saved")).toBe(true);
    expect(historyRead("history_changed")).toBe(true);
    expect(historyRead("history_failed")).toBe(false);
    expect(historyRead("level")).toBe(false);
  });
});

describe("historyStep", () => {
  it("reads at once while shown, and only once shown while hidden", () => {
    // A dictation saved while the History page is in the background, or the window in the tray:
    // nothing is read.
    let step = historyStep(false, false, true);
    expect(step).toEqual({ read: false, stale: true, catchUp: false });
    step = historyStep(step.stale, false, true);
    expect(step).toEqual({ read: false, stale: true, catchUp: false });
    // The page opens: it reads, once, and catches up.
    step = historyStep(step.stale, true, false);
    expect(step).toEqual({ read: true, stale: false, catchUp: true });
    expect(historyStep(step.stale, true, false)).toEqual({ read: false, stale: false, catchUp: false });
    // Shown, every change is read as it comes; the quick panel is always shown.
    expect(historyStep(false, true, true)).toEqual({ read: true, stale: false, catchUp: false });
    // A change that comes before the showing is taken: the read still catches up.
    expect(historyStep(true, true, true)).toEqual({ read: true, stale: false, catchUp: true });
    // A page never shown has read nothing yet: hidden it waits, shown it reads.
    expect(historyStep(true, false, false)).toEqual({ read: false, stale: true, catchUp: false });
    expect(historyStep(true, true, false)).toEqual({ read: true, stale: false, catchUp: true });
  });
});

describe("historyArgs", () => {
  it("asks for the newest entries the panel shows, or for all of them", () => {
    expect(historyArgs(3)).toEqual({ limit: 3 });
    expect(historyArgs(null)).toEqual({ limit: null });
    // None: where the file is, for the page's header before its first showing.
    expect(historyArgs(0)).toEqual({ limit: 0 });
    // The command takes the same argument.
    expect(commandsSource).toMatch(/pub (async )?fn get_history\(\s*limit: Option<usize>,/);
  });
});

describe("arrived", () => {
  it("gives the ids that are new, in their order", () => {
    expect(arrived(new Set([1, 2]), [3, 1, 4, 2])).toEqual([3, 4]);
    expect(arrived(new Set([1]), [1])).toEqual([]);
  });
});

describe("freshIds", () => {
  it("marks the entries that came while the list was watched", () => {
    expect(freshIds(new Set([1, 2]), [3, 1, 2], false)).toEqual([3]);
  });

  it("marks none in the first read, nor in a read that catches up as the page is shown", () => {
    expect(freshIds(null, [3, 1, 2], false)).toEqual([]);
    // Opened after dictating with the window in the tray: the new entries are just there.
    expect(freshIds(new Set([1, 2]), [5, 4, 3, 1, 2], true)).toEqual([]);
  });
});

describe("formAfterClose", () => {
  it("closes the form that closed, and only that one", () => {
    expect(formAfterClose("groq", "groq")).toBeNull();
    // Another form opened while the first one's key was checked stays open.
    expect(formAfterClose("openai", "groq")).toBe("openai");
    expect(formAfterClose(null, "groq")).toBeNull();
  });
});

describe("deleteThenRead", () => {
  it("lets the buttons come back only once the list was read again, also after a failure", async () => {
    const steps: string[] = [];
    let listed: () => void = () => {};
    const done = deleteThenRead(
      async () => steps.push("deleted"),
      () => new Promise<void>((resolve) => (listed = () => (steps.push("listed"), resolve()))),
      () => steps.push("failed"),
      () => steps.push("released"),
    );
    await Promise.resolve();
    await Promise.resolve();
    expect(steps).toEqual(["deleted"]);
    listed();
    await done;
    expect(steps).toEqual(["deleted", "listed", "released"]);

    steps.length = 0;
    await deleteThenRead(
      () => Promise.reject(new Error("in use")),
      async () => void steps.push("listed"),
      () => steps.push("failed"),
      () => steps.push("released"),
    );
    expect(steps).toEqual(["failed", "listed", "released"]);
  });
});

describe("splitCloud", () => {
  const engine = (id: string, service: CloudEngineInfo["service"]): CloudEngineInfo => ({
    id,
    name: id,
    service,
    translate: false,
    files: true,
    languages: [],
    takes_dictionary: false,
    takes_language: false,
  });
  const connection = (provider: Connection["provider"], base_url: string | null): Connection => ({
    id: provider,
    name: provider,
    provider,
    base_url,
    key_hint: null,
    added_at_ms: 0,
    models: [],
  });

  it("puts the services with a key first and hides the rest after", () => {
    const engines = [engine("groq", "groq"), engine("openai", "openai"), engine("deepgram", "deepgram")];
    const own = connection("open_ai_compatible", "http://localhost:11434/v1");
    const { connected, locked } = splitCloud(engines, [connection("deepgram", null), own]);
    expect(connected.map((item) => item.id)).toEqual(["deepgram"]);
    expect(locked.map((item) => item.id)).toEqual(["groq", "openai"]);
    expect(splitCloud(engines, []).connected).toEqual([]);
  });
});

let t: TFunction;

beforeAll(async () => {
  t = (await initI18n("en")).t;
});

describe("pausedRecording", () => {
  it("resumes the hotkeys when the field goes away while recording", async () => {
    let resumed = 0;
    const recording = pausedRecording(async () => resumed++);
    await recording.end();
    expect(resumed).toBe(1);
    // Ending again, as the field's cleanup after a finished recording does, resumes nothing more.
    expect(recording.end()).toBeNull();
    expect(resumed).toBe(1);
  });

  it("resumes after the save, also when the save fails", async () => {
    const steps: string[] = [];
    const recording = pausedRecording(async () => steps.push("resume"));
    await recording.end(async () => {
      steps.push("save");
      throw new Error("refused");
    });
    expect(steps).toEqual(["save", "resume"]);
  });

  it("survives hotkeys that cannot be resumed", async () => {
    const recording = pausedRecording(() => Promise.reject(new Error("no window")));
    await expect(recording.end()).resolves.toBeUndefined();
  });

  it("waits for an ending that runs, or ends the recording, until the hotkeys are resumed", async () => {
    let resumed = 0;
    let saved: () => void = () => {};
    const recording = pausedRecording(async () => resumed++);
    void recording.end(() => new Promise<void>((resolve) => (saved = resolve)));
    let done = false;
    const waiting = recording.ended().then(() => (done = true));
    await Promise.resolve();
    expect(done).toBe(false);
    saved();
    await waiting;
    expect(resumed).toBe(1);
    // A recording that still runs is ended by it.
    const running = pausedRecording(async () => resumed++);
    await running.ended();
    expect(resumed).toBe(2);
  });
});

describe("oneRecording", () => {
  it("lets one field record at a time: the one before ends and resumes the hotkeys first", async () => {
    const calls: string[] = [];
    const recorder = oneRecording(async () => calls.push("suspend"), async () => calls.push("resume"));
    let firstStopped = false;
    const first = await recorder.start(() => (firstStopped = true));
    expect(first).not.toBeNull();
    const second = await recorder.start(() => {});
    expect(firstStopped).toBe(true);
    expect(calls).toEqual(["suspend", "resume", "suspend"]);
    // The first field's own ending, when it notices, resumes nothing more.
    expect(first?.end()).toBeNull();
    await second?.end();
    expect(calls).toEqual(["suspend", "resume", "suspend", "resume"]);
  });

  it("takes two clicks at once one after the other, so the hotkeys stay paused for the second", async () => {
    const calls: string[] = [];
    const recorder = oneRecording(async () => calls.push("suspend"), async () => calls.push("resume"));
    const [first, second] = await Promise.all([recorder.start(() => {}), recorder.start(() => {})]);
    expect(calls).toEqual(["suspend", "resume", "suspend"]);
    expect(first?.end()).toBeNull();
    expect(second).not.toBeNull();
  });

  it("pauses for the next field only once a field that ended by itself has saved and resumed", async () => {
    const calls: string[] = [];
    const recorder = oneRecording(async () => calls.push("suspend"), async () => calls.push("resume"));
    let saved: () => void = () => {};
    const first = await recorder.start(() => {});
    // A combination was pressed: its save runs, the hotkeys come back after it.
    void first?.end(() => new Promise<void>((resolve) => (saved = resolve)));
    const second = recorder.start(() => {});
    for (let tick = 0; tick < 5; tick++) await Promise.resolve();
    expect(calls).toEqual(["suspend"]);
    saved();
    expect(await second).not.toBeNull();
    expect(calls).toEqual(["suspend", "resume", "suspend"]);
  });

  it("does not listen when the hotkeys cannot be paused, and the next field still can", async () => {
    let refuse = true;
    const recorder = oneRecording(
      async () => {
        if (refuse) throw new Error("no window");
      },
      async () => {},
    );
    expect(await recorder.start(() => {})).toBeNull();
    refuse = false;
    expect(await recorder.start(() => {})).not.toBeNull();
  });
});

describe("recordedKey", () => {
  const key = (code: string, mods: Partial<Record<"ctrlKey" | "altKey" | "shiftKey" | "metaKey", boolean>> = {}) => ({
    code,
    ctrlKey: false,
    altKey: false,
    shiftKey: false,
    metaKey: false,
    ...mods,
  });

  it("lets Tab and Shift+Tab move the focus on and back, as anywhere", () => {
    expect(recordedKey(key("Tab"))).toEqual({ kind: "pass" });
    expect(recordedKey(key("Tab", { shiftKey: true }))).toEqual({ kind: "pass" });
    expect(recordedKey(key("Tab", { ctrlKey: true }))).toEqual({ kind: "take", hotkey: "Control+Tab" });
    expect(recordedKey(key("Tab", { ctrlKey: true, shiftKey: true }))).toEqual({ kind: "take", hotkey: "Control+Shift+Tab" });
    expect(recordedKey(key("Tab", { altKey: true }))).toEqual({ kind: "take", hotkey: "Alt+Tab" });
    expect(recordedKey(key("Tab", { metaKey: true }))).toEqual({ kind: "take", hotkey: "Super+Tab" });
  });

  it("cancels on Escape alone", () => {
    expect(recordedKey(key("Escape"))).toEqual({ kind: "cancel" });
  });

  it("waits on a key that types alone and on modifiers alone, and takes a combination", () => {
    for (const code of ["Enter", "Space", "Backspace", "KeyA", "Digit1", "ArrowDown"]) expect(recordedKey(key(code)), code).toEqual({ kind: "wait" });
    expect(recordedKey(key("AltLeft", { altKey: true }))).toEqual({ kind: "wait" });
    expect(recordedKey(key("Space", { ctrlKey: true, altKey: true }))).toEqual({ kind: "take", hotkey: "Control+Alt+Space" });
    expect(recordedKey(key("F9"))).toEqual({ kind: "take", hotkey: "F9" });
  });
});

describe("hotkeyFieldName", () => {
  it("names the combination for screen readers, or that the field waits for one", () => {
    expect(hotkeyFieldName(t, "Main mode", "control+alt+KeyD", false)).toBe("Main mode: Ctrl + Alt + D");
    expect(hotkeyFieldName(t, "Main mode", "control+alt+KeyD", true)).toBe("Main mode: Press keys…");
    expect(hotkeyFieldName(t, "Mode of the active app", null, false)).toBe("Mode of the active app: Not set");
  });
});

describe("rovingStep", () => {
  const all = [true, true, true, true];

  it("moves with the arrows and goes round the ends", () => {
    expect(rovingStep("ArrowRight", 0, all)).toBe(1);
    expect(rovingStep("ArrowDown", 3, all)).toBe(0);
    expect(rovingStep("ArrowLeft", 0, all)).toBe(3);
    expect(rovingStep("ArrowUp", 2, all)).toBe(1);
  });

  it("goes to the ends with Home and End", () => {
    expect(rovingStep("Home", 2, all)).toBe(0);
    expect(rovingStep("End", 1, all)).toBe(3);
  });

  it("skips the items that cannot be chosen", () => {
    const some = [false, true, false, true];
    expect(rovingStep("ArrowRight", 1, some)).toBe(3);
    expect(rovingStep("ArrowRight", 3, some)).toBe(1);
    expect(rovingStep("Home", 3, some)).toBe(1);
    expect(rovingStep("End", 1, [true, true, false])).toBe(1);
    expect(rovingStep("ArrowLeft", 0, [true, false])).toBe(0);
  });

  it("leaves every other key alone", () => {
    expect(rovingStep("Enter", 0, all)).toBeNull();
    expect(rovingStep(" ", 0, all)).toBeNull();
    expect(rovingStep("Tab", 0, all)).toBeNull();
  });
});

describe("shownTitle", () => {
  it("finds the title of the screen shown, not of one kept hidden before it", () => {
    const title = (name: string, hidden: boolean) => ({ name, closest: () => (hidden ? {} : null) });
    const root = (...titles: ReturnType<typeof title>[]) => ({ querySelectorAll: () => titles }) as unknown as ParentNode;
    // The file view of Transcripts stays mounted, hidden, before the list.
    expect(shownTitle(root(title("file", true), title("list", false)))).toMatchObject({ name: "list" });
    expect(shownTitle(root(title("file", true)))).toBeNull();
  });
});

describe("drawnScreen", () => {
  it("names each screen of Transcripts apart, so another one starts at the top", () => {
    expect(drawnScreen({ kind: "list" }, false)).toBe("list");
    expect(drawnScreen({ kind: "file" }, true)).toBe("file");
    // The file screen without a file draws the list.
    expect(drawnScreen({ kind: "file" }, false)).toBe("list");
    expect(drawnScreen({ kind: "entry", id: 3 }, false)).toBe("entry:3");
    expect(drawnScreen({ kind: "entry", id: 4 }, false)).not.toBe(drawnScreen({ kind: "entry", id: 3 }, false));
  });
});

describe("afterCopy", () => {
  it("says what was copied, or that the clipboard refused it until the next copy", () => {
    expect(NO_COPIES).toEqual({ copied: null, failed: null });
    const refused = afterCopy("12", false);
    expect(refused).toEqual({ copied: null, failed: "12" });
    // Another copy that goes through takes the refusal away, and one refused takes "Copied" away.
    expect(afterCopy("12-raw", true)).toEqual({ copied: "12-raw", failed: null });
    expect(afterCopy("13", false)).toEqual({ copied: null, failed: "13" });
  });
});

describe("focusAfterDeletion", () => {
  it("waits for the list without the deleted entry, then takes the row in its place, else the title", () => {
    const one = { ids: [4], before: [5, 4, 3] };
    expect(focusAfterDeletion(one, [5, 4, 3])).toBe("wait");
    expect(focusAfterDeletion(one, [5, 3])).toBe(3);
    expect(focusAfterDeletion({ ids: [5], before: [5] }, [])).toBe("title");
  });

  it("puts the focus on the title after the history is cleared, also when a dictation came meanwhile", () => {
    const all = { ids: [5, 4, 3], before: [5, 4, 3] };
    expect(focusAfterDeletion(all, [5, 4, 3])).toBe("wait");
    expect(focusAfterDeletion(all, [])).toBe("title");
    expect(focusAfterDeletion(all, [6])).toBe("title");
  });
});

describe("renameKey", () => {
  it("saves the new title on Enter and drops it on Escape; both give the focus to the title", () => {
    expect(renameKey("Enter")).toBe(true);
    expect(renameKey("Escape")).toBe(false);
    expect(renameKey("a")).toBeNull();
    expect(renameKey("Tab")).toBeNull();
  });
});

describe("focusReturns", () => {
  it("brings the focus back to the field once its check is over, unless the user put it elsewhere", () => {
    expect(focusReturns(true, false, true)).toBe(true);
    expect(focusReturns(true, false, false)).toBe(false);
    // Still checking, or not checked at all.
    expect(focusReturns(true, true, true)).toBe(false);
    expect(focusReturns(false, false, true)).toBe(false);
  });
});

describe("rowAfterRemoval", () => {
  it("takes the row now in the place of the deleted one, else the one above, else none", () => {
    expect(rowAfterRemoval([5, 4, 3], 4, [5, 3])).toBe(3);
    expect(rowAfterRemoval([5, 4, 3], 3, [5, 4])).toBe(4);
    expect(rowAfterRemoval([5], 5, [])).toBeNull();
    // A new dictation came on top meanwhile; the row below is still the one in its place.
    expect(rowAfterRemoval([5, 4, 3], 4, [6, 5, 3])).toBe(3);
    expect(rowAfterRemoval([5, 4], 7, [5, 4])).toBeNull();
  });
});

describe("useRoving", () => {
  // A group of three tabs, the first chosen; what each arrow key focused and pressed.
  function group(options?: { manual?: boolean }) {
    const focused: number[] = [];
    const pressed: number[] = [];
    const items = [0, 1, 2].map((index) => ({ disabled: false, focus: () => focused.push(index), click: () => pressed.push(index) }));
    const roving = useRoving([{ checked: true }, { checked: false }, { checked: false }], options);
    const key = (key: string, from: number) =>
      roving.group.onKeyDown({
        key,
        target: items[from],
        currentTarget: { querySelectorAll: () => items },
        preventDefault: () => {},
      } as unknown as KeyboardEvent<HTMLElement>);
    return { key, focused, pressed };
  }

  it("chooses the item an arrow moves to", () => {
    const { key, focused, pressed } = group();
    key("ArrowDown", 0);
    expect(focused).toEqual([1]);
    expect(pressed).toEqual([1]);
  });

  it("only moves the focus when choosing is manual: Enter or Space on the button chooses", () => {
    const { key, focused, pressed } = group({ manual: true });
    key("ArrowDown", 0);
    key("End", 1);
    expect(focused).toEqual([1, 2]);
    expect(pressed).toEqual([]);
  });
});

describe("tabStop", () => {
  it("stops Tab at the chosen item, else at the first that can be chosen", () => {
    expect(tabStop([{ checked: false }, { checked: true }, { checked: false }])).toBe(1);
    expect(tabStop([{ checked: false, disabled: true }, { checked: false }, { checked: false }])).toBe(1);
    expect(tabStop([{ checked: true, disabled: true }, { checked: false }])).toBe(1);
    expect(tabStop([{ checked: false, disabled: true }])).toBe(-1);
  });
});

describe("pressGuard", () => {
  it("lets presses go for a while after a hold, so a double click acts once", () => {
    let now = 1000;
    const guard = pressGuard(() => now);
    const done: string[] = [];
    guard.press(400, () => done.push("first row"));
    now += 250;
    guard.press(400, () => done.push("the row that moved up"));
    expect(done).toEqual(["first row"]);
    now += 200;
    guard.press(400, () => done.push("next row"));
    expect(done).toEqual(["first row", "next row"]);
  });

  it("lets a button that has just appeared wait before it acts", () => {
    let now = 0;
    const guard = pressGuard(() => now);
    expect(guard.passes()).toBe(true);
    guard.hold(500);
    now = 300;
    expect(guard.passes()).toBe(false);
    now = 500;
    expect(guard.passes()).toBe(true);
  });
});

describe("trappedTab", () => {
  const items = ["usage", "got it"];

  it("keeps Tab inside the dialog at both ends", () => {
    expect(trappedTab(items, "got it", false)).toBe("usage");
    expect(trappedTab(items, "usage", true)).toBe("got it");
  });

  it("leaves the steps between to the browser", () => {
    expect(trappedTab(items, "usage", false)).toBeNull();
    expect(trappedTab(items, "got it", true)).toBeNull();
  });

  it("brings a focus from outside into the dialog", () => {
    expect(trappedTab(items, null, false)).toBe("usage");
    expect(trappedTab(items, "page behind", true)).toBe("got it");
    expect(trappedTab([], null, false)).toBeNull();
  });
});

describe("speakerModelsRead", () => {
  const model = (event: Extract<CoreEvent, { source: "model" }>["event"]): CoreEvent => ({ source: "model", event });
  const file = (event: Extract<CoreEvent, { source: "file" }>["event"]): CoreEvent => ({ source: "file", event });

  it("reads the speaker models again after their download ends, however it ends", () => {
    expect(speakerModelsRead(model({ type: "download_finished", model: "speakers" }))).toBe(true);
    expect(speakerModelsRead(model({ type: "download_failed", model: "speakers", failure: "cancelled", detail: "" }))).toBe(true);
    expect(speakerModelsRead(model({ type: "download_failed", model: "speakers", failure: "network", detail: "timeout" }))).toBe(true);
  });

  it("does not for other models or for the download's progress", () => {
    expect(speakerModelsRead(model({ type: "download_failed", model: "whisper-medium", failure: "cancelled", detail: "" }))).toBe(false);
    expect(speakerModelsRead(model({ type: "download_progress", model: "speakers", downloaded: 1, total: 2 }))).toBe(false);
  });

  it("reads them whenever a file's voices are told apart and when the file ends", () => {
    expect(speakerModelsRead(file({ type: "speakers", file: 4, step: "segments", done_ms: 0, total_ms: null }))).toBe(true);
    expect(speakerModelsRead(file({ type: "speakers", file: 4, step: "voices", done_ms: 1, total_ms: 2 }))).toBe(true);
    expect(speakerModelsRead(file({ type: "finished", file: 4, path: "talk.md", processing_error: null, transcript: 1 }))).toBe(true);
    expect(speakerModelsRead(file({ type: "failed", file: 4, problem: "save", detail: "", transcript: null, held: false }))).toBe(true);
    expect(speakerModelsRead(file({ type: "cancelled", file: 4 }))).toBe(true);
    expect(speakerModelsRead(file({ type: "progress", file: 5, done_ms: 1, total_ms: 2 }))).toBe(false);
  });
});

describe("the speaker models after their download", () => {
  const model = (event: Extract<CoreEvent, { source: "model" }>["event"]): CoreEvent => ({ source: "model", event });

  it("ends with its finish or its failure, of the speaker models only", () => {
    expect(speakerDownloadEnded(model({ type: "download_finished", model: "speakers" }))).toBe(true);
    expect(speakerDownloadEnded(model({ type: "download_failed", model: "speakers", failure: "cancelled", detail: "" }))).toBe(true);
    expect(speakerDownloadEnded(model({ type: "unpacking", model: "speakers" }))).toBe(false);
    expect(speakerDownloadEnded(model({ type: "download_finished", model: "whisper-medium" }))).toBe(false);
  });

  it("waits for the view read after the end: an earlier answer says nothing of it", () => {
    // The end came while read 3 was on its way; read 4 follows it.
    expect(speakerViewRead(4, 3)).toBe(4);
    expect(speakerViewRead(4, 4)).toBeNull();
    expect(speakerViewRead(4, 5)).toBeNull();
    expect(speakerViewRead(null, 2)).toBeNull();
  });
});

describe("speakerDeleteError", () => {
  it("names the backend's reason and keeps the detail of a failed deletion", () => {
    expect(speakerDeleteError(t, { kind: "in_use" })).toBe("The model is in use: a meeting or a file is being transcribed.");
    expect(speakerDeleteError(t, { kind: "store", detail: "permission denied" })).toBe("Could not delete the model: permission denied");
  });

  it("shows anything else as it is, never a key", () => {
    expect(speakerDeleteError(t, "no such command")).toBe("Could not delete the model: no such command");
    expect(speakerDeleteError(t, { kind: "new_reason" })).toBe("Could not delete the model: new_reason");
  });
});

describe("fileStartError", () => {
  it("names the backend's reasons", () => {
    expect(fileStartError(t, { kind: "text_too_long" })).toBe("The text file is over 1 MB.");
    expect(fileStartError(t, { kind: "read", detail: "denied" })).toBe("Could not read the file: denied");
  });

  it("shows a failure that is not one of them as it is, never a key", () => {
    expect(fileStartError(t, "command transcribe_file not found")).toBe("Could not start: command transcribe_file not found");
    expect(fileStartError(t, { kind: "new_reason" })).toBe("Could not start: new_reason");
  });
});

describe("commandError", () => {
  it("names a meeting command's failure by its kind", () => {
    expect(commandError(t, { kind: "in_use" })).toBe("This meeting is being recorded or transcribed.");
    expect(commandError(t, { kind: "save", detail: "denied" })).toBe("Could not write the file: denied");
    // Refused before the echo is found when the chosen engine cannot transcribe a meeting.
    expect(commandError(t, { kind: "no_word_times" })).toContain("cannot transcribe meetings");
    expect(commandError(t, { kind: "no_model" })).toBe("Choose a speech model first.");
    expect(commandError(t, { kind: "no_engine" })).toBe("No speech engine for the chosen model.");
  });

  it("shows any other failure with a text of its own, never a key", () => {
    expect(commandError(t, { kind: "new_reason" })).toBe("Something went wrong: new_reason");
    expect(commandError(t, "command rename_meeting not found")).toBe("Something went wrong: command rename_meeting not found");
  });
});

describe("stateText", () => {
  const meeting = (problem: string): MeetingSummary => ({
    id: 1,
    started_ms: 0,
    duration_ms: 0,
    title: null,
    state: "failed",
    problem: problem as MeetingSummary["problem"],
    preview: null,
    kind: "meeting",
    source: null,
    output: null,
  });

  it("says why a meeting failed, and only that it failed for a problem it has no words for", () => {
    expect(stateText(t, meeting("no_speech"), NO_LIVE)).toBe("No speech found.");
    expect(stateText(t, meeting("busy"), NO_LIVE)).toBe("Failed");
  });
});

describe("problemText", () => {
  it("says why an entry was not transcribed on its page, with the detail, and never shows a key", () => {
    expect(problemText(t, "read", "bad header")).toBe("Could not read the recording. bad header");
    expect(problemText(t, "no_speech")).toBe("No speech found.");
    expect(problemText(t, "busy", "another one runs")).toBe("Failed");
  });
});

describe("the reasons a file or a meeting was not transcribed", () => {
  // Every reason the backend gives: a new one is asked for here by the compiler.
  const files: Record<FileProblem, true> = {
    busy: true,
    no_model: true,
    no_engine: true,
    read: true,
    transcription: true,
    no_word_times: true,
    no_translation: true,
    no_speech: true,
    processing: true,
    save: true,
  };
  const meetings: Record<MeetingProblem, true> = {
    no_model: true,
    no_engine: true,
    read: true,
    transcription: true,
    no_word_times: true,
    no_speech: true,
    save: true,
  };

  it("have a text in every language", () => {
    for (const language of supportedLanguages) {
      const texts = resources[language].translation;
      for (const problem of Object.keys(files)) {
        expect((texts.files.problem as Record<string, string>)[problem], `${language}: files.problem.${problem}`).toBeTruthy();
      }
      for (const problem of Object.keys(meetings)) {
        expect((texts.meetings.problem as Record<string, string>)[problem], `${language}: meetings.problem.${problem}`).toBeTruthy();
      }
    }
  });

  it("say what to do once the model chosen midway cannot go on", () => {
    expect(problemText(t, "no_word_times")).toBe("The chosen model does not transcribe meetings. Choose another and transcribe again.");
    expect(t("files.problem.no_word_times", { detail: "" })).toBe("The chosen model does not transcribe files. Choose another and start again.");
    expect(t("files.problem.no_translation", { detail: "" })).toBe("The chosen model does not translate. Choose another and start again.");
  });
});

describe("connectionErrorText", () => {
  it("names the backend's reason, a refused save by the save's error, and anything else as it is", () => {
    expect(connectionErrorText(t, { kind: "unauthorized" })).toBe("The service did not accept the key. Check that it was copied in full.");
    expect(connectionErrorText(t, { kind: "network", detail: "timeout" })).toBe("Service unreachable: timeout");
    expect(connectionErrorText(t, { kind: "settings", detail: { kind: "read_only" } })).toBe("Settings file unreadable, changes are not saved.");
    expect(connectionErrorText(t, { kind: "new_reason" })).toBe("Something went wrong: new_reason");
    expect(connectionErrorText(t, "no such command")).toBe("Something went wrong: no such command");
  });

  it("names every reason the backend gives, in every language", () => {
    expect(connectionErrorText(t, { kind: "client_forgotten" })).toBe("ChatGPT no longer recognizes ahem. Sign in again.");
    const body = commandsSource.match(/pub enum ConnectionError \{([\s\S]*?)\n\}/)?.[1] ?? "";
    const kinds = Array.from(body.matchAll(/^\s{4}([A-Z]\w*)/gm), ([, name]) => name.replace(/(?<!^)([A-Z])/g, "_$1").toLowerCase());
    expect(kinds).toContain("client_forgotten");
    // A refused save is named by the save's error.
    const named = kinds.filter((kind) => kind !== "settings");
    for (const language of supportedLanguages) {
      const texts = resources[language].translation.connections.error as Record<string, string>;
      expect(named.filter((kind) => !texts[kind])).toEqual([]);
    }
  });
});

describe("chooseLanguage", () => {
  it("shows the language only once it is saved", async () => {
    const shown: string[] = [];
    const saved: SettingsChange[] = [];
    await chooseLanguage("ru", async (change) => (saved.push(change), null), (choice) => shown.push(choice));
    expect(saved).toEqual([{ ui_language: "ru" }]);
    expect(shown).toEqual(["ru"]);
  });

  it("keeps the language shown when the save is refused", async () => {
    const shown: string[] = [];
    await chooseLanguage("ru", async () => ({ kind: "read_only" }), (choice) => shown.push(choice));
    expect(shown).toEqual([]);
  });
});

describe("deleteModelError", () => {
  it("names why dictation could not move to another model by the save error", () => {
    const failure = { kind: "settings", detail: { kind: "hotkey_taken", detail: "control+alt+KeyD" } };
    expect(deleteModelError(t, failure)).toBe("Not deleted: dictation could not move to another model. This shortcut is already taken.");
  });

  it("names the other reasons and shows anything else as it is", () => {
    expect(deleteModelError(t, { kind: "downloading" })).toBe("Cancel the download first.");
    expect(deleteModelError(t, { kind: "deleting" })).toBe("This model is already being deleted.");
    expect(deleteModelError(t, { kind: "io", detail: "busy" })).toBe("Could not delete: busy");
    expect(deleteModelError(t, "no such command")).toBe("Could not delete: no such command");
  });
});

describe("steppedIdle", () => {
  it("steps the minutes from the policy saved last, so two quick presses make two steps", () => {
    const once = steppedIdle({ mode: "after_idle", minutes: 20 }, 1);
    expect(once).toEqual({ mode: "after_idle", minutes: 25 });
    expect(steppedIdle(once, 1)).toEqual({ mode: "after_idle", minutes: 30 });
    expect(steppedIdle({ mode: "after_idle", minutes: 20 }, -1)).toEqual({ mode: "after_idle", minutes: 15 });
  });

  it("keeps to the steps' grid and the bounds", () => {
    expect(steppedIdle({ mode: "after_idle", minutes: 7 }, 1)).toEqual({ mode: "after_idle", minutes: 10 });
    expect(steppedIdle({ mode: "after_idle", minutes: 5 }, -1)).toEqual({ mode: "after_idle", minutes: 5 });
    expect(steppedIdle({ mode: "after_idle", minutes: 120 }, 1)).toEqual({ mode: "after_idle", minutes: 120 });
  });

  it("leaves another policy saved meanwhile as it is", () => {
    expect(steppedIdle({ mode: "never" }, 1)).toEqual({ mode: "never" });
  });
});

describe("entryModeName", () => {
  const email = {
    id: "mode-lq4z9k",
    name: "Email",
    action: { kind: "process" as const, connection_id: "c1", model: "m", instruction: "x", output_language: null },
    hotkey: null,
    selection_hotkey: null,
  };
  const plain = { id: "plain", name: "", action: { kind: "plain" as const }, hotkey: null, selection_hotkey: null };

  it("names the mode of an entry as the Modes page does", () => {
    expect(entryModeName(t, [plain, email], "plain")).toBe("Plain dictation");
    expect(entryModeName(t, [plain, email], "mode-lq4z9k")).toBe("Email");
  });

  it("names a built-in mode turned off by its name, and no own mode deleted since", () => {
    expect(entryModeName(t, [plain], "english")).toBe("In English");
    expect(entryModeName(t, [plain], "mode-lq4z9k")).toBeNull();
  });
});

describe("reportCopy", () => {
  it("says the clipboard refused a copy, and takes it away once a copy goes through", () => {
    let shown: Message | null = { text: "Saved to /tmp/talk.md", error: false };
    const messages = ownMessages((change) => (shown = change(shown)));
    reportCopy(messages, false, "The clipboard did not take the text.");
    expect(shown).toEqual({ text: "The clipboard did not take the text.", error: true });
    reportCopy(messages, true, "The clipboard did not take the text.");
    expect(shown).toBeNull();
  });
});

describe("limitChoice", () => {
  it("saves nothing as Last is chosen with All saved: the field with a number shows first", () => {
    expect(limitChoice("last", null, false, false)).toBe("choose");
    expect(limitChoice("last", null, true, false)).toBe("none");
    expect(limitChoice("last", 25, false, false)).toBe("none");
  });

  it("saves All over a number, and drops a Last not saved yet", () => {
    expect(limitChoice("all", 25, false, false)).toEqual({ save: null });
    expect(limitChoice("all", null, true, false)).toBe("drop");
    expect(limitChoice("all", null, false, false)).toBe("none");
  });

  it("saves All after a number on its way, which a drop would let land", () => {
    expect(limitChoice("all", null, true, true)).toEqual({ save: null });
    expect(limitChoice("all", null, false, true)).toEqual({ save: null });
  });

  it("is made with the focus kept in the field: the number typed there is not saved first", () => {
    const limit = historyPanelSource.slice(historyPanelSource.indexOf("function HistoryLimit"));
    // A press on the choice does not take the focus out of the control…
    expect(limit).toMatch(
      /onMouseDown=\{\(event\) => \{\s*const radio = \(event\.target as HTMLElement\)\.closest<HTMLElement>\('\[role="radio"\]'\);\s*if \(!radio \|\| !focusWithin\(box\.current, document\.activeElement\)\) return;\s*event\.preventDefault\(\);/,
    );
    // …so only leaving it saves, and "All" drops the number typed, its field's focus going to "All".
    expect(limit.match(/onBlur=/g)).toEqual(["onBlur="]);
    expect(limit).toMatch(/<span\s+ref=\{box\}[\s\S]*onBlur=[\s\S]*<Segments/);
    expect(limit).toMatch(
      /\} else if \(choice !== "none"\) \{\s*dropped\.current = true;\s*if \(document\.activeElement === field\.current\) follow\(checked\);\s*backToAll\(\);\s*if \(choice !== "drop"\) void onChange\(choice\.save\);/,
    );
    expect(limit).toMatch(/limitChoice\(value, limit, choosing, saving\.current > 0\)/);
  });
});

describe("limitLeft", () => {
  it("saves the number as the focus leaves the control inside the window, not on its way to the choice nor to another window", () => {
    expect(limitLeft(false, true)).toBe(true);
    expect(limitLeft(true, true)).toBe(false);
    expect(limitLeft(false, false)).toBe(false);
    const limit = historyPanelSource.slice(historyPanelSource.indexOf("function HistoryLimit"));
    expect(limit).toMatch(/limitLeft\(focusWithin\(box\.current, event\.relatedTarget\), document\.hasFocus\(\)\)\) commit\(false\);/);
  });
});

describe("limitStep", () => {
  it("saves another number on Enter or as the field is left, and puts back the saved one for a wrong one", () => {
    expect(limitStep("5", 25, 25, true)).toEqual({ save: 5 });
    expect(limitStep(" 5 ", 25, 25, false)).toEqual({ save: 5 });
    expect(limitStep("25", 25, 25, false)).toEqual({ show: "25" });
    expect(limitStep("0", 25, 25, true)).toEqual({ show: "25" });
    expect(limitStep("many", 25, 25, false)).toEqual({ show: "25" });
  });

  it("with Last just chosen, saves the number shown only on Enter, and goes back to All when left without another", () => {
    expect(limitStep("10", null, 10, true)).toEqual({ save: 10 });
    expect(limitStep("10", null, 10, false)).toBe("all");
    expect(limitStep("500", null, 10, false)).toEqual({ save: 500 });
    expect(limitStep("", null, 10, false)).toBe("all");
    expect(limitStep("", null, 10, true)).toEqual({ show: "10" });
  });
});

describe("the form of a new own server", () => {
  it("opens empty and takes what it held away as it closes, so a key never goes to another address", () => {
    expect(NEW_SERVER).toEqual({ name: "", baseUrl: "", key: "" });
    const own = connectionsPanelSource.slice(connectionsPanelSource.indexOf("function OwnServers"));
    expect(own).toMatch(/useState<ServerDraft \| null>\(null\)/);
    expect(own).not.toMatch(/useState\(""\)/);
    // "Add server" opens it empty; "Cancel" and a server added close it.
    expect(own).toMatch(/onClick=\{\(\) => \{\s*setDraft\(NEW_SERVER\);/);
    expect(own).toMatch(/onCancel=\{\(\) => \{\s*toAdd\(\);\s*setDraft\(null\);/);
    expect(own).toMatch(/if \(added\) setDraft\(null\);/);
  });
});

describe("buttons that go with their action", () => {
  it("hand the focus they have to what is beside them", () => {
    // The last message closed: the title of the page shown.
    expect(appSource).toMatch(/onClose=\{\(\) => \{\s*follow\(\(\) => \(content\.current \? shownTitle\(content\.current\) : null\)\);/);
    // "Reset to default" of the filler words: the words.
    expect(modeEditorsSource).toMatch(/<textarea\s+ref=\{wordsField\}/);
    expect(modeEditorsSource).toMatch(/onClick=\{\(\) => \{\s*follow\(\(\) => wordsField\.current\);\s*setWords\(DEFAULT_FILLERS\);/);
    // The last recent app added: the field for a name.
    expect(modeEditorsSource).toMatch(/if \(!app\) return;\s*follow\(\(\) => typedField\.current\);\s*add\(app\);/);
    // "Recognize again": the dictation card's title.
    expect(statusPanelSource).toMatch(/<div ref=\{title\} className="hero-title" tabIndex=\{-1\}>/);
    expect(statusPanelSource).toMatch(/onClick=\{\(\) => \{\s*follow\(\(\) => title\.current\);\s*retry\.retry\(\);/);
    // "Last" not saved yet, dropped by Escape or refused after Enter: "All".
    expect(historyPanelSource).toMatch(/const checked = \(\) => box\.current\?\.querySelector<HTMLElement>\('\[role="radio"\]\[aria-checked="true"\]'\);/);
    expect(historyPanelSource).toMatch(/dropped\.current = true;\s*follow\(checked\);\s*backToAll\(\);/);
    expect(historyPanelSource).toMatch(
      /keepSaved\(onChange\(step\.save\), \(\) => \{\s*\/\/[^\n]*\n\s*if \(limit === null\) \{\s*dropped\.current = true;\s*follow\(checked\);\s*\}\s*setChoosing\(false\);/,
    );
  });
});

describe("outcomes and failures", () => {
  it("are said by screen readers as they show", () => {
    expect(filesPanelSource).toMatch(/<p className="hero-title" role="status">\s*\{t\("files\.saved"\)\}/);
    expect(filesPanelSource).toMatch(/<p className="caption" role="status">\s*\{t\("files\.cancelled"\)\}/);
    expect(filesPanelSource).toMatch(/role="alert">\s*\{t\(`files\.problem\.\$\{job\.problem\}`/);
    expect(filesPanelSource).toMatch(/role="alert">\s*\{t\("files\.processing_failed"/);
    expect(meetingsPanelSource).toMatch(/role="alert">\s*\{problemText\(t, problem, failure\?\.detail\)\}/);
    expect(modelsPanelSource).toMatch(/role="alert">\s*\{t\(`models\.failed\.\$\{download\.failure\}`\)\}/);
    expect(speakerModelsSource).toMatch(/role="alert">\s*\{t\(`models\.failed\.\$\{download\.failure\}`\)\}/);
    expect(transcriptsPanelSource).toMatch(/role="alert">\s*\{t\("transcripts\.load_failed"/);
  });
});

describe("a file's transcript kept although its result could not be written", () => {
  const file = (event: Extract<CoreEvent, { source: "file" }>["event"]): CoreEvent => ({ source: "file", event });

  it("is read into the list, as one kept with its result", () => {
    expect(transcriptKept(file({ type: "finished", file: 1, path: "a.md", processing_error: null, transcript: 3 }))).toBe(true);
    expect(transcriptKept(file({ type: "failed", file: 1, problem: "save", detail: "disk full", transcript: 3, held: true }))).toBe(true);
    expect(transcriptKept(file({ type: "finished", file: 1, path: "a.txt", processing_error: null, transcript: null }))).toBe(false);
    expect(transcriptKept(file({ type: "failed", file: 1, problem: "transcription", detail: "", transcript: null, held: false }))).toBe(false);
    expect(transcriptKept(file({ type: "cancelled", file: 1 }))).toBe(false);
  });

  it("opens from the file view, and its page says it is not in a file", () => {
    expect(filesPanelSource).toMatch(
      /\{transcript !== null && \(\s*<button type="button" className=\{job\.held \? "btn" : "btn primary"\} onClick=\{\(\) => onOpen\(transcript\)\}>\s*\{t\("transcripts\.open_transcript"\)\}/,
    );
    expect(meetingsPanelSource).toMatch(/\{unsavedFile\(meeting\) && <p className="caption">\{t\("transcripts\.not_saved"\)\}<\/p>\}/);
    expect(t("transcripts.not_saved")).toBe("Not saved to a file");
  });

  it("loses its mark once saved by \"Save as…\": the page reads the entry again with its saved file", () => {
    // The backend keeps the path and says the list changed; the list read again names the file.
    expect(meetingsPanelSource).toMatch(/if \(\(event\.type !== "recording" && event\.type !== "progress"\) \|\| fresh\) void load\(\);/);
    expect(meetingsPanelSource).toMatch(
      /invoke<Meeting & \{ audio: string \}>\("get_meeting", \{ id \}\)\.then\(setMeeting, \(\) => onBack\(\)\);\s*\}, \[id, summary\?\.state, summary\?\.title, summary\?\.output, onBack\]\);/,
    );
    expect(meetingsPanelSource).toMatch(/\{meeting\.output && \(\s*<button type="button" className="btn link" onClick=\{\(\) => reveal\(meeting\.output as string\)\}>/);
  });
});

describe("a file's result that could not be written", () => {
  it("is saved elsewhere from the file view while it is held, beside its transcript", () => {
    expect(filesPanelSource).toMatch(
      /\{job\.held \|\| transcript !== null \? \(\s*<div className="file-actions">\s*\{job\.held && \(\s*<button type="button" className="btn primary" onClick=\{onSaveHeld\}>\s*\{t\("meetings\.save"\)\}/,
    );
    expect(t("meetings.save")).toBe("Save as…");
  });

  it("offers the file it was to be written to, in its format", () => {
    expect(heldDialog("/Users/me/talk.md")).toEqual({ defaultPath: "/Users/me/talk.md", filters: [{ name: "MD", extensions: ["md"] }] });
    expect(heldDialog("C:\\talks\\talk.srt")).toEqual({ defaultPath: "C:\\talks\\talk.srt", filters: [{ name: "SRT", extensions: ["srt"] }] });
    expect(heldDialog(null)).toEqual({});
  });

  it("is written where the user chose, with the place said, or why it could not be", () => {
    const saving = filesPanelSource.slice(filesPanelSource.indexOf("async function saveHeld"));
    expect(saving).toMatch(/chosen = await save\(heldDialog\(started\?\.output \?\? null\)\);/);
    expect(saving).toMatch(/if \(chosen === started\?\.path\) return messages\.fail\(t\("files\.same_file"\)\);/);
    expect(saving).toMatch(
      /await invoke\("save_held_result", \{ path: chosen \}\);\s*messages\.succeed\(\);\s*setMessage\(\{ text: t\("meetings\.saved", \{ path: chosen \}\), error: false \}\);\s*\} catch \(failure\) \{\s*messages\.fail\(t\("meetings\.error\.save", \{ detail: String\(failure\) \}\)\);/,
    );
    expect(t("meetings.saved", { path: "/tmp/talk.md" })).toBe("Saved: /tmp/talk.md");
    expect(t("meetings.error.save", { detail: "No space left on device" })).toBe("Could not write the file: No space left on device");
  });
});

describe("modelCredit", () => {
  it("credits a model's author and license in the tooltip of its name", () => {
    expect(modelCredit(t, { author: "NVIDIA", license: "CC-BY-4.0" })).toBe("NVIDIA · CC-BY-4.0 license");
    expect(modelsPanelSource).toMatch(/<span className="strong" title=\{modelCredit\(t, model\)\}>/);
  });
});

describe("modelSource", () => {
  it("names the repository on Hugging Face without the revision and file", () => {
    expect(modelSource("https://huggingface.co/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/ggml-medium.bin")).toEqual({
      label: "huggingface.co/ggerganov/whisper.cpp",
      page: "https://huggingface.co/ggerganov/whisper.cpp",
    });
  });

  it("names the project and opens the release a GitHub download comes from", () => {
    expect(modelSource("https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-x.tar.bz2")).toEqual({
      label: "github.com/k2-fsa/sherpa-onnx",
      page: "https://github.com/k2-fsa/sherpa-onnx/releases/tag/asr-models",
    });
  });

  it("falls back to the host, and says nothing about an address that is none", () => {
    expect(modelSource("https://example.com/a/b.bin")).toEqual({ label: "example.com", page: "https://example.com" });
    expect(modelSource("not an address")).toBeNull();
  });

  it("gives every model of the catalog a place to read, and the Models page shows it", () => {
    const models = (JSON.parse(modelCatalog) as { models: { id: string; url: string }[] }).models;
    expect(models.length).toBeGreaterThan(0);
    for (const model of models) {
      const source = modelSource(model.url);
      expect(source?.label, model.id).toMatch(/^(huggingface\.co|github\.com)\/[^/]+\/[^/]+$/);
      expect(source?.page, model.id).toMatch(/^https:\/\/(huggingface\.co|github\.com)\//);
    }
    expect(modelsPanelSource).toMatch(/modelSource\(model\.url\)/);
  });

  it("is shown for the speaker models too", () => {
    expect(speakerModelsSource).toMatch(/modelSource\(view\.models\[0\]\.url\)/);
    expect(speakerModelsSource).toMatch(/t\("models\.source"\)/);
  });
});

describe("hardwareBadge", () => {
  it("tells a model that needs a card, one that uses it if there is one, and one on the processor alone", () => {
    expect(hardwareBadge({ hardware: { kind: "gpu", memory_gb: 6 } })).toBe("gpu");
    expect(hardwareBadge({ hardware: { kind: "cpu" }, llama: {} })).toBe("gpu_optional");
    expect(hardwareBadge({ hardware: { kind: "cpu" } })).toBe("cpu");
  });

  it("is read from the catalog: only the llama.cpp models are the optional ones", () => {
    const models = (JSON.parse(modelCatalog) as { models: { id: string; hardware: { kind: string }; llama?: object }[] }).models;
    const optional = models.filter((model) => hardwareBadge(model as Pick<ModelInfo, "hardware" | "llama">) === "gpu_optional").map((model) => model.id);
    expect(optional).toEqual(["qwen3-asr-1_7b", "qwen3-asr-0_6b"]);
    expect(modelsPanelSource).toMatch(/hardwareBadge\(model\)/);
  });
});
