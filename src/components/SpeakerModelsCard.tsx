import { useCallback, useEffect, useRef, useState } from "react";
import type { TFunction } from "i18next";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";
import type { Download } from "../dictation";
import { CORE_EVENT, useEvent, type CoreEvent } from "../events";
import { SPEAKER_MODELS, type SpeakerModel } from "../meetings";
import { failureText, runningPercent } from "../present";
import { Card, Progress, useConfirm, useFocusAfter } from "../ui";
import { modelSource, useConnecting } from "./ModelsPanel";

/// Mirrors `meeting_commands::SpeakerModelsView`.
interface SpeakerModelsView {
  ready: boolean;
  /// A stopped download left files.
  partial: boolean;
  models: SpeakerModel[];
  busy: boolean;
}

/// The events after which the speaker models are read again: their download ended, however it
/// ended (a stopped one leaves files to resume or delete), a meeting event other than the
/// recording's meter (a transcription that starts makes the models busy), and a file whose
/// voices are told apart or that ends.
export function speakerModelsRead(payload: CoreEvent): boolean {
  switch (payload.source) {
    case "model":
      return speakerDownloadEnded(payload);
    case "meeting":
      return payload.event.type !== "recording";
    case "file": {
      const type = payload.event.type;
      return type === "speakers" || type === "finished" || type === "failed" || type === "cancelled";
    }
    default:
      return false;
  }
}

/// Whether `payload` ends the speaker models' download, however it ends.
export function speakerDownloadEnded(payload: CoreEvent): boolean {
  if (payload.source !== "model" || payload.event.model !== SPEAKER_MODELS) return false;
  return payload.event.type === "download_finished" || payload.event.type === "download_failed";
}

/// The read the view waits for after a download ended (`ended`, the number of the first read
/// after the end) once the read `read` answered: none when it was that read or a later one.
export function speakerViewRead(ended: number | null, read: number): number | null {
  return ended !== null && ended <= read ? null : ended;
}

/// Why the speaker models were not deleted, by the backend's reason (`MeetingCommandError`);
/// anything else as it is.
export function speakerDeleteError(t: TFunction, failure: unknown): string {
  return failureText(t, failure, "speakers.error", "speakers.error.store");
}

/// Whether the speaker models are downloaded and free, read again after their download or
/// deletion and when a meeting or a file starts or stops using them. `settling`: their download
/// ended, and the view read before says nothing yet of how it left them.
export function useSpeakerModels() {
  const [view, setView] = useState<SpeakerModelsView | null>(null);
  // The reads asked for so far, and the first one after the download ended until it answers.
  const asked = useRef(0);
  const [ended, setEnded] = useState<number | null>(null);
  const load = useCallback(() => {
    const read = ++asked.current;
    invoke<SpeakerModelsView>("get_speaker_models").then((answer) => {
      setView(answer);
      setEnded((current) => speakerViewRead(current, read));
    }, () => {});
  }, []);
  useEffect(load, [load]);
  // In the same drawing as the end of the download, which comes with the same event.
  useEvent<CoreEvent>(CORE_EVENT, (payload) => {
    if (speakerDownloadEnded(payload)) setEnded(asked.current + 1);
    if (speakerModelsRead(payload)) load();
  });
  return { view, load, settling: ended !== null };
}

