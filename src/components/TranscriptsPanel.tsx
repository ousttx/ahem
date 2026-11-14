import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, type Dispatch, type SetStateAction } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open } from "@tauri-apps/plugin-dialog";
import type { Target } from "../App";
import { fileRunning, type FileState } from "../dictation";
import { split } from "../meetings";
import { ownMessages, type Message } from "../present";
import type { Settings, Update } from "../settings";
import { Card, IconCircle, useFocusAfter } from "../ui";
import { FileProgress, FilesPanel, MEDIA, recognitionText } from "./FilesPanel";
import { commandError, MeetingRow, MeetingView, RecordRow, useMeetings, type Run } from "./MeetingsPanel";

interface Props {
  settings: Settings;
  /// Saves "By speaker" of the file view.
  update: Update;
  file: FileState | null;
  modelName: string | null;
  /// A cloud service recognizes the audio.
  inCloud: boolean;
  /// The engine gives word times, which audio and video need.
  mediaFiles: boolean;
  /// Whether the model translates; `null` without a model.
  translates: boolean | null;
  /// The page is shown: only then does it take dropped files.
  active: boolean;
  /// Grows when "Transcripts" in the menu is clicked while the page is open: back to the list.
  home: number;
  /// Puts the focus on the title of the screen once it is drawn: the button that changed the
  /// screen is gone or hidden with the screen it was on.
  focusTitle: () => void;
  /// Scrolls the page to its top: all its screens are in one scrolling container.
  toTop: () => void;
  navigate: (target: Target) => void;
  setMessage: Dispatch<SetStateAction<Message | null>>;
}

/// What the page shows: the list, an entry of it, or the chosen file.
type Screen = { kind: "list" } | { kind: "entry"; id: number } | { kind: "file" };

/// The screen drawn, by name: the file screen without a file to show draws the list. Another
/// name is another screen, which starts at the top of the page.
export function drawnScreen(screen: Screen, fileShown: boolean): string {
  if (fileShown) return "file";
  return screen.kind === "entry" ? `entry:${screen.id}` : "list";
}

