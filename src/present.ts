// What the window says about the app's state: texts and looks made from the view and the
// settings, and the helpers that bring the answers of commands to the window (`latestOnly`,
// `useStatus`, `commandFailure`, `ownMessages`).
import { useCallback, useMemo, useState } from "react";
import type { TFunction } from "i18next";
import { invoke } from "@tauri-apps/api/core";
import type { InputDevice } from "./App";
import type { CloudEngineInfo } from "./components/CloudPanel";
import type { ModelInfo } from "./components/ModelsPanel";
import { formatHotkey, phase, type Download, type Notice, type Phase, type View } from "./dictation";
import { resultText, useEvent, type EngineState, type Failure } from "./events";
import { connectionLabel, serviceOf } from "./services";
import {
  BUILTIN_MODES,
  HOTKEYS_EVENT,
  MAX_DICTIONARY_CHARS,
  SETTINGS_EVENT,
  type CleanText,
  type Connection,
  type Mode,
  type RecordingMode,
  type SaveError,
  type Settings,
} from "./settings";
import type { SelectOption, Tone } from "./ui";

/// Mirrors `commands::Status`.
export interface Status {
  hotkey_registered: boolean;
  unregistered_hotkeys: string[];
  engine_available: boolean;
  settings_error: string | null;
}

/// Takes the answers of a request made again and again (`ask`), only the one to the latest
/// request: an earlier answer that comes late never overwrites it. A failed request is let go.
export function latestOnly<T>(take: (answer: T) => void): (ask: Promise<T>) => Promise<void> {
  let asked = 0;
  return (ask) => {
    const mine = ++asked;
    return ask.then(
      (answer) => {
        if (mine === asked) take(answer);
      },
      () => {},
    );
  };
}

/// The events after which the status is read again: every save of the settings, in this window
/// or elsewhere (the hotkeys and the engine follow them), and the paused hotkeys coming back
/// (which of them are registered is known only then).
export const STATUS_EVENTS = [SETTINGS_EVENT, HOTKEYS_EVENT] as const;

/// Reads the status with `ask` at every call; only the latest answer reaches `take`
/// (`latestOnly`), so a read made while the hotkeys were paused never comes after the one made
/// once they are back. A failed read leaves the status as it was.
export function statusReader(ask: () => Promise<Status>, take: (status: Status) => void): () => Promise<void> {
  const latest = latestOnly(take);
  return () => latest(ask());
}

/// The status for a window: `initial` until it is read, then read again on every
/// `STATUS_EVENTS` and whenever `read` is called (the quick panel at every opening).
export function useStatus<S extends Status | null>(initial: S): { status: Status | S; read: () => void } {
  const [status, setStatus] = useState<Status | S>(initial);
  const ask = useMemo(() => statusReader(() => invoke<Status>("get_status"), setStatus), []);
  const read = useCallback(() => void ask(), [ask]);
  const [saved, hotkeysBack] = STATUS_EVENTS;
  useEvent(saved, read);
  useEvent(hotkeysBack, read);
  return { status, read };
}

export function noticeText(t: TFunction, notice: Notice): string {
  switch (notice.kind) {
    case "inserted":
      return t("notice.inserted");
    case "kept":
      return t("notice.kept");
    case "kept_unprocessed":
      return t(`notice.kept_unprocessed.${unprocessedKind(notice.failure)}`, { detail: notice.failure.detail });
    case "discarded":
      return t(`notice.${notice.reason}`);
    case "failed":
      return t(`notice.${notice.failure.kind}`, { detail: notice.failure.detail });
    case "blocked":
      return t(`notice.${notice.reason}`, { detail: notice.detail });
    case "engine_failed":
      return t("notice.engine_failed", { detail: notice.detail });
    case "selection_done":
      return t("selection.done");
    case "selection_failed":
      return t(`selection.${notice.problem}`, { detail: notice.detail });
  }
}

/// Why a language model did not process the text; two of the failures have no detail of their own.
function unprocessedKind(failure: Failure): "processing" | "empty_processing" | "plan_limit" {
  return failure.kind === "empty_processing" || failure.kind === "plan_limit" ? failure.kind : "processing";
}

