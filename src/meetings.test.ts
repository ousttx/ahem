import type { TFunction } from "i18next";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  baseName,
  exportLabels,
  exportStem,
  fileStem,
  formatFilter,
  meetingTitle,
  namesSpeakers,
  NO_LIVE,
  progressShare,
  reduceMeeting,
  speakerName,
  split,
  trackProblemText,
  transcriptText,
  turnText,
  unsavedFile,
  type MeetingSummary,
  type Turn,
} from "./meetings";
import { resources } from "./i18n";

const t = ((key: string, values?: Record<string, unknown>) => {
  if (key === "meetings.speaker.me") return "Me";
  if (key === "meetings.speaker.others") return "Others";
  if (key === "meetings.speaker.person") return `Speaker ${values?.n}`;
  if (key === "meetings.untitled") return `Meeting ${values?.when}`;
  if (key === "files.speaker") return `Voice ${values?.n}`;
  if (key === "files.speaker_single") return "Voice";
  return key;
}) as unknown as TFunction;

function turn(speaker: Turn["speaker"], ...words: string[]): Turn {
  return { speaker, start: 0, end: 1, words: words.map((text, i) => ({ start: i, end: i + 0.5, text })) };
}

const summary: MeetingSummary = {
  id: 1,
  started_ms: Date.UTC(2026, 10, 9, 12, 0),
  duration_ms: 0,
  title: null,
  state: "done",
  problem: null,
  preview: null,
  kind: "meeting",
  source: null,
  output: null,
};

const file: MeetingSummary = { ...summary, id: 2, kind: "file", source: "C:\\Users\\me\\Lecture 3.final.mp4", output: "C:\\Users\\me\\Lecture 3.final.md" };

