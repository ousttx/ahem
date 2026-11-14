import { describe, expect, it } from "vitest";
import { HELP_URL, linkTarget, openable, PREVIEW_LINKS, PREVIEW_URL, SITE_URL, SUPPORT_URL } from "./links";
import settingsPanelSource from "./components/SettingsPanel.tsx?raw";

describe("openable", () => {
  it("offers only a page of the site that is up, over https", () => {
    expect(openable("")).toBe(false);
    expect(openable("http://example.com/support")).toBe(false);
    expect(openable("https://")).toBe(false);
    expect(openable("javascript:alert(1)")).toBe(false);
    expect(openable("https://example.com/support")).toBe(true);
  });

  it("sends people to the site's two blocks, over https, with names that never change", () => {
    expect(openable(SITE_URL)).toBe(true);
    expect(SUPPORT_URL).toBe(`${SITE_URL}/#donate`);
    expect(HELP_URL).toBe(`${SITE_URL}/#help`);
    // No language in the address: the site opens in the browser's language.
    expect(`${SUPPORT_URL}${HELP_URL}`).not.toMatch(/lang|\/ru|\/en/);
  });

  it("gives a link nothing to open until its page is up, unless it is previewed", () => {
    expect(linkTarget("", false)).toBe("");
    expect(linkTarget("http://example.com", false)).toBe("");
    expect(linkTarget("", true)).toBe(PREVIEW_URL);
    expect(openable(PREVIEW_URL)).toBe(true);
    expect(linkTarget("https://site.example/support", false)).toBe("https://site.example/support");
    expect(linkTarget("https://site.example/support", true)).toBe("https://site.example/support");
  });

  it("keeps the preview of the links off in what is committed", () => {
    expect(PREVIEW_LINKS).toBe(false);
  });

  it("shows the help and support rows in the settings by those targets", () => {
    expect(settingsPanelSource).toMatch(/linkTarget\(HELP_URL, PREVIEW_LINKS\)/);
    expect(settingsPanelSource).toMatch(/openUrl\(help\)/);
    expect(settingsPanelSource).toMatch(/linkTarget\(SUPPORT_URL, PREVIEW_LINKS\)/);
    expect(settingsPanelSource).toMatch(/\{support && \(/);
    expect(settingsPanelSource).toMatch(/openUrl\(support\)/);
  });
});
