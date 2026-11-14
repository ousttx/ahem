import type { TFunction } from "i18next";
import { useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { save } from "@tauri-apps/plugin-dialog";
import type { Target } from "../App";
import { fileRunning, type FileState } from "../dictation";
import { formatFilter, progressShare, split } from "../meetings";
import { failureText, modeCaption, modeName, runningPercent, type Message, type OwnMessages } from "../present";
import { useSpeakerModels } from "./SpeakerModelsCard";
import { revealFile } from "./MeetingsPanel";
import { SpeakerStages } from "./SpeakerStages";
import type { Settings, Update } from "../settings";
import { Card, clipTitle, Icon, IconCircle, Progress, Select, Toggle, useFocusAfter, useRoving } from "../ui";

/// Why a file did not start: a `FileStartError` of `commands.rs` by its kind; anything else, such
/// as a command that did not run, as it is.
export function fileStartError(t: TFunction, failure: unknown): string {
  return failureText(t, failure, "files.start_error", "files.start_error.other");
}

/// What the decoder reads (see `src-tauri/src/files/decode.rs`).
export const MEDIA = ["wav", "mp3", "m4a", "mp4", "m4v", "mov", "aac", "flac", "ogg", "oga", "opus", "mkv", "mka", "webm", "aif", "aiff"];
const TIMED = ["srt", "vtt", "json"];

/// The save dialog for a result that could not be written (`save_held_result`): where it was to
/// go, in its format, which the result is already in.
export function heldDialog(output: string | null): { defaultPath?: string; filters?: { name: string; extensions: string[] }[] } {
  if (!output) return {};
  const { extension } = split(output);
  return extension ? { defaultPath: output, filters: [formatFilter(extension)] } : { defaultPath: output };
}

interface Props {
  /// The view is on screen; hidden, it keeps its choices.
  shown: boolean;
  settings: Settings;
  /// Saves "By speaker", which stays as the user left it.
  update: Update;
  file: FileState | null;
  /// The file chosen on the Transcripts page; `null` once closed.
  path: string | null;
  /// A file is being dragged over the page.
  hover: boolean;
  modelName: string | null;
  /// A cloud service recognizes the audio.
  inCloud: boolean;
  /// The engine gives word times, which audio and video need (`gpt-transcribe` does not).
  mediaFiles: boolean;
  /// Whether the model translates; `null` without a model.
  translates: boolean | null;
  navigate: (target: Target) => void;
  /// The messages of the Transcripts page.
  messages: OwnMessages;
  /// For a saved file; failures go through `messages`.
  setMessage: (message: Message | null) => void;
  /// Back to the list; the file stays chosen.
  onBack: () => void;
  /// Closes the file and goes back to the list.
  onClose: () => void;
  /// Opens a kept transcript.
  onOpen: (id: number) => void;
}

/// The file view of the Transcripts page: transcribe a file with a mode and save the
/// result; an audio or video file's transcript is kept in the list too.
export function FilesPanel({
  shown,
  settings,
  update,
  file,
  path,
  hover,
  modelName,
  inCloud,
  mediaFiles,
  translates,
  navigate,
  messages,
  setMessage,
  onBack,
  onClose,
  onOpen,
}: Props) {
  const { t } = useTranslation();
  const [started, setStarted] = useState<{ path: string; output: string } | null>(null);
  const [modeId, setModeId] = useState("plain");
  const [format, setFormat] = useState("md");
  const [useReplacements, setUseReplacements] = useState(true);
  const bySpeaker = settings.files_by_speaker;
  const { view: speakerModels } = useSpeakerModels();
  const running = fileRunning(file);
  // "Start" and "Again" go once the file runs: the focus goes to the file's "Cancel", which
  // turns into "Close" in place when it ends.
  const follow = useFocusAfter();
  const fileButton = useRef<HTMLButtonElement>(null);

  const text = path !== null && split(path).extension === "txt";
  // A text file can only be processed by a language-model mode.
  const modes = text ? settings.modes.filter((item) => item.action.kind === "process") : settings.modes;
  const mode = modes.find((item) => item.id === modeId) ?? modes[0];
  const roving = useRoving(modes.map((item) => ({ checked: item.id === mode?.id, disabled: running })));
  const processes = mode?.action.kind === "process";
  const formats = text || processes ? ["md", "txt"] : ["md", "txt", ...TIMED];
  const chosenFormat = formats.includes(format) ? format : "md";
  // Replacements fix recognized speech, so not a text file.
  const offersReplacements = !text && settings.replacements.length > 0;
  // Voices are told apart in audio and video once the speaker models are downloaded.
  const offersSpeakers = !text && (speakerModels?.ready ?? false);

  async function start() {
    if (!path || !mode) return;
    const { folder, stem } = split(path);
    let output: string | null;
    try {
      output = await save({
        defaultPath: `${folder}${stem}.${chosenFormat}`,
        filters: [formatFilter(chosenFormat)],
      });
    } catch (failure) {
      return messages.fail(t("files.dialog_failed", { detail: String(failure) }));
    }
    if (!output) return;
    if (output === path) return messages.fail(t("files.same_file"));
    try {
      // The names a file gives its voices: "Speaker 1", "Speaker 2"…
      const speakers = { me: "", others: t("files.speaker_single"), person: t("files.speaker", { n: "{n}" }) };
      await invoke("transcribe_file", {
        path,
        output,
        format: chosenFormat,
        modeId: mode.id,
        replacements: offersReplacements && useReplacements,
        speakers: offersSpeakers && bySpeaker ? speakers : null,
      });
      setStarted({ path, output });
      messages.succeed();
    } catch (failure) {
      messages.fail(fileStartError(t, failure));
    }
  }

  // The result of the last file that could not be written, held until the next file: written
  // where the user chooses now.
  async function saveHeld() {
    let chosen: string | null;
    try {
      chosen = await save(heldDialog(started?.output ?? null));
    } catch (failure) {
      return messages.fail(t("files.dialog_failed", { detail: String(failure) }));
    }
    if (!chosen) return;
    if (chosen === started?.path) return messages.fail(t("files.same_file"));
    try {
      await invoke("save_held_result", { path: chosen });
      messages.succeed();
      setMessage({ text: t("meetings.saved", { path: chosen }), error: false });
    } catch (failure) {
      messages.fail(t("meetings.error.save", { detail: String(failure) }));
    }
  }

  const engineText = recognitionText(t, modelName, inCloud, mediaFiles);

  // The settings are shown before a file is chosen, so the choice is clear up front.
  const settingsCard = (
    <Card className="file-processing">
      <div className="file-processing-body">
        <div className="card-header inline">
          <h3 className="card-title">{t("files.processing_title")}</h3>
          <button type="button" className="btn link" onClick={() => navigate("modes")}>
            {t("nav.modes")}
          </button>
        </div>
        {modes.length > 0 ? (
          <div className="chips" role="radiogroup" aria-label={t("files.mode")} {...roving.group}>
            {modes.map((item, index) => (
              <button
                key={item.id}
                type="button"
                role="radio"
                aria-checked={item.id === mode?.id}
                tabIndex={roving.tabIndex(index)}
                className={item.id === mode?.id ? "chip checked" : "chip"}
                disabled={running}
                onClick={() => setModeId(item.id)}
              >
                {modeName(t, item)}
              </button>
            ))}
          </div>
        ) : (
          <p className="caption warning-text">{t("files.text_needs_mode")}</p>
        )}
        {mode && <p className="caption">{modeCaption(t, mode, settings.connections, translates)}</p>}
        <span className="field-label caption">{t("files.format")}</span>
        <Select
          label={t("files.format")}
          value={chosenFormat}
          disabled={running}
          options={formats.map((item) => ({ value: item, label: t(`files.format_name.${item}`) }))}
          onChange={setFormat}
        />
        {(text || processes) && <p className="caption">{t("files.processed_formats")}</p>}
        {offersSpeakers && (
          <div className="file-option">
            <span className="caption">{t("files.by_speaker")}</span>
            <Toggle
              label={t("files.by_speaker")}
              checked={bySpeaker}
              disabled={running}
              onChange={(on) => void update({ files_by_speaker: on })}
            />
          </div>
        )}
        {!text && speakerModels && !speakerModels.ready && (
          <p className="caption">
            {t("speakers.off")}{" "}
            <button type="button" className="btn link" onClick={() => navigate("models")}>
              {t("nav.models")}
            </button>
          </p>
        )}
        {offersReplacements && (
          <div className="file-option">
            <span className="caption">{t("files.replacements", { number: settings.replacements.length })}</span>
            <Toggle
              label={t("files.replacements", { number: settings.replacements.length })}
              checked={useReplacements}
              disabled={running}
              onChange={setUseReplacements}
            />
          </div>
        )}
      </div>
    </Card>
  );

  // The job shown is the one started for this file; the name covers a run whose file was closed.
  const shownPath = path ?? started?.path ?? "";
  const job = file && (started?.path === shownPath || running) ? file : null;
  const { folder, name } = split(shownPath || (job?.name ?? ""));
  const startLabel = text ? t("files.process") : t("files.start");
  const canStart = !running && mode !== undefined && (text ? modes.length > 0 : modelName !== null && mediaFiles);

  return (
    <div className="page files-page" hidden={!shown}>
      <div className="meeting-head">
        <button type="button" className="btn link" onClick={onBack}>
          {t("transcripts.back")}
        </button>
        <h1 className="page-title" tabIndex={-1}>{t("transcripts.file_title")}</h1>
      </div>
      <Card className={hover ? "file-card hover" : "file-card"}>
        <div className="file-row">
          <span className="file-tile">
            <Icon name={text ? "file" : "wave"} size={20} tone="accent" />
          </span>
          <div className="file-names">
            <span className="strong" {...clipTitle(shownPath)}>
              {name}
            </span>
            <span className="caption" {...clipTitle(folder)}>
              {text ? t("files.text_file") : `${folder}`}
            </span>
          </div>
          {job && <FileProgress job={job} />}
          {running ? (
            <button ref={fileButton} type="button" className="btn" onClick={() => void invoke("cancel_file").catch(() => {})}>
              {t("files.cancel")}
            </button>
          ) : (
            <button ref={fileButton} type="button" className="btn" onClick={onClose}>
              {t("files.close")}
            </button>
          )}
        </div>
      </Card>

      <div className="files-columns">
        <Card className="file-result">
          <FileResult
            job={job}
            output={started?.output ?? null}
            engineText={text ? t("files.text_engine") : engineText}
            startLabel={startLabel}
            canStart={canStart}
            onStart={() => {
              follow(() => fileButton.current);
              void start();
            }}
            onOpen={onOpen}
            onSaveHeld={() => void saveHeld()}
            onReveal={(saved) => revealFile(t, messages, saved)}
          />
        </Card>
        {settingsCard}
      </div>
    </div>
  );
}

/// What recognizes a file's audio, or why nothing can.
export function recognitionText(t: TFunction, modelName: string | null, inCloud: boolean, mediaFiles: boolean): string {
  const key = !mediaFiles ? "files.engine_no_files" : inCloud ? "files.engine_cloud" : "files.engine";
  return modelName ? t(key, { model: modelName }) : t("files.no_engine");
}

/// How far a running file is: recognition fills one bar; telling the voices apart, one for each
/// of its models; the language model, a word. Nothing once it has ended.
export function FileProgress({ job }: { job: FileState }) {
  const { t } = useTranslation();
  if (job.kind === "processing") return <span className="caption">{t("files.processing_short")}</span>;
  if (job.kind !== "transcribing" && job.kind !== "speakers") return null;
  const share = progressShare(job);
  return (
    <div className="file-progress">
      {job.kind === "speakers" ? (
        <SpeakerStages step={job.step} share={share} since={job.since} />
      ) : (
        <>
          <div className="file-progress-title">
            <span className="caption">{t("files.recognizing")}</span>
            {share !== null && <span className="caption">{runningPercent(share)}%</span>}
          </div>
          <Progress value={share ?? 0} label={t("files.recognizing")} />
        </>
      )}
    </div>
  );
}

function FileResult({
  job,
  output,
  engineText,
  startLabel,
  canStart,
  onStart,
  onOpen,
  onSaveHeld,
  onReveal,
}: {
  job: FileState | null;
  output: string | null;
  engineText: string;
  startLabel: string;
  canStart: boolean;
  onStart: () => void;
  onOpen: (id: number) => void;
  /// Writes the result held after it could not be written where the user chooses.
  onSaveHeld: () => void;
  /// Shows the saved file in its folder.
  onReveal: (path: string) => void;
}) {
  const { t } = useTranslation();
  const again = (
    <button type="button" className="btn" disabled={!canStart} onClick={onStart}>
      {t("files.again")}
    </button>
  );
  switch (job?.kind) {
    case "transcribing":
    case "speakers":
    case "processing":
      return (
        <div className="file-state">
          <p className="strong">{t(`files.${job.kind}`, { name: job.name })}</p>
          {output && <p className="caption selectable">{t("files.will_save", { path: output })}</p>}
          <p className="caption">{t("files.dictation_first")}</p>
        </div>
      );
    case "finished": {
      // A text file has no transcript to open.
      const transcript = job.transcript;
      return (
        <div className="file-state">
          <IconCircle icon="check" tone="ok" diameter={52} halo={0} />
          <p className="hero-title" role="status">
            {t("files.saved")}
          </p>
          <p className="caption selectable">{job.path}</p>
          {job.processing_error && (
            <p className="caption warning-text" role="alert">
              {t("files.processing_failed", { detail: job.processing_error })}
            </p>
          )}
          <div className="file-actions">
            {transcript !== null && (
              <button type="button" className="btn primary" onClick={() => onOpen(transcript)}>
                {t("transcripts.open_transcript")}
              </button>
            )}
            <button type="button" className={transcript !== null ? "btn" : "btn primary"} onClick={() => onReveal(job.path)}>
              {t("files.show")}
            </button>
            {again}
          </div>
        </div>
      );
    }
    case "failed": {
      // A result that could not be written is held until the next file, to be saved elsewhere
      // from here; its transcript is kept in the list too.
      const transcript = job.transcript;
      return (
        <div className="file-state">
          <IconCircle icon="warning" tone="danger" diameter={52} halo={0} />
          <p className="caption warning-text" role="alert">
            {t(`files.problem.${job.problem}`, { detail: job.detail })}
          </p>
          {job.held || transcript !== null ? (
            <div className="file-actions">
              {job.held && (
                <button type="button" className="btn primary" onClick={onSaveHeld}>
                  {t("meetings.save")}
                </button>
              )}
              {transcript !== null && (
                <button type="button" className={job.held ? "btn" : "btn primary"} onClick={() => onOpen(transcript)}>
                  {t("transcripts.open_transcript")}
                </button>
              )}
              {again}
            </div>
          ) : (
            again
          )}
        </div>
      );
    }
    case "cancelled":
      return (
        <div className="file-state">
          <p className="caption" role="status">
            {t("files.cancelled")}
          </p>
          {again}
        </div>
      );
    default:
      return (
        <div className="file-state">
          <p className="caption">{engineText}</p>
          <button type="button" className="btn primary large" disabled={!canStart} onClick={onStart}>
            {startLabel}
          </button>
        </div>
      );
  }
}
