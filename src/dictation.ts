import { useCallback, useEffect, useRef, useState, type RefObject } from "react";
import { invoke } from "@tauri-apps/api/core";
import { CORE_EVENT, useEvent } from "./events";
import type {
  BlockReason,
  CoreEvent,
  DictationResult,
  DiscardReason,
  DownloadFailure,
  EngineState,
  Failure,
  FileEvent,
  FileProblem,
  SelectionProblem,
  SessionState,
  SpeakersStep,
} from "./events";

/// `processing`: a language model works on the transcript.
export type Phase = "idle" | "recording" | "processing" | "transcribing" | "inserting" | "rewriting";

/// The latest outcome worth telling the user about.
export type Notice =
  | { kind: "inserted" }
  /// Recognized again: the text is in the last result, not typed.
  | { kind: "kept" }
  /// Recognized again, but its language model failed: the text is the transcript, not typed.
  | { kind: "kept_unprocessed"; failure: Failure }
  | { kind: "discarded"; reason: DiscardReason }
  | { kind: "failed"; failure: Failure }
  | { kind: "blocked"; reason: BlockReason; detail: string }
  | { kind: "engine_failed"; detail: string }
  | { kind: "selection_done" }
  | { kind: "selection_failed"; problem: SelectionProblem; detail: string };

export interface Download {
  downloaded: number;
  total: number;
  /// Downloaded; the files are being taken out of the archive.
  unpacking?: boolean;
  failure?: DownloadFailure;
}

/// The current or last file transcription. `since`: when the step of telling voices apart began
/// (`Date.now()`), so its time counts from there whenever it is shown.
export type FileState =
  | { kind: "transcribing"; file: number; name: string; done_ms: number; total_ms: number | null }
  | { kind: "speakers"; file: number; name: string; step: SpeakersStep; done_ms: number; total_ms: number | null; since: number }
  | { kind: "processing"; file: number; name: string }
  | { kind: "finished"; file: number; name: string; path: string; processing_error: string | null; transcript: number | null }
  | { kind: "failed"; file: number; name: string; problem: FileProblem; detail: string; transcript: number | null; held: boolean }
  | { kind: "cancelled"; file: number; name: string };

function fileState(current: FileState | null, event: FileEvent): FileState {
  // A file that failed before it started (e.g. "busy") has no name yet.
  const name = current && current.file === event.file ? current.name : "";
  switch (event.type) {
    case "started":
      return { kind: "transcribing", file: event.file, name: event.name, done_ms: 0, total_ms: null };
    case "progress":
      return { kind: "transcribing", file: event.file, name, done_ms: event.done_ms, total_ms: event.total_ms };
    case "speakers": {
      const same = current?.kind === "speakers" && current.file === event.file && current.step === event.step;
      const since = same ? current.since : Date.now();
      return { kind: "speakers", file: event.file, name, step: event.step, done_ms: event.done_ms, total_ms: event.total_ms, since };
    }
    case "processing":
      return { kind: "processing", file: event.file, name };
    case "finished":
      return {
        kind: "finished",
        file: event.file,
        name,
        path: event.path,
        processing_error: event.processing_error,
        transcript: event.transcript,
      };
    case "failed":
      return {
        kind: "failed",
        file: event.file,
        name,
        problem: event.problem,
        detail: event.detail,
        transcript: event.transcript,
        held: event.held,
      };
    case "cancelled":
      return { kind: "cancelled", file: event.file, name };
  }
}

export interface View {
  sessions: Record<number, SessionState>;
  /// Language-model failures of sessions still in progress: their transcript goes on unprocessed.
  processingFailures: Record<number, Failure>;
  engine: EngineState;
  rewriting: boolean;
  /// The mode a rule for the program in focus chose for the recording, if it did.
  modeByApp: string | null;
  /// The recording's mode takes context (selected text, clipboard, app) to its model.
  contextTaken: boolean;
  /// The microphone stopped giving sound while `session` recorded: its text has only what came
  /// before. Kept until a dictation begins after `session` has its outcome.
  microphoneLost: { session: number; detail: string } | null;
  /// The text of `session` did not go to the history. Kept until the next dictation begins.
  historyFailed: { session: number | null; detail: string } | null;
  notice: Notice | null;
  /// The dictation whose outcome `notice` is; `null` for any other notice.
  noticeSession: number | null;
  last: DictationResult | null;
  /// The recording of the last dictation that could not be recognized is kept: it can be
  /// recognized again.
  retry: boolean;
  downloads: Record<string, Download>;
  file: FileState | null;
}

