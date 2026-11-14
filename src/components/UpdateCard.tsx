import { useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { openUrl } from "@tauri-apps/plugin-opener";
import { updateSaid, updateText, useUpdate, type NotesBlock, type UpdateState } from "../update";
import { IconButton, Modal, Progress } from "../ui";

/// A new version in the menu of the window (`nav`) or in the quick panel (`quick`): to install
/// with one press, its installation, the version just installed with its "What's new", or a
/// failure. Nothing when there is no update.
export function UpdateCard({ variant }: { variant: "nav" | "quick" }) {
  const { t } = useTranslation();
  const { state, install, dismiss } = useUpdate();
  // "What's new" belongs to the state it was opened from: any change of the update closes it.
  const [notesOf, setNotesOf] = useState<UpdateState | null>(null);
  if (state.kind === "none") return null;

  const { title, note } = updateText(state, t);
  const said = updateSaid(state, t);
  let action: ReactNode = null;
  switch (state.kind) {
    case "available":
      action = (
        <button type="button" className="btn compact primary" onClick={() => install(state.version)}>
          {t("update.install")}
        </button>
      );
      break;
    case "installed":
      if (state.notes.length > 0 || state.page) {
        action = (
          <button type="button" className="btn link" onClick={() => (state.notes.length > 0 ? setNotesOf(state) : openPage(state.page))}>
            {t("update.whats_new")}
          </button>
        );
      }
      break;
    case "failed":
      action = (
        <button type="button" className="btn compact" onClick={() => install(state.version)}>
          {t("update.retry")}
        </button>
      );
      break;
  }
  const closable = state.kind === "installed" || state.kind === "failed";
  const close = closable && <IconButton icon="close" label={t("update.close")} size={22} onClick={dismiss} />;

  // In the menu the close button stands by the title and the action below; in the quick panel
  // both stand at the right, the action first.
  return (
    <>
      <div className={`update-card update-${variant}`}>
        <span className="update-head">
          {state.kind === "available" && <span className="update-dot" aria-hidden="true" />}
          {/* Screen readers are told the title without the share done (`updateSaid`): the
              progress bar has it. */}
          <span className="update-title" role="status">
            <span className="sr-only">{said}</span>
            <span aria-hidden="true">{title}</span>
          </span>
          {variant === "nav" && close}
        </span>
        {state.kind === "installing" && <Progress value={state.share ?? 0} indeterminate={state.share === null} label={said} />}
        {note && <span className="caption">{note}</span>}
        {variant === "nav" ? (
          action
        ) : (
          (action || close) && (
            <span className="update-actions">
              {action}
              {close}
            </span>
          )
        )}
      </div>
      {notesOf === state && state.kind === "installed" && (
        <WhatsNew version={state.version} notes={state.notes} page={state.page} inline={variant === "quick"} onClose={() => setNotesOf(null)} />
      )}
    </>
  );
}

function openPage(page: string | null) {
  if (page) void openUrl(page).catch(() => {});
}

/// The changes of the version just installed, over the window, or inside the quick panel
/// (`inline`); Escape or a click beside it closes it.
function WhatsNew({
  version,
  notes,
  page,
  inline,
  onClose,
}: {
  version: string;
  notes: NotesBlock[];
  page: string | null;
  inline: boolean;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  return (
    <Modal labelledBy="whats-new-title" inline={inline} onClose={onClose}>
      <h3 id="whats-new-title" className="card-title">
        {t("update.whats_new_title", { version })}
      </h3>
      <div className="whats-new-notes selectable">
        {notes.map((block, index) =>
          block.kind === "items" ? (
            <ul key={index}>
              {block.items.map((item, at) => (
                <li key={at}>{item}</li>
              ))}
            </ul>
          ) : (
            <p key={index}>{block.text}</p>
          ),
        )}
      </div>
      <div className="modal-actions">
        {page && (
          <button type="button" className="btn link quiet" onClick={() => openPage(page)}>
            {t("update.all_changes")}
          </button>
        )}
        <button type="button" className="btn primary" autoFocus onClick={onClose}>
          {t("update.got_it")}
        </button>
      </div>
    </Modal>
  );
}