/// The line under a title that says the text was not processed: only why.
function whyUnprocessed(t: TFunction, failure: Failure): string {
  return t(`dictation.why_unprocessed.${unprocessedKind(failure)}`, { detail: failure.detail });
}

/// The line under a title that says the selection was not replaced: only why, never "not
/// replaced" again.
function whyNotReplaced(t: TFunction, notice: Extract<Notice, { kind: "selection_failed" }>): string {
  switch (notice.problem) {
    case "insertion":
      return notice.detail;
    case "focus_changed":
    case "changed":
      return t(`dictation.why_not_replaced.${notice.problem}`);
    default:
      return noticeText(t, notice);
  }
}

/// The kind of a command's `{ kind, … }` failure; `null` for any other failure, such as a command
/// that did not run.
export function errorKind(failure: unknown): string | null {
  const error = failure as { kind?: unknown } | null;
  return typeof error === "object" && error !== null && typeof error.kind === "string" ? error.kind : null;
}

/// A command's failure as text. A `{ kind, detail }` failure is the key `${keys}.${kind}` with its
/// detail; a kind without a key of its own, and any other failure (a command that did not run),
/// is the `fallback` key with the detail, the kind or the failure itself: never a key.
export function failureText(t: TFunction, failure: unknown, keys: string, fallback: string): string {
  const kind = errorKind(failure);
  if (kind === null) return t(fallback, { detail: String(failure) });
  const error = failure as { detail?: unknown };
  const detail = typeof error.detail === "string" ? error.detail : "";
  return t(`${keys}.${kind}`, { detail, defaultValue: t(fallback, { detail: detail || kind }) });
}

/// Waits for a command: `null` once it worked, else its failure as `text` says it.
export async function commandFailure(command: Promise<unknown>, text: (detail: string) => string): Promise<string | null> {
  try {
    await command;
    return null;
  } catch (failure) {
    return text(String(failure));
  }
}

/// Outcomes that the dictation card shows only for a moment.
const CALM_NOTICES: Notice["kind"][] = ["inserted", "selection_done", "discarded"];
/// How long a calm outcome stays in the dictation card.
export const CALM_MS = 1800;

/// What stops a recording in each mode, beside the keys in the dictation card and the quick
/// panel: auto stops on the release of a hold or on the next press after a tap, and neither can
/// know which one the user did.
export const STOP_KEYS: Record<RecordingMode, "release" | "press_again" | "release_or_press"> = {
  hold: "release",
  toggle: "press_again",
  auto: "release_or_press",
};

/// Failures of the insertion: the text was not typed, and the windows offer to copy it.
const NOT_INSERTED_FAILURES = ["no_focus", "focus_changed", "insertion_blocked", "no_permission", "insertion"];
/// Failures of the language model: the transcript went in as it was recognized.
const UNPROCESSED_FAILURES = ["processing", "empty_processing", "plan_limit"];

export function modeName(t: TFunction, mode: Mode): string {
  return BUILTIN_MODES.includes(mode.id) ? t(`modes.builtin.${mode.id}`) : mode.name;
}

/// The microphones to choose from, in the settings and the quick panel: the system's, the
/// connected ones, and the chosen one while it is disconnected, so the field still says what is
/// chosen instead of showing nothing.
export function microphoneOptions(t: TFunction, devices: InputDevice[], chosen: string | null): SelectOption[] {
  const missing = chosen !== null && !devices.some((device) => device.id === chosen);
  return [
    { value: "", label: t("settings.system_microphone") },
    ...devices.map((device) => ({ value: device.id, label: device.name })),
    ...(missing ? [{ value: chosen, label: t("settings.microphone_missing") }] : []),
  ];
}

/// What the interface needs to know about the dictation model.
export type ModelTraits = Pick<ModelInfo, "name" | "translate" | "languages" | "takes_dictionary" | "takes_language">;