/// The Transcripts page: record a meeting or transcribe a file on top, one list of both
/// below; every entry opens the same page.
export function TranscriptsPanel({
  settings,
  update,
  file,
  modelName,
  inCloud,
  mediaFiles,
  translates,
  active,
  home,
  focusTitle,
  toTop,
  navigate,
  setMessage,
}: Props) {
  const { t } = useTranslation();
  const { overview, live, error, load } = useMeetings();
  // A success of this page clears only this page's failure, not a message of another part.
  const messages = useMemo(() => ownMessages(setMessage), [setMessage]);
  const [screen, setScreen] = useState<Screen>({ kind: "list" });
  const [path, setPath] = useState<string | null>(null);
  const [hover, setHover] = useState(false);
  const running = fileRunning(file);
  // The running file's "Open" goes as the file ends: the focus it had goes to "Choose file…" in
  // its place.
  const follow = useFocusAfter();
  const chooseButton = useRef<HTMLButtonElement>(null);
  // Another screen, chosen on this page: the focus goes to its title. An entry's title comes with
  // the entry, which is read first: its page puts the focus there itself.
  const show = useCallback(
    (next: Screen) => {
      setScreen(next);
      focusTitle();
    },
    [focusTitle],
  );
  // Stable: the entry page reads its entry again when this changes.
  const toList = useCallback(() => show({ kind: "list" }), [show]);
  // Stable: the rows of the list do not draw anew with every live event.
  const openEntry = useCallback((id: number) => show({ kind: "entry", id }), [show]);
  // The menu item clicked again leads back to the list, the focus staying in the menu; a running
  // file goes on and keeps its row on top.
  useEffect(() => {
    if (home > 0) setScreen({ kind: "list" });
  }, [home]);

  const run: Run = useCallback(
    async (command, args) => {
      try {
        const result = await invoke(command, args);
        messages.succeed();
        await load();
        return result;
      } catch (failure) {
        messages.fail(commandError(t, failure));
        return undefined;
      }
    },
    [load, messages, t],
  );

  function choose(chosen: string) {
    // One file at a time: the running one is shown instead.
    if (running) return show({ kind: "file" });
    const { extension, name } = split(chosen);
    if (extension !== "txt" && !MEDIA.includes(extension)) return messages.fail(t("files.unsupported", { name }));
    setPath(chosen);
    show({ kind: "file" });
    messages.succeed();
  }

  // The drop handler is subscribed once per showing; it calls the latest `choose`.
  const latestChoose = useRef(choose);
  latestChoose.current = choose;
  useEffect(() => {
    if (!active) return;
    const stop = getCurrentWebview().onDragDropEvent(({ payload }) => {
      if (payload.type === "enter" || payload.type === "over") setHover(true);
      else if (payload.type === "leave") setHover(false);
      else if (payload.type === "drop") {
        setHover(false);
        if (payload.paths.length === 1) latestChoose.current(payload.paths[0]);
      }
    });
    return () => {
      stop.then((unlisten) => unlisten());
    };
  }, [active]);

  async function pick() {
    let chosen: string | string[] | null;
    try {
      chosen = await open({
        multiple: false,
        directory: false,
        filters: [
          { name: t("files.media"), extensions: [...MEDIA, "txt"] },
          { name: t("files.text"), extensions: ["txt"] },
        ],
      });
    } catch (failure) {
      return messages.fail(t("files.dialog_failed", { detail: String(failure) }));
    }
    if (typeof chosen === "string") choose(chosen);
  }

  // The meeting being recorded is shown by its row on top, not in the list.
  const recordingId = live.recording?.meeting ?? overview?.recording ?? null;
  const listed = overview?.meetings.filter((meeting) => meeting.id !== recordingId) ?? [];
  const engine = recognitionText(t, modelName, inCloud, mediaFiles);
  // The file view needs a file: a chosen one or the one running.
  const fileShown = screen.kind === "file" && (path !== null || running);
  // The screens share the page's scrolling: another one is shown from its top, before it is
  // painted.
  const drawn = drawnScreen(screen, fileShown);
  useLayoutEffect(() => toTop(), [drawn, toTop]);

  return (
    <>
      {/* Always mounted: the chosen mode and format and the started run stay while hidden. */}
      <FilesPanel
        shown={fileShown}
        settings={settings}
        update={update}
        file={file}
        path={path}
        hover={hover}
        modelName={modelName}
        inCloud={inCloud}
        mediaFiles={mediaFiles}
        translates={translates}
        navigate={navigate}
        messages={messages}
        setMessage={setMessage}
        onBack={toList}
        onClose={() => {
          setPath(null);
          toList();
        }}
        onOpen={(id) => show({ kind: "entry", id })}
      />
      {screen.kind === "entry" && (
        <div className="page">
          <MeetingView id={screen.id} live={live} overview={overview} run={run} onBack={toList} setMessage={setMessage} messages={messages} />
        </div>
      )}
      {(screen.kind === "list" || (screen.kind === "file" && !fileShown)) && (
        <div className="page">
          <h1 className="page-title" tabIndex={-1}>{t("nav.transcripts")}</h1>
          <Card className={hover ? "transcripts-start hover" : "transcripts-start"}>
            <RecordRow overview={overview} live={live} run={run} />
            <div className="transcripts-divider" />
            {running && file ? (
              <div className="meeting-record-body">
                <IconCircle icon="wave" tone="accent" diameter={48} halo={0} />
                <div className="meeting-record-texts">
                  <p className="strong meeting-row-title">{file.name}</p>
                  <p className="caption">{t("files.dictation_first")}</p>
                </div>
                <FileProgress job={file} />
                <button
                  type="button"
                  className="btn"
                  onFocus={() => follow(() => chooseButton.current)}
                  onClick={() => show({ kind: "file" })}
                >
                  {t("transcripts.open")}
                </button>
              </div>
            ) : (
              <div className="meeting-record-body">
                <IconCircle icon="upload" tone="accent" diameter={48} halo={0} />
                <div className="meeting-record-texts">
                  <p className="strong">{t("transcripts.file_title")}</p>
                  <p className="caption">{t("transcripts.file_note")}</p>
                  <p className="caption">
                    {t("files.recognition", { engine })} ·{" "}
                    <button type="button" className="btn link" onClick={() => navigate("models")}>
                      {t("files.change")}
                    </button>
                  </p>
                </div>
                <button ref={chooseButton} type="button" className="btn" onClick={pick}>
                  {t("files.choose")}
                </button>
              </div>
            )}
          </Card>
          {overview && (
            <p className="caption meeting-hint">
              {t(overview.speakers_ready ? "speakers.on" : "speakers.off")}
              {!overview.speakers_ready && (
                <>
                  {" "}
                  <button type="button" className="btn link" onClick={() => navigate("models")}>
                    {t("nav.models")}
                  </button>
                </>
              )}
            </p>
          )}
          <Card title={t("transcripts.list")}>
            <div className="meeting-list">
              {listed.length === 0 && overview && <p className="caption">{t("transcripts.empty")}</p>}
              {listed.map((meeting) => (
                <MeetingRow key={meeting.id} meeting={meeting} progress={live.progress} onOpen={openEntry} />
              ))}
              {error && (
                <p className="caption warning-text" role="alert">
                  {t("transcripts.load_failed", { detail: error })}
                </p>
              )}
            </div>
          </Card>
        </div>
      )}
    </>
  );
}
