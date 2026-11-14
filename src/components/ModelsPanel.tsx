import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";
import { invoke } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";
import { shortcutModifierNames, type Download } from "../dictation";
import { CORE_EVENT, useEvent, type CoreEvent } from "../events";
import { localizedLanguages, useLanguageList, type LanguageInfo } from "../languages";
import { commandFailure, failureText, gigabytes, runningPercent, saveErrorText } from "../present";
import type { SaveError } from "../settings";
import { Card, clipTitle, Progress, Radio, rowAfterRemoval, shownTitle, useConfirmKey, useFocusAfter, useRoving } from "../ui";

type ModelState = { kind: "missing" } | { kind: "partial"; downloaded: number } | { kind: "ready" };

/// What the model needs to be fast enough: a graphics card with this much memory, or nothing.
type Hardware = { kind: "gpu"; memory_gb: number } | { kind: "cpu" };

/// Mirrors `commands::ModelInfo`.
export interface ModelInfo {
  id: string;
  name: string;
  /// Who made the model and its license, which the page credits.
  author: string;
  license: string;
  /// Where the files are downloaded from (pinned to a revision).
  url: string;
  /// Bytes to download and to keep: a llama.cpp model's projector counts too.
  download_size: number;
  translate: boolean;
  hardware: Hardware;
  /// Present for a model llama.cpp runs: it uses the graphics card when there is one.
  llama?: object;
  state: ModelState;
  /// Codes of the spoken languages it recognizes.
  languages: string[];
  /// Its engine reads the dictionary.
  takes_dictionary: boolean;
  /// Its engine takes the chosen spoken language instead of always detecting it.
  takes_language: boolean;
}

/// Who made a model and under which license, in the tooltip of its name: some licenses, such as
/// CC-BY-4.0, ask for that credit.
export function modelCredit(t: TFunction, model: Pick<ModelInfo, "author" | "license">): string {
  return t("models.credit", { author: model.author, license: model.license });
}

/// Which badge a model gets for its hardware: the card it needs; or none needed and none used
/// (`cpu`: sherpa-onnx runs on the processor alone); or none needed but used when there is one
/// (`gpu_optional`: llama.cpp offloads to the card by itself). Names the keys `models.hardware.*`.
export function hardwareBadge(model: Pick<ModelInfo, "hardware" | "llama">): "gpu" | "cpu" | "gpu_optional" {
  if (model.hardware.kind === "gpu") return "gpu";
  return model.llama ? "gpu_optional" : "cpu";
}

/// Where a model is downloaded from, for the line under its name: the place to read (the
/// repository on Hugging Face, the release of the project on GitHub) and the page that opens.
export function modelSource(url: string): { label: string; page: string } | null {
  let address: URL;
  try {
    address = new URL(url);
  } catch {
    return null;
  }
  const [owner, repository, section, kind, tag] = address.pathname.split("/").filter(Boolean);
  const host = address.hostname;
  if (host === "huggingface.co" && repository) {
    return { label: `${host}/${owner}/${repository}`, page: `https://${host}/${owner}/${repository}` };
  }
  if (host === "github.com" && repository && section === "releases" && kind === "download" && tag) {
    return { label: `${host}/${owner}/${repository}`, page: `https://${host}/${owner}/${repository}/releases/tag/${tag}` };
  }
  return { label: host, page: `https://${host}` };
}

/// The models being downloaded now, by id: a failed download runs no more.
export function runningDownloads(downloads: Record<string, Download>): string[] {
  return Object.keys(downloads)
    .filter((id) => !downloads[id].failure)
    .sort();
}

/// Downloads that ended (done, failed or cancelled) while the list shown still has the model as
/// it was before: each with the number of the first list read after its end.
export type EndedDownloads = Readonly<Record<string, number>>;

/// `ended` after the downloads of `before` that do not run `now` ended; the next list read is
/// `next`.
export function downloadsEnded(ended: EndedDownloads, before: readonly string[], now: readonly string[], next: number): EndedDownloads {
  const gone = before.filter((id) => !now.includes(id));
  return gone.length === 0 ? ended : { ...ended, ...Object.fromEntries(gone.map((id) => [id, next])) };
}

/// `ended` once the list read as `read` came: it knows how the downloads that ended before it
/// was read left their models.
export function listRead(ended: EndedDownloads, read: number): EndedDownloads {
  return Object.fromEntries(Object.entries(ended).filter(([, next]) => next > read));
}