/// `retry`: what the command `retry_available` said when the window opened.
export function initialView(last: DictationResult | null, retry = false): View {
  return {
    sessions: {},
    processingFailures: {},
    engine: "unloaded",
    rewriting: false,
    modeByApp: null,
    contextTaken: false,
    microphoneLost: null,
    historyFailed: null,
    notice: null,
    noticeSession: null,
    last,
    retry,
    downloads: {},
    file: null,
  };
}

/// Whether the file in `state` still runs, so another cannot start.
export function fileRunning(state: FileState | null): boolean {
  return state?.kind === "transcribing" || state?.kind === "speakers" || state?.kind === "processing";
}

export function phase(view: View): Phase {
  const states = Object.values(view.sessions).map((state) => state.kind);
  if (states.includes("recording")) return "recording";
  if (states.includes("post_processing")) return "processing";
  if (states.includes("transcribing")) return "transcribing";
  if (states.includes("inserting")) return "inserting";
  if (view.rewriting) return "rewriting";
  return "idle";
}

function outcome(state: SessionState): Notice | null {
  switch (state.kind) {
    case "inserted":
      return { kind: "inserted" };
    case "kept":
      return { kind: "kept" };
    case "discarded":
      return { kind: "discarded", reason: state.reason };
    case "failed":
      return { kind: "failed", failure: state.failure };
    default:
      return null;
  }
}

export function reduce(view: View, { source, event }: CoreEvent): View {
  switch (source) {
    case "session":
      if (event.type === "finished") return { ...view, last: event.result };
      if (event.type === "processing_failed") {
        return { ...view, processingFailures: { ...view.processingFailures, [event.session]: event.failure } };
      }
      {
        const sessions = { ...view.sessions };
        const processingFailures = { ...view.processingFailures };
        let notice = outcome(event.state);
        // A recording, or a kept recording recognized again: what was said about the dictation
        // before goes.
        const begun = notice === null && !(event.session in view.sessions);
        if (notice) {
          delete sessions[event.session];
          // A language-model failure matters more than the fact that the transcript went in, or
          // that a text recognized again is kept: the text is the transcript.
          const failure = processingFailures[event.session];
          if (failure && notice.kind === "inserted") notice = { kind: "failed", failure };
          if (failure && notice.kind === "kept") notice = { kind: "kept_unprocessed", failure };
          delete processingFailures[event.session];
        } else {
          sessions[event.session] = event.state;
        }
        const started = event.state.kind === "recording";
        return {
          ...view,
          sessions,
          processingFailures,
          modeByApp: started ? null : view.modeByApp,
          contextTaken: started ? false : view.contextTaken,
          // A dictation begun while the one that lost the microphone is still recognized does
          // not take the warning away from its outcome.
          microphoneLost: begun && !(view.microphoneLost && view.microphoneLost.session in view.sessions) ? null : view.microphoneLost,
          historyFailed: begun ? null : view.historyFailed,
          notice: notice ?? (started ? null : view.notice),
          noticeSession: notice ? event.session : started ? null : view.noticeSession,
        };
      }
    case "engine":
      if (event.type === "failed") return { ...view, notice: { kind: "engine_failed", detail: event.detail }, noticeSession: null };
      // An engine that loads again, with this model or another, has got over its failure.
      if (event.state === "loading" || event.state === "ready") return { ...forgetEngineFailure(view), engine: event.state };
      return { ...view, engine: event.state };
    case "dictation":
      switch (event.type) {
        case "level":
          // Twenty times a second: `useLevel` follows it, so the windows do not draw anew.
          return view;
        case "microphone_lost":
          return { ...view, microphoneLost: { session: event.session, detail: event.detail } };
        case "context_taken":
          return view.sessions[event.session]?.kind === "recording" ? { ...view, contextTaken: true } : view;
        case "mode_by_app":
          return view.sessions[event.session]?.kind === "recording" ? { ...view, modeByApp: event.mode_id } : view;
        case "blocked":
          return { ...view, notice: { kind: "blocked", reason: event.reason, detail: event.detail }, noticeSession: null };
        case "retry_available":
          return { ...view, retry: event.available };
        case "selection_started":
          return { ...view, rewriting: true, notice: null, noticeSession: null };
        case "selection_done":
          return { ...view, rewriting: false, notice: { kind: "selection_done" }, noticeSession: null };
        case "selection_failed":
          return {
            ...view,
            rewriting: false,
            notice: { kind: "selection_failed", problem: event.problem, detail: event.detail },
            noticeSession: null,
          };
        case "history_failed":
          // It comes right after the outcome of its dictation, which stays the notice: a text not
          // inserted, recognized again or not processed is still to be read.
          return { ...view, historyFailed: { session: view.noticeSession, detail: event.detail } };
        case "history_saved":
        case "history_changed":
        case "stats_changed":
          return view;
      }
      return view;
    case "meeting":
    case "update":
      // The transcripts page and the update card follow these themselves.
      return view;
    case "file":
      // A refused second file does not replace the one that runs.
      if (event.type === "failed" && event.problem === "busy" && fileRunning(view.file)) return view;
      return { ...view, file: fileState(view.file, event) };
    case "model": {
      const downloads = { ...view.downloads };
      const current = downloads[event.model] ?? { downloaded: 0, total: 0 };
      if (event.type === "download_progress") {
        downloads[event.model] = { downloaded: event.downloaded, total: event.total };
      } else if (event.type === "unpacking") {
        // A new attempt: the failure of the last one goes.
        downloads[event.model] = { downloaded: current.downloaded, total: current.total, unpacking: true };
      } else if (event.type === "download_finished") {
        delete downloads[event.model];
      } else {
        downloads[event.model] = { ...current, failure: event.failure };
      }
      return { ...view, downloads };
    }
  }
}

