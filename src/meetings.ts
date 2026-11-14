import type { TFunction } from "i18next";
import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { CORE_EVENT, useEvent, type CoreEvent, type MeetingEvent, type MeetingProblem, type MeetingStep, type TrackProblem } from "./events";

/// Mirrors `meetings::transcript::Speaker`.
export type Speaker = { kind: "me" } | { kind: "others" } | { kind: "person"; number: number };

/// Mirrors `files::Word`.
export interface Word {
  start: number;
  end: number;
  text: string;
}

/// Mirrors `meetings::transcript::Turn`; times in seconds.
export interface Turn {
  speaker: Speaker;
  start: number;
  end: number;
  words: Word[];
}

export type MeetingState = "recording" | "recorded" | "transcribing" | "done" | "failed";

/// Mirrors `meetings::store::MeetingSummary`: an entry of the list of transcripts, a recorded
/// meeting or the user's file.
export interface MeetingSummary {
  id: number;
  /// When the meeting started, or when the file was transcribed.
  started_ms: number;
  duration_ms: number;
  title: string | null;
  state: MeetingState;
  /// A `MeetingProblem` code when the transcription failed.
  problem: MeetingProblem | null;
  preview: string | null;
  kind: "meeting" | "file";
  /// A file's path.
  source: string | null;
  /// Where a file's result was saved; none for a meeting, or when it could not be written.
  output: string | null;
}

export interface Meeting extends MeetingSummary {
  turns: Turn[];
  /// The text of the file's mode with a language model.
  processed: string | null;
}

/// Mirrors `meetings::speakers::ModelInfo`.
export interface SpeakerModel {
  name: string;
  /// Where it is downloaded from.
  url: string;
  size: number;
}

/// Mirrors `meeting_commands::MeetingsOverview`.
export interface MeetingsOverview {
  meetings: MeetingSummary[];
  recording: number | null;
  transcribing: number | null;
  speakers_ready: boolean;
  system_audio: boolean;
}

/// The id of the speaker models' download events.
export const SPEAKER_MODELS = "speakers";

/// What the page shows live: the recording's meter and the transcription's step.
export interface MeetingLive {
  recording: {
    meeting: number;
    elapsed_ms: number;
    me_level: number;
    others_level: number;
    me_problem: TrackProblem | null;
    others_problem: TrackProblem | null;
  } | null;
  /// `since`: when the step began (`Date.now()`), so its time counts from there whenever it is
  /// shown.
  progress: { meeting: number; step: MeetingStep; done_ms: number; total_ms: number | null; since: number } | null;
  /// The last transcription that failed, until another starts.
  failure: { meeting: number; problem: MeetingProblem; detail: string } | null;
}

export const NO_LIVE: MeetingLive = { recording: null, progress: null, failure: null };

export function reduceMeeting(live: MeetingLive, event: MeetingEvent): MeetingLive {
  switch (event.type) {
    case "started":
      return {
        ...live,
        recording: { meeting: event.meeting, elapsed_ms: 0, me_level: 0, others_level: 0, me_problem: null, others_problem: null },
      };
    case "recording": {
      const { type: _type, ...recording } = event;
      return { ...live, recording };
    }
    case "recorded":
    case "discarded":
      return { ...live, recording: null };
    case "progress": {
      const { type: _type, ...progress } = event;
      const before = live.progress;
      const since = before?.meeting === event.meeting && before.step === event.step ? before.since : Date.now();
      const failure = live.failure?.meeting === event.meeting ? null : live.failure;
      return { ...live, progress: { ...progress, since }, failure };
    }
    case "failed":
      return { ...live, progress: null, failure: { meeting: event.meeting, problem: event.problem, detail: event.detail } };
    case "done":
    case "cancelled":
      return live.progress?.meeting === event.meeting ? { ...live, progress: null } : live;
    case "changed":
      return live;
  }
}

/// What a track's problem says under its level: the device is missing, or what is heard is
/// not being saved.
export function trackProblemText(t: TFunction, problem: TrackProblem): string {
  return t(problem === "no_device" ? "meetings.track_missing" : "meetings.track_not_saved");
}

/// The words as the engine wrote them, with single spaces (`files::join_text`).
export function turnText(turn: Turn): string {
  return turn.words
    .map((word) => word.text)
    .join("")
    .split(/\s+/)
    .filter(Boolean)
    .join(" ");
}

/// A meeting names the voices of the computer's sound as the other side of the meeting; a file,
/// as the file panel does ("Speaker 1").
export function speakerName(t: TFunction, speaker: Speaker, kind: MeetingSummary["kind"] = "meeting"): string {
  const file = kind === "file";
  switch (speaker.kind) {
    case "me":
      return t("meetings.speaker.me");
    case "others":
      return file ? t("files.speaker_single") : t("meetings.speaker.others");
    case "person":
      return file ? t("files.speaker", { n: speaker.number }) : t("meetings.speaker.person", { n: speaker.number });
  }
}