/// The model catalog with download states, reloaded when a download starts or ends and by
/// `reload`. With no model chosen yet, the first ready one becomes the dictation model.
/// `settling`: the models whose download ended before the list says how it left them; their rows
/// stay where they are until it does.
export function useModels(selected: string | null, downloads: Record<string, Download>, onUse: (id: string) => void) {
  const [models, setModels] = useState<ModelInfo[]>([]);
  const ids = runningDownloads(downloads);
  const running = ids.join();
  // The lists asked for so far: the read after a download ended is the next one.
  const asked = useRef(0);
  const [ended, setEnded] = useState<EndedDownloads>({});
  // Taken in the drawing where the download is gone, so its row never shows a frame elsewhere.
  const [seen, setSeen] = useState(running);
  if (seen !== running) {
    setSeen(running);
    setEnded((current) => downloadsEnded(current, seen ? seen.split(",") : [], ids, asked.current + 1));
  }

  const reload = useCallback(async () => {
    const read = ++asked.current;
    // A list that cannot be read leaves the one shown.
    const list = await invoke<ModelInfo[]>("list_models").catch(() => null);
    if (!list) return;
    setModels(list);
    setEnded((current) => listRead(current, read));
    const ready = list.filter((model) => model.state.kind === "ready");
    if (!selected && ready.length > 0) onUse(ready[0].id);
  }, [selected, onUse]);

  useEffect(() => {
    void reload();
  }, [running, reload]);

  const settling = useMemo(() => new Set(Object.keys(ended)), [ended]);
  return { models, reload, settling };
}

/// `connecting` without `model`: its download heard back, or its command failed.
function without(connecting: ReadonlySet<string>, model: string): ReadonlySet<string> {
  if (!connecting.has(model)) return connecting;
  const left = new Set(connecting);
  left.delete(model);
  return left;
}

/// The downloads asked for that still wait once `payload` came: any event of the model (its
/// first progress, sent once the server answers, or a failure) ends its wait.
export function heardFrom(connecting: ReadonlySet<string>, payload: CoreEvent): ReadonlySet<string> {
  return payload.source === "model" ? without(connecting, payload.event.model) : connecting;
}

/// The models whose download was asked for here and has said nothing yet (`heardFrom`), which
/// may take long on a poor network: meanwhile the row stays where it is, its button busy
/// ("Connecting…"), and a failure from before is hidden. `connect` notes a download asked for;
/// `giveUp`, one whose command failed.
export function useConnecting() {
  const [connecting, setConnecting] = useState<ReadonlySet<string>>(() => new Set());
  useEvent<CoreEvent>(CORE_EVENT, (payload) => setConnecting((current) => heardFrom(current, payload)));
  const connect = useCallback((id: string) => setConnecting((current) => new Set(current).add(id)), []);
  const giveUp = useCallback((id: string) => setConnecting((current) => without(current, id)), []);
  return { connecting, connect, giveUp };
}

/// Models on this computer first (downloaded, partly downloaded, downloading now, or downloaded
/// until the list read again says otherwise: `settling`), then the ones to download, each group
/// in catalog order.
export function splitModels(models: ModelInfo[], downloads: Record<string, Download>, settling: ReadonlySet<string> = new Set()) {
  const here = (model: ModelInfo) =>
    model.state.kind !== "missing" || (downloads[model.id] !== undefined && !downloads[model.id].failure) || settling.has(model.id);
  return { installed: models.filter(here), available: models.filter((model) => !here(model)) };
}

/// Where the focus goes in a row: its first button that can be pressed, else the first one,
/// disabled until the row is ready (followed with `disabledIsGone`).
export function rowFocus<E extends { disabled: boolean }>(buttons: readonly E[]): E | null {
  return buttons.find((button) => !button.disabled) ?? buttons[0] ?? null;
}

/// The followed downloads that have ended with the model ready.
export function installedNow(watched: ReadonlySet<string>, downloads: Record<string, Download>, models: ModelInfo[]): string[] {
  return [...watched].filter((id) => downloads[id] === undefined && models.some((model) => model.id === id && model.state.kind === "ready"));
}

/// Mirrors `DeleteModelError` in `commands.rs`: `settings` carries the `SaveError` of moving
/// dictation to another model.
type DeleteModelError = { kind: "settings"; detail: SaveError } | { kind: string; detail?: string };

