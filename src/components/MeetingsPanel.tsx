import { memo, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";
import { invoke } from "@tauri-apps/api/core";
import { save } from "@tauri-apps/plugin-dialog";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import { IS_MAC } from "../dictation";
import { CORE_EVENT, useEvent, type CoreEvent } from "../events";
import { clock, failureText, runningPercent, type Message, type OwnMessages } from "../present";
import {
  exportLabels,
  exportStem,
  formatFilter,
  meetingTitle,
  namesSpeakers,
  NO_LIVE,
  progressShare,
  reduceMeeting,
  SPEAKER_MODELS,
  speakerName,
  trackProblemText,
  transcriptText,
  turnText,
  unsavedFile,
  type Meeting,
  type MeetingLive,
  type MeetingsOverview,
  type MeetingSummary,
} from "../meetings";
import { Card, IconButton, IconCircle, isComposing, Progress, Select, useConfirm, useCopied, useFocusAfter, useLevelStrip, WaveStrip } from "../ui";
import { SpeakerStages } from "./SpeakerStages";

/// Mirrors `meeting_commands::MeetingCommandError`.
type CommandError = { kind: string; detail?: string };

/// A command's failure for the message bar: a `CommandError` by its kind; anything else, such as
/// a command that did not run, as it is.
export function commandError(t: TFunction, failure: unknown): string {
  return failureText(t, failure, "meetings.error", "meetings.error.other");
}

const FORMATS = ["md", "txt", "srt", "vtt", "json"];

/// Whether a file's event says its transcript was kept in the list: with the saved result, or
/// although the result could not be written.
export function transcriptKept(payload: CoreEvent): boolean {
  if (payload.source !== "file") return false;
  const event = payload.event;
  return (event.type === "finished" || event.type === "failed") && event.transcript !== null;
}

/// The list of transcripts, reloaded when a meeting is recorded, transcribed, renamed or
/// deleted and when a file's transcript is kept; and what is going on right now.
export function useMeetings() {
  const [overview, setOverview] = useState<MeetingsOverview | null>(null);
  const [live, setLive] = useState<MeetingLive>(NO_LIVE);
  const [error, setError] = useState<string | null>(null);

  // The meeting being transcribed, as last read: a progress event for another one reads again.
  const transcribing = useRef<number | null>(null);
  const load = useCallback(async () => {
    try {
      const read = await invoke<MeetingsOverview>("get_meetings");
      transcribing.current = read.transcribing;
      setOverview(read);
      setError(null);
    } catch (failure) {
      setError((failure as CommandError).detail ?? String(failure));
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);
  useEvent<CoreEvent>(CORE_EVENT, (payload) => {
    if (payload.source === "meeting") {
      setLive((current) => reduceMeeting(current, payload.event));
      const event = payload.event;
      const fresh = event.type === "progress" && event.meeting !== transcribing.current;
      if ((event.type !== "recording" && event.type !== "progress") || fresh) void load();
    } else if (payload.source === "model" && payload.event.model === SPEAKER_MODELS && payload.event.type === "download_finished") {
      void load();
    } else if (transcriptKept(payload)) {
      void load();
    }
  });

  return { overview, live, error, load };
}

/// Runs a meeting command, reads the list again and shows a failure in the message bar;
/// `undefined` when it failed.
export type Run = (command: string, args?: Record<string, unknown>) => Promise<unknown>;

/// Records a meeting: the microphone and the computer's sound. A row of the Transcripts page's top
/// card; while recording, the time, both tracks and the stop.
export function RecordRow({
  overview,
  live,
  run,
}: {
  overview: MeetingsOverview | null;
  live: MeetingLive;
  run: Run;
}) {
  const { t } = useTranslation();
  const [discarding, confirmDiscard] = useConfirm();
  // A press waits for its command: a second press would find nothing to stop.
  const [pending, setPending] = useState(false);
  // "Start" and "Stop" take each other's place, also when the recording starts or stops in the
  // quick panel: the focus goes from one to the other.
  const follow = useFocusAfter();
  const startButton = useRef<HTMLButtonElement>(null);
  const stopButton = useRef<HTMLButtonElement>(null);
  const toOther = () => follow(() => startButton.current ?? stopButton.current);
  const press = async (command: string) => {
    toOther();
    setPending(true);
    await run(command);
    setPending(false);
  };
  // Started here, in the quick panel, or before this page loaded.
  const recordingId = live.recording?.meeting ?? overview?.recording ?? null;
  const recording = recordingId !== null;
  const status = live.recording?.meeting === recordingId ? live.recording : null;
  const me = useLevelStrip((status?.me_level ?? 0) / 1000, recording, 24);
  const others = useLevelStrip((status?.others_level ?? 0) / 1000, recording, 24);

  if (!recording) {
    return (
      <div className="meeting-record-body">
        <IconCircle icon="people" tone="accent" diameter={48} halo={0} />
        <div className="meeting-record-texts">
          <p className="strong">{t("meetings.record_title")}</p>
          <p className="caption">{t("meetings.record_note")}</p>
          {overview && !overview.system_audio && <p className="caption">{t("meetings.no_system_audio")}</p>}
        </div>
        <button
          ref={startButton}
          type="button"
          className="btn primary"
          disabled={pending}
          onFocus={toOther}
          onClick={() => press("start_meeting")}
        >
          {t("meetings.start")}
        </button>
      </div>
    );
  }

  return (
    <>
      <div className="meeting-record-body">
        <span className="meeting-timer">{clock(status?.elapsed_ms ?? 0)}</span>
        <div className="meeting-tracks">
          <div className="meeting-track">
            <span className="caption">{t("meetings.speaker.me")}</span>
            <WaveStrip levels={me} recording />
            {status?.me_problem && <span className="caption warning-text">{trackProblemText(t, status.me_problem)}</span>}
          </div>
          <div className="meeting-track">
            <span className="caption">{t("meetings.track_others")}</span>
            <WaveStrip levels={others} recording />
            {status?.others_problem && <span className="caption warning-text">{trackProblemText(t, status.others_problem)}</span>}
          </div>
        </div>
        <div className="meeting-record-actions">
          <button
            ref={stopButton}
            type="button"
            className="btn primary"
            disabled={pending}
            onFocus={toOther}
            onClick={() => press("stop_meeting")}
          >
            {t("meetings.stop")}
          </button>
          <button
            type="button"
            className="btn link quiet"
            disabled={pending}
            onFocus={toOther}
            onClick={() => confirmDiscard(() => void press("discard_meeting"))}
          >
            {discarding ? t("meetings.confirm_discard") : t("meetings.discard")}
          </button>
        </div>
      </div>
      {IS_MAC && <p className="caption card-note">{t("meetings.mac_permission")}</p>}
    </>
  );
}

export function stateText(t: TFunction, meeting: MeetingSummary, live: Pick<MeetingLive, "progress">): string {
  if (live.progress?.meeting === meeting.id) {
    const share = progressShare(live.progress);
    const step = t(`meetings.step.${live.progress.step}`);
    return share === null ? step : `${step} · ${runningPercent(share)}%`;
  }
  if (meeting.state === "failed" && meeting.problem) return problemText(t, meeting.problem);
  return t(`meetings.state.${meeting.state}`);
}

/// Why a meeting or a file was not transcribed. A stored problem this version has no words for
/// says only that it failed.
export function problemText(t: TFunction, problem: string, detail = ""): string {
  return t(`meetings.problem.${problem}`, { detail, defaultValue: t("meetings.state.failed") }).trim();
}

/// An entry of the list of transcripts: a meeting or a file, by its badge. It takes only the
/// transcription's step (`progress`), not the recording's meter, so the rows do not draw anew
/// five times a second while a meeting is recorded.
export const MeetingRow = memo(function MeetingRow({
  meeting,
  progress,
  onOpen,
}: {
  meeting: MeetingSummary;
  progress: MeetingLive["progress"];
  onOpen: (id: number) => void;
}) {
  const { t, i18n } = useTranslation();
  const shown = meeting.state !== "done" || progress?.meeting === meeting.id;
  return (
    <button type="button" className="meeting-row" onClick={() => onOpen(meeting.id)}>
      <span className="meeting-row-head">
        <span className="meeting-row-title strong">{meetingTitle(t, meeting, i18n.language)}</span>
        <span className="badge neutral">{t(`transcripts.kind.${meeting.kind}`)}</span>
      </span>
      <span className="caption">
        {clock(meeting.duration_ms)}
        {shown && ` · ${stateText(t, meeting, { progress })}`}
      </span>
      {meeting.preview && <span className="meeting-row-preview caption">{meeting.preview}</span>}
    </button>
  );
});

/// Shows a file in its folder; the user may have moved or deleted it since, and the page's
/// messages then say it could not be shown.
export function revealFile(t: TFunction, messages: Pick<OwnMessages, "fail">, path: string) {
  revealItemInDir(path).catch((failure) => messages.fail(t("models.open_failed", { detail: String(failure) })));
}

/// Tells the page's messages how a copy went: a refusal (`refused`) is said, and a copy that goes
/// through takes the refusal before it away.
export function reportCopy(messages: Pick<OwnMessages, "fail" | "succeed">, took: boolean, refused: string) {
  if (took) messages.succeed();
  else messages.fail(refused);
}

/// What a key does in the field of a new title: Enter saves it (`true`), Escape drops it
/// (`false`); `null` for any other key. Either puts the field away with the focus it had, which
/// goes to the title back in its place.
export function renameKey(key: string): boolean | null {
  if (key === "Enter") return true;
  if (key === "Escape") return false;
  return null;
}

/// The page of an entry, a meeting or a file: its transcript by speaker, copy, save in
/// a format, rename, delete; a meeting can be transcribed again, a file shows its source and
/// result.
export function MeetingView({
  id,
  live,
  overview,
  run,
  onBack,
  setMessage,
  messages,
}: {
  id: number;
  live: MeetingLive;
  overview: MeetingsOverview | null;
  run: Run;
  onBack: () => void;
  /// For a saved file; failures go through `messages`.
  setMessage: (message: Message | null) => void;
  /// The messages of the Transcripts page.
  messages: OwnMessages;
}) {
  const { t, i18n } = useTranslation();
  const [meeting, setMeeting] = useState<(Meeting & { audio: string }) | null>(null);
  const [format, setFormat] = useState("md");
  const [deleting, confirmDelete] = useConfirm();
  const [renaming, setRenaming] = useState<string | null>(null);
  // Enter or Escape ended the renaming; the blur of the closing field must not save again.
  const renameDone = useRef(false);
  // The focus goes back to the title from the field that Enter or Escape put away.
  const titleBack = useRef(false);
  // Which text was just copied: the transcript or the processed text.
  const [copied, copyText] = useCopied<"transcript" | "processed">();
  // "Transcribe" waits for its command: a second press would find the meeting busy.
  const [transcribeSent, setTranscribeSent] = useState(false);
  const follow = useFocusAfter();
  const summary = overview?.meetings.find((item) => item.id === id) ?? null;
  // Joined once for each reading of the entry: the page draws anew with every live event.
  const texts = useMemo(() => meeting?.turns.map((turn) => turnText(turn)) ?? [], [meeting]);

  // Read again whenever the list changes: a finished transcription, a new title, a file's result
  // saved by "Save as…" after it could not be written.
  useEffect(() => {
    invoke<Meeting & { audio: string }>("get_meeting", { id }).then(setMeeting, () => onBack());
  }, [id, summary?.state, summary?.title, summary?.output, onBack]);
  // The page opens before its entry is read: its title takes the focus once it shows, the button
  // that opened it being gone.
  const titleRef = useRef<HTMLHeadingElement>(null);
  const shown = meeting !== null;
  useEffect(() => {
    if (shown) titleRef.current?.focus({ preventScroll: true });
  }, [shown]);
  useEffect(() => {
    if (renaming !== null || !titleBack.current) return;
    titleBack.current = false;
    titleRef.current?.focus();
  }, [renaming]);

  if (!meeting) return null;
  const title = meetingTitle(t, meeting, i18n.language);
  const progress = live.progress?.meeting === id ? live.progress : null;
  const transcribing = overview?.transcribing === id;
  const share = progressShare(progress);
  const failure = live.failure?.meeting === id ? live.failure : null;
  const problem = failure?.problem ?? (meeting.state === "failed" ? meeting.problem : null);
  const isFile = meeting.kind === "file";
  // A file whose voices were not told apart has one unnamed speaker.
  const named = namesSpeakers(meeting);

  async function exportTo() {
    if (!meeting) return;
    let output: string | null;
    try {
      output = await save({
        defaultPath: `${exportStem(meeting, title)}.${format}`,
        filters: [formatFilter(format)],
      });
    } catch (failure) {
      return messages.fail(t("files.dialog_failed", { detail: String(failure) }));
    }
    if (!output) return;
    const saved = await run("export_meeting", { id, output, format, title, labels: exportLabels(t, meeting.kind) });
    if (saved !== undefined) setMessage({ text: t("meetings.saved", { path: output }), error: false });
  }

  // The clipboard is used only on these explicit clicks.
  async function copy(which: "transcript" | "processed") {
    if (!meeting) return;
    const text = which === "processed" ? (meeting.processed ?? "") : transcriptText(t, meeting);
    reportCopy(messages, await copyText(which, text), t("last.copy_failed"));
  }

  function reveal(path: string) {
    revealFile(t, messages, path);
  }

  function startRenaming() {
    renameDone.current = false;
    setRenaming(meeting?.title ?? "");
  }

  async function rename(keep: boolean) {
    if (renaming === null || renameDone.current) return;
    renameDone.current = true;
    const typed = renaming;
    setRenaming(null);
    if (keep) await run("rename_meeting", { id, title: typed.trim() || null });
  }

  async function remove() {
    if ((await run("delete_meeting", { id })) !== undefined) onBack();
  }

  // The button hides while the meeting is transcribed: the focus goes to the title.
  async function transcribe() {
    follow(() => titleRef.current);
    setTranscribeSent(true);
    await run("transcribe_meeting", { id });
    setTranscribeSent(false);
  }

  return (
    <>
      <div className="meeting-head">
        <button type="button" className="btn link" onClick={onBack}>
          {t("transcripts.back")}
        </button>
        {renaming === null ? (
          <h1 ref={titleRef} className="page-title meeting-title" tabIndex={-1} title={t("meetings.rename")} onDoubleClick={startRenaming}>
            {title}
          </h1>
        ) : (
          <input
            className="meeting-title-input"
            value={renaming}
            placeholder={title}
            aria-label={t("meetings.rename")}
            autoFocus
            onChange={(event) => setRenaming(event.target.value)}
            onBlur={() => void rename(true)}
            onKeyDown={(event) => {
              if (isComposing(event)) return;
              const keep = renameKey(event.key);
              if (keep === null) return;
              titleBack.current = true;
              void rename(keep);
            }}
          />
        )}
        <p className="caption">
          {new Date(meeting.started_ms).toLocaleString(i18n.language, { dateStyle: "long", timeStyle: "short" })} · {clock(meeting.duration_ms)}
        </p>
      </div>

      <Card>
        {/* Two rows on purpose: the transcript's tools, then the entry's links. One row does
            not hold them all, and a row that wraps by itself looks broken. */}
        {meeting.turns.length > 0 && (
          <div className="meeting-actions">
            <IconButton icon={copied === "transcript" ? "check" : "copy"} label={t("meetings.copy")} onClick={() => void copy("transcript")} />
            <Select
              label={t("meetings.format")}
              value={format}
              options={FORMATS.map((item) => ({ value: item, label: t(`files.format_name.${item}`) }))}
              onChange={setFormat}
            />
            <button type="button" className="btn" onClick={exportTo}>
              {t("meetings.save")}
            </button>
          </div>
        )}
        <div className="meeting-links">
          {isFile ? (
            <>
              {meeting.source && (
                <button type="button" className="btn link" onClick={() => reveal(meeting.source as string)}>
                  {t("transcripts.show_source")}
                </button>
              )}
              {meeting.output && (
                <button type="button" className="btn link" onClick={() => reveal(meeting.output as string)}>
                  {t("transcripts.show_output")}
                </button>
              )}
            </>
          ) : (
            <button type="button" className="btn link" onClick={() => reveal(meeting.audio)}>
              {t("meetings.show_audio")}
            </button>
          )}
          <button type="button" className="btn link" onClick={startRenaming}>
            {t("meetings.rename")}
          </button>
          {/* One transcription at a time: hidden while any meeting is being transcribed. A file
              is transcribed again from its view, not here. */}
          {!isFile && overview?.transcribing === null && meeting.state !== "transcribing" && (
            <button type="button" className="btn link" disabled={transcribeSent} onClick={() => void transcribe()}>
              {meeting.state === "recorded" ? t("meetings.transcribe") : t("meetings.transcribe_again")}
            </button>
          )}
          <span className="meeting-actions-spacer" />
          <button type="button" className="btn link quiet" disabled={transcribing} onClick={() => confirmDelete(() => void remove())}>
            {deleting ? t("meetings.confirm_delete") : t("meetings.delete")}
          </button>
        </div>
      </Card>

      {transcribing && (
        <Card>
          <div className="meeting-progress">
            {progress && (progress.step === "segments" || progress.step === "voices") ? (
              <SpeakerStages step={progress.step} share={share} since={progress.since} />
            ) : (
              <>
                <div className="file-progress-title">
                  <span className="caption">{progress ? t(`meetings.step.${progress.step}`) : t("meetings.state.transcribing")}</span>
                  {share !== null && <span className="caption">{runningPercent(share)}%</span>}
                </div>
                <Progress value={share ?? 0} label={t("meetings.state.transcribing")} />
              </>
            )}
            <p className="caption">{t("meetings.dictation_first")}</p>
            <button
              type="button"
              className="btn link"
              // The card goes once the transcription ends or stops: the focus on its button goes
              // to the title.
              onFocus={() => follow(() => titleRef.current)}
              onClick={() => void invoke("cancel_meeting_transcription", { id }).catch(() => {})}
            >
              {t("meetings.cancel")}
            </button>
          </div>
        </Card>
      )}

      {!transcribing && problem && (
        <p className="caption warning-text" role="alert">
          {problemText(t, problem, failure?.detail)}
        </p>
      )}
      {!transcribing && meeting.state === "recorded" && <p className="caption">{t("meetings.not_transcribed")}</p>}
      {/* A file entry whose result could not be written: "Save as…" above writes one. */}
      {unsavedFile(meeting) && <p className="caption">{t("transcripts.not_saved")}</p>}

      {meeting.processed && (
        <Card
          title={t("transcripts.processed")}
          action={
            <IconButton icon={copied === "processed" ? "check" : "copy"} label={t("transcripts.copy_processed")} onClick={() => void copy("processed")} />
          }
        >
          <p className="meeting-processed selectable">{meeting.processed}</p>
        </Card>
      )}

      {meeting.turns.length > 0 && (
        <Card>
          <div className="meeting-turns">
            {meeting.turns.map((turn, index) => (
              <div key={index} className={turn.speaker.kind === "me" ? "meeting-turn me" : "meeting-turn"}>
                <span className="caption">
                  {named ? (
                    <>
                      <span className="strong">{speakerName(t, turn.speaker, meeting.kind)}</span> · {clock(turn.start * 1000)}
                    </>
                  ) : (
                    clock(turn.start * 1000)
                  )}
                </span>
                <p className="meeting-turn-text selectable">{texts[index]}</p>
              </div>
            ))}
          </div>
        </Card>
      )}
    </>
  );
}
