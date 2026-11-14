import { afterEach, expect, it, vi } from "vitest";

const commands = vi.hoisted(() => [] as [string, unknown][]);

vi.mock(import("@tauri-apps/api/core"), async (importOriginal) => ({
  ...(await importOriginal()),
  // No backend here: every command fails, so the window shows why it did not start.
  invoke: async <T>(command: string, args?: unknown): Promise<T> => {
    commands.push([command, args]);
    throw new Error("no backend");
  },
}));

afterEach(() => vi.unstubAllGlobals());

it("resumes the hotkeys a reloaded page left paused before it reads the status", async () => {
  const body = { textContent: "" };
  vi.stubGlobal("document", { body, documentElement: { dataset: {} }, getElementById: () => null });
  vi.stubGlobal("navigator", { languages: ["en-US"], userAgent: "" });
  await import("./main");
  await vi.waitFor(() => expect(body.textContent).toContain("ahem could not start"));
  expect(commands[0]).toEqual(["set_hotkey_suspended", { suspended: false }]);
  expect(commands.map(([command]) => command)).toContain("get_status");
});
