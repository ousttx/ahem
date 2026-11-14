import { memo, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";
import { invoke } from "@tauri-apps/api/core";
import { CORE_EVENT, useEvent, type CoreEvent } from "../events";
import { commandFailure, latestOnly, modeName } from "../present";
import { BUILTIN_MODES, HISTORY_LIMIT_DEFAULT, keepSaved, parseHistoryLimit, type Mode, type SaveError, type Settings, type Update } from "../settings";
import {
  Card,
  DELETE_GUARD_MS,
  focusLost,
  focusWithin,
  IconButton,
  isComposing,
  Row,
  rowAfterRemoval,
  Segments,
  shownTitle,
  Toggle,
  useConfirm,
  useCopied,
  useFocusAfter,
  useFresh,
  usePressGuard,
} from "../ui";

/// Mirrors `storage::history::Entry`.
export interface Entry {
  id: number;
  created_at_ms: number;
  mode_id: string;
  language: string | null;
  transcript: string;
  processed: string | null;
  inserted: boolean;
}

/// The dictation events after which a history list reads the history again.
export function historyRead(type: string): boolean {
  return type === "history_saved" || type === "history_changed";
}

/// The name of the mode an entry was dictated with: a built-in mode by its name also while it is
/// off; `null` for an own mode deleted since, whose id says nothing to the user.
export function entryModeName(t: TFunction, modes: readonly Mode[], id: string): string | null {
  const mode = modes.find((item) => item.id === id);
  if (mode) return modeName(t, mode);
  return BUILTIN_MODES.includes(id) ? t(`modes.builtin.${id}`) : null;
}

/// The entries a deletion takes away (one, or all of them by "Clear") and the entries then.
interface Deletion {
  ids: readonly number[];
  before: readonly number[];
}

/// Where the focus goes once the list read again (`left`) holds none of the deleted entries: the
/// entry now in the place of the deleted one, else the one above it (`rowAfterRemoval`), else the
/// page's title (`"title"`, also after "Clear"); `"wait"` while one of them is still listed.
export function focusAfterDeletion(deletion: Deletion, left: readonly number[]): number | "title" | "wait" {
  if (deletion.ids.some((id) => left.includes(id))) return "wait";
  if (deletion.ids.length !== 1) return "title";
  return rowAfterRemoval(deletion.before, deletion.ids[0], left) ?? "title";
}

interface HistoryView {
  path: string;
  /// There is a file to show or delete.
  file_exists: boolean;
  entries: Entry[];
}

/// The arguments of `get_history`: the newest `count` entries, or all of them (`null`).
export function historyArgs(count: number | null): { limit: number | null } {
  return { limit: count };
}

/// Whether a history list reads now, and whether it is stale after: a change (`changed`) makes
/// it stale; a shown list reads a stale one at once, a hidden one waits until it is shown. The
/// read of a stale list catches up (`catchUp`): what it brings came while nobody watched.
export function historyStep(stale: boolean, shown: boolean, changed: boolean): { read: boolean; stale: boolean; catchUp: boolean } {
  const now = stale || changed;
  return shown && now ? { read: true, stale: false, catchUp: stale } : { read: false, stale: now, catchUp: false };
}

/// What a read of the history gave: the history, or why it failed.
type HistoryAnswer = { history: HistoryView } | { failure: unknown };

/// The stored history, reloaded after every saved dictation, after entries or the file were
/// deleted in any window, and after the limit changes (a smaller one trims the entries); while
/// the list is not `shown` (a page in the background), only once it is (`historyStep`). The reads
/// run beside each other: only the latest one's answer is taken. `count`: only the newest
/// entries, as many; all of them without it. `quiet`: the history shown is the first answer
/// after a read that caught up, whose new entries are not new to the eye (`useFresh`).
export function useHistory(limit: number | null, count: number | null = null, shown = true) {
  const [history, setHistory] = useState<HistoryView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [quiet, setQuiet] = useState(false);
  // A read that catches up has begun and no answer has come since.
  const catchingUp = useRef(false);

  const take = useMemo(
    () =>
      latestOnly<HistoryAnswer>((answer) => {
        if ("history" in answer) {
          setHistory(answer.history);
          setQuiet(catchingUp.current);
          catchingUp.current = false;
          setError(null);
        } else {
          setError(String(answer.failure));
        }
      }),
    [],
  );
  const load = useCallback(
    () =>
      take(
        invoke<HistoryView>("get_history", historyArgs(count)).then(
          (history): HistoryAnswer => ({ history }),
          (failure): HistoryAnswer => ({ failure }),
        ),
      ),
    [count, take],
  );

  // Nothing is read yet: the first showing reads.
  const stale = useRef(true);
  const visible = useRef(shown);
  visible.current = shown;
  const step = useCallback(
    (changed: boolean) => {
      const next = historyStep(stale.current, visible.current, changed);
      stale.current = next.stale;
      if (next.catchUp) catchingUp.current = true;
      if (next.read) void load();
    },
    [load],
  );

  useEvent<CoreEvent>(CORE_EVENT, (payload) => {
    if (payload.source === "dictation" && historyRead(payload.event.type)) step(true);
  });
  useEffect(() => step(true), [limit, step]);
  useEffect(() => step(false), [shown, step]);

  return { history, error, load, quiet };
}

interface Props {
  settings: Settings;
  update: Update;
  /// The page is the one shown, in a window that is seen: otherwise it reads the history only
  /// once shown again.
  shown: boolean;
}

/// The History page: whether and how much to keep, the file, and the entries. Off by default;
/// entries stay until the user deletes them, also after turning history off. Drawn again only
/// when its settings change or it is shown or hidden, not with every level of a recording.
export const HistoryPanel = memo(function HistoryPanel({ settings, update, shown }: Props) {
  const { t, i18n } = useTranslation();
  const { history, error: loadError, load, quiet } = useHistory(settings.history_limit, null, shown);
  // Where the file is, read once as the page is made (no entries): the header has its row before
  // the page is first shown, so it does not grow above the entries as they come.
  const [place, setPlace] = useState<Pick<HistoryView, "path" | "file_exists"> | null>(null);
  useEffect(() => {
    invoke<HistoryView>("get_history", historyArgs(0)).then(setPlace, () => {});
  }, []);
  const file = history ?? place;
  const [error, setError] = useState<string | null>(null);
  const [clearing, confirmClear] = useConfirm();
  const [copied, copy, copyFailed] = useCopied<string>();
  const [fileFailure, setFileFailure] = useState<string | null>(null);
  const [deletingFile, confirmDeleteFile] = useConfirm();
  // A deleted file leaves its button disabled: the focus goes to "Show file".
  const follow = useFocusAfter();
  const showFile = useRef<HTMLButtonElement>(null);
  const deleteGuard = usePressGuard();
  // What is being deleted: once it is gone, the focus that went with its button (a row's, or
  // "Clear" below the rows) goes where `focusAfterDeletion` says.
  const removed = useRef<Deletion | null>(null);
  const list = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const deletion = removed.current;
    const left = history?.entries.map((entry) => entry.id);
    if (!deletion || !left) return;
    const next = focusAfterDeletion(deletion, left);
    if (next === "wait") return;
    removed.current = null;
    if (!focusLost()) return;
    const target =
      next === "title"
        ? shownTitle(list.current?.closest(".page") ?? document)
        : list.current?.querySelector<HTMLElement>(`[data-entry="${next}"] .history-actions > button:last-child`);
    target?.focus();
  }, [history]);

  async function deleteHistoryFile() {
    setFileFailure(await commandFailure(invoke("delete_history_file"), (detail) => t("history.delete_failed", { detail })));
    void load();
  }

  async function run(command: string, args?: Record<string, unknown>) {
    try {
      await invoke(command, args);
      setError(null);
      await load();
    } catch (failure) {
      removed.current = null;
      setError(t("history.delete_failed", { detail: String(failure) }));
    }
  }

  function deleteEntry(id: number) {
    removed.current = { ids: [id], before: entries.map((entry) => entry.id) };
    void run("delete_history_entry", { id });
  }

  function clearHistory() {
    const before = entries.map((entry) => entry.id);
    removed.current = { ids: before, before };
    void run("clear_history");
  }

  const dates = useMemo(() => new Intl.DateTimeFormat(i18n.language, { dateStyle: "short", timeStyle: "short" }), [i18n.language]);
  const entries = history?.entries ?? [];
  // A new dictation pushes the list apart and lights up for a moment; the ones that came while
  // the page was not seen are there at once as it is shown.
  const fresh = useFresh(
    entries.map((entry) => entry.id),
    history !== null,
    quiet,
  );

  return (
    <>
      <Card>
        <Row
          separator={false}
          title={t("history.enabled")}
          detail={t("history.note")}
          control={<Toggle label={t("history.enabled")} checked={settings.history_enabled} onChange={(history_enabled) => update({ history_enabled })} />}
        />
        <Row
          title={t("history.keep")}
          control={<HistoryLimit limit={settings.history_limit} onChange={(history_limit) => update({ history_limit })} />}
        />
        {file && (
          <Row
            title={t("history.file")}
            detail={
              <>
                <span className="selectable">{file.path}</span>
                {fileFailure && (
                  <span className="warning-text" role="alert">
                    {" "}
                    {fileFailure}
                  </span>
                )}
              </>
            }
            control={
              <>
                <button
                  ref={showFile}
                  type="button"
                  className="btn link"
                  onClick={() => {
                    const opening = invoke("open_data_folder", { folder: "history" });
                    void commandFailure(opening, (detail) => t("models.open_failed", { detail })).then(setFileFailure);
                  }}
                >
                  {t("history.show_file")}
                </button>
                <button
                  type="button"
                  className="btn link quiet"
                  disabled={!file.file_exists}
                  onClick={() =>
                    confirmDeleteFile(() => {
                      follow(() => showFile.current, { disabledIsGone: true });
                      void deleteHistoryFile();
                    })
                  }
                >
                  {deletingFile ? t("history.confirm_delete_file") : t("history.delete_file")}
                </button>
              </>
            }
          />
        )}
      </Card>
      <Card className="history-list">
        <div ref={list} className="history-body">
          {/* Not before the first read: a page not shown yet has read nothing. */}
          {history !== null && entries.length === 0 && (
            <p className="caption">{t(settings.history_enabled ? "history.empty" : "history.off")}</p>
          )}
          {entries.map((entry) => (
            <div key={entry.id} data-entry={entry.id} className={fresh.has(entry.id) ? "history-entry fresh" : "history-entry"}>
              <div className="history-entry-body">
                <div className="caption">
                  {[dates.format(entry.created_at_ms), entryModeName(t, settings.modes, entry.mode_id), !entry.inserted && t("history.not_inserted")]
                    .filter(Boolean)
                    .join(" · ")}
                </div>
                <p className="history-text">{entry.processed ?? entry.transcript}</p>
                {entry.processed !== null && (
                  <p className="history-text caption">
                    {t("history.recognized")}: {entry.transcript}
                  </p>
                )}
                <div className="history-actions">
                  <IconButton
                    icon={copied === `${entry.id}` ? "check" : "copy"}
                    label={t("last.copy")}
                    size={26}
                    onClick={() => copy(`${entry.id}`, entry.processed ?? entry.transcript)}
                  />
                  {entry.processed !== null && (
                    <button type="button" className="btn link" onClick={() => copy(`${entry.id}-raw`, entry.transcript)}>
                      {copied === `${entry.id}-raw` ? t("last.copied") : t("history.copy_recognized")}
                    </button>
                  )}
                  <IconButton
                    icon="trash"
                    label={t("history.delete")}
                    size={26}
                    onClick={() => deleteGuard.press(DELETE_GUARD_MS, () => deleteEntry(entry.id))}
                  />
                </div>
                {(copyFailed === `${entry.id}` || copyFailed === `${entry.id}-raw`) && (
                  <p className="caption warning-text" role="alert">
                    {t("last.copy_failed")}
                  </p>
                )}
              </div>
            </div>
          ))}
          {entries.length > 0 && (
            <div className="history-footer">
              <button type="button" className="btn link quiet" onClick={() => confirmClear(clearHistory)}>
                {clearing ? t("history.confirm_clear") : t("history.clear")}
              </button>
            </div>
          )}
          {(error || loadError) && (
            <p className="caption warning-text" role="alert">
              {error ?? t("history.load_failed", { detail: loadError })}
            </p>
          )}
        </div>
      </Card>
    </>
  );
});

