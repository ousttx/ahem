import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { useLanguageList } from "../languages";
import { groupConnections, SERVICES, type ServiceId } from "../services";
import type { Connection, Settings } from "../settings";
import { Card, Radio, useFocusAfter, useRoving } from "../ui";
import { ServiceKeyForm } from "./ConnectionsPanel";
import { LanguageList } from "./ModelsPanel";

/// Mirrors `commands::CloudEngineInfo`.
export interface CloudEngineInfo {
  id: string;
  name: string;
  /// The named service whose key it uses.
  service: ServiceId;
  translate: boolean;
  /// It gives word times, so it transcribes audio and video files.
  files: boolean;
  languages: string[];
  takes_dictionary: boolean;
  takes_language: boolean;
}

/// The cloud engines; the list is fixed, so it is loaded once.
export function useCloudEngines(): CloudEngineInfo[] {
  const [engines, setEngines] = useState<CloudEngineInfo[]>([]);
  useEffect(() => {
    invoke<CloudEngineInfo[]>("list_cloud_engines").then(setEngines, () => {});
  }, []);
  return engines;
}

/// The key form open once the form of `id` closed: none if it was that one; another one opened
/// meanwhile stays open.
export function formAfterClose(open: string | null, id: string): string | null {
  return open === id ? null : open;
}

/// Engines whose service has a key first, then the ones that need a key, each group in catalog
/// order.
export function splitCloud(engines: CloudEngineInfo[], connections: Connection[]) {
  const { named } = groupConnections(connections);
  const ready = (engine: CloudEngineInfo) => named[engine.service] !== undefined;
  return { connected: engines.filter(ready), locked: engines.filter((engine) => !ready(engine)) };
}

interface Props {
  engines: CloudEngineInfo[];
  settings: Settings;
  onUse: (id: string) => void;
  onSettings: (settings: Settings) => void;
  /// Opens the Services page.
  onKeys: () => void;
}

/// "Cloud services" in the recognition settings: a service recognizes speech
/// instead of a model on this computer. Audio goes to the service only while it is chosen here.
export function CloudPanel({ engines, settings, onUse, onSettings, onKeys }: Props) {
  const { t } = useTranslation();
  const languages = useLanguageList();
  const [adding, setAdding] = useState<string | null>(null);
  // A key being checked: no other form opens meanwhile, so its answer is seen where it was asked.
  const [checking, setChecking] = useState(false);
  const { connected, locked } = splitCloud(engines, settings.connections);
  const roving = useRoving(connected.map((engine) => ({ checked: settings.model_id === engine.id })));
  // The form shows only while the engine's service has no key: one added meanwhile on the
  // Services page closes it for good, so a second key makes no second connection.
  const addingReady = connected.some((engine) => engine.id === adding);
  useEffect(() => {
    if (addingReady) setAdding(null);
  }, [addingReady]);
  // A closed form takes away the focused field or button: the focus goes to the engine's row, to
  // its radio once connected, else to its "Add key".
  const follow = useFocusAfter();
  const rows = useRef<Record<string, HTMLDivElement | null>>({});
  const toRow = (id: string) => follow(() => rows.current[id]?.querySelector<HTMLElement>("button:not(:disabled)"));

  /// A row of an engine; one without a key cannot be chosen, and its disabled radio is out of the
  /// Tab order and of the arrows.
  function row(engine: CloudEngineInfo, ready: boolean, index: number) {
    const service = SERVICES.find((item) => item.id === engine.service);
    const selected = settings.model_id === engine.id;
    return (
      <div
        key={engine.id}
        ref={(element) => {
          rows.current[engine.id] = element;
        }}
        className="model-row"
      >
        <button
          type="button"
          role="radio"
          aria-checked={selected}
          aria-label={engine.name}
          className="radio-button"
          tabIndex={ready ? roving.tabIndex(index) : undefined}
          disabled={!ready}
          onClick={() => onUse(engine.id)}
        >
          <Radio checked={ready && selected} available={ready} />
        </button>
        <div className="model-texts">
          <span className="strong">{engine.name}</span>
          <span className="caption">
            {ready ? t("cloud.connected") : t("cloud.no_key")}
            {!engine.translate && ` · ${t("models.no_translation")}`}
            {!engine.files && ` · ${t("cloud.no_files")}`}
          </span>
          <LanguageList codes={engine.languages} all={languages} />
          {adding === engine.id && !ready && service && (
            <ServiceKeyForm
              service={service}
              onSettings={onSettings}
              onLeave={() => toRow(engine.id)}
              onClose={() => setAdding((open) => formAfterClose(open, engine.id))}
              onBusy={setChecking}
            />
          )}
        </div>
        <div className="model-actions">
          {ready
            ? selected && <span className="caption">{t("models.in_use")}</span>
            : adding !== engine.id && (
                <button type="button" className="btn compact" disabled={checking} onClick={() => setAdding(engine.id)}>
                  {t("connections.add_key")}
                </button>
              )}
        </div>
      </div>
    );
  }

  return (
    <Card>
      <div className="section-header">
        <span className="section-label">{t("cloud.title")}</span>
        <button type="button" className="btn link" onClick={onKeys}>
          {t("cloud.keys")}
        </button>
      </div>
      <div className="model-rows" role="radiogroup" aria-label={t("cloud.title")} {...roving.group}>
        {connected.map((engine, index) => row(engine, true, index))}
      </div>
      {locked.length > 0 && (
        <details className="model-more">
          <summary>{t("cloud.locked", { number: locked.length })}</summary>
          <div className="model-rows" role="radiogroup" aria-label={t("cloud.locked", { number: locked.length })}>
            {locked.map((engine, index) => row(engine, false, index))}
          </div>
        </details>
      )}
    </Card>
  );
}