/// `translates` is whether the dictation model translates; `null` when no model is chosen.
/// `clean` is how plain dictation cleans its text; `null` where it does not apply (files).
export function modeCaption(
  t: TFunction,
  mode: Mode,
  connections: Connection[],
  translates: boolean | null = null,
  clean: CleanText | null = null,
): string {
  switch (mode.action.kind) {
    case "plain": {
      if (clean?.level === "fillers") return t("modes.caption.plain_fillers");
      if (clean?.level === "model") {
        const connection = connections.find((item) => item.id === clean.connection_id);
        return connection
          ? t("modes.caption.plain_model", { service: connectionLabel(connection), model: clean.model })
          : t("modes.caption.plain_fillers");
      }
      return t("modes.caption.plain");
    }
    case "translate_to_english":
      return t(translates === false ? "modes.caption.english_unsupported" : "modes.caption.english");
    case "process": {
      const { connection_id, model } = mode.action;
      const connection = connections.find((item) => item.id === connection_id);
      return connection ? `${connectionLabel(connection)} · ${model}` : t("modes.caption.no_connection");
    }
  }
}

/// The note of plain dictation's editor while it is the main mode: it runs on the main hotkey
/// from the settings. Not the main mode, it has no note: its editor offers "Make main".
export function plainModeNote(t: TFunction, settings: Pick<Settings, "hotkey" | "main_mode_id">): string | null {
  return settings.main_mode_id === "plain" ? t("modes.plain_note", { hotkey: formatHotkey(settings.hotkey) }) : null;
}

/// Why the "In English" mode does not translate with the dictation model, naming the models
/// that do; `null` when it translates or no model is chosen.
export function translationWarning(t: TFunction, model: ModelTraits | null, all: ModelTraits[]): string | null {
  if (!model || model.translate) return null;
  const translating = all.filter((item) => item.translate).map((item) => item.name);
  return t("modes.no_translation", { model: model.name, models: translating.join(", ") });
}

/// The lines under the dictionary's name: what goes there, then how much of it the engine takes,
/// or that the dictation model does not read it at all.
export function dictionaryNotes(t: TFunction, model: ModelTraits | null): string[] {
  return [t("settings.dictionary_hint"), recognitionNote(t, model, "dictionary", null) ?? t("settings.dictionary_limit")];
}

/// Why the spoken language or the dictionary does not reach the dictation model's engine;
/// `null` when it does.
export function recognitionNote(
  t: TFunction,
  model: ModelTraits | null,
  setting: "language" | "dictionary",
  spokenLanguage: string | null,
): string | null {
  if (!model) return null;
  if (setting === "dictionary") return model.takes_dictionary ? null : t("settings.dictionary_ignored", { model: model.name });
  if (!model.takes_language) {
    return t(model.languages.length === 1 ? "settings.language_single" : "settings.language_ignored", { model: model.name });
  }
  if (spokenLanguage && !model.languages.includes(spokenLanguage)) return t("settings.language_unsupported", { model: model.name });
  return null;
}

/// The characters of `text` as a reader sees them: a flag, a family of emoji, a skin tone or a
/// letter with a combining mark is one. A web view without `Intl.Segmenter` gives code points.
export function graphemes(text: string): string[] {
  if (typeof Intl.Segmenter !== "function") return Array.from(text);
  return Array.from(new Intl.Segmenter(undefined, { granularity: "grapheme" }).segment(text), (part) => part.segment);
}

/// The text in the quotation marks of the interface language, on one line and cut to `length`
/// characters: whole characters (`graphemes`), so an emoji or an accent is never cut off.
export function quote(t: TFunction, text: string, length: number): string {
  const line = text.split(/\s+/).filter(Boolean).join(" ");
  const characters = graphemes(line);
  return t("dictation.quote", { text: characters.length <= length ? line : `${characters.slice(0, length).join("").trimEnd()}…` });
}

export function saveErrorText(t: TFunction, error: SaveError): string {
  switch (error?.kind) {
    case "read_only":
      return t("settings.read_only");
    case "invalid_hotkey":
      return t("settings.hotkey_invalid");
    case "hotkey_taken":
      return t("settings.hotkey_taken");
    case "invalid":
      // `detail` names the setting the backend refused.
      if (error.detail === "dictionary") return t("settings.dictionary_too_long", { max: MAX_DICTIONARY_CHARS });
      return t("settings.save_failed", { detail: error.detail });
    case "io":
      return t("settings.save_failed", { detail: error.detail });
    default:
      // Not a `SaveError`: the command itself failed.
      return t("settings.save_failed", { detail: String(error) });
  }
}

/// A message of the window's message bar.
export interface Message {
  text: string;
  error: boolean;
}

