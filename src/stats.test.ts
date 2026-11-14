import type { TFunction } from "i18next";
import { describe, expect, it } from "vitest";
import { initI18n } from "./i18n";
import { formatDuration, savedNote, savedSeconds, wordsPerMinute } from "./stats";

const t = ((key: string, values: Record<string, number>) =>
  key === "stats.seconds"
    ? `${values.seconds} s`
    : key === "stats.minutes"
      ? `${values.minutes} min`
      : `${values.hours} h ${values.minutes} min`) as unknown as TFunction;

describe("the statistics", () => {
  it("save the typing time at 40 words a minute, minus the recording time", () => {
    // 120 words take 3 minutes to type; they were said in 1 minute.
    expect(savedSeconds({ dictations: 3, words: 120, recording_ms: 60_000 })).toBe(120);
    // Slower than typing saves nothing rather than a negative time.
    expect(savedSeconds({ dictations: 1, words: 10, recording_ms: 60_000 })).toBe(0);
    expect(savedSeconds({ dictations: 0, words: 0, recording_ms: 0 })).toBe(0);
  });

  it("measure the speed in words per minute of recording", () => {
    expect(wordsPerMinute({ dictations: 2, words: 150, recording_ms: 60_000 })).toBe(150);
    expect(wordsPerMinute({ dictations: 1, words: 50, recording_ms: 20_000 })).toBe(150);
    expect(wordsPerMinute({ dictations: 0, words: 0, recording_ms: 0 })).toBeNull();
  });

  it("name the typing speed they count with, in every language", async () => {
    const i18n = await initI18n("en");
    expect(savedNote(i18n.t)).toContain("at 40 words a minute");
    await i18n.changeLanguage("ru");
    expect(savedNote(i18n.t)).toContain("40 слов в минуту");
  });

  it("show seconds under a minute, then minutes, then hours and minutes", () => {
    expect(formatDuration(t, 0)).toBe("0 s");
    expect(formatDuration(t, 29)).toBe("29 s");
    expect(formatDuration(t, 60)).toBe("1 min");
    expect(formatDuration(t, 6 * 60 + 30)).toBe("6 min");
    expect(formatDuration(t, 85 * 60)).toBe("1 h 25 min");
  });
});
