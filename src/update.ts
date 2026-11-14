import { useState } from "react";
import type { TFunction } from "i18next";
import { invoke } from "@tauri-apps/api/core";
import { CORE_EVENT, useEvent, type CoreEvent, type UpdateEvent } from "./events";
import { runningPercent } from "./present";

/// Whether the app can check for, download and install new versions: the backend registers
/// `install_update` and sends `CoreEvent::Update` events. A switch must do something, so a test
/// keeps this in step with the command's registration.
export const UPDATES_READY = true;

/// What the window and the quick panel show about a new version: nothing, a version to install,
/// its installation, the version just installed, or an installation that failed. No system
/// notifications.
export type UpdateState =
  | { kind: "none" }
  | { kind: "available"; version: string; urgent: boolean; ready: boolean }
  | { kind: "installing"; version: string; share: number | null }
  | { kind: "installed"; version: string; notes: NotesBlock[]; page: string | null }
  | { kind: "failed"; version: string };

export const NO_UPDATE: UpdateState = { kind: "none" };

export function reduceUpdate(state: UpdateState, event: UpdateEvent): UpdateState {
  switch (event.type) {
    case "available":
      // Shown once it can be installed with one press, or when nothing downloads by itself.
      return state.kind === "installing"
        ? state
        : { kind: "available", version: event.version, urgent: event.urgent, ready: event.ready };
    case "progress":
      return { kind: "installing", version: event.version, share: event.total ? Math.min(1, event.done / event.total) : null };
    case "installed":
      return { kind: "installed", version: event.version, notes: event.notes ? notesBlocks(event.notes) : [], page: event.page };
    case "failed":
      return { kind: "failed", version: event.version };
  }
}

/// The card's title and the line under it.
export function updateText(state: Exclude<UpdateState, { kind: "none" }>, t: TFunction): { title: string; note: string | null } {
  switch (state.kind) {
    case "available":
      // Ready: downloaded and installed with one press. Not downloaded yet, the version stands
      // alone, unless it is an important fix.
      return {
        title: t("update.available", { version: state.version }),
        note: state.urgent ? t("update.urgent") : state.ready ? t("update.ready") : null,
      };
    case "installing":
      // The share done; at the whole of it what is left is the restart.
      return {
        title:
          state.share === null
            ? t("update.installing")
            : state.share >= 1
              ? t("update.restarting")
              : t("update.installing_share", { percent: runningPercent(state.share) }),
        note: t("update.restarts"),
      };
    case "installed":
      return { title: t("update.installed", { version: state.version }), note: null };
    case "failed":
      return { title: t("update.failed", { version: state.version }), note: t("update.failed_note") };
  }
}

/// What the card tells screen readers as it changes (`role="status"`): its title, without the
/// share done while installing, which the progress bar tells; a live title would be read out at
/// every percent.
export function updateSaid(state: Exclude<UpdateState, { kind: "none" }>, t: TFunction): string {
  if (state.kind === "installing" && state.share !== null && state.share < 1) return t("update.installing");
  return updateText(state, t).title;
}

/// The notes of a version as the "What's new" dialog shows them: items (`- …`) and paragraphs,
/// in order; blank lines dropped.
export type NotesBlock = { kind: "items"; items: string[] } | { kind: "text"; text: string };

export function notesBlocks(notes: string): NotesBlock[] {
  const blocks: NotesBlock[] = [];
  for (const raw of notes.split("\n")) {
    const line = raw.trim();
    if (!line) continue;
    const item = /^[-*•]\s+(.*)$/.exec(line);
    const last = blocks[blocks.length - 1];
    if (item && last?.kind === "items") last.items.push(item[1]);
    else if (item) blocks.push({ kind: "items", items: [item[1]] });
    else blocks.push({ kind: "text", text: line });
  }
  return blocks;
}

/// "Update" pressed: the installation shows at once, so a second press cannot start another, and
/// a refused command shows as a failure. What follows an accepted one comes as events.
export function startInstall(
  version: string,
  change: (next: (state: UpdateState) => UpdateState) => void,
  command: () => Promise<unknown> = () => invoke("install_update"),
): Promise<void> {
  change((state) => reduceUpdate(state, { type: "progress", version, done: 0, total: null }));
  return command().then(
    () => {},
    (error: unknown) => change((state) => reduceUpdate(state, { type: "failed", version, detail: String(error) })),
  );
}

/// The update shown in this window; `dismiss` hides "updated to" and a failure.
export function useUpdate(): { state: UpdateState; install: (version: string) => void; dismiss: () => void } {
  const [state, setState] = useState<UpdateState>(NO_UPDATE);
  useEvent<CoreEvent>(CORE_EVENT, (payload) => {
    if (payload.source === "update") setState((current) => reduceUpdate(current, payload.event));
  });
  const install = (version: string) => void startInstall(version, setState);
  return { state, install, dismiss: () => setState(NO_UPDATE) };
}