/// The message shown after a save that worked: the error of the save before it goes, any other
/// message (a saved file, a failed start) stays.
export function afterGoodSave(shown: Message | null, failed: string | null): Message | null {
  return shown !== null && shown.text === failed ? null : shown;
}

/// The messages of one part of the window: its failure stays until it works again, and then only
/// that failure goes; a message of another part stays.
export function ownMessages(setMessage: (change: (shown: Message | null) => Message | null) => void) {
  let failed: string | null = null;
  return {
    fail(text: string) {
      failed = text;
      setMessage(() => ({ text, error: true }));
    },
    succeed() {
      const text = failed;
      failed = null;
      if (text !== null) setMessage((shown) => afterGoodSave(shown, text));
    },
  };
}

export type OwnMessages = ReturnType<typeof ownMessages>;

export interface Hero {
  icon: string;
  tone: Tone;
  /// The halo of a running recording.
  ring: boolean;
  title: string;
  subtitle: string;
  keyNote: string;
  wave: boolean;
  /// Offer to open the recognition settings.
  configure: boolean;
  /// Offer to open the ChatGPT usage settings: the plan's limit for ahem is reached.
  manageUsage: boolean;
  /// Offer to copy the last text: it was not inserted.
  copy: boolean;
  /// The clipboard refused the text of the copy offered (`last.copy_failed`); `null` otherwise.
  copyFailure: string | null;
  /// Offer to recognize again the kept recording of a dictation that was not recognized.
  retry: boolean;
  /// The recording's time inside the title, which screen readers are not to read every second.
  clock: string | null;
  /// Lines about the last dictation beside the outcome (`dictationWarnings`).
  warnings: string[];
}

/// A time from milliseconds: 0:05, 12:34, 1:02:03; `padded` gives the minutes two digits, 00:05,
/// as the clock of a dictation shows them.
export function clock(ms: number, padded = false): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor(total / 60) % 60;
  const seconds = String(total % 60).padStart(2, "0");
  if (hours > 0) return `${hours}:${String(minutes).padStart(2, "0")}:${seconds}`;
  return `${padded ? String(minutes).padStart(2, "0") : minutes}:${seconds}`;
}

/// The percent of something still running: rounded down and below 100 until it ends.
export function runningPercent(share: number): number {
  return Math.max(0, Math.min(99, Math.floor(share * 100)));
}

/// One number format for each interface language: a page lists many sizes.
const GIGABYTE_FORMATS = new Map<string, Intl.NumberFormat>();

/// Bytes as gigabytes with two decimals, written the way the interface language writes numbers.
export function gigabytes(bytes: number, locale: string): string {
  let format = GIGABYTE_FORMATS.get(locale);
  if (!format) {
    format = new Intl.NumberFormat(locale, { minimumFractionDigits: 2, maximumFractionDigits: 2 });
    GIGABYTE_FORMATS.set(locale, format);
  }
  return format.format(bytes / 1e9);
}

/// How full a level bar is, 0–1, for a microphone level (RMS, 0–1) on a decibel scale: room
/// noise (−50 dBFS) is empty, loud speech (−12 dBFS) full, so ordinary speech moves the bars
/// well and silence stays flat.
export function meter(level: number): number {
  if (level <= 0) return 0;
  const decibels = 20 * Math.log10(level);
  return Math.min(1, Math.max(0, (decibels + 50) / 38));
}

/// What a warning about a dictation needs to know of the view.
type WarningView = Pick<View, "sessions" | "microphoneLost" | "historyFailed" | "notice" | "noticeSession" | "rewriting">;

/// Whether a warning about the dictation `session` belongs beside what the window shows: while
/// that dictation runs (no outcome yet) or its own outcome is shown. A rewritten selection, a
/// blocked press, an engine failure or another dictation being recorded is about something else,
/// also when the outcome of `session` came during that recording.
function ownWarning(view: WarningView, session: number | null): boolean {
  if (view.rewriting) return false;
  if (Object.entries(view.sessions).some(([id, state]) => state.kind === "recording" && Number(id) !== session)) return false;
  return view.notice === null || (session !== null && view.noticeSession === session);
}

