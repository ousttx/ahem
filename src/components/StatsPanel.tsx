import { useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useTranslation } from "react-i18next";
import { commandFailure } from "../present";
import { formatDuration, savedNote, savedSeconds, useStats, wordsPerMinute, type StatsSummary } from "../stats";
import { Card, useConfirm, useFocusAfter } from "../ui";

/// Today, the last 7 days and everything: the time saved against typing, the words and the
/// speed. Counted for every inserted dictation, also with the history off; no text is kept.
/// The three tiles share one grid, so their lines stay level whatever the numbers.
export function StatsPanel() {
  const { t, i18n } = useTranslation();
  const stats = useStats();
  const [resetting, confirmReset, disarm] = useConfirm();
  // Why the statistics were not reset, until the next press.
  const [failure, setFailure] = useState<string | null>(null);
  // "Reset" goes once nothing is counted: the focus goes to the card's title.
  const follow = useFocusAfter();
  const title = useRef<HTMLHeadingElement>(null);
  if (!stats) return null;

  const reset = () => {
    follow(() => title.current);
    setFailure(null);
    void commandFailure(invoke("reset_stats"), (detail) => t("stats.reset_failed", { detail })).then(setFailure);
  };

  const number = new Intl.NumberFormat(i18n.language);
  const periods: { key: keyof StatsSummary; label: string }[] = [
    { key: "today", label: t("stats.today") },
    { key: "week", label: t("stats.week") },
    { key: "total", label: t("stats.total") },
  ];

  // Laid out like the pipeline card above it, so the two line up.
  return (
    <Card>
      <div className="pipeline">
        <div className="pipeline-header">
          <h3 ref={title} className="card-title" tabIndex={-1}>
            {t("stats.title")}
          </h3>
          {stats.total.dictations > 0 && (
            <button
              type="button"
              className="btn link quiet"
              onClick={() => confirmReset(reset)}
              onBlur={disarm}
            >
              {resetting ? t("stats.confirm_reset") : t("stats.reset")}
            </button>
          )}
        </div>
        {failure && (
          <span className="caption warning-text" role="alert">
            {failure}
          </span>
        )}
        <div className="stat-tiles">
          {periods.map(({ key, label }) => {
            const totals = stats[key];
            const speed = wordsPerMinute(totals);
            const words = t("stats.words", { words: number.format(totals.words) });
            return (
              <div key={key} className="stat-tile">
                <span className="caption">{label}</span>
                <span className="stat-saved" title={savedNote(t)}>
                  <span className="stat-value">{formatDuration(t, savedSeconds(totals))}</span>
                  <span className="caption">{t("stats.saved")}</span>
                </span>
                <span className="caption step-line">
                  {speed === null ? words : `${words} · ${t("stats.speed", { speed: number.format(speed) })}`}
                </span>
              </div>
            );
          })}
        </div>
      </div>
    </Card>
  );
}
