import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { forgetEngineFailure, formatHotkey, initialView, IS_MAC, reduce } from "./dictation";
import { CORE_EVENT, useEvent, type CoreEvent, type DictationResult } from "./events";
import type { Language } from "./i18n";
import { canRecognize, downloadBadge, engineStatus, ownMessages, saveErrorText, useStatus, type Message, type Status } from "./present";
import { applyTheme, saveSettings, savesInOrder, SETTINGS_EVENT, type Settings, type Update } from "./settings";
import { EngineStatus, Icon, InfoBar, readOnFocus, shownTitle, useFocusAfter, useThumb, useWindowSeen } from "./ui";
import { DictationPage } from "./components/DictationPage";
import { TranscriptsPanel } from "./components/TranscriptsPanel";
import { UpdateCard } from "./components/UpdateCard";
import { SPEAKER_MODELS } from "./meetings";
import { dragWindowByHand } from "./windowDrag";
import { ModesPanel } from "./components/ModesPanel";
import { DictionaryPage, ModelsPage, ServicesPage, SettingsPanel, type SettingsTab } from "./components/SettingsPanel";
import { HistoryPanel } from "./components/HistoryPanel";
import { TitleBar } from "./components/TitleBar";
import { useModels } from "./components/ModelsPanel";
import { useCloudEngines } from "./components/CloudPanel";
import "./App.css";

interface Props {
  systemLanguage: Language;
  initialSettings: Settings;
  initialLast: DictationResult | null;
  /// A recording that was not recognized is kept to be recognized again.
  initialRetry: boolean;
  initialStatus: Status;
}

type Page = "dictation" | "models" | "dictionary" | "modes" | "services" | "transcripts" | "history" | "settings";

/// Where a link inside a page leads: a page, a tab of the settings, or a mode on the Modes page.
export type Target = Page | `settings:${SettingsTab}` | `mode:${string}`;

export interface InputDevice {
  id: string;
  name: string;
  default: boolean;
}

/// Two groups: what the user does, then the setup: modes, dictionary, services, models.
export const NAVIGATION: { group: "work" | "setup"; pages: { page: Page; icon: string }[] }[] = [
  {
    group: "work",
    pages: [
      { page: "dictation", icon: "microphone" },
      // Meetings and files in one list.
      { page: "transcripts", icon: "wave" },
      { page: "history", icon: "clock" },
    ],
  },
  {
    group: "setup",
    pages: [
      { page: "modes", icon: "sparkle" },
      { page: "dictionary", icon: "book" },
      { page: "services", icon: "key" },
      { page: "models", icon: "chip" },
    ],
  },
];