/// What the window says about a microphone that went away during the last recording; `null`
/// when it did not, or when the outcome shown is not that dictation's (`ownWarning`).
export function microphoneLostText(t: TFunction, view: WarningView): string | null {
  const lost = view.microphoneLost;
  return lost === null || !ownWarning(view, lost.session) ? null : t("dictation.microphone_lost", { detail: lost.detail });
}

/// What went wrong with the last dictation besides its outcome, until the next one begins: the
/// microphone went away midway, its text did not go to the history. Only beside that dictation's
/// own outcome (`ownWarning`).
function dictationWarnings(t: TFunction, view: WarningView): string[] {
  const lost = microphoneLostText(t, view);
  const failed = view.historyFailed;
  const history = failed && ownWarning(view, failed.session) ? t("history.save_failed", { detail: failed.detail }) : null;
  return [lost, history].filter((warning): warning is string => Boolean(warning));
}

/// A title around its clock, so that the clock can be kept from screen readers: the text before
/// it, the clock, the text after it. Without a clock in it, the whole title comes first.
export function splitClock(title: string, time: string | null): [string, string, string] {
  const at = time ? title.lastIndexOf(time) : -1;
  if (!time || at < 0) return [title, "", ""];
  return [title.slice(0, at), time, title.slice(at + time.length)];
}

/// Whether the engine `chosen` can recognize, as the backend decides it before a recording: a
/// downloaded model, or a cloud engine whose service is connected.
export function canRecognize(chosen: string | null, models: ModelInfo[], cloud: CloudEngineInfo[], connections: Connection[]): boolean {
  const model = models.find((item) => item.id === chosen);
  if (model) return model.state.kind === "ready";
  const engine = cloud.find((item) => item.id === chosen);
  return engine !== undefined && connections.some((connection) => serviceOf(connection) === engine.service);
}

/// A refused press whose reason has gone since: a usable engine chosen (`no_model`), the engine
/// of the chosen model there (`no_engine`).
function blockPassed(notice: Notice, usable: boolean, status: Status): boolean {
  if (notice.kind !== "blocked") return false;
  if (notice.reason === "no_model") return usable;
  if (notice.reason === "no_engine") return status.engine_available;
  return false;
}

interface HeroInput {
  view: View;
  settings: Settings;
  status: Status;
  modelName: string | null;
  /// The chosen engine can recognize now (`canRecognize`).
  usable: boolean;
  /// The notice arrived less than `CALM_MS` ago.
  fresh: boolean;
  /// Milliseconds since the recording started.
  elapsed: number;
  /// The clipboard refused the last text when its copy button was pressed.
  copyFailed?: boolean;
}

/// The dictation card: what happens now, else the last outcome, else whether dictation works.
/// A refused copy is said beside the copy button, while it is offered.
export function hero(t: TFunction, input: HeroInput): Hero {
  const card = heroCard(t, input);
  return { ...card, copyFailure: card.copy && input.copyFailed ? t("last.copy_failed") : null };
}

