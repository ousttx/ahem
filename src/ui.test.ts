import { afterEach, describe, expect, it, vi } from "vitest";
import {
  clipTitle,
  closesDialog,
  confirmPress,
  debouncedSave,
  DELETE_GUARD_MS,
  focusWithin,
  followFocus,
  isClipped,
  isComposing,
  readOnFocus,
  watchSeen,
  writeClipboard,
  type FocusWait,
} from "./ui";

describe("readOnFocus", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("reads now and again whenever the window gets the focus back, until it is stopped", () => {
    const window = new EventTarget();
    vi.stubGlobal("window", window);
    let reads = 0;
    const stop = readOnFocus(() => reads++);
    expect(reads).toBe(1);
    window.dispatchEvent(new Event("focus"));
    window.dispatchEvent(new Event("focus"));
    expect(reads).toBe(3);
    stop();
    window.dispatchEvent(new Event("focus"));
    expect(reads).toBe(3);
  });
});

describe("watchSeen", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("tells now and whenever the window is shown or hidden whether it is seen, until it is stopped", () => {
    const document = Object.assign(new EventTarget(), { visibilityState: "hidden" });
    vi.stubGlobal("document", document);
    const told: boolean[] = [];
    const stop = watchSeen((seen) => told.push(seen));
    expect(told).toEqual([false]);
    // Shown from the tray, hidden there again.
    document.visibilityState = "visible";
    document.dispatchEvent(new Event("visibilitychange"));
    document.visibilityState = "hidden";
    document.dispatchEvent(new Event("visibilitychange"));
    expect(told).toEqual([false, true, false]);
    stop();
    document.visibilityState = "visible";
    document.dispatchEvent(new Event("visibilitychange"));
    expect(told).toEqual([false, true, false]);
  });
});

describe("writeClipboard", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("says whether the clipboard took the text", async () => {
    const written: string[] = [];
    vi.stubGlobal("navigator", { clipboard: { writeText: async (text: string) => void written.push(text) } });
    expect(await writeClipboard("hello")).toBe(true);
    expect(written).toEqual(["hello"]);
    vi.stubGlobal("navigator", { clipboard: { writeText: () => Promise.reject(new Error("not allowed")) } });
    expect(await writeClipboard("hello")).toBe(false);
    // A web view without a clipboard refuses too.
    vi.stubGlobal("navigator", {});
    expect(await writeClipboard("hello")).toBe(false);
  });
});

describe("debouncedSave", () => {
  afterEach(() => vi.useRealTimers());

  it("saves once typing pauses, and at once on a flush, which drops the save waiting", () => {
    vi.useFakeTimers();
    const timer: { current: ReturnType<typeof setTimeout> | null } = { current: null };
    const saved: string[] = [];
    const save = debouncedSave(timer, (text: string) => saved.push(text), 800);
    save.schedule("a");
    vi.advanceTimersByTime(500);
    save.schedule("ab");
    vi.advanceTimersByTime(799);
    expect(saved).toEqual([]);
    vi.advanceTimersByTime(1);
    expect(saved).toEqual(["ab"]);
    expect(timer.current).toBeNull();
    save.schedule("abc");
    save.flush("abcd");
    vi.advanceTimersByTime(1000);
    expect(saved).toEqual(["ab", "abcd"]);
  });

  it("saves with the save given when it was scheduled", () => {
    vi.useFakeTimers();
    const timer: { current: ReturnType<typeof setTimeout> | null } = { current: null };
    const saved: string[] = [];
    debouncedSave(timer, (text: string) => saved.push(`first ${text}`), 800).schedule("a");
    // The next drawing makes its own; the save waiting stays the one of its keystroke.
    debouncedSave(timer, (text: string) => saved.push(`second ${text}`), 800);
    vi.advanceTimersByTime(800);
    expect(saved).toEqual(["first a"]);
  });
});

describe("isClipped", () => {
  it("is true when the element or something in it shows less than it holds", () => {
    expect(isClipped({ scrollWidth: 151, clientWidth: 140 })).toBe(true);
    expect(isClipped({ scrollWidth: 200, clientWidth: 200, children: [{ scrollWidth: 151, clientWidth: 140 }] })).toBe(true);
  });

  it("is false when everything fits", () => {
    expect(isClipped({ scrollWidth: 140, clientWidth: 140 })).toBe(false);
    expect(isClipped({ scrollWidth: 200, clientWidth: 200, children: [{ scrollWidth: 90, clientWidth: 140 }] })).toBe(false);
  });
});

