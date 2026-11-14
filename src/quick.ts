// What the quick panel offers: pure functions of the settings, the models, the history and the
// dictation's state.
import type { TFunction } from "i18next";
import type { CloudEngineInfo } from "./components/CloudPanel";
import type { Entry } from "./components/HistoryPanel";
import type { ModelInfo } from "./components/ModelsPanel";
import { formatHotkey, type Phase, type View } from "./dictation";
import { errorKind, failureText, noticeText, STOP_KEYS, type Status } from "./present";
import { serviceOf } from "./services";
import type { Connection, RecordingMode } from "./settings";

export interface QuickModel {
  id: string;
  name: string;
}

/// The models dictation can switch to at once: downloaded ones and cloud engines whose
/// service is connected. The chosen one stays in the list even if it is not usable now.
export function quickModels(
  models: ModelInfo[],
  cloud: CloudEngineInfo[],
  connections: Connection[],
  chosen: string | null,
): QuickModel[] {
  const services = new Set(connections.map(serviceOf));
  const local = models
    .filter((model) => model.state.kind === "ready" || model.id === chosen)
    .map((model) => ({ id: model.id, name: model.name }));
  const remote = cloud
    .filter((engine) => services.has(engine.service) || engine.id === chosen)
    .map((engine) => ({ id: engine.id, name: engine.name }));
  return [...local, ...remote];
}

/// How many of the newest dictations the panel shows: it reads no more of the history.
export const QUICK_RECENT = 3;

/// The newest dictations for the panel, as the text that was inserted.
export function recentTexts(entries: Entry[], count: number): { id: number; time: number; text: string }[] {
  return entries.slice(0, count).map((entry) => ({ id: entry.id, time: entry.created_at_ms, text: entry.processed ?? entry.transcript }));
}

/// The line about a dictation that was not recognized: it can go again (`retry`), or it went
/// again and its text is in "Last", processed or, with a `warning`, not; `null` when there is none.
export function retryLine(
  t: TFunction,
  view: Pick<View, "retry" | "notice">,
): { text: string; retry: boolean; warning: boolean } | null {
  if (view.retry) return { text: t("dictation.not_recognized"), retry: true, warning: false };
  if (view.notice?.kind === "kept") return { text: noticeText(t, view.notice), retry: false, warning: false };
  if (view.notice?.kind === "kept_unprocessed") return { text: noticeText(t, view.notice), retry: false, warning: true };
  return null;
}

/// The head of the panel at rest: ready, with the shortcut's hint and a live dot; not set up
/// without a model; not ready, with the reason, when the main shortcut is not registered or the
/// engine of the chosen model is missing (`status`, read at every opening; `null` until then).
export function restingState(
  t: TFunction,
  { chosen, status, hotkey, recordingMode }: { chosen: boolean; status: Status | null; hotkey: string; recordingMode: RecordingMode },
): { title: string; hint: string; live: boolean } {
  if (!chosen) return { title: t("quick.not_set_up"), hint: t("quick.set_up"), live: false };
  if (status && !status.engine_available) return { title: t("quick.not_ready"), hint: t("status.engine_missing"), live: false };
  if (status && !status.hotkey_registered) {
    return { title: t("quick.not_ready"), hint: t("status.hotkey_missing", { hotkey: formatHotkey(hotkey) }), live: false };
  }
  return { title: t("quick.ready"), hint: t(`quick.hint_${recordingMode}`), live: true };
}

/// The head of the panel: at rest, `restingState`; while a dictation runs, its state with the
/// dot live and the shortcut's hint: how to stop while recording, as the dictation card says it
/// (`STOP_KEYS`). Why the panel is not ready at rest does not concern a dictation that runs, such
/// as one started by a mode's shortcut while the main one is taken.
export function quickHead(
  t: TFunction,
  current: Phase,
  resting: { chosen: boolean; status: Status | null; hotkey: string; recordingMode: RecordingMode },
): { title: string; hint: string; live: boolean } {
  if (current === "idle") return restingState(t, resting);
  if (current === "recording") return { title: t("quick.listening"), hint: t(`dictation.keys.${STOP_KEYS[resting.recordingMode]}`), live: true };
  return { title: t(`status.${current}`), hint: t(`quick.hint_${resting.recordingMode}`), live: true };
}

/// What the panel says when its meeting command failed: a meeting that could not be saved
/// (`store`, such as a full disk), whether it was starting or stopping, as the main window says
/// it; anything else, that the recording could not start or stop.
export function meetingFailureText(t: TFunction, action: "start" | "stop", failure: unknown): string {
  if (errorKind(failure) === "store") return failureText(t, failure, "meetings.error", "meetings.error.other");
  return t(`quick.meeting_${action}_failed`);
}

/// Whether the panel rises in when it opens: not on macOS, where the menus of the menu bar are
/// there at once (measured 2026-11-12: whole in the first frame, no fade, no slide), and not
/// with the system's reduced motion.
export function panelRisesIn(mac: boolean, reducedMotion: boolean): boolean {
  return !mac && !reducedMotion;
}