/// Why a model was not deleted; a failure that is not a `DeleteModelError` as it is.
export function deleteModelError(t: TFunction, failure: unknown): string {
  const error = failure as Partial<DeleteModelError> | null;
  if (error?.kind === "settings") return t("models.delete_error.settings", { detail: saveErrorText(t, error.detail as SaveError) });
  return failureText(t, failure, "models.delete_error", "models.delete_error.io");
}

/// Deletes a model (`remove`) and reads the list again (`reread`) before its buttons come back
/// (`release`): a deleted model's "Delete" never shows enabled, nor takes the focus back. A
/// failure goes to `fail`; the list is read again all the same.
export async function deleteThenRead(
  remove: () => Promise<unknown>,
  reread: () => Promise<void>,
  fail: (error: unknown) => void,
  release: () => void,
): Promise<void> {
  try {
    await remove();
  } catch (error) {
    fail(error);
  }
  try {
    await reread();
  } finally {
    release();
  }
}

/// The model's languages by their names in the interface language: named and sorted once, not at
/// every drawing of the page.
export function LanguageList({ codes, all }: { codes: string[]; all: LanguageInfo[] }) {
  const { t, i18n } = useTranslation();
  const names = useMemo(() => {
    const known = all.filter((language) => codes.includes(language.code));
    return localizedLanguages(known, i18n.language).map((language) => language.label);
  }, [codes, all, i18n.language]);
  return (
    <details className="languages">
      <summary className="caption">{t("models.languages", { number: codes.length })}</summary>
      <p className="caption">{names.join(", ")}</p>
    </details>
  );
}

interface Props {
  models: ModelInfo[];
  selected: string | null;
  downloads: Record<string, Download>;
  /// Models whose download ended before the list was read again (`useModels`).
  settling: ReadonlySet<string>;
  onUse: (id: string) => void;
  /// Called after a model was deleted; resolves once the list is read again.
  onDeleted: () => Promise<void>;
  /// When the model's memory is freed, right under the models on this computer.
  unload?: ReactNode;
}

