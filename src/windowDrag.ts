import { currentMonitor, getCurrentWindow } from "@tauri-apps/api/window";
import { LogicalPosition, LogicalSize } from "@tauri-apps/api/dpi";

// Dragging the main window by its top on macOS. Tauri's own drag (`data-tauri-drag-region`, which
// asks the system to move the window) does nothing on this system although the press reaches the
// strip, so the window is moved by hand: the press notes where the window and the pointer are,
// and each move of the pointer puts the window the same distance away. A double click maximizes
// the window by hand too, in one step: the system's animated zoom resizes the window many times
// and the page lags behind it, cut off at its old width. Windows and Linux keep Tauri's drag.

/// One element of the path of a press, as far as dragging cares.
export interface DragNode {
  /// A button, link, field and the like: it takes the press for itself.
  clickable: boolean;
  /// Its `data-tauri-drag-region`: `null` when it has none.
  attr: string | null;
  /// It is the element that was pressed itself, not one inside it.
  target: boolean;
}

/// Whether a press whose path is `path` (from the element pressed outwards) starts a drag, by
/// the rules of `data-tauri-drag-region`: "deep" takes a press anywhere inside it, a bare one
/// only on itself, "false" and a button on the way refuse.
export function startsDrag(path: readonly DragNode[]): boolean {
  for (const node of path) {
    if (node.clickable && node.attr === null) return false;
    if (node.attr === null) continue;
    if (node.attr === "false") return false;
    if (node.attr === "deep") return true;
    return node.target;
  }
  return false;
}

/// Where the window goes: its place at the press moved by what the pointer moved since.
export function draggedTo(
  window: { x: number; y: number },
  from: { x: number; y: number },
  now: { x: number; y: number },
): { x: number; y: number } {
  return { x: window.x + now.x - from.x, y: window.y + now.y - from.y };
}

/// A window's place and size, in logical pixels.
export interface Frame {
  x: number;
  y: number;
  width: number;
  height: number;
}

/// Whether two frames are the same, give or take `tolerance` pixels (the system rounds).
export function sameFrame(a: Frame, b: Frame, tolerance = 2): boolean {
  return (["x", "y", "width", "height"] as const).every((key) => Math.abs(a[key] - b[key]) <= tolerance);
}

/// What a double click does to a window now at `now` on a screen whose usable area is `area`,
/// having been maximized from `before` (`null` when it was not): the frame it goes to, and the one
/// to go back to. A window that is no longer where the maximizing put it, because it was dragged
/// or resized since, is not maximized any more and is maximized again.
export function afterDoubleClick(now: Frame, area: Frame, before: Frame | null): { go: Frame; back: Frame | null } {
  if (before !== null && sameFrame(now, area)) return { go: before, back: null };
  return { go: area, back: now };
}

const CLICKABLE_TAGS = new Set(["A", "BUTTON", "INPUT", "SELECT", "TEXTAREA", "LABEL", "SUMMARY"]);
const INTERACTIVE_ROLES = new Set(["button", "link", "menuitem", "tab", "checkbox", "radio", "switch", "option"]);

function nodes(path: EventTarget[]): DragNode[] {
  return path
    .filter((item): item is HTMLElement => item instanceof HTMLElement)
    .map((element, index) => ({
      clickable:
        CLICKABLE_TAGS.has(element.tagName) ||
        (element.hasAttribute("contenteditable") && element.getAttribute("contenteditable") !== "false") ||
        (element.hasAttribute("tabindex") && element.getAttribute("tabindex") !== "-1") ||
        INTERACTIVE_ROLES.has(element.getAttribute("role") ?? ""),
      attr: element.getAttribute("data-tauri-drag-region"),
      target: index === 0,
    }));
}

/// Starts listening; the returned function stops it.
export function dragWindowByHand(): () => void {
  const appWindow = getCurrentWindow();
  let drag: { from: { x: number; y: number }; at: { x: number; y: number }; window: { x: number; y: number } | null } | null = null;
  let press = 0;
  // Where the window was before the double click maximized it.
  let before: Frame | null = null;

  const toggleMaximized = async () => {
    const [position, size, scale, monitor] = await Promise.all([
      appWindow.outerPosition(),
      appWindow.innerSize(),
      appWindow.scaleFactor(),
      currentMonitor(),
    ]);
    if (!monitor) return;
    const logical = (x: number, y: number, width: number, height: number): Frame => ({ x: x / scale, y: y / scale, width: width / scale, height: height / scale });
    const now = logical(position.x, position.y, size.width, size.height);
    const work = monitor.workArea;
    const area = logical(work.position.x, work.position.y, work.size.width, work.size.height);
    const next = afterDoubleClick(now, area, before);
    before = next.back;
    await appWindow.setPosition(new LogicalPosition(next.go.x, next.go.y));
    await appWindow.setSize(new LogicalSize(next.go.width, next.go.height));
  };

  const place = () => {
    if (!drag || !drag.window) return;
    // A press that has not moved (the first click of a double click) leaves the window alone.
    if (drag.at.x === drag.from.x && drag.at.y === drag.from.y) return;
    const to = draggedTo(drag.window, drag.from, drag.at);
    void appWindow.setPosition(new LogicalPosition(to.x, to.y)).catch(() => {});
  };

  const down = (event: MouseEvent) => {
    if (event.button !== 0 || (event.detail !== 1 && event.detail !== 2) || !startsDrag(nodes(event.composedPath()))) return;
    event.preventDefault();
    // Tauri's own drag and maximizing would run after this one and fight it.
    event.stopImmediatePropagation();
    if (event.detail === 2) {
      // The first click of the double click started a drag that never moved: it ends here.
      press++;
      drag = null;
      void toggleMaximized().catch(() => {});
      return;
    }
    const mine = ++press;
    drag = { from: { x: event.screenX, y: event.screenY }, at: { x: event.screenX, y: event.screenY }, window: null };
    void Promise.all([appWindow.outerPosition(), appWindow.scaleFactor()]).then(([position, scale]) => {
      if (!drag || mine !== press) return;
      drag.window = { x: position.x / scale, y: position.y / scale };
      place();
    }, () => {});
  };
  const move = (event: MouseEvent) => {
    if (!drag) return;
    // The button went up where the page could not see it.
    if ((event.buttons & 1) === 0) {
      drag = null;
      return;
    }
    drag.at = { x: event.screenX, y: event.screenY };
    place();
  };
  const stop = () => {
    drag = null;
  };

  document.addEventListener("mousedown", down, true);
  window.addEventListener("mousemove", move, true);
  window.addEventListener("mouseup", stop, true);
  window.addEventListener("blur", stop);
  return () => {
    document.removeEventListener("mousedown", down, true);
    window.removeEventListener("mousemove", move, true);
    window.removeEventListener("mouseup", stop, true);
    window.removeEventListener("blur", stop);
  };
}
