import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";
import { invoke } from "@tauri-apps/api/core";
import { formatHotkey, hotkeyFromKey, hotkeyKeys, type KeyPress } from "../dictation";
import type { SaveError } from "../settings";
import { Icon } from "../ui";

interface Props {
  /// The accessible name.
  label: string;
  hotkey: string | null;
  /// `null` removes an optional hotkey. Errors are shown by the window's message bar.
  onSave: (hotkey: string | null) => Promise<SaveError | null>;
  /// Optional hotkeys can be removed; the main hotkey cannot.
  optional?: boolean;
  disabled?: boolean;
  minWidth?: number;
}

const suspendHotkeys = () => invoke("set_hotkey_suspended", { suspended: true });
const resumeHotkeys = () => invoke("set_hotkey_suspended", { suspended: false });

/// One recording of a combination while the global hotkeys are paused. However it ends (a
/// combination, Escape, a click elsewhere, the field going away), the hotkeys are resumed once,
/// after the save and whatever the save does. `end` gives `null` when it had ended already;
/// `ended` ends it if it still runs and resolves once the hotkeys are resumed, however it ended.
export function pausedRecording(resume: () => Promise<unknown>) {
  let ending: Promise<void> | null = null;
  function end(save?: () => Promise<unknown>): Promise<void> | null {
    if (ending) return null;
    ending = (async () => {
      try {
        await save?.();
      } catch {
        // A refused save is shown by the window's message bar.
      }
      await resume().catch(() => {});
    })();
    return ending;
  }
  return { end, ended: (): Promise<void> => end() ?? (ending as Promise<void>) };
}

type PausedRecording = ReturnType<typeof pausedRecording>;

/// The hotkey fields record one at a time, and the pause of the global hotkeys is one switch for
/// all of them. Starts run one after another: a field that starts ends the recording of the
/// field before (`stopped` tells that field) and pauses the hotkeys only once that one has
/// resumed them, also when it ended by itself and its save still runs. `start` gives `null` when
/// the hotkeys could not be paused.
export function oneRecording(suspend: () => Promise<unknown>, resume: () => Promise<unknown>) {
  let current: { recording: PausedRecording; stopped: () => void } | null = null;
  let queue: Promise<unknown> = Promise.resolve();
  return {
    start(stopped: () => void): Promise<PausedRecording | null> {
      const run = queue.then(async () => {
        const before = current;
        current = null;
        if (before) {
          before.stopped();
          await before.recording.ended();
        }
        try {
          await suspend();
        } catch {
          // Not paused: pressing a hotkey would dictate, so the field does not listen.
          return null;
        }
        const recording = pausedRecording(resume);
        current = { recording, stopped };
        return recording;
      });
      queue = run.catch(() => undefined);
      return run;
    },
  };
}

const recorder = oneRecording(suspendHotkeys, resumeHotkeys);

/// What a key press does to a field that records: Tab and Shift+Tab move the focus on and back
/// as anywhere (`pass`; leaving the field ends the recording), Escape alone cancels, a
/// combination is taken. Modifiers alone, and a key that types or moves the cursor without a
/// modifier, `wait`.
type RecordedKey = { kind: "pass" } | { kind: "cancel" } | { kind: "take"; hotkey: string } | { kind: "wait" };

export function recordedKey(key: KeyPress): RecordedKey {
  const modifiers = key.ctrlKey || key.altKey || key.shiftKey || key.metaKey;
  if (key.code === "Tab" && !key.ctrlKey && !key.altKey && !key.metaKey) return { kind: "pass" };
  if (key.code === "Escape" && !modifiers) return { kind: "cancel" };
  const hotkey = hotkeyFromKey(key);
  return hotkey ? { kind: "take", hotkey } : { kind: "wait" };
}

/// The field's name for screen readers: what it sets, then its combination, or that it waits for
/// one while it records.
export function hotkeyFieldName(t: TFunction, label: string, hotkey: string | null, listening: boolean): string {
  const keys = listening ? t("hotkey.press") : hotkey ? formatHotkey(hotkey) : t("hotkey.none");
  return t("hotkey.name", { label, keys });
}

/// A hotkey field: key caps in a frame; a click starts recording a new
/// combination. Global hotkeys are paused meanwhile, otherwise pressing one would start a
/// dictation instead of reaching this field.
export function HotkeyField({ label, hotkey, onSave, optional = false, disabled = false, minWidth = 150 }: Props) {
  const { t } = useTranslation();
  const [recording, setRecording] = useState<PausedRecording | null>(null);
  const listening = recording !== null;
  const field = useRef<HTMLButtonElement>(null);
  const mounted = useRef(false);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  async function start() {
    if (listening || disabled) return;
    const next = await recorder.start(() => setRecording(null));
    if (!next) return;
    // The field went away while the hotkeys were being paused.
    if (!mounted.current) return void next.end();
    // A click does not focus a button in WebKit (macOS): with the focus here, a click elsewhere
    // ends the recording.
    field.current?.focus();
    setRecording(next);
  }

  useEffect(() => {
    if (!recording) return;
    const active = recording;
    // The focus stays on the field, so the keyboard goes on from there.
    function finish(next: string | null) {
      const ending = active.end(next && next !== hotkey ? () => onSave(next) : undefined);
      if (!ending) return;
      setRecording(null);
    }
    function onKey(event: KeyboardEvent) {
      const key = recordedKey(event);
      if (key.kind === "pass") return;
      // Nothing else reaches the page: no typing, scrolling or pressing of the field.
      event.preventDefault();
      if (key.kind === "cancel") finish(null);
      else if (key.kind === "take") finish(key.hotkey);
    }
    const cancel = () => finish(null);
    const element = field.current;
    window.addEventListener("keydown", onKey);
    // Another window took the focus: the keys pressed there never reach this field.
    window.addEventListener("blur", cancel);
    element?.addEventListener("blur", cancel);
    return () => {
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("blur", cancel);
      element?.removeEventListener("blur", cancel);
      // The field went away while listening, such as with its page or its mode, or another field
      // started: the hotkeys work again.
      void active.end();
    };
  }, [recording]); // `onSave` and `hotkey` come from the render that started listening, on purpose.

  const clearable = optional && hotkey !== null && !listening && !disabled;
  return (
    <span className={`hotkey-field${listening ? " listening" : ""}`} style={{ minWidth }}>
      <button
        ref={field}
        type="button"
        className="hotkey-face"
        aria-label={hotkeyFieldName(t, label, hotkey, listening)}
        disabled={disabled}
        onClick={start}
      >
        {listening ? (
          <span className="hotkey-hint">{t("hotkey.press")}</span>
        ) : hotkey ? (
          <span className="hotkey-caps">
            {hotkeyKeys(hotkey).map((part, index) => (
              <kbd key={index}>{part}</kbd>
            ))}
          </span>
        ) : (
          <span className="hotkey-hint">{t("hotkey.none")}</span>
        )}
      </button>
      {clearable && (
        <button
          type="button"
          className="hotkey-clear"
          aria-label={t("hotkey.clear")}
          title={t("hotkey.clear")}
          onClick={() => {
            // The button goes with the hotkey: the focus goes to the field, not to the page.
            field.current?.focus();
            void onSave(null);
          }}
        >
          <Icon name="close" size={11} tone="muted" />
        </button>
      )}
    </span>
  );
}