describe("clipTitle", () => {
  const element = (scrollWidth: number) => {
    const state = { title: "old", scrollWidth, clientWidth: 140, removeAttribute: () => { state.title = ""; } };
    return state;
  };

  it("gives the whole text as a tooltip only while it does not fit", () => {
    const clipped = element(151);
    clipTitle("Whisper Large v3 Turbo").onMouseEnter({ currentTarget: clipped as unknown as HTMLElement });
    expect(clipped.title).toBe("Whisper Large v3 Turbo");

    const whole = element(120);
    clipTitle("Loads on recording").onMouseEnter({ currentTarget: whole as unknown as HTMLElement });
    expect(whole.title).toBe("");
  });
});

describe("confirmPress", () => {
  it("arms a row first and acts on the second press of the same row", () => {
    const first = confirmPress<string>(null, "a", 1000);
    expect(first).toEqual({ armed: { key: "a", at: 1000 }, act: false });
    expect(confirmPress(first.armed, "a", 2000)).toEqual({ armed: null, act: true });
  });

  it("arms another row instead of acting on it", () => {
    expect(confirmPress({ key: "a", at: 1000 }, "b", 2000)).toEqual({ armed: { key: "b", at: 2000 }, act: false });
  });

  it("takes the second click of a double click for no second thought: the button stays armed", () => {
    const armed = { key: "a", at: 1000 };
    const double = confirmPress(armed, "a", 1000 + DELETE_GUARD_MS - 1);
    expect(double).toEqual({ armed, act: false });
    // The next deliberate press acts; the time counts from the arming, not from the ignored click.
    expect(confirmPress(double.armed, "a", 1000 + DELETE_GUARD_MS)).toEqual({ armed: null, act: true });
  });
});

describe("closesDialog", () => {
  const backdrop = {} as EventTarget;
  const dialog = {} as EventTarget;

  it("closes on a click that began and ended beside the dialog", () => {
    expect(closesDialog(true, backdrop, backdrop)).toBe(true);
  });

  it("stays for a selection dragged out of the dialog and for clicks inside it", () => {
    expect(closesDialog(false, backdrop, backdrop)).toBe(false);
    expect(closesDialog(true, dialog, backdrop)).toBe(false);
  });
});

