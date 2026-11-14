import { useEffect, useId, useRef, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import type { Target } from "../App";
import { hotkeyKeys } from "../dictation";
import { modeLaunch } from "../shortcuts";
import { useLanguageOptions } from "../languages";
import { modeCaption, modeName, translationWarning, type ModelTraits } from "../present";
import { defaultModel } from "../services";
import { choosableModes, ENGLISH_MODE, withNewMode, type Mode, type Settings, type Update } from "../settings";
import { Card, ChoiceRow, useFocusAfter, useRoving } from "../ui";
import { DraftEditor, EnglishEditor, ModeEditor, splitModel, uniqueName, type Draft } from "./ModeEditors";

interface Props {
  settings: Settings;
  /// The speech models and cloud engines, and the one dictation uses (`null` when none is chosen).
  models: ModelTraits[];
  model: ModelTraits | null;
  /// The mode a link asked to open; a new object each time.
  focus: { id: string } | null;
  update: Update;
  /// Settings saved by a connection command (a service connected in a new mode).
  onSettings: (settings: Settings) => void;
  navigate: (target: Target) => void;
}

export function ModesPanel({ settings, models, model, focus, update, onSettings, navigate }: Props) {
  const { t } = useTranslation();
  const [selected, setSelected] = useState(settings.main_mode_id);
  const [draft, setDraft] = useState<Draft | null>(null);
  const languages = useLanguageOptions();
  // The list is a column of tabs over the editor.
  const ids = useId();
  const rowId = (id: string) => `${ids}-${id}`;
  const editorId = `${ids}-editor`;

  useEffect(() => {
    if (!focus) return;
    setDraft(null);
    setSelected(focus.id);
  }, [focus]);

  // The translation to English is a row of its own, under plain dictation, also while it is off.
  const english = settings.modes.find((item) => item.id === ENGLISH_MODE.id);
  const choosable = choosableModes(settings.modes);
  const listed = [
    ...choosable.filter((item) => item.id === "plain"),
    english ?? ENGLISH_MODE,
    ...choosable.filter((item) => item.id !== "plain"),
  ];
  const mode = listed.find((item) => item.id === selected) ?? listed[0];
  const translates = model?.translate ?? null;
  // The rows are tabs, and a draft is the last of them. Another row chosen drops the draft, so
  // while there is one the arrows only move the focus: Enter or Space chooses.
  const roving = useRoving([...listed.map((item) => ({ checked: !draft && item.id === mode.id })), ...(draft ? [{ checked: true }] : [])], {
    manual: draft !== null,
  });
  // A mode created, deleted or made the main one, a draft cancelled, or the main mode's last
  // program removed takes away the button pressed: the focus goes to the chosen row of the list.
  const follow = useFocusAfter();
  const list = useRef<HTMLDivElement>(null);
  const toChosenRow = () => follow(() => list.current?.querySelector<HTMLElement>('[aria-selected="true"]'));

  function newMode() {
    // A draft being written stays: a second press must not empty it.
    if (draft) return;
    setDraft({
      id: `mode-${Date.now().toString(36)}`,
      name: uniqueName(t("modes.new_name"), settings.modes),
      model: defaultModel(settings.modes, settings.connections),
      instruction: "",
      output_language: "",
    });
  }

  async function create(current: Draft) {
    const created: Mode = {
      id: current.id,
      name: current.name.trim(),
      action: {
        kind: "process",
        ...splitModel(current.model),
        instruction: current.instruction.trim(),
        output_language: current.output_language || null,
      },
      hotkey: null,
      selection_hotkey: null,
    };
    toChosenRow();
    // A second press while the first one saves finds the mode there and adds nothing.
    if (!(await update((current) => withNewMode(current, created)))) {
      setDraft(null);
      setSelected(created.id);
    }
  }

  const trailing = (item: Mode): ReactNode =>
    item.id === settings.main_mode_id ? <span className="badge">{t("modes.main_badge")}</span> : undefined;

  // How each mode is started: its hotkey, its selected-text hotkey, its programs.
  const tags = (item: Mode): string[] => {
    const launch = modeLaunch(settings, item);
    const short = (hotkey: string) => hotkeyKeys(hotkey).join("+");
    return [
      ...(launch.hotkey ? [short(launch.hotkey)] : []),
      ...(launch.selection ? [t("modes.tag_selection", { hotkey: short(launch.selection) })] : []),
      ...(launch.apps > 0 ? [t("modes.tag_apps", { count: launch.apps })] : []),
    ];
  };
  const caption = (item: Mode) =>
    item.id === ENGLISH_MODE.id && !english
      ? t("modes.english.off")
      : modeCaption(t, item, settings.connections, translates, settings.clean_text);

  return (
    <div className="page">
      <div className="page-header">
        <div>
          <h1 className="page-title" tabIndex={-1}>{t("nav.modes")}</h1>
          <p className="caption">{t("modes.subtitle")}</p>
        </div>
        <button type="button" className="btn primary" onClick={newMode}>
          {t("modes.add")}
        </button>
      </div>
      <div className="modes-body">
        <Card className="modes-list">
          <div ref={list} className="choice-list" role="tablist" aria-label={t("nav.modes")} {...roving.group}>
            {listed.map((item, index) => (
              <ChoiceRow
                key={item.id}
                id={rowId(item.id)}
                controls={editorId}
                title={modeName(t, item)}
                caption={caption(item)}
                tags={tags(item)}
                checked={!draft && item.id === mode.id}
                tabIndex={roving.tabIndex(index)}
                onSelect={() => {
                  setDraft(null);
                  setSelected(item.id);
                }}
                trailing={trailing(item)}
              />
            ))}
            {draft && (
              <ChoiceRow
                id={rowId(draft.id)}
                controls={editorId}
                title={draft.name || t("modes.new_name")}
                caption={t("modes.unsaved")}
                checked
                tabIndex={roving.tabIndex(listed.length)}
                onSelect={() => {}}
              />
            )}
          </div>
        </Card>
        <Card className="modes-editor" id={editorId} role="tabpanel" labelledBy={rowId(draft ? draft.id : mode.id)}>
          {draft ? (
            <DraftEditor
              draft={draft}
              setDraft={setDraft}
              settings={settings}
              languages={languages}
              onCreate={create}
              onCancel={() => {
                toChosenRow();
                setDraft(null);
              }}
              onSettings={onSettings}
              navigate={navigate}
            />
          ) : mode.id === ENGLISH_MODE.id ? (
            <EnglishEditor settings={settings} translationNote={translationWarning(t, model, models)} update={update} />
          ) : (
            <ModeEditor
              key={mode.id}
              mode={mode}
              settings={settings}
              languages={languages}
              update={update}
              navigate={navigate}
              onRemoved={() => setSelected(settings.main_mode_id === mode.id ? "plain" : settings.main_mode_id)}
              onLeave={toChosenRow}
            />
          )}
        </Card>
      </div>
    </div>
  );
}