/// The view once the engine's failure no longer holds: it loads again, or another model was
/// chosen since. Any other notice stays.
export function forgetEngineFailure(view: View): View {
  return view.notice?.kind === "engine_failed" ? { ...view, notice: null, noticeSession: null } : view;
}

/// The input level, 0–1, after `event`: the latest level while recording, 0 when a recording
/// starts.
export function levelAfter(level: number, { source, event }: CoreEvent): number {
  if (source === "dictation" && event.type === "level") return event.permille / 1000;
  if (source === "session" && event.type === "state_changed" && event.state.kind === "recording") return 0;
  return level;
}

/// The input level, kept outside the component's state: twenty events a second draw nothing; a
/// level strip (`useLevelStrip`) reads it at its own pace.
export function useLevel(): RefObject<number> {
  const level = useRef(0);
  useEvent<CoreEvent>(CORE_EVENT, (event) => {
    level.current = levelAfter(level.current, event);
  });
  return level;
}

/// "Recognize again": the kept recording of the dictation that was not recognized goes to the
/// engine chosen now; the text goes to the last result, not into a window. `available` is
/// `View.retry`. `failed`: the command did not go through, until the next press, `reset`, or the
/// recording is let go.
export function useRetry(available: boolean): { pending: boolean; failed: boolean; retry: () => void; reset: () => void } {
  const [pending, setPending] = useState(false);
  const [failed, setFailed] = useState(false);
  // A failure belongs to the recording it was for: the next one starts clean.
  useEffect(() => {
    if (!available) setFailed(false);
  }, [available]);
  const retry = useCallback(() => {
    setPending(true);
    invoke("retry_dictation")
      .then(
        () => setFailed(false),
        () => setFailed(true),
      )
      .finally(() => setPending(false));
  }, []);
  const reset = useCallback(() => setFailed(false), []);
  return { pending, failed: failed && available, retry, reset };
}

const MODIFIER_CODES = /^(Control|Shift|Alt|Meta|OS)(Left|Right)?$/;

/// Mirrors `hotkeys::types`: keys (by `KeyboardEvent.code`) that type text or move through it.
/// Alone, taken for the whole system, such a key would be gone from typing in every program.
const TYPING_KEYS = new Set([
  "Space",
  "Enter",
  "Tab",
  "Backspace",
  "Delete",
  "Escape",
  "ArrowUp",
  "ArrowDown",
  "ArrowLeft",
  "ArrowRight",
  "Home",
  "End",
  "PageUp",
  "PageDown",
  "Minus",
  "Equal",
  "BracketLeft",
  "BracketRight",
  "Backslash",
  "Semicolon",
  "Quote",
  "Backquote",
  "Comma",
  "Period",
  "Slash",
]);

function typesText(code: string): boolean {
  return /^(Key|Digit|Numpad)/.test(code) || TYPING_KEYS.has(code);
}

/// A key press: the parts of a `KeyboardEvent` a hotkey is made of.
export interface KeyPress {
  code: string;
  ctrlKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
  metaKey: boolean;
}

