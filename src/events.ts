// Mirrors `domain::events::CoreEvent` and the types inside it (Rust tests pin the JSON shape).
import { useEffect, useRef } from "react";
import { listen } from "@tauri-apps/api/event";

export type FailureKind =
  | "recording"
  | "transcription"
  | "processing"
  | "empty_processing"
  /// The share of the ChatGPT plan given to ahem is used up; the transcript went in.
  | "plan_limit"
  | "no_focus"
  | "focus_changed"
  | "insertion_blocked"
  | "no_permission"
  | "insertion";

export interface Failure {
  kind: FailureKind;
  detail: string;
}

export type DiscardReason = "too_short" | "no_speech";

export type SessionState =
  | { kind: "recording" }
  | { kind: "transcribing" }
  | { kind: "post_processing" }
  | { kind: "inserting" }
  | { kind: "inserted" }
  /// The text went to the last result (and to the history, when it is kept) without being
  /// typed: a dictation recognized again (`retry_dictation`).
  | { kind: "kept" }
  | { kind: "discarded"; reason: DiscardReason }
  | { kind: "failed"; failure: Failure };

export interface DictationResult {
  mode_id: string;
  transcript: string;
  language: string | null;
  processed: string | null;
  inserted: boolean;
}

export type SessionEvent =
  | { type: "state_changed"; session: number; state: SessionState }
  | { type: "processing_failed"; session: number; failure: Failure }
  | { type: "finished"; session: number; result: DictationResult };

export type EngineState = "unloaded" | "loading" | "ready" | "unloading";

export type EngineEvent = { type: "state_changed"; state: EngineState } | { type: "failed"; detail: string };

export type DownloadFailure = "cancelled" | "no_space" | "network" | "verification" | "disk";

export type ModelEvent =
  | { type: "download_progress"; model: string; downloaded: number; total: number }
  | { type: "unpacking"; model: string }
  | { type: "download_finished"; model: string }
  | { type: "download_failed"; model: string; failure: DownloadFailure; detail: string };

export type BlockReason = "no_model" | "no_engine" | "microphone";

export type SelectionProblem =
  | "no_focus"
  | "focus_changed"
  | "password"
  | "not_supported"
  | "no_selection"
  | "several_ranges"
  | "read_only"
  | "changed"
  | "processing"
  | "insertion_blocked"
  | "insertion"
  | "no_permission"
  | "unsupported"
  | "failed";

export type DictationEvent =
  | { type: "blocked"; reason: BlockReason; detail: string }
  /// Whether the recording of the last dictation that could not be recognized is kept, to be
  /// recognized again (`retry_dictation`); the command `retry_available` says it for a window
  /// that opens later.
  | { type: "retry_available"; available: boolean }
  /// The microphone stopped giving sound during the recording (a headset went away): what was
  /// recorded before it is recognized.
  | { type: "microphone_lost"; session: number; detail: string }
  | { type: "level"; permille: number }
  | { type: "mode_by_app"; session: number; mode_id: string }
  | { type: "context_taken"; session: number }
  | { type: "selection_started"; mode_id: string }
  | { type: "selection_done" }
  | { type: "selection_failed"; problem: SelectionProblem; detail: string }
  | { type: "history_saved" }
  /// Entries or the whole file were deleted: the history lists read it again.
  | { type: "history_changed" }
  | { type: "history_failed"; detail: string }
  /// A dictation was counted, or the statistics were reset: every window reads them again.
  | { type: "stats_changed" };

export type FileProblem =
  /// Another file or a meeting's recording is being transcribed: this file did not start.
  | "busy"
  | "no_model"
  | "no_engine"
  | "read"
  | "transcription"
  /// The model chosen while it was transcribed gives no word times.
  | "no_word_times"
  /// The model chosen while it was translated to English does not translate.
  | "no_translation"
  | "no_speech"
  | "processing"
  | "save";

export type FileEvent =
  | { type: "started"; file: number; name: string }
  | { type: "progress"; file: number; done_ms: number; total_ms: number | null }
  | { type: "speakers"; file: number; step: SpeakersStep; done_ms: number; total_ms: number | null }
  | { type: "processing"; file: number }
  /// `transcript`: the entry kept in the list of transcripts; none for a text file.
  | { type: "finished"; file: number; path: string; processing_error: string | null; transcript: number | null }
  /// `transcript`: the entry the transcript was kept in although the file could not be written
  /// (`save`), so it can be saved elsewhere; none otherwise. `held`: the result that could not be
  /// written is held until the next file, to be written elsewhere (`save_held_result`).
  | { type: "failed"; file: number; problem: FileProblem; detail: string; transcript: number | null; held: boolean }
  | { type: "cancelled"; file: number };

/// Telling voices apart: where voices speak (no measure of how far), then whose they are.
export type SpeakersStep = "segments" | "voices";

export type MeetingStep = "me" | "others" | SpeakersStep;

/// Why a meeting's track records nothing of what is heard: its device is gone or cannot be
/// opened (it records silence until one opens), or its file cannot be written (a full disk; it
/// stopped writing for the rest of the meeting).
export type TrackProblem = "no_device" | "cannot_write";

/// `no_word_times`: the model chosen while it was transcribed gives no word times.
export type MeetingProblem = "no_model" | "no_engine" | "read" | "transcription" | "no_word_times" | "no_speech" | "save";

export type MeetingEvent =
  | { type: "started"; meeting: number }
  | {
      type: "recording";
      meeting: number;
      elapsed_ms: number;
      me_level: number;
      others_level: number;
      me_problem: TrackProblem | null;
      others_problem: TrackProblem | null;
    }
  | { type: "recorded"; meeting: number }
  | { type: "discarded"; meeting: number }
  | { type: "progress"; meeting: number; step: MeetingStep; done_ms: number; total_ms: number | null }
  | { type: "done"; meeting: number }
  | { type: "failed"; meeting: number; problem: MeetingProblem; detail: string }
  | { type: "cancelled"; meeting: number }
  | { type: "changed" };

export type CoreEvent =
  | { source: "session"; event: SessionEvent }
  | { source: "engine"; event: EngineEvent }
  | { source: "model"; event: ModelEvent }
  | { source: "dictation"; event: DictationEvent }
  | { source: "file"; event: FileEvent }
  | { source: "meeting"; event: MeetingEvent }
  | { source: "update"; event: UpdateEvent };

/// Mirrors `domain::events::UpdateEvent`: a new version of ahem.
export type UpdateEvent =
  | { type: "available"; version: string; urgent: boolean; ready: boolean }
  | { type: "progress"; version: string; done: number; total: number | null }
  | { type: "installed"; version: string; notes: string | null; page: string | null }
  | { type: "failed"; version: string; detail: string };

export const CORE_EVENT = "core-event";

export function resultText(result: DictationResult): string {
  return result.processed ?? result.transcript;
}

/// Calls `handler` with the payload of every `name` event while the component is mounted. It is
/// always the latest `handler`, so it may read the current props and state.
export function useEvent<T>(name: string, handler: (payload: T) => void) {
  const latest = useRef(handler);
  latest.current = handler;
  useEffect(() => {
    const unlisten = listen<T>(name, ({ payload }) => latest.current(payload));
    return () => {
      unlisten.then((stop) => stop());
    };
  }, [name]);
}
