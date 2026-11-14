// The quick panel next to the tray icon (src-tauri/src/tray_panel.rs), top to bottom: the state
// and the main hotkey, "Recognize again", the mode of the main hotkey, the microphone and the
// model, the meeting recording, the day's statistics, the last text, the recent dictations and
// a new version when there is one. Everything else is in the full window.
import React, { useCallback, useEffect, useMemo, useRef, useState } from "react";
import ReactDOM from "react-dom/client";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { InputDevice } from "./App";
import { initialView, IS_MAC, phase, reduce, useLevel, useRetry } from "./dictation";
import { CORE_EVENT, resultText, useEvent, type CoreEvent, type DictationResult } from "./events";
import { initI18n, pickLanguage, startFailure } from "./i18n";
import { useMeetingRecording } from "./meetings";
import { UpdateCard } from "./components/UpdateCard";
import { clock, microphoneLostText, microphoneOptions, modeName, saveErrorText, useStatus } from "./present";
import { meetingFailureText, panelRisesIn, QUICK_RECENT, quickHead, quickModels, recentTexts, retryLine } from "./quick";
import { formatDuration, savedSeconds, useStats } from "./stats";
import { applyTheme, choosableModes, loadSettings, resolveLanguage, saveSettings, savesInOrder, SETTINGS_EVENT, type Settings } from "./settings";
import { clipTitle, Icon, KeyCaps, readOnFocus, Select, useCopied, useFresh, useLevelStrip, useRoving, useThumb } from "./ui";
import type { CloudEngineInfo } from "./components/CloudPanel";
import { useHistory } from "./components/HistoryPanel";
import type { ModelInfo } from "./components/ModelsPanel";
import "./App.css";
import "./tray.css";

const BARS = 22;

/// The panel rises from the tray icon and its blocks show one after another, with the motion
/// tokens of App.css; on macOS it is there at once, as a menu of the menu bar (`panelRisesIn`).
function riseIn(panel: HTMLElement) {
  if (!panelRisesIn(IS_MAC, window.matchMedia("(prefers-reduced-motion: reduce)").matches)) return;
  const tokens = getComputedStyle(document.documentElement);
  const ms = (name: string) => parseFloat(tokens.getPropertyValue(name)) || 0;
  const easing = tokens.getPropertyValue("--ease-out").trim() || "ease-out";
  const rise = (by: number) => [
    { opacity: 0, transform: `translateY(${by}px)` },
    { opacity: 1, transform: "none" },
  ];
  panel.animate(rise(10), { duration: ms("--d3"), easing });
  Array.from(panel.children).forEach((block, index) => {
    block.animate(rise(6), { duration: ms("--d2"), easing, delay: 40 + index * 30, fill: "backwards" });
  });
}

/// The input level under the state. Only this block draws it, every 80 ms while recording, so the
/// rest of the panel does not draw with it.
function QuickLevel({ recording }: { recording: boolean }) {
  const level = useLevel();
  const levels = useLevelStrip(level, recording, BARS);
  return (
    <div className="quick-level" aria-hidden="true">
      {levels.map((level, index) => (
        <span key={index} className={recording ? "live" : ""} style={{ height: 4 + 14 * Math.sqrt(level) }} />
      ))}
    </div>
  );
}