/// A hotkey in the backend's syntax from a key press; `null` while only modifiers are down, and
/// for a key that types or moves the cursor pressed without a modifier (the backend refuses it).
export function hotkeyFromKey(key: KeyPress): string | null {
  if (!key.code || MODIFIER_CODES.test(key.code)) return null;
  const parts = [
    key.ctrlKey && "Control",
    key.altKey && "Alt",
    key.shiftKey && "Shift",
    key.metaKey && "Super",
  ].filter((part): part is string => Boolean(part));
  if (parts.length === 0 && typesText(key.code)) return null;
  return [...parts, key.code].join("+");
}

/// Whether the interface runs on macOS: its keyboards name the modifiers differently.
export const IS_MAC = typeof navigator !== "undefined" && /Mac/.test(navigator.userAgent);

/// Modifier names as printed on the keyboard. The backend's `alt` is Option on a Mac, `super` is
/// Command.
const MODIFIER_NAMES: Record<string, string> = { control: "Ctrl", alt: "Alt", shift: "Shift", super: "Win" };
const MAC_MODIFIER_NAMES: Record<string, string> = { control: "Control", alt: "Option", shift: "Shift", super: "Command" };

/// Keys named by the sign on them rather than by their code: signs as on a US keyboard. The code
/// names the key's place; another layout may print another sign there.
const KEY_SIGNS = new Map([
  ["Backquote", "`"],
  ["Minus", "-"],
  ["Equal", "="],
  ["BracketLeft", "["],
  ["BracketRight", "]"],
  ["Semicolon", ";"],
  ["Quote", "'"],
  ["Comma", ","],
  ["Period", "."],
  ["Slash", "/"],
  ["Backslash", "\\"],
  ["ArrowUp", "↑"],
  ["ArrowDown", "↓"],
  ["ArrowLeft", "←"],
  ["ArrowRight", "→"],
]);

/// The number pad's keys other than its digits, by their sign.
const NUMPAD_SIGNS = new Map([
  ["Add", "+"],
  ["Subtract", "-"],
  ["Multiply", "*"],
  ["Divide", "/"],
  ["Decimal", "."],
  ["Equal", "="],
  ["Comma", ","],
]);

/// Keys a Mac keyboard names otherwise: its Backspace is "delete", Enter is "return", and the
/// forward delete of an extended keyboard is ⌦.
const MAC_KEY_NAMES = new Map([
  ["Backspace", "Delete"],
  ["Enter", "Return"],
  ["Delete", "⌦"],
  ["Escape", "Esc"],
]);

/// A key by its `KeyboardEvent.code` as printed on the keyboard: `KeyZ` → `Z`, `Digit1` → `1`,
/// `Backquote` → `` ` ``, `ArrowUp` → `↑`, `Numpad1` → `Num 1`, `NumpadAdd` → `Num +`; on a Mac
/// `Backspace` → `Delete`, `Enter` → `Return`.
function keyName(code: string, mac: boolean): string {
  const name = mac ? MAC_KEY_NAMES.get(code) : undefined;
  if (name !== undefined) return name;
  const sign = KEY_SIGNS.get(code);
  if (sign !== undefined) return sign;
  if (code.startsWith("Numpad") && code.length > "Numpad".length) {
    const key = code.slice("Numpad".length);
    return `Num ${NUMPAD_SIGNS.get(key) ?? key}`;
  }
  return code.replace(/^Key(?=[A-Z]$)/, "").replace(/^Digit(?=\d$)/, "");
}

/// The keys of a hotkey as printed on the keyboard: `control+alt+Space` → `Ctrl`, `Alt`, `Space`
/// (`Control`, `Option`, `Space` on a Mac).
export function hotkeyKeys(canonical: string, mac = IS_MAC): string[] {
  const names = mac ? MAC_MODIFIER_NAMES : MODIFIER_NAMES;
  return canonical.split("+").map((part) => names[part.toLowerCase()] ?? keyName(part, mac));
}

/// `control+alt+Space` → `Ctrl + Alt + Space` (`Control + Option + Space` on a Mac),
/// `shift+KeyZ` → `Shift + Z`, `alt+Backquote` → `` Alt + ` ``.
export function formatHotkey(canonical: string, mac = IS_MAC): string {
  return hotkeyKeys(canonical, mac).join(" + ");
}

/// The modifiers that turn typed text into shortcuts while held: "Ctrl, Alt, Win" or
/// "Control, Option, Command".
export function shortcutModifierNames(mac = IS_MAC): string[] {
  const names = mac ? MAC_MODIFIER_NAMES : MODIFIER_NAMES;
  return [names.control, names.alt, names.super];
}
