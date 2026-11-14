import { describe, expect, it } from "vitest";
import chatgptLogo from "./assets/chatgpt-logo-white.svg?raw";
import { mark, markPath } from "./logos";

describe("the OpenAI mark", () => {
  it("is drawn from OpenAI's own sign-in button file, the one copy of its path", () => {
    const path = markPath(chatgptLogo);
    expect(path).toMatch(/^M[\d.]+ /);
    expect(mark("openai").paths).toEqual([path]);
    expect(mark("chatgpt")).toBe(mark("openai"));
  });
});