/// "On this computer" in the recognition settings.
export function ModelsPanel({ models, selected, downloads, settling, onUse, onDeleted, unload }: Props) {
  const { t, i18n } = useTranslation();
  const languages = useLanguageList();
  const [deleting, confirmDelete] = useConfirmKey<string>();
  const [failure, setFailure] = useState<{ id: string; text: string } | null>(null);
  // Models being deleted: their buttons wait, as the backend takes no download meanwhile.
  const [removing, setRemoving] = useState<ReadonlySet<string>>(() => new Set());
  const [folders, setFolders] = useState<{ models: string; all: string } | null>(null);
  const [folderFailure, setFolderFailure] = useState<string | null>(null);

  // A model downloaded while the app runs gets a badge with a check that draws itself.
  const watched = useRef(new Set<string>());
  const [installedHere, setInstalledHere] = useState<ReadonlySet<string>>(() => new Set());
  useEffect(() => {
    for (const [id, download] of Object.entries(downloads)) if (!download.failure) watched.current.add(id);
    const done = installedNow(watched.current, downloads, models);
    if (done.length === 0) return;
    for (const id of done) watched.current.delete(id);
    setInstalledHere((current) => new Set([...current, ...done]));
  }, [downloads, models]);

  useEffect(() => {
    invoke<{ models: string; all: string }>("data_folders").then(setFolders, () => setFolders(null));
  }, []);

  async function openFolder(folder: "models" | "all") {
    setFolderFailure(null);
    const failure = await commandFailure(invoke("open_data_folder", { folder }), (detail) => t("models.open_failed", { detail }));
    if (failure !== null) setFolderFailure(failure);
  }

  // A deleted model's row goes to the models to download, with the focus on its button: the
  // focus goes to the model now in its place on this computer, else the one above, else the
  // page's title.
  const follow = useFocusAfter();
  const installedRows = useRef<HTMLDivElement>(null);
  const rowsHere = () => Array.from(installedRows.current?.children ?? []).filter((row): row is HTMLElement => row instanceof HTMLElement);

  // A second click confirms. Deleting the dictation model moves dictation to another
  // downloaded model, or to none.
  function remove(id: string) {
    setFailure(null);
    confirmDelete(id, () => {
      const before = rowsHere().map((row) => row.dataset.model);
      const page = installedRows.current?.closest(".page") ?? document;
      follow(() => {
        const rows = rowsHere();
        const next = rowAfterRemoval(before, id, rows.map((row) => row.dataset.model));
        const row = rows.find((item) => item.dataset.model === next);
        return row ? row.querySelector<HTMLElement>("button:not(:disabled)") : shownTitle(page);
      });
      void deleteModel(id);
    });
  }

  async function deleteModel(id: string) {
    setRemoving((ids) => new Set(ids).add(id));
    await deleteThenRead(
      () => invoke("delete_model", { id }),
      onDeleted,
      (error) => setFailure({ id, text: deleteModelError(t, error) }),
      () =>
        setRemoving((ids) => {
          const left = new Set(ids);
          left.delete(id);
          return left;
        }),
    );
  }

  // "Cancel" goes as the download ends, or while an archive is unpacked: the focus it had goes to
  // the row's first button that can be pressed once the list is read again (the radio of a ready
  // model, "Resume", "Delete"); a row back among the models to download, folded, gives it to
  // their summary.
  function toRow(id: string) {
    follow(
      () => {
        const card = installedRows.current?.closest(".card");
        const row = Array.from(card?.querySelectorAll<HTMLElement>("[data-model]") ?? []).find((item) => item.dataset.model === id);
        const more = row?.closest("details");
        if (more && !more.open) return more.querySelector<HTMLElement>("summary");
        return rowFocus(Array.from(row?.querySelectorAll<HTMLButtonElement>("button") ?? []));
      },
      { disabledIsGone: true },
    );
  }

  // A model to download moves to the models on this computer once its download starts (the
  // server answered), and its button goes: the focus goes to its row there, to "Cancel".
  const { connecting, connect, giveUp } = useConnecting();
  function startDownload(id: string) {
    follow(() => {
      const row = rowsHere().find((item) => item.dataset.model === id);
      return row?.querySelector<HTMLElement>(".model-actions button:not(:disabled)") ?? row?.querySelector<HTMLElement>("button:not(:disabled)");
    });
    connect(id);
    void invoke("download_model", { id }).catch(() => giveUp(id));
  }

  /// Downloaded and not being downloaded again: it can be chosen.
  const readyNow = (model: ModelInfo) => model.state.kind === "ready" && !(downloads[model.id] !== undefined && !downloads[model.id].failure);

  /// A row of a model; one that cannot be chosen yet (not downloaded, or downloading again), and
  /// its disabled radio, is out of the Tab order and of the arrows.
  function row(model: ModelInfo, index: number) {
    const download = downloads[model.id];
    const active = download !== undefined && !download.failure;
    // Its download ended, and the list does not say yet how it left the model: no buttons of the
    // state before it.
    const ending = !active && settling.has(model.id);
    const waiting = !active && connecting.has(model.id);
    const done = active ? download.downloaded : model.state.kind === "partial" ? model.state.downloaded : 0;
    const share = model.download_size > 0 ? done / model.download_size : 0;
    const ready = readyNow(model);
    const [a, b, c] = shortcutModifierNames();
    const about = t(`models.about.${model.id}`, { defaultValue: "", a, b, c });
    const source = modelSource(model.url);
    return (
      <div key={model.id} className="model-row" data-model={model.id}>
        <button
          type="button"
          role="radio"
          aria-checked={selected === model.id}
          aria-label={model.name}
          className="radio-button"
          tabIndex={ready ? roving.tabIndex(index) : undefined}
          disabled={!ready}
          onClick={() => onUse(model.id)}
        >
          <Radio checked={ready && selected === model.id} available={ready} />
        </button>
        <div className="model-texts">
          <span className="model-title">
            <span className="strong" title={modelCredit(t, model)}>
              {model.name}
            </span>
            <span className="badge neutral" title={t(`models.hardware.${hardwareBadge(model)}_hint`)}>
              {model.hardware.kind === "gpu"
                ? t("models.hardware.gpu", { memory: model.hardware.memory_gb })
                : t(`models.hardware.${hardwareBadge(model)}`)}
            </span>
          </span>
          <span className="caption">
            {t("models.size", { size: gigabytes(model.download_size, i18n.language) })}
            {about && ` · ${about}`}
            {!model.translate && ` · ${t("models.no_translation")}`}
          </span>
          {source && (
            <span className="caption">
              {t("models.source")}:{" "}
              <button type="button" className="btn link quiet" title={source.page} onClick={() => void openUrl(source.page).catch(() => {})}>
                {source.label}
              </button>
            </span>
          )}
          <LanguageList codes={model.languages} all={languages} />
          {active && (
            <span className="model-progress">
              <Progress value={share} label={model.name} />
              <span className="caption tabular">
                {download.unpacking ? t("models.unpacking") : t("models.partial", { percent: runningPercent(share) })}
              </span>
            </span>
          )}
          {!active && !ending && model.state.kind === "partial" && (
            <span className="caption">{t("models.partial", { percent: runningPercent(share) })}</span>
          )}
          {download?.failure && !waiting && (
            <span className="caption warning-text" role="alert">
              {t(`models.failed.${download.failure}`)}
            </span>
          )}
          {failure?.id === model.id && (
            <span className="caption warning-text" role="alert">
              {failure.text}
            </span>
          )}
        </div>
        <div className="model-actions">
          {(active && download.unpacking) || ending ? null : active ? (
            <button
              type="button"
              className="btn compact"
              onFocus={() => toRow(model.id)}
              onClick={() => void invoke("cancel_model_download", { id: model.id }).catch(() => {})}
            >
              {t("models.cancel")}
            </button>
          ) : model.state.kind !== "ready" ? (
            <button
              type="button"
              className="btn compact"
              disabled={removing.has(model.id) || waiting}
              aria-busy={waiting || undefined}
              onClick={() => startDownload(model.id)}
            >
              {waiting ? t("models.connecting") : model.state.kind === "partial" ? t("models.resume") : t("models.download")}
            </button>
          ) : installedHere.has(model.id) ? (
            <span className="badge neutral installed">
              <svg width="11" height="9" viewBox="0 0 17 12" aria-hidden="true">
                <path pathLength={1} d="M1.5 6l5 5 9-9.5" />
              </svg>
              {t(selected === model.id ? "models.in_use" : "models.installed")}
            </span>
          ) : (
            <span className="caption">{t(selected === model.id ? "models.in_use" : "models.ready")}</span>
          )}
          {!active && !ending && model.state.kind !== "missing" && (
            <button type="button" className="btn link quiet" disabled={removing.has(model.id) || waiting} onClick={() => remove(model.id)}>
              {deleting === model.id
                ? t(selected === model.id ? "models.confirm_delete_in_use" : "models.confirm_delete")
                : t("models.delete")}
            </button>
          )}
        </div>
      </div>
    );
  }

  const { installed, available } = splitModels(models, downloads, settling);
  // Only a downloaded model can be chosen: the models to download have no roving of their own.
  const roving = useRoving(installed.map((model) => ({ checked: selected === model.id, disabled: !readyNow(model) })));

  const used = models.reduce((sum, model) => {
    if (model.state.kind === "ready") return sum + model.download_size;
    if (model.state.kind === "partial") return sum + model.state.downloaded;
    return sum;
  }, 0);

  return (
    <Card>
      <div className="section-header">
        <span className="section-label">{t("models.title")}</span>
        <span className="caption">{t("models.used", { size: gigabytes(used, i18n.language) })}</span>
      </div>
      <div ref={installedRows} className="model-rows" role="radiogroup" aria-label={t("models.title")} {...roving.group}>
        {installed.map((model, index) => row(model, index))}
      </div>
      {unload}
      {/* Hidden, not taken away, when empty: unfolded, it stays so for a model that comes back
          (a failed download, its reason inside). */}
      <details className="model-more" hidden={available.length === 0}>
        <summary>{t("models.available", { number: available.length })}</summary>
        <div className="model-rows" role="radiogroup" aria-label={t("models.available", { number: available.length })}>
          {available.map((model, index) => row(model, index))}
        </div>
      </details>
      {folders && (
        <div className="model-folders">
          <span className="caption folder-path" {...clipTitle(folders.models)}>
            {folders.models}
          </span>
          <button type="button" className="btn link quiet" onClick={() => openFolder("models")}>
            {t("models.open_models_folder")}
          </button>
          <button type="button" className="btn link quiet" onClick={() => openFolder("all")} title={`${t("models.data_folder_hint")}\n${folders.all}`}>
            {t("models.open_data_folder")}
          </button>
          {folderFailure && (
            <span className="caption warning-text folder-failure" role="alert">
              {folderFailure}
            </span>
          )}
        </div>
      )}
    </Card>
  );
}
