import { invoke } from "@tauri-apps/api/core";
import type { TFunction } from "i18next";
import { useCallback, useEffect, useState } from "react";
import { CORE_EVENT, useEvent, type CoreEvent } from "./events";
import { readOnFocus } from "./ui";

/// Dictations, words and recording time of a period (`storage::stats::Totals`).
export interface Totals {
  dictations: number;
  words: number;
  recording_ms: number;
}

export interface StatsSummary {
  today: Totals;
  week: Totals;
  total: Totals;
}

/// The typing speed the saved time is measured against, in words per minute.
const TYPING_WPM = 40;

/// The time typing the same words would take, minus the time spent recording them.
export function savedSeconds(totals: Totals): number {
  return Math.max(0, Math.round((totals.words / TYPING_WPM) * 60 - totals.recording_ms / 1000));
}

/// How the saved time is counted, with the typing speed it is measured against.
export function savedNote(t: TFunction): string {
  return t("stats.note", { wpm: TYPING_WPM });
}

/// Words per minute of recording; `null` before anything was recorded.
export function wordsPerMinute(totals: Totals): number | null {
  return totals.recording_ms > 0 ? Math.round(totals.words / (totals.recording_ms / 60_000)) : null;
}

/// Seconds under a minute, then minutes, then hours and minutes; abbreviated, so no plural
/// forms are needed.
export function formatDuration(t: TFunction, seconds: number): string {
  if (seconds < 60) return t("stats.seconds", { seconds: Math.round(seconds) });
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return t("stats.minutes", { minutes });
  return t("stats.hours", { hours: Math.floor(minutes / 60), minutes: minutes % 60 });
}

/// The statistics, read again after every counted dictation, after a reset in any window and
/// whenever the window gets the focus: "today" and "7 days" move on at midnight without a
/// dictation. The quick panel gets the focus at every opening.
export function useStats(): StatsSummary | null {
  const [stats, setStats] = useState<StatsSummary | null>(null);
  const load = useCallback(() => {
    invoke<StatsSummary>("get_stats").then(setStats, () => {});
  }, []);
  useEffect(() => readOnFocus(load), [load]);
  useEvent<CoreEvent>(CORE_EVENT, (payload) => {
    if (payload.source === "dictation" && payload.event.type === "stats_changed") load();
  });
  return stats;
}
