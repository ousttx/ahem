import { useTranslation } from "react-i18next";
import type { InputDevice, Target } from "../App";
import { formatHotkey, type View } from "../dictation";
import { modeCaption, modeName, type Status } from "../present";
import type { SaveError, Settings } from "../settings";
import { shortcuts, type Shortcut } from "../shortcuts";
import { Card, KeyCaps, StepTile } from "../ui";
import { OnboardingPanel } from "./OnboardingPanel";
import { StatsPanel } from "./StatsPanel";
import { StatusPanel } from "./StatusPanel";

interface Props {
  view: View;
  settings: Settings;
  status: Status;
  modelName: string | null;
  /// The chosen engine can recognize now (`canRecognize`).
  usable: boolean;
  /// A cloud service recognizes the dictation.
  inCloud: boolean;
  /// Whether the dictation model translates; `null` without a model.
  translates: boolean | null;
  devices: InputDevice[];
  update: (change: Partial<Settings>) => Promise<SaveError | null>;
  navigate: (target: Target) => void;
}

/// The Dictation page: the first-run steps, the state, the pipeline of the main hotkey, the
/// other hotkeys and the statistics. The main mode is chosen on the Modes page, which the
/// pipeline's processing step opens, and in the quick panel; the history has its own page.
export function DictationPage({ view, settings, status, modelName, usable, inCloud, translates, devices, update, navigate }: Props) {
  const { t } = useTranslation();
  const microphone =
    settings.microphone_id === null
      ? t("dictation.step.system_microphone")
      : (devices.find((device) => device.id === settings.microphone_id)?.name ??
        t(devices.length > 0 ? "dictation.step.microphone_missing" : "dictation.step.microphone"));
  const main = settings.modes.find((mode) => mode.id === settings.main_mode_id) ?? settings.modes[0];

  return (
    <div className="page">
      <h1 className="page-title" tabIndex={-1}>{t("nav.dictation")}</h1>
      <OnboardingPanel settings={settings} update={update} navigate={navigate} />
      <StatusPanel view={view} settings={settings} status={status} modelName={modelName} usable={usable} navigate={navigate} />

      <Card>
        <div className="pipeline">
          <div className="pipeline-header">
            <h3 className="card-title">{t("dictation.pipeline", { hotkey: formatHotkey(settings.hotkey) })}</h3>
            <span className="caption">{t("dictation.pipeline_hint")}</span>
          </div>
          <div className="pipeline-tiles">
            <StepTile
              icon="microphone"
              step={t("dictation.step.recording")}
              value={microphone}
              note={t(settings.mute_while_recording ? "dictation.step.muted" : "dictation.step.not_muted")}
              onClick={() => navigate("settings:microphone")}
            />
            <StepTile
              icon="chip"
              step={t("dictation.step.recognition")}
              value={modelName ?? t("dictation.step.no_model")}
              note={t(modelName ? (inCloud ? "dictation.step.cloud" : "dictation.step.local") : "dictation.step.configure")}
              onClick={() => navigate("models")}
            />
            <StepTile
              icon="sparkle"
              step={t("dictation.step.processing")}
              value={main.id === "plain" ? t("dictation.step.no_processing") : modeName(t, main)}
              note={modeCaption(t, main, settings.connections, translates, settings.clean_text)}
              onClick={() => navigate("modes")}
            />
            <StepTile
              icon="cursor"
              step={t("dictation.step.insertion")}
              value={t("dictation.step.at_cursor")}
              note={t("dictation.step.no_clipboard")}
            />
          </div>
        </div>
      </Card>

      <MoreShortcuts settings={settings} navigate={navigate} />

      <StatsPanel />
    </div>
  );
}

/// The hotkeys besides the main one, each with what it does; a row opens its mode, or the Keys
/// tab for the "by application" hotkey. Without a selected-text hotkey, a line says it exists.
function MoreShortcuts({ settings, navigate }: { settings: Settings; navigate: (target: Target) => void }) {
  const { t } = useTranslation();
  const list = shortcuts(settings).filter((item) => item.kind !== "main" && item.hotkey !== null);
  const rewrites = list.some((item) => item.kind === "selection");
  const label = (item: Shortcut) => {
    switch (item.kind) {
      case "selection":
        return t("keys.selection_in", { mode: modeName(t, item.mode) });
      case "mode":
        return t("keys.dictation_in", { mode: modeName(t, item.mode) });
      default:
        return t("keys.app");
    }
  };
  const target = (item: Shortcut): Target => (item.kind === "mode" || item.kind === "selection" ? `mode:${item.mode.id}` : "settings:keys");

  return (
    <Card
      title={t("keys.more")}
      action={
        <button type="button" className="btn link" onClick={() => navigate("settings:keys")}>
          {t("keys.all")}
        </button>
      }
    >
      <div className="shortcut-list">
        {list.map((item) => (
          <button key={`${item.kind}-${"mode" in item ? item.mode.id : ""}`} type="button" className="shortcut-row" onClick={() => navigate(target(item))}>
            {item.hotkey && <KeyCaps hotkey={item.hotkey} />}
            <span className="shortcut-text">{label(item)}</span>
          </button>
        ))}
        {!rewrites && (
          <button type="button" className="shortcut-row" onClick={() => navigate("settings:keys")}>
            <span className="caption">{t("keys.selection_hint")}</span>
          </button>
        )}
      </div>
    </Card>
  );
}