function heroCard(t: TFunction, { view, settings, status, modelName, usable, fresh, elapsed }: HeroInput): Omit<Hero, "copyFailure"> {
  const mode = settings.recording_mode;
  const idleKeys = t(`dictation.keys.${mode}`);
  const base = {
    ring: false,
    keyNote: idleKeys,
    wave: false,
    configure: false,
    manageUsage: false,
    copy: false,
    retry: false,
    clock: null,
    warnings: dictationWarnings(t, view),
  };
  // What runs now, as the capsule and the quick panel say it.
  switch (phase(view)) {
    case "recording": {
      const time = clock(elapsed, true);
      return {
        ...base,
        icon: "microphone",
        tone: "danger",
        ring: true,
        title: t("dictation.hero.listening", { time }),
        clock: time,
        // How to stop is the note beside the keys.
        subtitle: "",
        keyNote: t(`dictation.keys.${STOP_KEYS[mode]}`),
        wave: true,
      };
    }
    case "processing":
      return { ...base, icon: "sparkle", tone: "accent", title: t("dictation.hero.processing"), subtitle: "" };
    case "transcribing": {
      const loading = view.engine === "loading";
      return {
        ...base,
        icon: "microphone",
        tone: "accent",
        title: t(loading ? "dictation.hero.preparing" : "dictation.hero.recognizing"),
        subtitle: modelName ?? "",
      };
    }
    case "inserting":
      return { ...base, icon: "microphone", tone: "accent", title: t("dictation.hero.inserting"), subtitle: "" };
    case "rewriting":
      return { ...base, icon: "sparkle", tone: "accent", title: t("dictation.hero.rewriting"), subtitle: "" };
    case "idle":
      break;
  }

  // Nothing runs: the recording of a dictation that was not recognized can go again, whatever
  // came after it.
  const idle = { ...base, retry: view.retry };
  // Ready needs no description: the keys and their note beside it say how to dictate.
  const ready = { ...idle, icon: "microphone", tone: "ok" as Tone, title: t("dictation.hero.ready"), subtitle: "" };
  // A refused press says nothing once its reason has gone: the card says what holds now.
  const notice = view.notice && !blockPassed(view.notice, usable, status) ? view.notice : null;
  if (notice && (fresh || !CALM_NOTICES.includes(notice.kind))) {
    const problem = (title: string) => ({
      ...idle,
      icon: "warning",
      tone: "danger" as Tone,
      title,
      subtitle: noticeText(t, notice),
    });
    switch (notice.kind) {
      case "inserted":
        return {
          ...idle,
          icon: "check",
          tone: "ok",
          title: t("dictation.hero.inserted"),
          subtitle: view.last ? quote(t, resultText(view.last), 100) : "",
        };
      // Not typed anywhere: the card keeps the text to copy until the next dictation.
      case "kept":
        return {
          ...idle,
          icon: "check",
          tone: "ok",
          title: t("dictation.hero.kept"),
          subtitle: view.last ? quote(t, resultText(view.last), 100) : "",
          copy: view.last !== null,
        };
      case "kept_unprocessed":
        // The title says it was not processed; the line below says only why.
        return {
          ...problem(t("dictation.hero.kept_unprocessed")),
          subtitle: whyUnprocessed(t, notice.failure),
          copy: view.last !== null,
          manageUsage: notice.failure.kind === "plan_limit",
        };
      case "selection_done":
        return { ...idle, icon: "check", tone: "ok", title: t("dictation.hero.replaced"), subtitle: "" };
      case "discarded":
        return { ...ready, title: noticeText(t, notice) };
      case "failed": {
        // The title says what did not happen; the line below says only why.
        const { kind, detail } = notice.failure;
        if (NOT_INSERTED_FAILURES.includes(kind)) {
          const subtitle = kind === "insertion" ? detail : noticeText(t, notice);
          return { ...problem(t("dictation.hero.not_inserted")), subtitle, copy: view.last !== null };
        }
        if (UNPROCESSED_FAILURES.includes(kind)) {
          return { ...problem(t("dictation.hero.unprocessed")), subtitle: whyUnprocessed(t, notice.failure), manageUsage: kind === "plan_limit" };
        }
        return problem(t("dictation.hero.failed"));
      }
      case "blocked":
        return { ...problem(t("dictation.hero.unavailable")), configure: notice.reason === "no_model" };
      case "engine_failed":
        return problem(t("dictation.hero.unavailable"));
      case "selection_failed":
        return { ...problem(t("dictation.hero.not_replaced")), subtitle: whyNotReplaced(t, notice) };
    }
  }
  const unavailable = (subtitle: string, configure: boolean) => ({
    ...idle,
    icon: "warning",
    tone: "danger" as Tone,
    title: t("dictation.hero.unavailable"),
    subtitle,
    configure,
  });
  if (settings.model_id === null) return unavailable(t("notice.no_model"), true);
  if (!status.engine_available) return unavailable(t("status.engine_missing"), false);
  // The failure itself is gone (a later outcome, or the window opened after it).
  if (view.retry) {
    return { ...idle, icon: "warning", tone: "danger", title: t("dictation.hero.failed"), subtitle: t("dictation.not_recognized") };
  }
  return ready;
}

/// The indicator's capsule: its look, the text of a problem or a discarded recording, and the
/// label of the other states; `null` when there is nothing to show.
export interface Capsule {
  state: "recording" | "busy" | "success" | "problem" | "quiet";
  text: string | null;
  label: string;
}