describe("meetings", () => {
  it("join a turn's words as the engine wrote them", () => {
    expect(turnText(turn({ kind: "me" }, " Hello", " world.", "  "))).toBe("Hello world.");
    // Languages without spaces keep their words together.
    expect(turnText(turn({ kind: "me" }, "你", "好"))).toBe("你好");
  });

  it("name the speakers and keep a number slot for the saved file", () => {
    expect(speakerName(t, { kind: "me" })).toBe("Me");
    expect(speakerName(t, { kind: "person", number: 2 })).toBe("Speaker 2");
    expect(exportLabels(t)).toEqual({ me: "Me", others: "Others", person: "Speaker {n}" });
    const text = transcriptText(t, { kind: "meeting", turns: [turn({ kind: "me" }, " Hi"), turn({ kind: "others" }, " Hello")] });
    expect(text).toBe("Me: Hi\n\nOthers: Hello");
  });

  it("name a file's voices as the Files view did", () => {
    expect(speakerName(t, { kind: "person", number: 2 }, "file")).toBe("Voice 2");
    expect(exportLabels(t, "file")).toEqual({ me: "Me", others: "Voice", person: "Voice {n}" });
    const turns = [turn({ kind: "person", number: 1 }, " Hi"), turn({ kind: "person", number: 2 }, " Hello")];
    expect(transcriptText(t, { kind: "file", turns })).toBe("Voice 1: Hi\n\nVoice 2: Hello");
  });

  it("title a meeting by its start unless the user named it", () => {
    expect(meetingTitle(t, summary, "en")).toMatch(/^Meeting /);
    expect(meetingTitle(t, { ...summary, title: "Planning" }, "en")).toBe("Planning");
    expect(fileStem('Call: "Q4" / plan?')).toBe("Call Q4 plan");
    expect(fileStem(" :: ")).toBe("meeting");
  });

  it("title a file by its name and offer it without the extension", () => {
    expect(meetingTitle(t, file, "en")).toBe("Lecture 3.final.mp4");
    expect(exportStem(file, meetingTitle(t, file, "en"))).toBe("Lecture 3.final");
    expect(baseName("/home/me/talk.ogg")).toBe("talk.ogg");
    // A title the user gave wins for both.
    const named = { ...file, title: "Lecture: 3" };
    expect(meetingTitle(t, named, "en")).toBe("Lecture: 3");
    expect(exportStem(named, "Lecture: 3")).toBe("Lecture 3");
    expect(exportStem(summary, "Planning")).toBe("Planning");
  });

  it("leave the speaker out of a file whose voices were not told apart", () => {
    const unnamed = [turn({ kind: "others" }, " Hello."), turn({ kind: "others" }, " Bye.")];
    expect(namesSpeakers({ kind: "file", turns: unnamed })).toBe(false);
    expect(namesSpeakers({ kind: "meeting", turns: unnamed })).toBe(true);
    expect(namesSpeakers({ kind: "file", turns: [turn({ kind: "person", number: 1 }, "a")] })).toBe(true);
    expect(transcriptText(t, { kind: "file", turns: unnamed })).toBe("Hello.\n\nBye.");
  });

  it("follow the recording and the transcription", () => {
    let live = reduceMeeting(NO_LIVE, {
      type: "recording",
      meeting: 4,
      elapsed_ms: 1200,
      me_level: 300,
      others_level: 0,
      me_problem: "cannot_write",
      others_problem: "no_device",
    });
    expect(live.recording).toMatchObject({ meeting: 4, elapsed_ms: 1200, me_problem: "cannot_write", others_problem: "no_device" });
    live = reduceMeeting(live, { type: "recorded", meeting: 4 });
    expect(live.recording).toBeNull();
    // Started in another window: this one shows it at once, and its end.
    live = reduceMeeting(live, { type: "started", meeting: 5 });
    expect(live.recording).toMatchObject({ meeting: 5, elapsed_ms: 0, me_problem: null, others_problem: null });
    live = reduceMeeting(live, { type: "discarded", meeting: 5 });
    expect(live.recording).toBeNull();
    live = reduceMeeting(live, { type: "progress", meeting: 4, step: "me", done_ms: 30_000, total_ms: 60_000 });
    expect(progressShare(live.progress)).toBe(0.5);
    live = reduceMeeting(live, { type: "failed", meeting: 4, problem: "no_engine", detail: "" });
    expect(live.progress).toBeNull();
    expect(live.failure?.problem).toBe("no_engine");
    // Trying again clears the failure.
    live = reduceMeeting(live, { type: "progress", meeting: 4, step: "segments", done_ms: 0, total_ms: null });
    expect(live.failure).toBeNull();
    expect(progressShare(live.progress)).toBeNull();
    // The second model of telling voices apart has a share.
    live = reduceMeeting(live, { type: "progress", meeting: 4, step: "voices", done_ms: 15_000, total_ms: 60_000 });
    expect(live.progress?.step).toBe("voices");
    expect(progressShare(live.progress)).toBe(0.25);
    live = reduceMeeting(live, { type: "done", meeting: 4 });
    expect(live.progress).toBeNull();
  });

  it("tell a track without a device from a track that is not being saved", () => {
    expect(trackProblemText(t, "no_device")).toBe("meetings.track_missing");
    expect(trackProblemText(t, "cannot_write")).toBe("meetings.track_not_saved");
    for (const language of ["en", "ru"] as const) {
      const meetings = resources[language].translation.meetings as Record<string, unknown>;
      expect(meetings.track_missing).not.toBe(meetings.track_not_saved);
      expect(meetings.track_not_saved).toEqual(expect.any(String));
    }
  });

  it("say which file entries were not saved to a file", () => {
    expect(unsavedFile(file)).toBe(false);
    expect(unsavedFile({ ...file, output: null })).toBe(true);
    // A meeting has no result file.
    expect(unsavedFile(summary)).toBe(false);
  });

  describe("the time of a step", () => {
    afterEach(() => vi.useRealTimers());

    it("counts from the step's start, not from when the page shows it", () => {
      vi.useFakeTimers();
      const step = (meeting: number, name: "segments" | "voices") =>
        ({ type: "progress", meeting, step: name, done_ms: 0, total_ms: null }) as const;
      vi.setSystemTime(1_000);
      let live = reduceMeeting(NO_LIVE, step(4, "segments"));
      expect(live.progress?.since).toBe(1_000);
      vi.setSystemTime(9_000);
      live = reduceMeeting(live, step(4, "segments"));
      expect(live.progress?.since).toBe(1_000);
      expect(reduceMeeting(live, step(4, "voices")).progress?.since).toBe(9_000);
      expect(reduceMeeting(live, step(5, "segments")).progress?.since).toBe(9_000);
    });
  });
});

describe("split", () => {
  it("takes a path apart on Windows and elsewhere", () => {
    expect(split("C:\\Users\\me\\talk.MP3")).toEqual({
      folder: "C:\\Users\\me\\",
      name: "talk.MP3",
      stem: "talk",
      extension: "mp3",
    });
    expect(split("/home/me/notes.txt").extension).toBe("txt");
    expect(split("/home/me/README")).toMatchObject({ stem: "README", extension: "" });
    // A name that begins with its only dot has no extension.
    expect(split("/home/me/.profile")).toMatchObject({ name: ".profile", stem: ".profile", extension: "" });
  });
});

describe("formatFilter", () => {
  it("names a save dialog's format by its extension", () => {
    expect(formatFilter("md")).toEqual({ name: "MD", extensions: ["md"] });
    expect(formatFilter("srt")).toEqual({ name: "SRT", extensions: ["srt"] });
  });
});