describe("followFocus", () => {
  // Elements as the drawing leaves them: still there or gone, enabled or disabled.
  const element = (name: string, isConnected = true, disabled = false) => ({ name, isConnected, disabled });
  type Element = ReturnType<typeof element>;
  const waiting = (from: Element, target: Element | null, thrown = false): FocusWait<Element> => ({ from, find: () => target, thrown });

  it("waits while the element of the action keeps the focus", () => {
    const button = element("delete");
    const wait = waiting(button, element("next row"));
    expect(followFocus(wait, button, false)).toEqual({ focus: null, wait });
  });

  it("moves the focus where the action leads once its element is gone, and the focus with it", () => {
    const next = element("next row");
    expect(followFocus(waiting(element("delete", false), next), null, true)).toEqual({ focus: next, wait: null });
    // Nowhere to go: the focus stays where it fell.
    expect(followFocus(waiting(element("delete", false), null), null, true)).toEqual({ focus: null, wait: null });
  });

  it("follows an element from the moment it has the focus, for work that takes it away by itself", () => {
    // "Start recording" focused, and the recording starts in the quick panel: "Stop" comes instead.
    const start = element("start");
    const stop = element("stop");
    const wait = waiting(start, stop);
    expect(followFocus(wait, start, false)).toEqual({ focus: null, wait });
    expect(followFocus(wait, start, false)).toEqual({ focus: null, wait });
    start.isConnected = false;
    expect(followFocus(wait, null, true)).toEqual({ focus: stop, wait: null });
  });

  it("lets the focus be once the user has put it elsewhere", () => {
    const elsewhere = element("search");
    expect(followFocus(waiting(element("delete"), element("next row")), elsewhere, false).wait).toBeNull();
    // Gone, but something else took the focus, such as a dialog.
    expect(followFocus(waiting(element("delete", false), element("next row")), elsewhere, false)).toEqual({ focus: null, wait: null });
    // Still there and enabled, the focus thrown out by a click beside it: not the action's doing.
    expect(followFocus(waiting(element("create"), element("row")), null, true)).toEqual({ focus: null, wait: null });
  });

  it("waits through a button disabled while its action runs, and gives the focus back if it stays", () => {
    const button = element("start", true, true);
    const wait = waiting(button, element("stop"));
    const busy = followFocus(wait, null, true);
    expect(busy.focus).toBeNull();
    expect(busy.wait).toMatchObject({ from: button, thrown: true });
    // The command failed: the button is enabled again and takes the focus back, still followed.
    button.disabled = false;
    const back = followFocus(busy.wait as FocusWait<Element>, null, true);
    expect(back.focus).toBe(button);
    expect(back.wait).toMatchObject({ from: button, thrown: false });
  });

  it("forgets that the focus was thrown out once someone else gave it back", () => {
    // The key's field, disabled while the key is checked; refused, the form gives it the focus back.
    const field = element("key", true, true);
    const busy = followFocus(waiting(field, element("add key")), null, true);
    expect(busy.wait).toMatchObject({ thrown: true });
    field.disabled = false;
    const back = followFocus(busy.wait as FocusWait<Element>, field, false);
    expect(back.focus).toBeNull();
    expect(back.wait).toMatchObject({ from: field, thrown: false });
    // A click beside it later puts the focus nowhere: it stays there, not back in the field.
    expect(followFocus(back.wait as FocusWait<Element>, null, true)).toEqual({ focus: null, wait: null });
  });

  it("takes a button the action leaves disabled for gone when asked to, whether it keeps the focus or not", () => {
    const other = element("plus");
    const less = element("minus", true, true);
    const wait: FocusWait<Element> = { ...waiting(less, other), disabledIsGone: true };
    expect(followFocus(wait, null, true)).toEqual({ focus: other, wait: null });
    // Some web views leave the focus on a disabled button.
    expect(followFocus(wait, less, false)).toEqual({ focus: other, wait: null });
    // Not at the bound: it is still there and keeps the focus.
    less.disabled = false;
    expect(followFocus(wait, less, false)).toEqual({ focus: null, wait });
  });

  it("waits for a target that is disabled until the action ends, then focuses it", () => {
    // "Stop" came with the meeting's event before the command answered: it is disabled still.
    const stop = element("stop", true, true);
    const moved = followFocus(waiting(element("start", false), stop), null, true);
    expect(moved.focus).toBeNull();
    expect(moved.wait).toMatchObject({ from: stop, thrown: true });
    stop.disabled = false;
    expect(followFocus(moved.wait as FocusWait<Element>, null, true).focus).toBe(stop);
  });

  it("looks again at every drawing for a target found disabled, when a disabled one counts as gone", () => {
    // A model's "Cancel" went as its archive is unpacked: nothing in the row can be pressed yet,
    // its radio waits; a failure brings "Resume" instead, the radio still disabled.
    const radio = element("radio", true, true);
    const resume = element("resume", true, true);
    let row = [radio];
    const find = () => row.find((button) => !button.disabled) ?? row[0];
    const wait: FocusWait<Element> = { from: element("cancel", false), find, thrown: false, disabledIsGone: true };
    const unpacking = followFocus(wait, null, true);
    expect(unpacking).toMatchObject({ focus: null, wait: { from: radio, thrown: true, disabledIsGone: true } });
    row = [radio, resume];
    expect(followFocus(unpacking.wait as FocusWait<Element>, null, true).wait).toMatchObject({ from: radio });
    resume.disabled = false;
    expect(followFocus(unpacking.wait as FocusWait<Element>, null, true)).toEqual({ focus: resume, wait: null });
    // Downloaded: the radio of the ready model takes it.
    row = [radio];
    radio.disabled = false;
    expect(followFocus(unpacking.wait as FocusWait<Element>, null, true).focus).toBe(radio);
    // The user put the focus elsewhere meanwhile: it stays there.
    radio.disabled = true;
    expect(followFocus(unpacking.wait as FocusWait<Element>, element("search"), false)).toEqual({ focus: null, wait: null });
  });
});

describe("focusWithin", () => {
  const box = (inside: unknown[]) => ({ contains: (node: Node | null) => inside.includes(node) });
  const field = {} as Element;

  it("tells whether the focus is in a row", () => {
    expect(focusWithin(box([field]), field)).toBe(true);
    // The user took it elsewhere, such as a key's field: not the row's to move.
    expect(focusWithin(box([]), field)).toBe(false);
    expect(focusWithin(box([field]), null)).toBe(false);
    expect(focusWithin(null, field)).toBe(false);
  });
});

describe("isComposing", () => {
  const key = (isComposing: boolean, keyCode: number) => ({ nativeEvent: { isComposing }, keyCode });

  it("tells the keys of an input method's composing from the keys of the field", () => {
    expect(isComposing(key(true, 13))).toBe(true);
    // Some web views end the composing before the key's event; its code still says so.
    expect(isComposing(key(false, 229))).toBe(true);
    expect(isComposing(key(false, 13))).toBe(false);
    expect(isComposing(key(false, 27))).toBe(false);
  });
});
