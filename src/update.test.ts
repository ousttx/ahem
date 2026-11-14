import { beforeAll, describe, expect, it } from "vitest";
import type { TFunction } from "i18next";
import { initI18n } from "./i18n";
import { NO_UPDATE, notesBlocks, reduceUpdate, startInstall, updateSaid, updateText, type UpdateState } from "./update";
import { UPDATES_READY } from "./update";
import settingsSource from "./components/SettingsPanel.tsx?raw";
import libSource from "../src-tauri/src/lib.rs?raw";

/// The backend registers the command the interface calls to install an update.
const registersInstallUpdate = /install_update/.test(libSource);

let t: TFunction;

beforeAll(async () => {
  t = (await initI18n("en")).t;
});

describe("reduceUpdate", () => {
  it("goes from an available version through its installation to the version installed", () => {
    let state = reduceUpdate(NO_UPDATE, { type: "available", version: "0.3.0", urgent: false, ready: true });
    expect(state).toEqual({ kind: "available", version: "0.3.0", urgent: false, ready: true });
    state = reduceUpdate(state, { type: "progress", version: "0.3.0", done: 25, total: 100 });
    expect(state).toEqual({ kind: "installing", version: "0.3.0", share: 0.25 });
    // A check during the installation does not take the progress away.
    expect(reduceUpdate(state, { type: "available", version: "0.3.0", urgent: false, ready: true })).toBe(state);
    expect(reduceUpdate(state, { type: "progress", version: "0.3.0", done: 5, total: null })).toEqual({
      kind: "installing",
      version: "0.3.0",
      share: null,
    });
    expect(reduceUpdate(state, { type: "installed", version: "0.3.0", notes: "- One", page: "https://example.com" })).toEqual({
      kind: "installed",
      version: "0.3.0",
      notes: [{ kind: "items", items: ["One"] }],
      page: "https://example.com",
    });
    expect(reduceUpdate(state, { type: "failed", version: "0.3.0", detail: "network" })).toEqual({ kind: "failed", version: "0.3.0" });
  });

  it("keeps no notes when they are missing or blank, so 'What's new' opens the page", () => {
    for (const notes of [null, "", " \n\t "]) {
      expect(reduceUpdate(NO_UPDATE, { type: "installed", version: "0.3.0", notes, page: "https://example.com" })).toMatchObject({
        notes: [],
        page: "https://example.com",
      });
    }
  });

  it("makes a new state of every installed version, so an open 'What's new' does not carry over", () => {
    const event = { type: "installed", version: "0.3.0", notes: "- One", page: null } as const;
    const first = reduceUpdate(NO_UPDATE, event);
    expect(reduceUpdate(first, event)).not.toBe(first);
  });
});

describe("startInstall", () => {
  const available: UpdateState = { kind: "available", version: "0.3.0", urgent: false, ready: true };

  it("shows the installation at once and a refused command as a failure", async () => {
    let state: UpdateState = available;
    const done = startInstall("0.3.0", (next) => (state = next(state)), () => Promise.reject(new Error("unknown command")));
    // The button is gone before the command answers: a second press cannot start another.
    expect(state).toEqual({ kind: "installing", version: "0.3.0", share: null });
    await done;
    expect(state).toEqual({ kind: "failed", version: "0.3.0" });
  });

  it("leaves an accepted installation to the backend's events", async () => {
    let state: UpdateState = available;
    await startInstall("0.3.0", (next) => (state = next(state)), () => Promise.resolve(null));
    expect(state).toEqual({ kind: "installing", version: "0.3.0", share: null });
  });
});

describe("updateText", () => {
  it("promises an installation in one press only once the version is downloaded", () => {
    const available = { kind: "available", version: "0.3.0", urgent: false } as const;
    expect(updateText({ ...available, ready: true }, t)).toEqual({ title: "Update 0.3.0", note: "Ready to install" });
    expect(updateText({ ...available, ready: false }, t)).toEqual({ title: "Update 0.3.0", note: null });
    expect(updateText({ ...available, urgent: true, ready: false }, t).note).toBe("Important fix");
  });

  it("counts the installation to the restart", () => {
    const installing = { kind: "installing", version: "0.3.0" } as const;
    expect(updateText({ ...installing, share: null }, t).title).toBe("Updating…");
    expect(updateText({ ...installing, share: 0.256 }, t).title).toBe("Updating… 25%");
    // Not done yet: never 100 % before the restart.
    expect(updateText({ ...installing, share: 0.998 }, t).title).toBe("Updating… 99%");
    expect(updateText({ ...installing, share: 1 }, t)).toEqual({ title: "Restarting…", note: "ahem will restart on its own" });
    expect(updateText({ kind: "failed", version: "0.3.0" }, t)).toEqual({
      title: "Update 0.3.0 not installed",
      note: "This version keeps working",
    });
  });
});

describe("updateSaid", () => {
  it("tells screen readers the installation without its share, which the progress bar has", () => {
    const installing = { kind: "installing", version: "0.3.0" } as const;
    expect(updateSaid({ ...installing, share: 0.256 }, t)).toBe("Updating…");
    expect(updateSaid({ ...installing, share: 0.5 }, t)).toBe(updateSaid({ ...installing, share: 0.256 }, t));
    expect(updateSaid({ ...installing, share: null }, t)).toBe("Updating…");
    expect(updateSaid({ ...installing, share: 1 }, t)).toBe("Restarting…");
    expect(updateSaid({ kind: "failed", version: "0.3.0" }, t)).toBe("Update 0.3.0 not installed");
  });
});

describe("notesBlocks", () => {
  it("groups the items of the changes and keeps the paragraphs between them", () => {
    const notes = "A faster start.\n\n- Calls are told apart by voice\n* Copy in one click\nAnd a fix:\n• Paste works in Telegram";
    expect(notesBlocks(notes)).toEqual([
      { kind: "text", text: "A faster start." },
      { kind: "items", items: ["Calls are told apart by voice", "Copy in one click"] },
      { kind: "text", text: "And a fix:" },
      { kind: "items", items: ["Paste works in Telegram"] },
    ]);
    expect(notesBlocks("  \n ")).toEqual([]);
  });
});

describe("the update switches in the settings", () => {
  it("stay hidden until the backend can update, and the version row has no line above it meanwhile", () => {
    expect(UPDATES_READY).toBe(registersInstallUpdate);
    expect(settingsSource).toMatch(/\{UPDATES_READY && \(/);
    expect(settingsSource).toMatch(/<Row separator=\{UPDATES_READY\} title=\{t\("settings\.version"\)\}/);
  });
});
