import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import type { Target } from "../App";
import { phase, useLevel, useRetry, type View } from "../dictation";
import { resultText } from "../events";
import { CALM_MS, hero, splitClock, type Status } from "../present";
import type { Settings } from "../settings";
import { Card, Icon, IconCircle, KeyCaps, useCopied, useElapsed, useFocusAfter, useLevelStrip, WaveStrip } from "../ui";
import { openChatGptUsage } from "./ConnectionsPanel";

const WAVE_BARS = 28;

interface Props {
  view: View;
  settings: Settings;
  status: Status;
  modelName: string | null;
  /// The chosen engine can recognize now (`canRecognize`).
  usable: boolean;
  navigate: (target: Target) => void;
}

/// The dictation card at the top of the Dictation page: what happens now and the main hotkey.
export function StatusPanel({ view, settings, status, modelName, usable, navigate }: Props) {
  const { t } = useTranslation();
  const recording = phase(view) === "recording";

  // A calm outcome ("Text inserted") is shown for a moment, then the card is ready again.
  const [fresh, setFresh] = useState(false);
  useEffect(() => {
    if (!view.notice) return setFresh(false);
    setFresh(true);
    const timer = setTimeout(() => setFresh(false), CALM_MS);
    return () => clearTimeout(timer);
  }, [view.notice]);

  // The text that was not inserted; the clipboard is written only on this click. Copies are
  // keyed by the text, so what was said of one goes with the next dictation's.
  const [copiedText, copyText, refusedText] = useCopied<string>();
  const last = view.last ? resultText(view.last) : null;
  const copied = copiedText !== null && copiedText === last;
  async function copy() {
    if (last !== null) await copyText(last, last);
  }

  const elapsed = useElapsed(recording, 250);
  const retry = useRetry(view.retry);
  // "Recognize again" goes as the recording is recognized: the focus goes to the card's title.
  const follow = useFocusAfter();
  const title = useRef<HTMLDivElement>(null);

  // The keys go down with the hotkey: held for the whole recording in hold mode, a short tap at
  // the start and at the end otherwise.
  const holds = settings.recording_mode === "hold";
  const [tap, setTap] = useState(0);
  const wasRecording = useRef(recording);
  useEffect(() => {
    if (recording !== wasRecording.current && !holds) setTap((count) => count + 1);
    wasRecording.current = recording;
  }, [recording, holds]);

  // Only this card draws the level, so the rest of the window does not draw with it.
  const level = useLevel();
  const levels = useLevelStrip(level, recording, WAVE_BARS);

  const copyFailed = refusedText !== null && refusedText === last;
  const state = hero(t, { view, settings, status, modelName, usable, fresh, elapsed, copyFailed });
  // The clock changes every second: screen readers hear the state, not each second of it.
  const [beforeClock, clock, afterClock] = splitClock(state.title, state.clock);
  return (
    <Card>
      <div className="hero-row">
        <IconCircle icon={state.icon} tone={state.tone} ring={state.ring} diameter={38} halo={4} />
        <div className="hero-texts" aria-live="polite">
          <div ref={title} className="hero-title" tabIndex={-1}>
            {beforeClock}
            {clock && <span aria-hidden="true">{clock}</span>}
            {afterClock}
          </div>
          {state.subtitle && <div className="caption">{state.subtitle}</div>}
          {retry.failed && state.retry && <div className="caption warning-text">{t("dictation.retry_failed")}</div>}
          {state.warnings.map((warning) => (
            <div key={warning} className="caption warning-text">
              {warning}
            </div>
          ))}
          {state.copyFailure && <div className="caption warning-text">{state.copyFailure}</div>}
        </div>
        {state.wave && <WaveStrip levels={levels} recording />}
        {state.copy && (
          <button type="button" className={copied ? "btn copy-button copied" : "btn copy-button"} onClick={copy}>
            <Icon name="check" size={14} />
            <span key={String(copied)} className="swap">
              {copied ? t("last.copied") : t("last.copy")}
            </span>
          </button>
        )}
        {state.retry && (
          <button
            type="button"
            className="btn"
            disabled={retry.pending}
            onClick={() => {
              follow(() => title.current);
              retry.retry();
            }}
          >
            {t("dictation.retry")}
          </button>
        )}
        {state.manageUsage && (
          <button type="button" className="btn" onClick={openChatGptUsage}>
            {t("chatgpt.manage_usage")}
          </button>
        )}
        {state.configure && (
          <button type="button" className="btn" onClick={() => navigate("models")}>
            {t("dictation.configure")}
          </button>
        )}
        <div className="hero-keys">
          <KeyCaps hotkey={settings.hotkey} large pressed={holds && recording} tap={tap} />
          <span className="caption">{state.keyNote}</span>
        </div>
      </div>
    </Card>
  );
}