function QuickPanel({
  initialSettings,
  initialLast,
  initialRetry,
}: {
  initialSettings: Settings;
  initialLast: DictationResult | null;
  initialRetry: boolean;
}) {
  const { t, i18n } = useTranslation();
  const [settings, setSettings] = useState(initialSettings);
  const latest = useRef(settings);
  const [view, setView] = useState(() => initialView(initialLast, initialRetry));
  const [devices, setDevices] = useState<InputDevice[]>([]);
  const [models, setModels] = useState<ModelInfo[]>([]);
  // Whether the main shortcut is registered and the engine there: the head says why not. Read at
  // every opening and after every save, such as a model chosen here (`useStatus`).
  const { status, read: readStatus } = useStatus(null);
  const [cloud, setCloud] = useState<CloudEngineInfo[]>([]);
  // Keyed by the text: what was said of one copy goes with the next dictation's text. "Copied"
  // goes back to "Copy" after 2 s, as in the main window.
  const [copiedText, copyText, refusedText, resetCopies] = useCopied<string>();
  // Only the dictations the panel shows (`QUICK_RECENT`), not the whole history.
  const { history } = useHistory(settings.history_limit, QUICK_RECENT);
  const stats = useStats();
  const meeting = useMeetingRecording();
  const reloadMeeting = meeting.reload;
  const [meetingFailed, setMeetingFailed] = useState<{ action: "start" | "stop"; failure: unknown } | null>(null);
  // Why the last choice in the panel was not saved.
  const [saveFailed, setSaveFailed] = useState<string | null>(null);
  const retry = useRetry(view.retry);
  const resetRetry = retry.reset;
  const panel = useRef<HTMLElement>(null);
  const current = phase(view);

  useEvent<CoreEvent>(CORE_EVENT, (event) => setView((state) => reduce(state, event)));
  useEvent<Settings>(SETTINGS_EVENT, (saved) => {
    latest.current = saved;
    setSettings(saved);
  });

  useEffect(() => {
    applyTheme(settings.theme);
  }, [settings.theme]);

  // Devices and downloads change while the panel is hidden: refresh on every opening. The panel
  // rises in each time, its blocks one after another.
  const [opened, setOpened] = useState(0);
  const refresh = useCallback(() => {
    invoke<InputDevice[]>("list_input_devices").then(setDevices, () => setDevices([]));
    invoke<ModelInfo[]>("list_models").then(setModels, () => setModels([]));
    invoke<CloudEngineInfo[]>("list_cloud_engines").then(setCloud, () => setCloud([]));
    readStatus();
    resetCopies();
    setMeetingFailed(null);
    setSaveFailed(null);
    resetRetry();
    // Asked at every opening too, so the panel never offers a recording that is gone.
    invoke<boolean>("retry_available").then((available) => setView((state) => ({ ...state, retry: available })), () => {});
    reloadMeeting();
    setOpened((count) => count + 1);
    if (panel.current) riseIn(panel.current);
  }, [readStatus, resetCopies]);
  useEffect(() => readOnFocus(refresh), [refresh]);

  // The window takes the panel's height, so nothing transparent catches clicks.
  useEffect(() => {
    const element = panel.current;
    if (!element) return;
    // With the panel's margins (`tray.css`: 16 px, on macOS 1 px all around).
    const report = () => {
      const style = getComputedStyle(element);
      const height = element.getBoundingClientRect().height + parseFloat(style.marginTop) + parseFloat(style.marginBottom);
      void invoke("fit_tray_panel", { height }).catch(() => {});
    };
    const observer = new ResizeObserver(report);
    observer.observe(element);
    report();
    return () => observer.disconnect();
  }, []);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") void invoke("hide_tray_panel").catch(() => {});
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  // A refused choice stays as it was saved, and the panel says why.
  const translate = useRef(t);
  translate.current = t;
  const update = useMemo(
    () =>
      savesInOrder(
        saveSettings,
        latest,
        (saved) => {
          setSettings(saved);
          setSaveFailed(null);
        },
        (error) => setSaveFailed(saveErrorText(translate.current, error)),
      ),
    [],
  );

  const choices = useMemo(
    () => quickModels(models, cloud, settings.connections, settings.model_id),
    [models, cloud, settings.connections, settings.model_id],
  );
  const chosen = choices.find((item) => item.id === settings.model_id) ?? null;
  const recent = history ? recentTexts(history.entries, QUICK_RECENT) : [];
  const fresh = useFresh(
    recent.map((entry) => entry.id),
    history !== null,
  );
  const modes = useRef<HTMLDivElement>(null);
  const modeClip = useThumb(modes, '.chip[aria-checked="true"]', settings.main_mode_id, opened, "clip");
  const last = view.last ? resultText(view.last) : null;
  const copied = last !== null && copiedText === last;
  const copyFailed = last !== null && refusedText === last;
  const retryShown = retryLine(t, view);
  const microphoneLost = microphoneLostText(t, view);
  const time = (ms: number) => new Date(ms).toLocaleTimeString(i18n.language, { hour: "2-digit", minute: "2-digit" });

  const head = quickHead(t, current, { chosen: chosen !== null, status, hotkey: settings.hotkey, recordingMode: settings.recording_mode });
  const device = settings.microphone_id ?? "";
  const choosable = choosableModes(settings.modes);
  const roving = useRoving(choosable.map((mode) => ({ checked: mode.id === settings.main_mode_id })));

  // Start: the microphone of the settings and the computer's sound; stop: kept and transcribed.
  // A press waits for its command: a second press would find nothing to stop.
  const [meetingPending, setMeetingPending] = useState(false);
  async function toggleMeeting() {
    const action = meeting.recording === null ? "start" : "stop";
    setMeetingPending(true);
    try {
      await invoke(`${action}_meeting`);
      setMeetingFailed(null);
    } catch (failure) {
      setMeetingFailed({ action, failure });
    }
    setMeetingPending(false);
    meeting.reload();
  }

  async function copy() {
    if (!last) return;
    await copyText(last, last);
  }

  return (
    <main className="quick" ref={panel} aria-label={t("quick.label")}>
      <header className="quick-header">
        <span className="quick-brand">ahem</span>
      </header>

      <section className="quick-card quick-state" aria-live="polite">
        <div className="quick-state-row">
          <div className="quick-state-texts">
            <span className="quick-title">
              <span className={head.live ? "quick-dot live" : "quick-dot"} />
              {head.title}
            </span>
            <span className="caption">{head.hint}</span>
            {microphoneLost && <span className="caption warning-text">{microphoneLost}</span>}
          </div>
          <KeyCaps hotkey={settings.hotkey} large />
        </div>
        <QuickLevel recording={current === "recording"} />
      </section>

      {retryShown && (
        <section className="quick-card quick-retry" aria-live="polite">
          <span className={retry.failed || retryShown.warning ? "caption warning-text" : "caption"}>
            {retry.failed ? t("dictation.retry_failed") : retryShown.text}
          </span>
          {retryShown.retry && (
            <button type="button" className="btn compact" disabled={retry.pending} onClick={retry.retry}>
              {t("dictation.retry")}
            </button>
          )}
        </section>
      )}

      {/* The chosen look is a copy of the chips laid over them; its clip slides to the chosen
          mode, so the pill and its text move together. */}
      <div className="quick-modes-wrap">
        <div ref={modes} className="quick-modes" role="radiogroup" aria-label={t("dictation.main_mode")} {...roving.group}>
          {choosable.map((mode, index) => (
            <button
              key={mode.id}
              type="button"
              role="radio"
              aria-checked={mode.id === settings.main_mode_id}
              tabIndex={roving.tabIndex(index)}
              className="chip"
              {...clipTitle(modeName(t, mode))}
              onClick={() => update({ main_mode_id: mode.id })}
            >
              <span className="chip-name">{modeName(t, mode)}</span>
            </button>
          ))}
        </div>
        <div className="quick-modes quick-modes-chosen" style={modeClip} aria-hidden="true">
          {choosable.map((mode) => (
            <span key={mode.id} className="chip">
              <span className="chip-name">{modeName(t, mode)}</span>
            </span>
          ))}
        </div>
      </div>

      <section className="quick-card quick-rows">
        <label className="quick-row">
          <span className="caption">{t("quick.microphone")}</span>
          <Select
            label={t("quick.microphone")}
            value={device}
            options={microphoneOptions(t, devices, settings.microphone_id)}
            onChange={(value) => update({ microphone_id: value || null })}
          />
        </label>
        <label className="quick-row">
          <span className="caption">{t("quick.model")}</span>
          <Select
            label={t("quick.model")}
            value={settings.model_id ?? ""}
            options={[
              ...(chosen ? [] : [{ value: "", label: t("quick.no_model"), disabled: true }]),
              ...choices.map((item) => ({ value: item.id, label: item.name })),
            ]}
            onChange={(value) => value && update({ model_id: value })}
          />
        </label>
        {saveFailed && <span className="caption warning-text">{saveFailed}</span>}
      </section>

      <section className="quick-card quick-meeting">
        <span className="quick-meeting-text">
          {meeting.recording !== null && <span className="quick-dot live" />}
          <span className={meeting.recording !== null ? "caption quick-meeting-time" : "caption"}>
            {meeting.recording !== null ? `${t("quick.meeting_recording")} · ${clock(meeting.elapsed)}` : t("quick.meeting")}
          </span>
        </span>
        <button type="button" className={meeting.recording !== null ? "btn compact primary" : "btn compact"} disabled={meetingPending} onClick={toggleMeeting}>
          {meeting.recording !== null ? t("quick.meeting_stop") : t("quick.meeting_start")}
        </button>
        {meetingFailed && (
          <span className="caption warning-text quick-meeting-failure">{meetingFailureText(t, meetingFailed.action, meetingFailed.failure)}</span>
        )}
      </section>

      {stats && stats.today.dictations > 0 && (
        <p className="caption quick-stats">
          {t("quick.stats", {
            time: formatDuration(t, savedSeconds(stats.today)),
            words: new Intl.NumberFormat(i18n.language).format(stats.today.words),
          })}
        </p>
      )}

      {last && (
        <section className="quick-card quick-last">
          <div className="quick-last-head">
            <span className="key-meta">{t("quick.last")}</span>
            <button type="button" className={copied ? "btn compact primary copy-button copied" : "btn compact primary copy-button"} onClick={copy}>
              <Icon name="check" size={14} tone="text" />
              <span key={String(copied)} className="swap">
                {copied ? t("last.copied") : t("last.copy")}
              </span>
            </button>
          </div>
          <p className="selectable">{last}</p>
          {copyFailed && <span className="caption warning-text">{t("last.copy_failed")}</span>}
        </section>
      )}

      {recent.length > 0 && (
        <ul className="quick-recent" aria-label={t("quick.recent")}>
          {recent.map((entry) => (
            <li key={entry.id} className={fresh.has(entry.id) ? "fresh" : undefined}>
              <span className="key-meta">{time(entry.time)}</span>
              <span className="quick-recent-text">{entry.text}</span>
            </li>
          ))}
        </ul>
      )}

      <UpdateCard variant="quick" />

      <footer className="quick-footer">
        <button type="button" className="btn primary" onClick={() => void invoke("open_main_window").catch(() => {})}>
          {t("quick.open")}
        </button>
        <button type="button" className="btn" onClick={() => void invoke("quit_app").catch(() => {})}>
          {t("quick.quit")}
        </button>
      </footer>
    </main>
  );
}

// macOS hangs the panel from the menu bar like its menus (`tray.css`).
if (IS_MAC) document.documentElement.dataset.os = "mac";

async function start() {
  const [locales, settings, last, retry] = await Promise.all([
    invoke<string[]>("system_locales"),
    loadSettings(),
    invoke<DictationResult | null>("get_last_result"),
    invoke<boolean>("retry_available").catch(() => false),
  ]);
  const systemLanguage = pickLanguage(locales);
  const i18n = await initI18n(resolveLanguage(settings.ui_language, systemLanguage));
  listen<Settings>(SETTINGS_EVENT, ({ payload }) => {
    void i18n.changeLanguage(resolveLanguage(payload.ui_language, systemLanguage));
  });
  ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
    <React.StrictMode>
      <QuickPanel initialSettings={settings} initialLast={last} initialRetry={retry} />
    </React.StrictMode>,
  );
}

// A failed start shows why instead of an empty window.
start().catch((error) => {
  document.body.textContent = startFailure(error, navigator.languages);
});
