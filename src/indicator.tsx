// The indicator window: it never takes focus and the backend moves it on and off the screen
// (src-tauri/src/indicator.rs); this page only draws the state from `core-event`s. The look is
// a capsule in the window's theme: the level and the time while recording, the state while busy,
// a check when the text is inserted or recognized again, and the text of a problem, also of one
// beside a text that went in (the microphone went away midway, the history was not saved). While
// recording it also names the mode when a rule for the program in focus chose it instead of the
// main one. It grows in from a point, its width flows between the states, and it fades just
// before the backend moves it away.
import React, { useLayoutEffect, useRef, useState } from "react";
import ReactDOM from "react-dom/client";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { initialView, phase, reduce, useLevel } from "./dictation";
import { CORE_EVENT, useEvent, type CoreEvent } from "./events";
import { initI18n, pickLanguage, startFailure } from "./i18n";
import { capsule, capsuleWarning, clock, meter, modeName, newCapsuleVisit, type CapsuleShown } from "./present";
import { applyTheme, loadSettings, resolveLanguage, SETTINGS_EVENT, type Settings } from "./settings";
import { useElapsed, useLevelStrip } from "./ui";
import "./indicator.css";

const BARS = 20;
/// Bar heights in CSS pixels; 2.5 is 3 pixels at the 1.2 zoom of Windows, so the bars stay sharp.
const BAR_MIN = 2.5;
const BAR_MAX = 20;

function Indicator({ settings: initial }: { settings: Settings }) {
  const { t } = useTranslation();
  const [settings, setSettings] = useState(initial);
  useEvent<Settings>(SETTINGS_EVENT, setSettings);
  const [view, setView] = useState(() => initialView(null));
  const current = phase(view);
  const level = useLevel();
  const levels = useLevelStrip(level, current === "recording", BARS);

  useEvent<CoreEvent>(CORE_EVENT, (event) => setView((state) => reduce(state, event)));

  // A dictation that starts after the capsule was idle, or a new notice while idle that the
  // backend shows the window for, is a new visit: the capsule grows in (`newCapsuleVisit`).
  // Before the paint, so the old capsule never shows in the new state.
  const [visit, setVisit] = useState(0);
  const shownBefore = useRef<CapsuleShown>({ idle: true, notice: null });
  useLayoutEffect(() => {
    const now = { idle: current === "idle", notice: view.notice };
    if (newCapsuleVisit(shownBefore.current, now)) setVisit((count) => count + 1);
    shownBefore.current = now;
  }, [current, view.notice]);

  const elapsed = useElapsed(current === "recording", 250);

  const shown = capsule(t, current, view.notice, capsuleWarning(t, view));
  if (!shown) return null;
  const { state, text } = shown;
  const chosen = state === "recording" ? settings.modes.find((mode) => mode.id === view.modeByApp) : undefined;
  const withContext = state === "recording" && view.contextTaken;
  const label = chosen ? modeName(t, chosen) : shown.label;
  // Every part is always there and only folds, so the capsule's width flows between states.
  const part = (name: string, on: boolean) => `part part-${name}${on ? " on" : ""}`;
  const working = state === "recording" || state === "busy";
  return (
    // A new key for every visit: the capsule grows in again.
    <div key={visit} className={`pill ${state}${text ? " with-text" : ""}`} role="status" aria-label={withContext ? `${label}, ${t("indicator.context")}` : label}>
      <span className={part("check", state === "success")}>
        <svg className="check" width="16" height="12" viewBox="0 0 17 12" aria-hidden="true">
          <path pathLength={1} d="M1.5 6l5 5 9-9.5" />
        </svg>
      </span>
      <span className={part("dot", state === "problem" || state === "quiet")}>
        <span className="dot" />
      </span>
      <span className={part("bars", working)} aria-hidden="true">
        <span className="bars">
          {levels.map((level, index) => (
            <span
              key={index}
              style={
                state === "recording"
                  ? { height: BAR_MIN + (BAR_MAX - BAR_MIN) * meter(level) }
                  : { animationDelay: `${-index * 0.125}s` }
              }
            />
          ))}
        </span>
      </span>
      <span className={part("clip", withContext)} title={t("indicator.context")}>
        <svg className="clip" width="13" height="15" viewBox="0 0 13 15" aria-hidden="true">
          <path d="M11 7.2 6.6 11.6a2.9 2.9 0 0 1-4.1-4.1l4.6-4.6a1.9 1.9 0 0 1 2.7 2.7L5.3 10a.9.9 0 0 1-1.3-1.3l4.1-4.1" />
        </svg>
      </span>
      <span className={part("clock", state === "recording")}>
        <span className="clock">{clock(elapsed, true)}</span>
      </span>
      <span className={part("label", state === "busy" || state === "success" || chosen !== undefined)}>
        <span key={label} className="swap">
          {label}
        </span>
      </span>
      <span className={part("text", text !== null)}>
        <span className="text">{text}</span>
      </span>
    </div>
  );
}

async function start() {
  const [locales, settings] = await Promise.all([invoke<string[]>("system_locales"), loadSettings()]);
  const systemLanguage = pickLanguage(locales);
  applyTheme(settings.theme);
  const i18n = await initI18n(resolveLanguage(settings.ui_language, systemLanguage));
  // The language can change in the main window.
  listen<Settings>(SETTINGS_EVENT, ({ payload }) => {
    void i18n.changeLanguage(resolveLanguage(payload.ui_language, systemLanguage));
    applyTheme(payload.theme);
  });
  ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
    <React.StrictMode>
      <Indicator settings={settings} />
    </React.StrictMode>,
  );
}

// A failed start shows why instead of an empty window.
start().catch((error) => {
  document.body.textContent = startFailure(error, navigator.languages);
});
