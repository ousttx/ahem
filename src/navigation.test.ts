import { describe, expect, it } from "vitest";
import { linkMovesFocus, NAVIGATION, nextTranscriptsHome, targetPage } from "./App";
import { afterGoodSave } from "./present";

describe("targetPage", () => {
  it("leads to a page or to a tab of the settings", () => {
    expect(targetPage("models")).toEqual(["models", null, null]);
    expect(targetPage("services")).toEqual(["services", null, null]);
    expect(targetPage("settings:keys")).toEqual(["settings", "keys", null]);
    expect(targetPage("settings:microphone")).toEqual(["settings", "microphone", null]);
    expect(targetPage("settings:general")).toEqual(["settings", "general", null]);
    expect(targetPage("history")).toEqual(["history", null, null]);
    expect(targetPage("transcripts")).toEqual(["transcripts", null, null]);
  });

  it("opens a mode on the Modes page", () => {
    expect(targetPage("mode:mode-business")).toEqual(["modes", null, "mode-business"]);
  });
});

describe("nextTranscriptsHome", () => {
  it("counts only a click on Transcripts while it is open", () => {
    expect(nextTranscriptsHome("transcripts", "transcripts", 0)).toBe(1);
    expect(nextTranscriptsHome("transcripts", "transcripts", 1)).toBe(2);
    expect(nextTranscriptsHome("dictation", "transcripts", 1)).toBe(1);
    expect(nextTranscriptsHome("transcripts", "history", 1)).toBe(1);
    // Another page clicked again leaves an open transcript where it is.
    expect(nextTranscriptsHome("history", "history", 1)).toBe(1);
  });
});

describe("NAVIGATION", () => {
  it("lists the setup pages in their order: modes, dictionary, services, models", () => {
    const setup = NAVIGATION.find(({ group }) => group === "setup")?.pages.map(({ page }) => page);
    expect(setup).toEqual(["modes", "dictionary", "services", "models"]);
  });
});

describe("afterGoodSave", () => {
  it("clears only the error of the failed save", () => {
    const failed = { text: "Not saved", error: true };
    expect(afterGoodSave(failed, "Not saved")).toBeNull();
    const other = { text: "Saved: notes.md", error: false };
    expect(afterGoodSave(other, "Not saved")).toBe(other);
    expect(afterGoodSave(other, null)).toBe(other);
    expect(afterGoodSave(null, null)).toBeNull();
  });
});

describe("linkMovesFocus", () => {
  it("moves the focus to the title of another page, not within the page the link is on", () => {
    expect(linkMovesFocus("dictation", "settings:microphone")).toBe(true);
    expect(linkMovesFocus("models", "services")).toBe(true);
    expect(linkMovesFocus("settings", "mode:plain")).toBe(true);
    expect(linkMovesFocus("modes", "mode:plain")).toBe(false);
    expect(linkMovesFocus("settings", "settings:keys")).toBe(false);
  });
});
