// The editors of the Modes page: the built-in plain mode (clean text, apps), the English
// mode, a language-model mode and the draft of a new one, with their rows and choices.
import { useEffect, useRef, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import type { Target } from "../App";
import { addRule, appKey, appRulesView, focusAfterRule, offeredApps, removeRule, ruleFor, typedApp, type AppId, type AppRule } from "../appRules";
import { formatHotkey } from "../dictation";
import type { LanguageOption } from "../languages";
import { modeName, plainModeNote } from "../present";
import { connectionLabel, defaultModel, hasLanguageModels, isChatGptAccount, isLocal } from "../services";
import {
  CLEAN_INSTRUCTION,
  DEFAULT_FILLERS,
  ENGLISH_MODE,
  NO_CONTEXT,
  saveText,
  toggledContext,
  withEnglish,
  withModeChange,
  type CleanText,
  type Connection,
  type ContextSources,
  type Mode,
  type Settings,
  type Update,
} from "../settings";
import {
  Icon,
  IconButton,
  isComposing,
  readOnFocus,
  rowAfterRemoval,
  Segments,
  Select,
  type SelectOption,
  Toggle,
  useConfirm,
  useDebouncedSave,
  useFocusAfter,
} from "../ui";
import { openChatGptUsage, ServiceList } from "./ConnectionsPanel";
import { HotkeyField } from "./HotkeyField";

/// The instruction is saved this long after the last key press.
const INSTRUCTION_SAVE_DELAY_MS = 800;

type ProcessAction = Extract<Mode["action"], { kind: "process" }>;

export interface Draft {
  id: string;
  name: string;
  /// `connection id|model`.
  model: string;
  instruction: string;
  output_language: string;
}

/// `connection id|model` of every model of the connected services.
function modelOptions(connections: Connection[], current?: string): SelectOption[] {
  const options = connections.flatMap((connection) =>
    connection.models.map((model) => ({ value: `${connection.id}|${model}`, label: `${connectionLabel(connection)} · ${model}` })),
  );
  if (current && !options.some((option) => option.value === current)) {
    const [id, ...rest] = current.split("|");
    const connection = connections.find((item) => item.id === id);
    if (connection) options.unshift({ value: current, label: `${connectionLabel(connection)} · ${rest.join("|")}` });
  }
  return options;
}

export function splitModel(value: string): { connection_id: string; model: string } {
  const [connection_id, ...rest] = value.split("|");
  return { connection_id, model: rest.join("|") };
}

export function uniqueName(base: string, modes: Mode[]): string {
  const names = new Set(modes.map((mode) => mode.name));
  if (!names.has(base)) return base;
  let number = 2;
  while (names.has(`${base} ${number}`)) number += 1;
  return `${base} ${number}`;
}

/// A line of the editor: name and caption on the left, the control on the right;
/// `stacked` puts a wide control under the caption.
function EditorRow({
  title,
  caption,
  stacked,
  children,
}: {
  title: string;
  caption?: string;
  stacked?: boolean;
  children?: ReactNode;
}) {
  return (
    <div className={stacked ? "editor-row stacked" : "editor-row"}>
      <div className="editor-row-text">
        <div>{title}</div>
        {caption && <div className="caption">{caption}</div>}
      </div>
      {children}
    </div>
  );
}

function ModelChoice({
  value,
  connections,
  onChange,
  navigate,
}: {
  value: string;
  connections: Connection[];
  onChange: (value: string) => void;
  navigate: (target: Target) => void;
}) {
  const { t } = useTranslation();
  const options = modelOptions(connections, value || undefined);
  // Nothing is preselected without an earlier mode's model: the first one in the list could be
  // any model of the service.
  const choose = value === "" ? [{ value: "", label: t("modes.choose_model"), disabled: true }] : [];
  const chosen = connections.find((connection) => connection.id === value.split("|")[0]);
  return (
    <div className="model-choice">
      <span className="caption">{t("modes.model")}</span>
      {options.length > 0 ? (
        <>
          <Select label={t("modes.model")} value={value} options={[...choose, ...options]} onChange={onChange} />
          {chosen && isChatGptAccount(chosen) && (
            <span className="caption plan-note">
              {t("chatgpt.using_plan")} ·{" "}
              <button type="button" className="btn link" onClick={openChatGptUsage}>
                {t("chatgpt.manage_usage")}
              </button>
            </span>
          )}
        </>
      ) : (
        <>
          <Select label={t("modes.model")} value="" options={[{ value: "", label: t("modes.no_services") }]} onChange={() => {}} disabled />
          <button type="button" className="btn link" onClick={() => navigate("services")}>
            {t("modes.add_key")}
          </button>
        </>
      )}
    </div>
  );
}

function LanguageChoice({ value, languages, onChange }: { value: string; languages: LanguageOption[]; onChange: (value: string) => void }) {
  const { t } = useTranslation();
  return (
    <Select
      label={t("modes.output_language")}
      width={220}
      value={value}
      options={[{ value: "", label: t("modes.same_language") }, ...languages.map((language) => ({ value: language.code, label: language.label }))]}
      onChange={onChange}
    />
  );
}

const CLEAN_LEVELS: CleanText["level"][] = ["off", "fillers", "model"];

/// How plain dictation cleans its text. With a model, the level is saved once a model is chosen:
/// the last language-model mode's model is taken when there is one.
function CleanTextSection({ settings, update, navigate }: { settings: Settings; update: Update; navigate: (target: Target) => void }) {
  const { t } = useTranslation();
  const clean = settings.clean_text;
  const [level, setLevel] = useState(clean.level);
  const saved = clean.level === "model" ? `${clean.connection_id}|${clean.model}` : "";
  const missing = clean.level === "model" && !settings.connections.some((item) => item.id === clean.connection_id);
  const [words, setWords] = useState(settings.fillers);
  const [editing, setEditing] = useState(false);
  const [prompt, setPrompt] = useState(false);
  const count = words.split(/[,\n]/).filter((word) => word.trim()).length;
  const saveWords = useDebouncedSave((text: string) => {
    // A refused list goes back to the saved one.
    if (text !== settings.fillers) void saveText(update({ fillers: text }), text, settings.fillers, setWords);
  }, INSTRUCTION_SAVE_DELAY_MS);
  // "Reset to default" goes once the words are: the focus goes to the words.
  const follow = useFocusAfter();
  const wordsField = useRef<HTMLTextAreaElement>(null);

  // The saved level wins when it changes elsewhere (the tray, a removed service).
  useEffect(() => setLevel(clean.level), [clean.level]);

  function choose(next: CleanText["level"]) {
    setLevel(next);
    // A level that was not saved goes back to the saved one.
    const save = (clean_text: CleanText) =>
      void update({ clean_text }).then((failed) => {
        if (failed) setLevel(clean.level);
      });
    if (next !== "model") return save({ level: next });
    const preset = saved || defaultModel(settings.modes, settings.connections);
    if (preset) save({ level: "model", ...splitModel(preset) });
  }

  return (
    <>
      <EditorRow title={t("modes.clean.title")} caption={level === "fillers" ? undefined : t(`modes.clean.hint_${level}`)}>
        <Segments
          label={t("modes.clean.title")}
          options={CLEAN_LEVELS.map((value) => ({ value, label: t(`modes.clean.${value}`) }))}
          value={level}
          onChange={choose}
        />
      </EditorRow>
      {level !== "off" && (
        <EditorRow title={t("modes.clean.words")} caption={t("modes.clean.words_count", { count })}>
          <button type="button" className="btn" aria-expanded={editing} onClick={() => setEditing(!editing)}>
            {t(editing ? "modes.clean.hide_words" : "modes.clean.edit_words")}
          </button>
        </EditorRow>
      )}
      {level !== "off" && editing && (
        <div className="processing">
          <textarea
            ref={wordsField}
            className="instruction"
            aria-label={t("modes.clean.words")}
            value={words}
            onChange={(event) => {
              const text = event.currentTarget.value;
              setWords(text);
              saveWords.schedule(text);
            }}
            onBlur={(event) => saveWords.flush(event.currentTarget.value)}
          />
          <p className="caption editor-note">{t("modes.clean.words_hint")}</p>
          {words !== DEFAULT_FILLERS && (
            <button
              type="button"
              className="btn link"
              onClick={() => {
                follow(() => wordsField.current);
                setWords(DEFAULT_FILLERS);
                saveWords.flush(DEFAULT_FILLERS);
              }}
            >
              {t("modes.clean.reset_words")}
            </button>
          )}
        </div>
      )}
      {level === "model" && (
        <div className="processing">
          <ModelChoice
            value={saved}
            connections={settings.connections}
            onChange={(value) => void update({ clean_text: { level: "model", ...splitModel(value) } })}
            navigate={navigate}
          />
          {missing && <p className="caption warning-text">{t("modes.clean.connection_missing")}</p>}
          <p className="caption editor-note">{t("modes.clean.model_note")}</p>
          <button type="button" className="btn link" aria-expanded={prompt} onClick={() => setPrompt(!prompt)}>
            {t(prompt ? "modes.clean.hide_prompt" : "modes.clean.show_prompt")}
          </button>
          {prompt && (
            <>
              <p className="caption editor-note">{t("modes.clean.prompt_note")}</p>
              <textarea className="instruction" aria-label={t("modes.clean.show_prompt")} value={CLEAN_INSTRUCTION} readOnly />
            </>
          )}
        </div>
      )}
    </>
  );
}

/// The programs where the per-app hotkey dictates with this mode. Only the user adds them; the
/// recent ones are the programs dictated into since the app started. The main mode takes none,
/// but shows the ones it had before, to be removed (`appRulesView`).
function AppRulesSection({
  mode,
  settings,
  update,
  navigate,
  onLeave,
}: {
  mode: Mode;
  settings: Settings;
  update: Update;
  navigate: (target: Target) => void;
  /// Called as the main mode's last program is removed: the focus goes to the list of modes.
  onLeave: () => void;
}) {
  const { t } = useTranslation();
  const [recent, setRecent] = useState<AppId[]>([]);
  const [typed, setTyped] = useState("");
  // The page stays mounted: the programs dictated into meanwhile come with the window's focus.
  useEffect(() => readOnFocus(() => void invoke<AppId[]>("recent_apps").then(setRecent, () => setRecent([]))), []);
  const follow = useFocusAfter();
  const chips = useRef<HTMLUListElement>(null);
  const typedField = useRef<HTMLInputElement>(null);

  const { rules, main, waiting } = appRulesView(settings, mode.id);
  const offered = offeredApps(settings, recent, mode.id);
  const add = (app: AppId) => void update((current) => addRule(current, app, mode.id));
  // The focus goes from a removed program to the next one, else to the one before, else to the
  // field for a name; the main mode has none: to the list of modes (`focusAfterRule`).
  function remove(rule: AppRule) {
    const before = rules.map((item) => item.app_id);
    if (focusAfterRule(before, rule.app_id, main) === "list") onLeave();
    else
      follow(() => {
        const left = Array.from(chips.current?.querySelectorAll<HTMLElement>("[data-rule]") ?? []);
        const next = rowAfterRemoval(before, rule.app_id, left.map((chip) => chip.dataset.rule ?? ""));
        return left.find((chip) => chip.dataset.rule === next)?.querySelector("button") ?? typedField.current;
      });
    void update((current) => removeRule(current, rule));
  }

  const chipList = rules.length > 0 && (
    <ul ref={chips} className="chips app-chips" aria-label={t("modes.apps.title")}>
      {rules.map((rule) => (
        <li key={rule.app_id} className="chip app-chip" data-rule={rule.app_id}>
          <span>{rule.app_name}</span>
          <IconButton icon="close" size={20} label={t("modes.apps.remove", { app: rule.app_name })} onClick={() => remove(rule)} />
        </li>
      ))}
    </ul>
  );

  if (main) {
    return (
      <>
        <EditorRow title={t("modes.apps.title")} caption={t("modes.apps.main_hint")} />
        {waiting && (
          <div className="app-rules">
            <p className="caption">{t("modes.apps.main_rules")}</p>
            {chipList}
          </div>
        )}
      </>
    );
  }

  function addTyped() {
    if (!appKey(typed)) return;
    add(typedApp(typed));
    setTyped("");
    // "Add" is disabled with the field emptied, which would throw the focus out: the next name
    // begins in the field.
    typedField.current?.focus();
  }
  // A program another mode has moves to this one; the option says so.
  function label(app: AppId): string {
    const other = settings.modes.find((item) => item.id === ruleFor(settings, app)?.mode_id);
    return other ? t("modes.apps.moves", { app: app.name, mode: modeName(t, other) }) : app.name;
  }

  return (
    <>
      <EditorRow
        title={t("modes.apps.title")}
        caption={
          settings.app_hotkey
            ? t("modes.apps.hint", { hotkey: formatHotkey(settings.app_hotkey) })
            : t("modes.apps.hint_no_hotkey")
        }
      />
      <div className="app-rules">
        {!settings.app_hotkey && rules.length > 0 && (
          <p className="caption warning-text">
            {t("modes.apps.no_hotkey")}{" "}
            <button type="button" className="btn link" onClick={() => navigate("settings:keys")}>
              {t("modes.apps.set_hotkey")}
            </button>
          </p>
        )}
        {chipList}
        <div className="app-add">
          {offered.length > 0 && (
            <Select
              label={t("modes.apps.recent")}
              value=""
              width={200}
              options={[
                { value: "", label: t("modes.apps.choose_recent"), disabled: true },
                ...offered.map((app) => ({ value: app.id, label: label(app) })),
              ]}
              // The last recent app added takes the list away: the focus goes to the field for a name.
              onChange={(id) => {
                const app = offered.find((item) => item.id === id);
                if (!app) return;
                follow(() => typedField.current);
                add(app);
              }}
            />
          )}
          <input
            ref={typedField}
            className="text-input"
            aria-label={t("modes.apps.typed")}
            placeholder={t("modes.apps.typed_placeholder")}
            value={typed}
            onChange={(event) => setTyped(event.currentTarget.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter" && !isComposing(event)) addTyped();
            }}
          />
          <button type="button" className="btn" disabled={!appKey(typed)} onClick={addTyped}>
            {t("modes.apps.add")}
          </button>
        </div>
        {offered.length === 0 && <p className="caption editor-note">{t("modes.apps.no_recent")}</p>}
      </div>
    </>
  );
}

const CONTEXT_SOURCES: (keyof ContextSources)[] = ["selection", "clipboard", "app"];

/// What the mode's language model sees besides the dictation, read at the hotkey press.
function ContextChoice({
  sources,
  connection,
  onToggle,
}: {
  sources: ContextSources;
  connection: Connection | undefined;
  /// Turns a source on or off in the mode as saved last: quick presses each count.
  onToggle: (source: keyof ContextSources) => void;
}) {
  const { t } = useTranslation();
  const any = CONTEXT_SOURCES.some((source) => sources[source]);
  return (
    <>
      <EditorRow title={t("modes.context.title")} caption={t("modes.context.hint")} stacked>
        <div className="chips" role="group" aria-label={t("modes.context.title")}>
          {CONTEXT_SOURCES.map((source) => (
            <button
              key={source}
              type="button"
              className={`chip${sources[source] ? " checked" : ""}`}
              aria-pressed={sources[source]}
              onClick={() => onToggle(source)}
            >
              {t(`modes.context.${source}`)}
            </button>
          ))}
        </div>
      </EditorRow>
      {any && connection && !isLocal(connection) && (
        <p className="caption editor-note">{t("modes.context.cloud_note", { service: connectionLabel(connection) })}</p>
      )}
      {sources.clipboard && <p className="caption editor-note">{t("modes.context.clipboard_note")}</p>}
    </>
  );
}

/// What a mode does, as one line of text: the action is fixed by the mode's kind.
function ActionRow({ action }: { action: Mode["action"]["kind"] }) {
  const { t } = useTranslation();
  return <EditorRow title={t("modes.action")} caption={t(`modes.action_kind.${action}`)} />;
}

/// The translation to English: a mode of its own, turned on and off here, with its own hotkey;
/// it works when the dictation model translates.
export function EnglishEditor({
  settings,
  translationNote,
  update,
}: {
  settings: Settings;
  /// Why the dictation model cannot translate to English.
  translationNote: string | null;
  update: Update;
}) {
  const { t } = useTranslation();
  const english = settings.modes.find((item) => item.id === ENGLISH_MODE.id);
  const cannotTranslate = translationNote !== null;
  return (
    <div className="mode-editor">
      <div className="mode-editor-header">
        <span className="mode-name-text">{modeName(t, ENGLISH_MODE)}</span>
      </div>
      <EditorRow title={t("modes.english.title")} caption={t("modes.english.hint")}>
        <Toggle
          label={t("modes.english.title")}
          checked={english !== undefined}
          disabled={cannotTranslate && english === undefined}
          onChange={(on) => void update((current) => withEnglish(current, on))}
        />
      </EditorRow>
      {/* Not an error: some models simply do not translate. */}
      {translationNote && <p className="caption editor-note">{translationNote}</p>}
      {english && (
        <EditorRow title={t("modes.english.hotkey")} caption={t("modes.english.hotkey_hint")}>
          <HotkeyField
            label={t("modes.english.hotkey")}
            hotkey={english.hotkey}
            optional
            disabled={cannotTranslate}
            minWidth={160}
            onSave={(hotkey) => update((current) => withModeChange(current, ENGLISH_MODE.id, () => ({ hotkey })))}
          />
        </EditorRow>
      )}
    </div>
  );
}

export function ModeEditor({
  mode,
  settings,
  languages,
  update,
  navigate,
  onRemoved,
  onLeave,
}: {
  mode: Mode;
  settings: Settings;
  languages: LanguageOption[];
  update: Update;
  navigate: (target: Target) => void;
  onRemoved: () => void;
  /// Called as an action begins that takes away its button ("Make main", "Delete", the main
  /// mode's last program removed): the focus goes to the list once the button is gone.
  onLeave: () => void;
}) {
  const { t } = useTranslation();
  const action = mode.action;
  const processes = action.kind === "process";
  const [name, setName] = useState(mode.name);
  const [instruction, setInstruction] = useState(processes ? action.instruction : "");
  const [removing, confirmRemove] = useConfirm();

  // Every change applies to the mode as saved last, not as rendered.
  function save(change: (item: Mode) => Partial<Mode>) {
    return update((current) => withModeChange(current, mode.id, change));
  }

  function saveAction(change: Partial<ProcessAction> | ((action: ProcessAction) => Partial<ProcessAction>)) {
    return save((item) =>
      item.action.kind === "process" ? { action: { ...item.action, ...(typeof change === "function" ? change(item.action) : change) } } : {},
    );
  }

  const saveInstruction = useDebouncedSave((text: string) => {
    if (action.kind !== "process" || !text.trim() || text.trim() === action.instruction) return;
    // A refused instruction goes back to the saved one.
    void saveText(saveAction({ instruction: text.trim() }), text, action.instruction, setInstruction);
  }, INSTRUCTION_SAVE_DELAY_MS);

  function saveName() {
    const trimmed = name.trim();
    // A refused name goes back to the saved one.
    if (trimmed && trimmed !== mode.name) void saveText(save(() => ({ name: trimmed })), name, mode.name, setName);
    else setName(mode.name);
  }

  async function remove() {
    const failure = await update((current) => ({
      modes: current.modes.filter((item) => item.id !== mode.id),
      main_mode_id: current.main_mode_id === mode.id ? "plain" : current.main_mode_id,
    }));
    if (!failure) onRemoved();
  }

  const main = mode.id === settings.main_mode_id;
  const connected = processes && settings.connections.some((item) => item.id === action.connection_id);

  // The main mode's mark, or the button that makes this mode the main one.
  const mainFooter = main ? (
    <span className="main-label">
      <Icon name="check" size={15} tone="accent" />
      <span className="accent-text">{t("modes.is_main")}</span>
    </span>
  ) : (
    <button
      type="button"
      className="btn"
      onClick={() => {
        onLeave();
        void update({ main_mode_id: mode.id });
      }}
    >
      {t("modes.make_main")}
    </button>
  );

  // Plain dictation has no processing and runs on the main hotkey; it cleans the text, and it
  // can be the mode of programs. It can be made the main mode again, not deleted.
  if (action.kind === "plain") {
    const note = plainModeNote(t, settings);
    return (
      <div className="mode-editor">
        <div className="mode-editor-header">
          <span className="mode-name-text">{modeName(t, mode)}</span>
        </div>
        {note && <p className="caption editor-note">{note}</p>}
        <CleanTextSection settings={settings} update={update} navigate={navigate} />
        <AppRulesSection mode={mode} settings={settings} update={update} navigate={navigate} onLeave={onLeave} />
        <div className="editor-footer">{mainFooter}</div>
      </div>
    );
  }

  return (
    <div className="mode-editor">
      <div className="mode-editor-header">
        <input
          className="mode-name"
          aria-label={t("modes.name")}
          value={name}
          onChange={(event) => setName(event.currentTarget.value)}
          onBlur={saveName}
          // Saved where it is typed: the focus stays in the field.
          onKeyDown={(event) => {
            if (event.key === "Enter" && !isComposing(event)) saveName();
          }}
        />
        <span className="caption">{t("modes.kind_custom")}</span>
      </div>
      <ActionRow action={action.kind} />
      {action.kind === "process" && (
        <div className="processing">
          <ModelChoice
            value={`${action.connection_id}|${action.model}`}
            connections={settings.connections}
            onChange={(value) => void saveAction(splitModel(value))}
            navigate={navigate}
          />
          {!connected && <p className="caption warning-text">{t("modes.connection_missing")}</p>}
          <textarea
            className="instruction"
            aria-label={t("modes.instruction")}
            placeholder={t("modes.instruction_hint")}
            value={instruction}
            onChange={(event) => {
              const text = event.currentTarget.value;
              setInstruction(text);
              saveInstruction.schedule(text);
            }}
            onBlur={(event) => saveInstruction.flush(event.currentTarget.value)}
          />
          {!instruction.trim() && <p className="caption warning-text">{t("modes.instruction_required")}</p>}
          <EditorRow title={t("modes.output_language")} caption={t("modes.output_language_hint")}>
            <LanguageChoice
              value={action.output_language ?? ""}
              languages={languages}
              onChange={(value) => void saveAction({ output_language: value || null })}
            />
          </EditorRow>
          <ContextChoice
            sources={action.context ?? NO_CONTEXT}
            connection={settings.connections.find((item) => item.id === action.connection_id)}
            onToggle={(source) => void saveAction((saved) => ({ context: toggledContext(saved.context, source) }))}
          />
        </div>
      )}
      <h4 className="section-label editor-section">{t("modes.how_to_run")}</h4>
      <EditorRow title={t("modes.hotkey")} caption={t("modes.hotkey_hint")}>
        <HotkeyField label={t("modes.hotkey")} hotkey={mode.hotkey} optional minWidth={160} onSave={(hotkey) => save(() => ({ hotkey }))} />
      </EditorRow>
      <EditorRow title={t("modes.selection_hotkey")} caption={t(processes ? "modes.selection_hint" : "modes.selection_unavailable")}>
        <HotkeyField
          label={t("modes.selection_hotkey")}
          hotkey={mode.selection_hotkey}
          optional
          disabled={!processes}
          minWidth={160}
          onSave={(selection_hotkey) => save(() => ({ selection_hotkey }))}
        />
      </EditorRow>
      <AppRulesSection mode={mode} settings={settings} update={update} navigate={navigate} onLeave={onLeave} />
      <div className="editor-footer">
        {mainFooter}
        <button
          type="button"
          className="btn link quiet"
          onClick={() =>
            confirmRemove(() => {
              onLeave();
              void remove();
            })
          }
        >
          {removing ? t("modes.confirm_delete") : t("modes.delete")}
        </button>
      </div>
    </div>
  );
}

export function DraftEditor({
  draft,
  setDraft,
  settings,
  languages,
  onCreate,
  onCancel,
  onSettings,
  navigate,
}: {
  draft: Draft;
  setDraft: (draft: Draft) => void;
  settings: Settings;
  languages: LanguageOption[];
  onCreate: (draft: Draft) => void;
  onCancel: () => void;
  onSettings: (settings: Settings) => void;
  navigate: (target: Target) => void;
}) {
  const { t } = useTranslation();
  const complete = draft.name.trim() !== "" && draft.model !== "" && draft.instruction.trim() !== "";
  // A new mode needs a language-model service: it is connected right here.
  if (!settings.connections.some(hasLanguageModels)) {
    return (
      <div className="mode-editor">
        <div className="mode-editor-header">
          <span className="mode-name-text">{draft.name}</span>
          <span className="caption">{t("modes.kind_new")}</span>
        </div>
        <p className="caption editor-note">{t("modes.connect_first")}</p>
        <div className="card nested-services">
          <ServiceList settings={settings} onSettings={onSettings} only="text" />
        </div>
        <div className="editor-footer">
          <button type="button" className="btn link" onClick={() => navigate("services")}>
            {t("modes.own_server")}
          </button>
          <button type="button" className="btn link" onClick={onCancel}>
            {t("modes.cancel")}
          </button>
        </div>
      </div>
    );
  }
  return (
    <div className="mode-editor">
      <div className="mode-editor-header">
        <input
          className="mode-name"
          aria-label={t("modes.name")}
          value={draft.name}
          autoFocus
          onChange={(event) => setDraft({ ...draft, name: event.currentTarget.value })}
        />
        <span className="caption">{t("modes.kind_new")}</span>
      </div>
      <ActionRow action="process" />
      <div className="processing">
        <ModelChoice value={draft.model} connections={settings.connections} onChange={(model) => setDraft({ ...draft, model })} navigate={navigate} />
        <textarea
          className="instruction"
          aria-label={t("modes.instruction")}
          placeholder={t("modes.instruction_hint")}
          value={draft.instruction}
          onChange={(event) => setDraft({ ...draft, instruction: event.currentTarget.value })}
        />
        <EditorRow title={t("modes.output_language")} caption={t("modes.output_language_hint")}>
          <LanguageChoice value={draft.output_language} languages={languages} onChange={(output_language) => setDraft({ ...draft, output_language })} />
        </EditorRow>
      </div>
      <p className="caption">{t("modes.hotkeys_after_create")}</p>
      <div className="editor-footer">
        <button type="button" className="btn primary" disabled={!complete} onClick={() => onCreate(draft)}>
          {t("modes.create")}
        </button>
        <button type="button" className="btn link" onClick={onCancel}>
          {t("modes.cancel")}
        </button>
      </div>
    </div>
  );
}
