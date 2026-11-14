import { useEffect, useId, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { getVersion } from "@tauri-apps/api/app";
import { openUrl } from "@tauri-apps/plugin-opener";
import type { InputDevice, Target } from "../App";
import type { Download } from "../dictation";
import { supportedLanguages, type Language } from "../i18n";
import { spokenLanguageChoices, useLanguageOptions } from "../languages";
import { dictionaryNotes, microphoneOptions, modeName, recognitionNote } from "../present";
import {
  ENGLISH_MODE,
  MAX_DICTIONARY_CHARS,
  resolveLanguage,
  saveText,
  withModeChange,
  type Mode,
  type RecordingMode,
  type SaveError,
  type Settings,
  type Theme,
  type UiLanguage,
  type UnloadPolicy,
  type Update,
} from "../settings";
import { UPDATES_READY } from "../update";
import { HELP_URL, linkTarget, PREVIEW_LINKS, SUPPORT_URL } from "../links";
import { shortcuts, type Shortcut } from "../shortcuts";
import { Card, Row, Segments, Select, tabPanelId, Tabs, Toggle, useFocusAfter } from "../ui";
import { CloudPanel, type CloudEngineInfo } from "./CloudPanel";
import { ConnectionsPanel } from "./ConnectionsPanel";
import { HotkeyField } from "./HotkeyField";
import { ModelsPanel, type ModelInfo } from "./ModelsPanel";
import { ReplacementsPanel } from "./ReplacementsPanel";
import { SpeakerModelsCard } from "./SpeakerModelsCard";

export type SettingsTab = "general" | "keys" | "microphone";

const TABS: SettingsTab[] = ["general", "keys", "microphone"];

/// The Settings page's three tabs.
interface Props {
  tab: SettingsTab;
  onTab: (tab: SettingsTab) => void;
  /// Links to other pages, such as a mode from the Keys tab.
  navigate: (target: Target) => void;
  settings: Settings;
  systemLanguage: Language;
  devices: InputDevice[];
  refreshDevices: () => void;
  update: Update;
}

/// The Models page.
interface ModelsProps {
  /// The link to the services when a cloud engine has no key.
  onOpenServices: () => void;
  settings: Settings;
  models: ModelInfo[];
  cloud: CloudEngineInfo[];
  reloadModels: () => Promise<void>;
  downloads: Record<string, Download>;
  /// Models whose download ended before the list was read again (`useModels`).
  settling: ReadonlySet<string>;
  update: Update;
  onSettings: (settings: Settings) => void;
}

/// The models: on this computer (with when their memory is freed) and in the cloud, with the
/// spoken language; then the models that tell voices apart in meetings and files.
export function ModelsPage({ speakerDownload, ...props }: ModelsProps & { speakerDownload: Download | undefined }) {
  const { t } = useTranslation();
  return (
    <div className="page settings-page">
      <div className="page-header">
        <div>
          <h1 className="page-title" tabIndex={-1}>{t("nav.models")}</h1>
          <span className="caption">{t("models.page_note")}</span>
        </div>
      </div>
      <div className="tab-page">
        <Recognition {...props} />
        <SpeakerModelsCard download={speakerDownload} />
      </div>
    </div>
  );
}

/// Services: named services with their keys and own servers. `active`: the page is the one shown.
export function ServicesPage({ settings, onSettings, active }: Pick<ModelsProps, "settings" | "onSettings"> & { active: boolean }) {
  const { t } = useTranslation();
  return (
    <div className="page settings-page">
      <h1 className="page-title" tabIndex={-1}>{t("nav.services")}</h1>
      <div className="tab-page">
        <ConnectionsPanel settings={settings} onSettings={onSettings} active={active} />
      </div>
    </div>
  );
}

/// Settings in three tabs: general, every hotkey, and the microphone with the indicator on
/// screen.
export function SettingsPanel(props: Props) {
  const { t } = useTranslation();
  const { tab, onTab, refreshDevices } = props;
  const tabs = useId();

  useEffect(() => {
    if (tab === "microphone") refreshDevices();
  }, [tab, refreshDevices]);

  return (
    <div className="page settings-page">
      <h1 className="page-title" tabIndex={-1}>{t("nav.settings")}</h1>
      <Tabs id={tabs} tabs={TABS.map((value) => ({ value, label: t(`settings.tab.${value}`) }))} value={tab} onChange={onTab} />
      <div className="tab-page" role="tabpanel" id={tabPanelId(tabs)} aria-labelledby={`${tabs}-${tab}`}>
        {tab === "general" && <GeneralTab {...props} />}
        {tab === "keys" && <KeysTab {...props} />}
        {tab === "microphone" && <MicrophoneTab {...props} />}
      </div>
    </div>
  );
}

/// Saves the interface language and shows it only once saved: a refused save keeps the language
/// shown.
export async function chooseLanguage(choice: UiLanguage, update: Update, show: (choice: UiLanguage) => unknown): Promise<void> {
  if (!(await update({ ui_language: choice }))) await show(choice);
}

function GeneralTab({ settings, systemLanguage, update }: Props) {
  const { t, i18n } = useTranslation();
  const [version, setVersion] = useState("");
  useEffect(() => {
    getVersion().then(setVersion, () => {});
  }, []);
  const themes: Theme[] = ["system", "light", "dark"];
  const languages: UiLanguage[] = ["system", ...supportedLanguages];
  // Empty until the site is up (`links.ts`): then the row is not shown (unless previewed).
  const support = linkTarget(SUPPORT_URL, PREVIEW_LINKS);
  const help = linkTarget(HELP_URL, PREVIEW_LINKS);

  return (
    <>
      <Card>
        <Row
          separator={false}
          title={t("settings.theme")}
          control={
            <Segments
              label={t("settings.theme")}
              options={themes.map((value) => ({ value, label: t(`settings.themes.${value}`) }))}
              value={settings.theme}
              onChange={(theme) => update({ theme })}
            />
          }
        />
        <Row
          title={t("language.label")}
          control={
            <Select
              label={t("language.label")}
              width={240}
              value={settings.ui_language}
              options={languages.map((value) => ({
                value,
                label:
                  value === "system"
                    ? t("language.system", { language: i18n.getFixedT(systemLanguage)("language.name") })
                    : i18n.getFixedT(value)("language.name"),
              }))}
              onChange={(value) =>
                void chooseLanguage(value as UiLanguage, update, (choice) => i18n.changeLanguage(resolveLanguage(choice, systemLanguage)))
              }
            />
          }
        />
        <Row
          title={t("settings.launch_at_login")}
          detail={t("settings.launch_at_login_hint")}
          control={
            <Toggle
              label={t("settings.launch_at_login")}
              checked={settings.launch_at_login}
              onChange={(launch_at_login) => update({ launch_at_login })}
            />
          }
        />
      </Card>
      <Card>
        {UPDATES_READY && (
          <>
            <Row
              separator={false}
              title={t("settings.auto_update")}
              detail={t("settings.auto_update_hint")}
              control={<Toggle label={t("settings.auto_update")} checked={settings.auto_update} onChange={(auto_update) => update({ auto_update })} />}
            />
            <Row
              title={t("settings.beta_updates")}
              detail={t("settings.beta_updates_hint")}
              control={<Toggle label={t("settings.beta_updates")} checked={settings.beta_updates} onChange={(beta_updates) => update({ beta_updates })} />}
            />
          </>
        )}
        <Row separator={UPDATES_READY} title={t("settings.version")} control={<span className="caption tabular">{version}</span>} />
        {help && (
          <Row
            title={t("settings.help")}
            detail={t("settings.help_hint")}
            control={
              <button type="button" className="btn compact" onClick={() => void openUrl(help).catch(() => {})}>
                {t("settings.help_open")}
              </button>
            }
          />
        )}
        {support && (
          <Row
            title={t("settings.support")}
            detail={t("settings.support_hint")}
            control={
              <button type="button" className="btn compact" onClick={() => void openUrl(support).catch(() => {})}>
                {t("settings.support_open")}
              </button>
            }
          />
        )}
      </Card>
    </>
  );
}

/// Every hotkey in one list: dictation, then the selected text, then how a hotkey records. The
/// hotkeys of the modes are the same values as in their editors on the Modes page.
function KeysTab({ settings, update, navigate }: Props) {
  const { t } = useTranslation();
  const recordingModes: RecordingMode[] = ["auto", "hold", "toggle"];
  const list = shortcuts(settings);
  const saveMode = (id: string, change: Partial<Mode>) => update((current) => withModeChange(current, id, () => change));

  function row(item: Shortcut, first: boolean) {
    const field = (label: string, onSave: (hotkey: string | null) => Promise<SaveError | null>, optional = true) => (
      <HotkeyField label={label} hotkey={item.hotkey} optional={optional} minWidth={172} onSave={onSave} />
    );
    switch (item.kind) {
      case "main":
        return (
          <Row
            key="main"
            separator={!first}
            title={t("keys.main")}
            detail={t("keys.main_note", { mode: modeName(t, item.mode) })}
            control={field(t("keys.main"), (hotkey) => update({ hotkey: hotkey ?? settings.hotkey }), false)}
          />
        );
      case "app":
        return (
          <Row
            key="app"
            separator={!first}
            title={t("keys.app")}
            detail={t("settings.app_hotkey_hint")}
            control={field(t("keys.app"), (app_hotkey) => update({ app_hotkey }))}
          />
        );
      case "mode":
        return (
          <Row
            key={`mode-${item.mode.id}`}
            separator={!first}
            title={modeName(t, item.mode)}
            detail={t(item.mode.id === ENGLISH_MODE.id ? "modes.english.hotkey_hint" : "modes.hotkey_hint")}
            control={field(t("keys.dictation_in", { mode: modeName(t, item.mode) }), (hotkey) => saveMode(item.mode.id, { hotkey }))}
          />
        );
      case "selection":
        return (
          <Row
            key={`selection-${item.mode.id}`}
            separator={!first}
            title={modeName(t, item.mode)}
            detail={t("modes.selection_hint")}
            control={field(t("keys.selection_in", { mode: modeName(t, item.mode) }), (selection_hotkey) => saveMode(item.mode.id, { selection_hotkey }))}
          />
        );
    }
  }

  const dictation = list.filter((item) => item.kind !== "selection");
  const selection = list.filter((item) => item.kind === "selection");
  return (
    <>
      <Card title={t("keys.dictation")}>{dictation.map((item, index) => row(item, index === 0))}</Card>
      <Card title={t("keys.selection")}>
        {selection.length > 0 ? (
          selection.map((item, index) => row(item, index === 0))
        ) : (
          <p className="caption card-note">
            {t("keys.no_selection")}{" "}
            <button type="button" className="btn link" onClick={() => navigate("modes")}>
              {t("nav.modes")}
            </button>
          </p>
        )}
      </Card>
      <Card>
        <Row
          separator={false}
          title={t("settings.recording_mode")}
          detail={t("settings.recording_mode_hint")}
          control={
            <Segments
              label={t("settings.recording_mode")}
              options={recordingModes.map((value) => ({ value, label: t(`settings.${value}`) }))}
              value={settings.recording_mode}
              onChange={(recording_mode) => update({ recording_mode })}
            />
          }
        />
      </Card>
    </>
  );
}

/// The microphone, muting the system's sound while recording, and the indicator on screen.
function MicrophoneTab({ settings, devices, update }: Props) {
  const { t } = useTranslation();

  return (
    <>
      <Card>
        <Row
          separator={false}
          title={t("settings.microphone")}
          control={
            <Select
              label={t("settings.microphone")}
              width={260}
              value={settings.microphone_id ?? ""}
              options={microphoneOptions(t, devices, settings.microphone_id)}
              onChange={(value) => update({ microphone_id: value || null })}
            />
          }
        />
        <Row
          title={t("settings.mute")}
          detail={t("settings.mute_hint")}
          control={
            <Toggle
              label={t("settings.mute")}
              checked={settings.mute_while_recording}
              onChange={(mute_while_recording) => update({ mute_while_recording })}
            />
          }
        />
        <Row
          title={t("settings.indicator")}
          detail={t("settings.indicator_hint")}
          control={
            <Toggle
              label={t("settings.indicator")}
              checked={settings.show_indicator}
              onChange={(show_indicator) => update({ show_indicator })}
            />
          }
        />
      </Card>
    </>
  );
}

const MIN_IDLE = 5;
const MAX_IDLE = 120;
const IDLE_STEP = 5;
const DEFAULT_IDLE = 20;

/// The policy after a press of "−" (`-1`) or "+" (`1`) on the minutes: one step, on the steps'
/// grid and within the bounds. Made from the policy saved last, so two quick presses make two
/// steps; another policy saved meanwhile stays.
export function steppedIdle(policy: UnloadPolicy, direction: 1 | -1): UnloadPolicy {
  if (policy.mode !== "after_idle") return policy;
  const minutes = Math.min(MAX_IDLE, Math.max(MIN_IDLE, Math.round(policy.minutes / IDLE_STEP) * IDLE_STEP + direction * IDLE_STEP));
  return { mode: "after_idle", minutes };
}

function Recognition({ settings, models, cloud, reloadModels, downloads, settling, update, onSettings, onOpenServices }: ModelsProps) {
  const { t } = useTranslation();
  const languages = useLanguageOptions();

  const policy = settings.unload_policy;
  const unloadModes: UnloadPolicy["mode"][] = ["immediate", "after_idle", "never"];
  function setUnload(mode: UnloadPolicy["mode"]) {
    update({ unload_policy: mode === "after_idle" ? { mode, minutes: DEFAULT_IDLE } : { mode } });
  }

  // The language reaches only the engines that read it.
  const local = models.find((item) => item.id === settings.model_id) ?? null;
  const model = local ?? cloud.find((item) => item.id === settings.model_id) ?? null;
  const choices = spokenLanguageChoices(languages, model, settings.spoken_language);
  const languageNote = recognitionNote(t, model, "language", settings.spoken_language);

  const unload = (
    <Row
      title={t(local?.hardware.kind === "cpu" ? "settings.unload_cpu" : "settings.unload")}
      detail={t("settings.unload_hint")}
      control={
        <span className="control-group">
          {policy.mode === "after_idle" && (
            <MinuteStepper
              value={policy.minutes}
              label={t("settings.minutes_label")}
              suffix={t("settings.minutes")}
              onStep={(direction) => void update((current) => ({ unload_policy: steppedIdle(current.unload_policy, direction) }))}
            />
          )}
          <Select
            label={t("settings.unload")}
            width={220}
            value={policy.mode}
            options={unloadModes.map((value) => ({ value, label: t(`settings.${value}`) }))}
            onChange={(value) => setUnload(value as UnloadPolicy["mode"])}
          />
        </span>
      }
    />
  );

  return (
    <>
      <ModelsPanel
        models={models}
        selected={settings.model_id}
        downloads={downloads}
        settling={settling}
        onUse={(model_id) => update({ model_id })}
        onDeleted={reloadModels}
        unload={unload}
      />
      <CloudPanel
        engines={cloud}
        settings={settings}
        onUse={(model_id) => update({ model_id })}
        onSettings={onSettings}
        onKeys={onOpenServices}
      />
      <Card>
        <Row
          separator={false}
          title={t("settings.spoken_language")}
          detail={languageNote ?? undefined}
          control={
            <Select
              label={t("settings.spoken_language")}
              width={240}
              value={model?.takes_language === false ? "" : (settings.spoken_language ?? "")}
              disabled={model?.takes_language === false}
              options={[
                { value: "", label: t("settings.auto_language") },
                ...choices.map((language) => ({ value: language.code, label: language.label })),
              ]}
              onChange={(value) => update({ spoken_language: value || null })}
            />
          }
        />
      </Card>
    </>
  );
}

/// The dictionary: the hint for the engine and the replacements after it, for every model.
export function DictionaryPage({ settings, models, cloud, update }: Pick<ModelsProps, "settings" | "models" | "cloud" | "update">) {
  const { t } = useTranslation();
  // The dictionary reaches only the engines that read it.
  const model = [...models, ...cloud].find((item) => item.id === settings.model_id) ?? null;
  const [dictionary, setDictionary] = useState(settings.dictionary);
  // A save made elsewhere shows here.
  useEffect(() => setDictionary(settings.dictionary), [settings.dictionary]);

  function saveDictionary(text: string) {
    if (text === settings.dictionary) return;
    // A refused dictionary goes back to the saved one.
    void saveText(update({ dictionary: text }), text, settings.dictionary, setDictionary);
  }

  return (
    <div className="page settings-page">
      <div className="page-header">
        <div>
          <h1 className="page-title" tabIndex={-1}>{t("nav.dictionary")}</h1>
          <span className="caption">{t("dictionary.page_note")}</span>
        </div>
      </div>
      <div className="tab-page">
        <Card>
          <Row
            separator={false}
            title={t("settings.dictionary")}
            detail={dictionaryNotes(t, model).join(" ")}
          >
            <textarea
              className="dictionary"
              rows={3}
              maxLength={MAX_DICTIONARY_CHARS}
              aria-label={t("settings.dictionary")}
              value={dictionary}
              onChange={(event) => setDictionary(event.currentTarget.value)}
              onBlur={(event) => saveDictionary(event.currentTarget.value)}
            />
          </Row>
        </Card>
        <ReplacementsPanel settings={settings} update={update} />
      </div>
    </div>
  );
}

function MinuteStepper({
  value,
  label,
  suffix,
  onStep,
}: {
  value: number;
  label: string;
  suffix: string;
  /// One step down or up from the minutes saved last (`steppedIdle`).
  onStep: (direction: 1 | -1) => void;
}) {
  const { t } = useTranslation();
  // A step to a bound disables its button, which throws the focus out: it goes to the other one.
  const follow = useFocusAfter();
  const less = useRef<HTMLButtonElement>(null);
  const more = useRef<HTMLButtonElement>(null);
  const step = (direction: 1 | -1) => {
    follow(() => (direction < 0 ? more.current : less.current), { disabledIsGone: true });
    onStep(direction);
  };
  return (
    <span className="stepper" role="group" aria-label={label}>
      <button ref={less} type="button" aria-label={t("settings.idle_less")} disabled={value <= MIN_IDLE} onClick={() => step(-1)}>
        −
      </button>
      <span aria-live="polite">
        {value} {suffix}
      </span>
      <button ref={more} type="button" aria-label={t("settings.idle_more")} disabled={value >= MAX_IDLE} onClick={() => step(1)}>
        +
      </button>
    </span>
  );
}
