import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { open, save } from "@tauri-apps/plugin-dialog";
import { draftAfterRefusal, mergeReplacements, phraseKey, removeEntry, replaceEntry, rowEntries, sentAfter } from "../replacements";
import { afterRefusal, keepSaved, type Replacement, type SaveError, type Settings, type Update } from "../settings";
import { Card, DELETE_GUARD_MS, IconButton, useFocusAfter, usePressGuard } from "../ui";

const FILE_FILTERS = [{ name: "JSON", extensions: ["json"] }];
/// The text to insert grows with its lines up to this many.
const MOST_LINES = 6;

/// Replacements: a phrase as the engine writes it → the text that goes in instead.
export function ReplacementsPanel({ settings, update }: { settings: Settings; update: Update }) {
  const { t } = useTranslation();
  // The new entry's fields.
  const [draft, setDraft] = useState<Replacement>({ from: "", to: "" });
  const [failure, setFailure] = useState<string | null>(null);
  const list = settings.replacements;
  const deleteGuard = usePressGuard();
  const phrase = useRef<HTMLInputElement>(null);
  // Rows are keyed by place: a deletion takes away the last row, whichever entry went. Its
  // focus goes to the row now last, else to the new phrase.
  const follow = useFocusAfter();
  const rows = useRef<HTMLDivElement>(null);
  const lastRowDelete = () => {
    const buttons = rows.current?.querySelectorAll<HTMLElement>(".icon-button");
    return buttons && buttons.length > 0 ? buttons[buttons.length - 1] : phrase.current;
  };

  function change(next: (current: Replacement[]) => Replacement[]): Promise<SaveError | null> {
    return update((current) => ({ replacements: next(current.replacements) }));
  }

  function add() {
    if (!phraseKey(draft.from)) return;
    const sent = draft;
    // A refused entry comes back into the fields, unless the next one is being typed there.
    void keepSaved(change((current) => mergeReplacements(current, [{ from: sent.from.trim(), to: sent.to }])), () =>
      setDraft((shown) => draftAfterRefusal(shown, sent)),
    );
    setDraft({ from: "", to: "" });
    // "Add" is disabled with the phrase emptied, which would throw the focus out: the next
    // entry begins in the phrase field.
    phrase.current?.focus();
  }

  async function exportList() {
    try {
      const path = await save({ defaultPath: "ahem-replacements.json", filters: FILE_FILTERS });
      if (!path) return;
      await invoke("export_replacements", { path });
      setFailure(null);
    } catch (error) {
      setFailure(t("replacements.export_failed", { detail: String(error) }));
    }
  }

  async function importList() {
    try {
      const path = await open({ multiple: false, directory: false, filters: FILE_FILTERS });
      if (typeof path !== "string") return;
      const added = await invoke<Replacement[]>("import_replacements", { path });
      void change((current) => mergeReplacements(current, added));
      setFailure(null);
    } catch (error) {
      setFailure(t("replacements.import_failed", { detail: String(error) }));
    }
  }

  return (
    <Card
      title={t("replacements.title")}
      action={
        <span className="control-group">
          <button type="button" className="btn link" onClick={importList}>
            {t("replacements.import")}
          </button>
          <button type="button" className="btn link" disabled={list.length === 0} onClick={exportList}>
            {t("replacements.export")}
          </button>
        </span>
      }
    >
      <p className="caption card-note">
        {t("replacements.hint")}
        {failure && (
          <span className="warning-text" role="alert">
            {" "}
            {failure}
          </span>
        )}
      </p>
      <div ref={rows} className="replacements">
        <div className="replacement" aria-hidden="true">
          <span className="caption">{t("replacements.from")}</span>
          <span />
          <span className="caption">{t("replacements.to")}</span>
          <span />
        </div>
        {/* Keyed by place, so a row keeps the focus while its phrase is saved; changes find
            their entry by what it holds. */}
        {list.map((item, index) => (
          <ReplacementRow
            key={index}
            item={item}
            onChange={(shown, next) => change((current) => replaceEntry(current, shown, next))}
            onDelete={(shown) =>
              deleteGuard.press(DELETE_GUARD_MS, () => {
                follow(lastRowDelete);
                void change((current) => removeEntry(current, shown));
              })
            }
          />
        ))}
        <div className="replacement">
          <input
            ref={phrase}
            className="text-input"
            aria-label={t("replacements.from")}
            placeholder={t("replacements.from_placeholder")}
            value={draft.from}
            onChange={(event) => {
              const from = event.currentTarget.value;
              setDraft((shown) => ({ ...shown, from }));
            }}
          />
          <span className="replacement-arrow" aria-hidden="true">
            →
          </span>
          <textarea
            aria-label={t("replacements.to")}
            placeholder={t("replacements.to_placeholder")}
            rows={lines(draft.to)}
            value={draft.to}
            onChange={(event) => {
              const to = event.currentTarget.value;
              setDraft((shown) => ({ ...shown, to }));
            }}
          />
          <button type="button" className="btn" disabled={!phraseKey(draft.from)} onClick={add}>
            {t("replacements.add")}
          </button>
        </div>
      </div>
    </Card>
  );
}

function ReplacementRow({
  item,
  onChange,
  onDelete,
}: {
  item: Replacement;
  /// Changes the row's entry, found as one of `shown` (`rowEntries`); resolves with the save's
  /// error, or `null` once saved.
  onChange: (shown: Replacement[], next: Replacement) => Promise<SaveError | null>;
  onDelete: (shown: Replacement[]) => void;
}) {
  const { t } = useTranslation();
  const [from, setFrom] = useState(item.from);
  const [to, setTo] = useState(item.to);
  // A save made elsewhere, such as an import, shows here.
  useEffect(() => setFrom(item.from), [item.from]);
  useEffect(() => setTo(item.to), [item.to]);
  // What the row sent and has not seen saved yet, the latest last: a delete pressed right after
  // a change, before the list is saved, looks for the changed entry too.
  const sent = useRef<Replacement[]>([]);
  useEffect(() => {
    sent.current = sentAfter(sent.current, item);
  }, [item.from, item.to]);

  function commit() {
    const latest = sent.current[sent.current.length - 1] ?? item;
    // A phrase cannot be emptied: the saved one comes back; deleting is the button's job.
    if (!phraseKey(from)) return setFrom(latest.from);
    const next = { from: from.trim(), to };
    if (next.from === latest.from && next.to === latest.to) return;
    const entries = rowEntries(sent.current, item);
    sent.current = [...sent.current, next];
    // A refused row goes back to what it held, but for what was typed since: its own save follows.
    void keepSaved(onChange(entries, next), () => {
      sent.current = sent.current.filter((entry) => entry !== next);
      setFrom((shown) => afterRefusal(shown, from, latest.from));
      setTo((shown) => afterRefusal(shown, to, latest.to));
    });
  }

  return (
    <div className="replacement">
      <input
        className="text-input"
        aria-label={t("replacements.from")}
        value={from}
        onChange={(event) => setFrom(event.currentTarget.value)}
        onBlur={commit}
      />
      <span className="replacement-arrow" aria-hidden="true">
        →
      </span>
      <textarea
        aria-label={t("replacements.to")}
        placeholder={t("replacements.removes")}
        rows={lines(to)}
        value={to}
        onChange={(event) => setTo(event.currentTarget.value)}
        onBlur={commit}
      />
      <IconButton icon="trash" label={t("replacements.delete")} onClick={() => onDelete(rowEntries(sent.current, item))} />
    </div>
  );
}

function lines(text: string): number {
  return Math.min(MOST_LINES, text.split("\n").length);
}
