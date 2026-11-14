import { describe, expect, it } from "vitest";
import { buildManifest } from "../tools/update-manifest.mjs";

const files = {
  "ahem.app.tar.gz": "SIG-MAC\n",
  "ahem_0.2.0_x64-setup.exe": "SIG-WIN\n",
  "ahem_0.2.0_aarch64.dmg": "",
};

describe("buildManifest", () => {
  it("names the package and the signature of each system, and the release they come from", () => {
    const manifest = buildManifest({ version: "0.2.0", notes: "- A faster start", files, date: "2026-11-15T00:00:00Z" });
    expect(manifest).toEqual({
      version: "0.2.0",
      notes: "- A faster start",
      pub_date: "2026-11-15T00:00:00Z",
      platforms: {
        "darwin-aarch64": {
          signature: "SIG-MAC",
          url: "https://github.com/ousttx/ahem/releases/download/v0.2.0/ahem.app.tar.gz",
        },
        "windows-x86_64": {
          signature: "SIG-WIN",
          url: "https://github.com/ousttx/ahem/releases/download/v0.2.0/ahem_0.2.0_x64-setup.exe",
        },
      },
    });
  });

  it("leaves out a system without a package, and refuses a package without a signature or no package at all", () => {
    const without = (name: string) => Object.fromEntries(Object.entries(files).filter(([key]) => key !== name));
    const date = "2026-11-15T00:00:00Z";
    const macOnly = buildManifest({ version: "0.2.0", files: without("ahem_0.2.0_x64-setup.exe"), date });
    expect(Object.keys(macOnly.platforms)).toEqual(["darwin-aarch64"]);
    expect(() => buildManifest({ version: "0.2.0", files: { ...files, "ahem_0.2.0_x64-setup.exe": " " }, date })).toThrow(/signature/);
    expect(() => buildManifest({ version: "0.2.0", files: { "ahem_0.2.0_aarch64.dmg": "" }, date })).toThrow(/no package/);
  });

  it("uses https only, so the app's updater accepts every address", () => {
    const manifest = buildManifest({ version: "0.2.0", files, date: "d" });
    for (const platform of Object.values(manifest.platforms)) expect(platform.url.startsWith("https://")).toBe(true);
  });
});