/// What a choice in "How many to keep" does, with `limit` saved: "All" saves `null` when a number
/// is saved or on its way (`saving`: it would land after the drop), and drops a "Last" not saved
/// yet (`choosing`); a number typed and not saved goes with either. "Last" with "All" saved only
/// shows the field with a number (`"choose"`), and nothing is saved until a number is (`limitStep`).
export function limitChoice(
  value: "last" | "all",
  limit: number | null,
  choosing: boolean,
  saving: boolean,
): { save: null } | "choose" | "drop" | "none" {
  if (value === "all") return limit !== null || saving ? { save: null } : choosing ? "drop" : "none";
  return limit === null && !choosing ? "choose" : "none";
}

/// Whether the focus going away from "How many to keep" leaves it, so the number typed in its
/// field is saved (`limitStep`): not while it stays `within` (the field and its choice are one
/// control: "All" reached from the field drops the number), nor when the window lost it
/// (`windowFocused` false: another window or app was chosen, and the focus comes back to the field
/// with the window).
export function limitLeft(within: boolean, windowFocused: boolean): boolean {
  return !within && windowFocused;
}

/// What the field of "Last" does with its `text` on Enter (`enter`) or as it is left: save a
/// number, or show a number again. With "All" saved (`limit` `null`), "Last" was just chosen with
/// `offered` in the field: Enter saves the number shown, leaving saves only another one, and
/// leaving without one goes back to "All" (`"all"`), so no dictation is deleted unasked.
export function limitStep(text: string, limit: number | null, offered: number, enter: boolean): { save: number } | { show: string } | "all" {
  const typed = parseHistoryLimit(text);
  if (limit !== null) {
    if (typed === null) return { show: String(limit) };
    return typed === limit ? { show: String(typed) } : { save: typed };
  }
  if (typed === null) return enter ? { show: String(offered) } : "all";
  return !enter && typed === offered ? "all" : { save: typed };
}

