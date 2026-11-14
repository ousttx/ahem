import React from "react";
import ReactDOM from "react-dom/client";
import { invoke } from "@tauri-apps/api/core";
import App from "./App";
import type { Status } from "./present";
import type { DictationResult } from "./events";
import { initI18n, pickLanguage, startFailure } from "./i18n";
import { loadSettings, resolveLanguage } from "./settings";

async function start() {
  // Only this window records hotkeys (its hotkey fields): a recording its page did not end, as
  // when the page reloaded meanwhile, left the hotkeys paused. They come back before the status
  // is read.
  await invoke("set_hotkey_suspended", { suspended: false }).catch(() => {});
  const [locales, settings, last, status, retry] = await Promise.all([
    invoke<string[]>("system_locales"),
    loadSettings(),
    invoke<DictationResult | null>("get_last_result"),
    invoke<Status>("get_status"),
    // Not worth an empty window: without it the button waits for the next event.
    invoke<boolean>("retry_available").catch(() => false),
  ]);
  const systemLanguage = pickLanguage(locales);
  await initI18n(resolveLanguage(settings.ui_language, systemLanguage));
  ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
    <React.StrictMode>
      <App systemLanguage={systemLanguage} initialSettings={settings} initialLast={last} initialRetry={retry} initialStatus={status} />
    </React.StrictMode>,
  );
}

// A failed start shows why instead of an empty window.
start().catch((error) => {
  document.body.textContent = startFailure(error, navigator.languages);
});