function App({ systemLanguage, initialSettings, initialLast, initialRetry, initialStatus }: Props) {
  const { t } = useTranslation();
  const [settings, setSettings] = useState(initialSettings);
  // Read again after every save and after the paused hotkeys came back (`useStatus`).
  const { status } = useStatus(initialStatus);
  const [view, setView] = useState(() => initialView(initialLast, initialRetry));
  const [page, setPage] = useState<Page>("dictation");
  // The window may be hidden in the tray with any page open.
  const seen = useWindowSeen();
  // Counts the clicks on "Transcripts" in the menu while it is open: the page goes back to its list.
  const [transcriptsHome, setTranscriptsHome] = useState(0);
  const openPage = useCallback(
    (chosen: Page) => {
      setTranscriptsHome((count) => nextTranscriptsHome(page, chosen, count));
      setPage(chosen);
    },
    [page],
  );
  const [settingsTab, setSettingsTab] = useState<SettingsTab>("general");
  // The mode a link asked to open on the Modes page.
  const [modeFocus, setModeFocus] = useState<{ id: string } | null>(null);
  const [message, setMessage] = useState<Message | null>(null);
  const [dismissed, setDismissed] = useState<string[]>([]);
  const [devices, setDevices] = useState<InputDevice[]>([]);
  const latest = useRef(settings);

  useEvent<CoreEvent>(CORE_EVENT, (event) => setView((current) => reduce(current, event)));
  // The settings as saved: by every save that went through, here or elsewhere (the mode chosen in
  // the tray menu, a connection command), and as connection commands return them, having saved
  // them themselves; the status is read on their `SETTINGS_EVENT`.
  const replaceSettings = useCallback((saved: Settings) => {
    latest.current = saved;
    setSettings(saved);
  }, []);
  useEvent<Settings>(SETTINGS_EVENT, replaceSettings);

  const refreshDevices = useCallback(() => {
    invoke<InputDevice[]>("list_input_devices").then(setDevices, () => setDevices([]));
  }, []);
  // A microphone plugged in or out while the window was in the background shows when it is back.
  useEffect(() => readOnFocus(refreshDevices), [refreshDevices]);

  useEffect(() => applyTheme(settings.theme), [settings.theme]);
  // macOS: the window is dragged by hand (`windowDrag.ts`); the system's own drag does nothing.
  useEffect(() => (IS_MAC ? dragWindowByHand() : undefined), []);

  // A good save clears only the error of the save before it, not another message.
  const saveMessages = useMemo(() => ownMessages(setMessage), []);
  const translate = useRef(t);
  translate.current = t;
  const update: Update = useMemo(
    () =>
      savesInOrder(
        saveSettings,
        latest,
        (saved) => {
          setSettings(saved);
          saveMessages.succeed();
        },
        (error) => saveMessages.fail(saveErrorText(translate.current, error)),
      ),
    [saveMessages],
  );

  const chooseModel = useCallback((id: string) => void update({ model_id: id }), [update]);
  // The speaker models, for the transcripts of meetings and files, are not speech models.
  const modelDownloads = useMemo(() => {
    const { [SPEAKER_MODELS]: _speakers, ...rest } = view.downloads;
    return rest;
  }, [view.downloads]);
  const { models, reload: reloadModels, settling } = useModels(settings.model_id, modelDownloads, chooseModel);
  const cloud = useCloudEngines();
  // The dictation engine: a model on this computer or a cloud service.
  const engines = useMemo(() => [...models, ...cloud], [models, cloud]);
  const model = engines.find((item) => item.id === settings.model_id) ?? null;
  const modelName = model?.name ?? null;
  const cloudEngine = cloud.find((item) => item.id === settings.model_id) ?? null;
  const inCloud = cloudEngine !== null;
  const usable = canRecognize(settings.model_id, models, cloud, settings.connections);
  // An engine that failed was the model's chosen before: another one has not failed yet.
  useEffect(() => setView((current) => forgetEngineFailure(current)), [settings.model_id]);

  // A link moves the focus to the title of the page it opens: the link itself is on a page that
  // is hidden now. So does a new screen within a page (`focusTitle`), once it is drawn.
  const shownPage = useRef(page);
  shownPage.current = page;
  const content = useRef<HTMLElement>(null);
  const [titleFocus, setTitleFocus] = useState(0);
  const focusTitle = useCallback(() => setTitleFocus((count) => count + 1), []);
  const navigate = useCallback(
    (target: Target) => {
      const [next, tab, mode] = targetPage(target);
      if (tab) setSettingsTab(tab);
      // A new object each time: the same mode opens again after the user chose another.
      if (mode) setModeFocus({ id: mode });
      if (linkMovesFocus(shownPage.current, target)) focusTitle();
      setPage(next);
    },
    [focusTitle],
  );
  useEffect(() => {
    if (titleFocus === 0 || !content.current) return;
    shownTitle(content.current)?.focus({ preventScroll: true });
  }, [titleFocus]);
  // The screens of Transcripts share its scrolling container.
  const transcriptsScroll = useRef<HTMLDivElement>(null);
  const transcriptsTop = useCallback(() => {
    if (transcriptsScroll.current) transcriptsScroll.current.scrollTop = 0;
  }, []);

  const warnings = useMemo(() => {
    const others = status.unregistered_hotkeys.filter((other) => other !== settings.hotkey);
    return [
      status.settings_error && t("status.settings_read_only", { detail: status.settings_error }),
      !status.hotkey_registered && t("status.hotkey_missing", { hotkey: formatHotkey(settings.hotkey) }),
      others.length > 0 && t("status.hotkeys_missing", { hotkeys: others.map((hotkey) => formatHotkey(hotkey)).join(", ") }),
      !status.engine_available && t("status.engine_missing"),
    ].filter((warning): warning is string => Boolean(warning) && !dismissed.includes(warning as string));
  }, [status, settings.hotkey, dismissed, t]);

  const nav = useRef<HTMLElement>(null);
  const thumb = useThumb(nav, ".nav-item.active", page);
  // The last message closed takes the bar and its close button away: the focus goes to the
  // title of the page shown.
  const follow = useFocusAfter();

  const shown: Message | null = message ?? (warnings.length > 0 ? { text: warnings[0], error: true } : null);
  const engine = engineStatus(t, { engine: view.engine, modelName, available: status.engine_available, cloud: inCloud });
  // The Models page holds the speaker models too.
  const downloading = downloadBadge(view.downloads);

  return (
    <div className="window">
      <nav ref={nav} className={IS_MAC ? "nav-pane nav-pane-mac" : "nav-pane"} aria-label={t("nav.label")}>
        {/* The pill of the open section slides to the one chosen. */}
        <span className="nav-thumb" style={thumb} aria-hidden="true" />
        {/* The window is dragged by its top; on macOS the system window buttons stand above the brand. */}
        {IS_MAC && <span className="nav-top" data-tauri-drag-region />}
        <span className="nav-brand" data-tauri-drag-region>
          ahem
        </span>
        {NAVIGATION.map(({ group, pages }) => (
          <div key={group} className="nav-group" role="group" aria-labelledby={`nav-group-${group}`}>
            <span id={`nav-group-${group}`} className="nav-group-label">
              {t(`nav.group.${group}`)}
            </span>
            {pages.map((item) => (
              <NavItem
                key={item.page}
                icon={item.icon}
                label={t(`nav.${item.page}`)}
                active={page === item.page}
                badge={item.page === "models" ? downloading : null}
                onClick={() => openPage(item.page)}
              />
            ))}
          </div>
        ))}
        <span className="nav-spacer" />
        <UpdateCard variant="nav" />
        <EngineStatus {...engine} />
        <NavItem icon="gear" label={t("nav.settings")} active={page === "settings"} onClick={() => setPage("settings")} />
      </nav>
      <main ref={content} className="content-layer">
        <TitleBar />
        {shown && (
          <InfoBar
            text={shown.text}
            error={shown.error}
            closeLabel={t("info.close")}
            onClose={() => {
              follow(() => (content.current ? shownTitle(content.current) : null));
              if (message) setMessage(null);
              else setDismissed((list) => [...list, shown.text]);
            }}
          />
        )}
        <div className="page-scroll" hidden={page !== "dictation"}>
          <DictationPage
            view={view}
            settings={settings}
            status={status}
            modelName={modelName}
            usable={usable}
            inCloud={inCloud}
            translates={model?.translate ?? null}
            devices={devices}
            update={update}
            navigate={navigate}
          />
        </div>
        <div className="page-scroll" hidden={page !== "models"}>
          <ModelsPage
            speakerDownload={view.downloads[SPEAKER_MODELS]}
            settings={settings}
            models={models}
            cloud={cloud}
            reloadModels={reloadModels}
            downloads={modelDownloads}
            settling={settling}
            update={update}
            onSettings={replaceSettings}
            onOpenServices={() => navigate("services")}
          />
        </div>
        <div className="page-scroll" hidden={page !== "dictionary"}>
          <DictionaryPage settings={settings} models={models} cloud={cloud} update={update} />
        </div>
        <div className="page-scroll" hidden={page !== "services"}>
          <ServicesPage settings={settings} onSettings={replaceSettings} active={page === "services"} />
        </div>
        <div className="page-scroll" hidden={page !== "history"}>
          <div className="page">
            <h1 className="page-title" tabIndex={-1}>
              {t("nav.history")}
            </h1>
            <HistoryPanel settings={settings} update={update} shown={page === "history" && seen} />
          </div>
        </div>
        <div ref={transcriptsScroll} className="page-scroll" hidden={page !== "transcripts"}>
          <TranscriptsPanel
            settings={settings}
            update={update}
            file={view.file}
            modelName={modelName}
            inCloud={inCloud}
            mediaFiles={cloudEngine?.files ?? true}
            translates={model?.translate ?? null}
            active={page === "transcripts"}
            home={transcriptsHome}
            focusTitle={focusTitle}
            toTop={transcriptsTop}
            navigate={navigate}
            setMessage={setMessage}
          />
        </div>
        <div className="page-scroll" hidden={page !== "modes"}>
          <ModesPanel
            settings={settings}
            models={engines}
            model={model}
            focus={modeFocus}
            update={update}
            onSettings={replaceSettings}
            navigate={navigate}
          />
        </div>
        <div className="page-scroll" hidden={page !== "settings"}>
          <SettingsPanel
            tab={settingsTab}
            onTab={setSettingsTab}
            settings={settings}
            systemLanguage={systemLanguage}
            devices={devices}
            refreshDevices={refreshDevices}
            update={update}
            navigate={navigate}
          />
        </div>
      </main>
    </div>
  );
}