/// The names the saved file uses (`meetings::transcript::Labels`): `person` keeps `{n}`.
export function exportLabels(t: TFunction, kind: MeetingSummary["kind"] = "meeting"): { me: string; others: string; person: string } {
  const file = kind === "file";
  return {
    me: t("meetings.speaker.me"),
    others: file ? t("files.speaker_single") : t("meetings.speaker.others"),
    person: file ? t("files.speaker", { n: "{n}" }) : t("meetings.speaker.person", { n: "{n}" }),
  };
}

/// Whether the turns name their speakers: a meeting always does; a file only when its voices
/// were told apart, else its turns are one unnamed speaker.
export function namesSpeakers(meeting: Pick<Meeting, "kind" | "turns">): boolean {
  return meeting.kind === "meeting" || meeting.turns.some((turn) => turn.speaker.kind === "person");
}

/// The whole transcript as text for the clipboard: one turn per paragraph, with the speaker
/// when the entry names them.
export function transcriptText(t: TFunction, meeting: Pick<Meeting, "kind" | "turns">): string {
  const named = namesSpeakers(meeting);
  return meeting.turns
    .map((turn) => (named ? `${speakerName(t, turn.speaker, meeting.kind)}: ${turnText(turn)}` : turnText(turn)))
    .join("\n\n");
}

/// A path taken apart, on Windows and elsewhere: its folder (with the last separator), its name,
/// the name without its extension, and the extension in lower case ("" when there is none).
export function split(path: string): { folder: string; name: string; stem: string; extension: string } {
  const cut = Math.max(path.lastIndexOf("\\"), path.lastIndexOf("/"));
  const name = path.slice(cut + 1);
  const dot = name.lastIndexOf(".");
  return {
    folder: path.slice(0, cut + 1),
    name,
    stem: dot > 0 ? name.slice(0, dot) : name,
    extension: dot > 0 ? name.slice(dot + 1).toLowerCase() : "",
  };
}

/// The last part of a path, on Windows and elsewhere.
export function baseName(path: string): string {
  return split(path).name;
}

/// The filter of a save dialog for one format, named by its extension: `md` is "MD".
export function formatFilter(extension: string): { name: string; extensions: string[] } {
  return { name: extension.toUpperCase(), extensions: [extension] };
}

/// The user's title; else a file's name, or the date and time the meeting started.
export function meetingTitle(t: TFunction, meeting: MeetingSummary, locale: string): string {
  if (meeting.title) return meeting.title;
  if (meeting.kind === "file" && meeting.source) return baseName(meeting.source);
  const when = new Date(meeting.started_ms).toLocaleString(locale, { dateStyle: "medium", timeStyle: "short" });
  return t("meetings.untitled", { when });
}

/// A file name from the title: no characters that file systems refuse.
export function fileStem(title: string): string {
  const stem = title.replace(/[\\/:*?"<>|]+/g, " ").replace(/\s+/g, " ").trim();
  return stem || "meeting";
}

/// The name "Save as…" offers: the title, or the name of a file without its extension.
export function exportStem(meeting: MeetingSummary, title: string): string {
  if (meeting.kind === "file" && !meeting.title && meeting.source) return fileStem(split(meeting.source).stem);
  return fileStem(title);
}

/// A file's entry whose result could not be written: its transcript is only in the list.
export function unsavedFile(entry: Pick<MeetingSummary, "kind" | "output">): boolean {
  return entry.kind === "file" && entry.output === null;
}

/// The share of the current step that is done; `null` when unknown.
export function progressShare(progress: { done_ms: number; total_ms: number | null } | null): number | null {
  if (!progress || !progress.total_ms) return null;
  return Math.min(1, progress.done_ms / progress.total_ms);
}

/// The meeting being recorded and its time in whole seconds, for the quick panel; `reload` reads
/// it again.
export function useMeetingRecording() {
  const [recording, setRecording] = useState<number | null>(null);
  const [elapsed, setElapsed] = useState(0);
  const reload = useCallback(() => {
    invoke<MeetingsOverview>("get_meetings").then(
      (overview) => {
        setRecording(overview.recording);
        if (overview.recording === null) setElapsed(0);
      },
      () => {},
    );
  }, []);
  useEffect(reload, [reload]);
  useEvent<CoreEvent>(CORE_EVENT, (payload) => {
    if (payload.source !== "meeting") return;
    const event = payload.event;
    if (event.type === "started") {
      setRecording(event.meeting);
      setElapsed(0);
    } else if (event.type === "recording") {
      setRecording(event.meeting);
      // In whole seconds, as the clock shows it: the events come five times a second, and the
      // panel draws anew only when the second changes.
      setElapsed(Math.floor(event.elapsed_ms / 1000) * 1000);
    } else if (event.type === "recorded" || event.type === "discarded" || event.type === "changed") {
      reload();
    }
  });
  return { recording, elapsed, reload };
}