/// What the capsule says instead of a dictation's success: its microphone went away midway, or
/// its text did not go to the history. Only for the dictation whose outcome is the notice.
export function capsuleWarning(t: TFunction, view: Pick<View, "microphoneLost" | "historyFailed" | "noticeSession">): string | null {
  const session = view.noticeSession;
  if (session === null) return null;
  if (view.microphoneLost?.session === session) return t("indicator.microphone_lost");
  if (view.historyFailed?.session === session) return t("history.save_failed", { detail: view.historyFailed.detail });
  return null;
}

/// `warning` (`capsuleWarning`) takes the place of a success; a problem stays the outcome's own.
export function capsule(t: TFunction, current: Phase, notice: Notice | null, warning: string | null = null): Capsule | null {
  if (current === "recording") return { state: "recording", text: null, label: t("status.recording") };
  if (current !== "idle") return { state: "busy", text: null, label: t(`status.${current}`) };
  if (!notice) return null;
  const success = (label: string): Capsule => (warning ? { state: "problem", text: warning, label: warning } : { state: "success", text: null, label });
  switch (notice.kind) {
    case "inserted":
      return success(t("notice.inserted"));
    case "kept":
      return success(t("dictation.hero.kept"));
    case "selection_done":
      return { state: "success", text: null, label: t("selection.done") };
    case "discarded": {
      const text = noticeText(t, notice);
      return { state: "quiet", text, label: text };
    }
    default: {
      // Text that was not inserted waits in the window to be copied.
      const notInserted = notice.kind === "failed" && NOT_INSERTED_FAILURES.includes(notice.failure.kind);
      const text = notInserted ? t(`indicator.${notice.failure.kind}`) : noticeText(t, notice);
      return { state: "problem", text, label: text };
    }
  }
}

/// What the capsule showed at a drawing: whether the dictation was idle, and its notice.
export interface CapsuleShown {
  idle: boolean;
  notice: Notice | null;
}

/// The notices the backend shows the capsule for by itself while idle (`indicator.rs`): a
/// dictation refused before it recorded, a selection that could not be read before its rewrite
/// began (one that began is not idle).
const SHOWN_WHILE_IDLE: Notice["kind"][] = ["blocked", "selection_failed"];

/// Whether the capsule shows anew, growing in, after it showed `before`: a dictation that starts
/// from idle, or a new notice while idle that the backend shows (`SHOWN_WHILE_IDLE`). The window
/// was hidden since the last notice, and that capsule has faded out. The outcome of a dictation
/// that ran is the same visit, and so is a warning beside it (`history_failed` keeps the
/// notice); an engine that failed while idle shows no window: a capsule still there from the
/// last outcome only changes its text.
export function newCapsuleVisit(before: CapsuleShown, now: CapsuleShown): boolean {
  if (!before.idle) return false;
  if (!now.idle) return true;
  return now.notice !== null && now.notice !== before.notice && SHOWN_WHILE_IDLE.includes(now.notice.kind);
}

/// The model state at the bottom of the navigation pane. A cloud engine loads nothing here.
export function engineStatus(
  t: TFunction,
  {
    engine,
    modelName,
    available,
    cloud = false,
  }: { engine: EngineState; modelName: string | null; available: boolean; cloud?: boolean },
): { title: string; note: string; tone: Tone } {
  if (modelName === null) return { title: t("engine.none"), note: t("engine.choose"), tone: "danger" };
  if (cloud) return { title: modelName, note: t("engine.cloud"), tone: "ok" };
  if (!available) return { title: modelName, note: t("engine.missing"), tone: "danger" };
  switch (engine) {
    case "loading":
      return { title: modelName, note: t("engine.loading"), tone: "accent" };
    case "ready":
      return { title: modelName, note: t("engine.ready"), tone: "ok" };
    case "unloading":
      return { title: modelName, note: t("engine.unloading"), tone: "faint" };
    case "unloaded":
      return { title: modelName, note: t("engine.unloaded"), tone: "faint" };
  }
}

/// Percent of the running model downloads for the navigation, or `null` when none runs.
export function downloadBadge(downloads: Record<string, Download>): string | null {
  const running = Object.values(downloads).filter((download) => !download.failure);
  const total = running.reduce((sum, download) => sum + download.total, 0);
  if (running.length === 0 || total <= 0) return null;
  const done = running.reduce((sum, download) => sum + download.downloaded, 0);
  return `${runningPercent(done / total)}%`;
}