function NavItem({
  icon,
  label,
  active,
  badge = null,
  onClick,
}: {
  icon: string;
  label: string;
  active: boolean;
  /// Short live state next to the label, such as a download's percent.
  badge?: string | null;
  onClick: () => void;
}) {
  return (
    <button type="button" className={active ? "nav-item active" : "nav-item"} aria-current={active ? "page" : undefined} onClick={onClick}>
      <Icon name={icon} size={18} />
      <span className="nav-label">{label}</span>
      {badge && <span className="nav-badge">{badge}</span>}
    </button>
  );
}

/// A click on "Transcripts" in the menu while that page is open counts once more; any other
/// click does not.
export function nextTranscriptsHome(page: Page, chosen: Page, home: number): number {
  return page === "transcripts" && chosen === "transcripts" ? home + 1 : home;
}

/// Whether a link on `page` to `target` moves the focus to the title of another page.
export function linkMovesFocus(page: Page, target: Target): boolean {
  return targetPage(target)[0] !== page;
}

/// The page a link leads to, the settings tab when it is one, and the mode to open.
export function targetPage(target: Target): [Page, SettingsTab | null, string | null] {
  if (target.startsWith("mode:")) return ["modes", null, target.slice("mode:".length)];
  if (!target.startsWith("settings:")) return [target as Page, null, null];
  return ["settings", target.slice("settings:".length) as SettingsTab, null];
}

export default App;