/// The two models that tell voices apart, for meetings and files: what is downloaded, its size,
/// download and delete.
export function SpeakerModelsCard({ download }: { download: Download | undefined }) {
  const { t } = useTranslation();
  const { view, load, settling } = useSpeakerModels();
  const [deleting, confirmDelete] = useConfirm();
  const [failure, setFailure] = useState<string | null>(null);
  // Every action takes its button away: "Delete" gives "Download", "Download" gives "Cancel", and
  // "Cancel" goes as the download ends, however it ends. The focus goes to the first button
  // there then.
  const follow = useFocusAfter();
  const actions = useRef<HTMLDivElement>(null);
  const toActions = () => follow(() => actions.current?.querySelector<HTMLElement>("button"));
  const { connecting, connect, giveUp } = useConnecting();
  if (!view) return null;
  const ready = view.ready;

  const megabytes = (bytes: number) => Math.max(1, Math.round(bytes / 1_000_000));
  const total = megabytes(view.models.reduce((sum, model) => sum + model.size, 0));
  const going = download !== undefined && download.failure === undefined;
  // The download ended and the view is not read again yet: "Cancel" stays, disabled, instead of
  // the buttons of the state before it.
  const ending = !going && settling;
  // Asked for, and the server has not answered yet (`useConnecting`).
  const waiting = !going && connecting.has(SPEAKER_MODELS);
  const share = download && download.total > 0 ? download.downloaded / download.total : 0;

  async function remove() {
    try {
      await invoke("delete_speaker_models");
      setFailure(null);
    } catch (error) {
      setFailure(speakerDeleteError(t, error));
    }
    load();
  }

  const deleteButton = (
    <button
      type="button"
      className="btn link quiet"
      disabled={view.busy || waiting}
      onClick={() =>
        confirmDelete(() => {
          toActions();
          void remove();
        })
      }
    >
      {deleting ? t("speakers.confirm_delete") : t("models.delete")}
    </button>
  );

  // One model to the user: segmentation plus voice prints, downloaded together.
  const name = view.models.map((model) => model.name).join(" + ");
  // Both models come from the same project: its release is the place to read.
  const source = view.models.length > 0 ? modelSource(view.models[0].url) : null;
  return (
    <Card title={t("speakers.title")}>
      <div className="model-rows">
        <div className="model-row">
          <div className="model-texts">
            <span className="model-title">
              <span className="strong" title={t("speakers.source")}>
                {name}
              </span>
              <span className="badge neutral">{t("models.hardware.cpu")}</span>
            </span>
            <span className="caption">
              {t("speakers.size", { size: total })} · {t("speakers.note")}
            </span>
            {source && (
              <span className="caption">
                {t("models.source")}:{" "}
                <button type="button" className="btn link quiet" title={source.page} onClick={() => void openUrl(source.page).catch(() => {})}>
                  {source.label}
                </button>
              </span>
            )}
            {going && (
              <span className="model-progress">
                <Progress value={share} label={name} />
                <span className="caption tabular">{t("models.partial", { percent: runningPercent(share) })}</span>
              </span>
            )}
            {download?.failure && !waiting && (
              <span className="caption warning-text" role="alert">
                {t(`models.failed.${download.failure}`)}
              </span>
            )}
            {failure && (
              <span className="caption warning-text" role="alert">
                {failure}
              </span>
            )}
          </div>
          <div ref={actions} className="model-actions">
            {going || ending ? (
              <button
                type="button"
                className="btn compact"
                disabled={ending}
                onFocus={toActions}
                onClick={() => void invoke("cancel_model_download", { id: SPEAKER_MODELS }).catch(() => {})}
              >
                {t("models.cancel")}
              </button>
            ) : ready ? (
              <>
                <span className="caption">{t("models.ready")}</span>
                {deleteButton}
              </>
            ) : (
              <>
                <button
                  type="button"
                  className="btn compact"
                  disabled={waiting}
                  aria-busy={waiting || undefined}
                  onClick={() => {
                    toActions();
                    connect(SPEAKER_MODELS);
                    void invoke("download_speaker_models").catch(() => {
                      giveUp(SPEAKER_MODELS);
                      load();
                    });
                  }}
                >
                  {waiting ? t("models.connecting") : view.partial ? t("models.resume") : t("models.download")}
                </button>
                {/* What a stopped download left can be deleted too. */}
                {view.partial && deleteButton}
              </>
            )}
          </div>
        </div>
      </div>
    </Card>
  );
}
