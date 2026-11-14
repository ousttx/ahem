import { describe, expect, it } from "vitest";
import { afterDoubleClick, draggedTo, sameFrame, startsDrag, type DragNode, type Frame } from "./windowDrag";
import appSource from "./App.tsx?raw";
import dragSource from "./windowDrag.ts?raw";
import controls from "../src-tauri/capabilities/window-controls.json";

const node = (attr: string | null, extra: Partial<DragNode> = {}): DragNode => ({ clickable: false, attr, target: false, ...extra });

describe("startsDrag", () => {
  it("takes a press anywhere inside a deep drag region, as Tauri does", () => {
    expect(startsDrag([node(null, { target: true }), node("deep")])).toBe(true);
  });

  it("takes a press on a bare drag region only on itself", () => {
    expect(startsDrag([node("", { target: true })])).toBe(true);
    expect(startsDrag([node(null, { target: true }), node("")])).toBe(false);
  });

  it("lets a button on the way keep its press, unless it is a drag region itself", () => {
    expect(startsDrag([node(null, { clickable: true, target: true }), node("deep")])).toBe(false);
    expect(startsDrag([node("deep", { clickable: true, target: true })])).toBe(true);
  });

  it("refuses where dragging is turned off or there is no region", () => {
    expect(startsDrag([node("false", { target: true }), node("deep")])).toBe(false);
    expect(startsDrag([node(null, { target: true }), node(null)])).toBe(false);
  });
});

describe("draggedTo", () => {
  it("puts the window as far from where it was as the pointer went", () => {
    expect(draggedTo({ x: 300, y: 80 }, { x: 700, y: 20 }, { x: 760, y: 70 })).toEqual({ x: 360, y: 130 });
    expect(draggedTo({ x: 300, y: 80 }, { x: 700, y: 20 }, { x: 700, y: 20 })).toEqual({ x: 300, y: 80 });
  });
});

const frame = (x: number, y: number, width: number, height: number): Frame => ({ x, y, width, height });

describe("afterDoubleClick", () => {
  const area = frame(0, 38, 1440, 800);
  const small = frame(204, 56, 1000, 660);

  it("maximizes a window to the usable area and remembers where it was", () => {
    expect(afterDoubleClick(small, area, null)).toEqual({ go: area, back: small });
  });

  it("puts a maximized window back where it was", () => {
    expect(afterDoubleClick(area, area, small)).toEqual({ go: small, back: null });
    expect(afterDoubleClick(frame(1, 38, 1440, 801), area, small)).toEqual({ go: small, back: null });
  });

  it("maximizes again a window that was dragged or resized since, and remembers its new place", () => {
    const moved = frame(100, 100, 1000, 660);
    expect(afterDoubleClick(moved, area, small)).toEqual({ go: area, back: moved });
  });

  it("does not take a window that happens to fill the area for a maximized one it never maximized", () => {
    expect(afterDoubleClick(area, area, null)).toEqual({ go: area, back: area });
  });
});

describe("sameFrame", () => {
  it("allows for the system's rounding only", () => {
    expect(sameFrame(frame(0, 0, 100, 100), frame(2, -2, 101, 99))).toBe(true);
    expect(sameFrame(frame(0, 0, 100, 100), frame(5, 0, 100, 100))).toBe(false);
  });
});

describe("dragging the main window on macOS", () => {
  it("is started by the app on macOS only, and a double click maximizes by hand, not with Tauri's animated zoom", () => {
    expect(appSource).toMatch(/IS_MAC \? dragWindowByHand\(\) : undefined/);
    expect(dragSource).toMatch(/event\.detail === 2/);
    expect(dragSource).toMatch(/toggleMaximized\(\)/);
  });

  it("does not touch the window for a press that has not moved, as the first click of a double click", () => {
    expect(dragSource).toMatch(/drag\.at\.x === drag\.from\.x && drag\.at\.y === drag\.from\.y\) return/);
  });

  it("may move the window and read where it is", () => {
    for (const permission of [
      "core:window:allow-set-position",
      "core:window:allow-set-size",
      "core:window:allow-outer-position",
      "core:window:allow-inner-size",
      "core:window:allow-scale-factor",
      "core:window:allow-current-monitor",
    ]) {
      expect(controls.permissions).toContain(permission);
    }
  });
});