/// "Last" with a number the user types, or "All". The number is saved on Enter or when the
/// control is left inside the window (`limitLeft`); a wrong or refused one goes back to the saved
/// number. A smaller number deletes the older dictations. "Last" chosen with "All" saved only shows
/// the field, with the focus in it; Escape goes back to "All" (`limitChoice`, `limitStep`).
function HistoryLimit({ limit, onChange }: { limit: number | null; onChange: (limit: number | null) => Promise<SaveError | null> }) {
  const { t } = useTranslation();
  const [text, setText] = useState(String(limit ?? HISTORY_LIMIT_DEFAULT));
  // The number "Last" comes back with after "All".
  const [last, setLast] = useState(limit ?? HISTORY_LIMIT_DEFAULT);
  // "Last" chosen while "All" is saved, no number saved yet.
  const [choosing, setChoosing] = useState(false);
  const field = useRef<HTMLInputElement>(null);
  const box = useRef<HTMLSpanElement>(null);
  // The number in the field was dropped ("All", Escape, a refusal): the blur of the field going
  // away saves nothing.
  const dropped = useRef(false);
  // Saves of a number not answered yet.
  const saving = useRef(0);
  // The field that goes away gives the focus to the choice: "All" then.
  const follow = useFocusAfter();
  const checked = () => box.current?.querySelector<HTMLElement>('[role="radio"][aria-checked="true"]');
  useEffect(() => {
    if (limit !== null) {
      setText(String(limit));
      setLast(limit);
    }
    setChoosing(false);
  }, [limit]);
  useEffect(() => {
    if (!choosing) return;
    field.current?.focus();
    field.current?.select();
  }, [choosing]);

  function backToAll() {
    setChoosing(false);
    setText(String(last));
  }

  function choose(value: "last" | "all") {
    const choice = limitChoice(value, limit, choosing, saving.current > 0);
    if (choice === "choose") {
      setText(String(last));
      setChoosing(true);
    } else if (choice !== "none") {
      dropped.current = true;
      if (document.activeElement === field.current) follow(checked);
      backToAll();
      if (choice !== "drop") void onChange(choice.save);
    }
  }

  function commit(enter: boolean) {
    const step = limitStep(text, limit, last, enter);
    if (step === "all") return backToAll();
    if ("show" in step) return setText(step.show);
    setText(String(step.save));
    saving.current += 1;
    void keepSaved(onChange(step.save), () => {
      // A "Last" not saved yet goes away with its field, as on Escape.
      if (limit === null) {
        dropped.current = true;
        follow(checked);
      }
      setChoosing(false);
      setText(String(limit ?? last));
    }).finally(() => {
      saving.current -= 1;
    });
  }

  return (
    <span
      ref={box}
      className="history-limit"
      // A press on the choice keeps the focus where it is in the control: leaving the field would
      // save the number typed in it before the choice is made, and "All" drops it.
      onMouseDown={(event) => {
        const radio = (event.target as HTMLElement).closest<HTMLElement>('[role="radio"]');
        if (!radio || !focusWithin(box.current, document.activeElement)) return;
        event.preventDefault();
        if (document.activeElement !== field.current) radio.focus();
      }}
      onBlur={(event) => {
        if (dropped.current || (limit === null && !choosing)) return;
        if (limitLeft(focusWithin(box.current, event.relatedTarget), document.hasFocus())) commit(false);
      }}
    >
      <Segments
        label={t("history.keep")}
        value={limit === null && !choosing ? "all" : "last"}
        options={[
          { value: "last", label: t("history.last_label") },
          { value: "all", label: t("history.unlimited") },
        ]}
        onChange={choose}
      />
      {(limit !== null || choosing) && (
        <input
          ref={field}
          className="text-input history-limit-number tabular"
          aria-label={t("history.last_number")}
          inputMode="numeric"
          value={text}
          onChange={(event) => {
            dropped.current = false;
            setText(event.currentTarget.value);
          }}
          onFocus={() => {
            dropped.current = false;
          }}
          // Enter keeps the focus in the field. Escape puts the saved number back, so leaving the
          // field then commits nothing (a smaller number deletes dictations), or drops a "Last"
          // not saved yet.
          onKeyDown={(event) => {
            if (isComposing(event)) return;
            if (event.key === "Enter") commit(true);
            if (event.key !== "Escape") return;
            if (!choosing) return setText(String(limit ?? last));
            dropped.current = true;
            follow(checked);
            backToAll();
          }}
        />
      )}
    </span>
  );
}
