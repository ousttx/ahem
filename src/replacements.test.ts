import { describe, expect, it } from "vitest";
import { draftAfterRefusal, mergeReplacements, phraseKey, removeEntry, replaceEntry, rowEntries, sentAfter } from "./replacements";

describe("replacements", () => {
  it("compare phrases with single spaces in any case", () => {
    expect(phraseKey("  Эй   ай ")).toBe("эй ай");
    expect(phraseKey(" \n ")).toBe("");
    expect(phraseKey("Эй ай,ей  яй\n, ")).toBe("эй ай, ей яй");
    expect(phraseKey(" , ")).toBe("");
  });

  it("take imported entries in place of the same phrase and add the rest", () => {
    const list = [
      { from: "эй ай", to: "AI" },
      { from: "гугл", to: "Google" },
    ];
    const merged = mergeReplacements(list, [
      { from: "Эй  ай", to: "A.I." },
      { from: "моя подпись", to: "С уважением,\nИван" },
      { from: " ", to: "nothing" },
    ]);
    expect(merged).toEqual([
      { from: "Эй  ай", to: "A.I." },
      { from: "гугл", to: "Google" },
      { from: "моя подпись", to: "С уважением,\nИван" },
    ]);
    expect(list[0].to).toBe("AI");
  });

  it("give the same list back when it is imported again", () => {
    const list = [{ from: "эй ай", to: "AI" }];
    expect(mergeReplacements(list, list)).toEqual(list);
  });
});

describe("a row of replacements", () => {
  const list = [
    { from: "эй ай", to: "AI" },
    { from: "гугл", to: "Google" },
    { from: "джипити", to: "GPT" },
  ];

  it("is deleted once, also when its button is pressed again before the list is saved", () => {
    const once = removeEntry(list, [list[0]]);
    expect(once).toEqual([list[1], list[2]]);
    // The second press still holds the first row; the row now first is another one.
    expect(removeEntry(once, [list[0]])).toBe(once);
  });

  it("changes the row it shows, wherever that row is now", () => {
    const moved = [list[1], list[0], list[2]];
    expect(replaceEntry(moved, [list[0]], { from: "эй ай", to: "A.I." })).toEqual([list[1], { from: "эй ай", to: "A.I." }, list[2]]);
    expect(replaceEntry([list[1]], [list[0]], { from: "x", to: "y" })).toEqual([list[1]]);
  });

  it("is deleted when its trash is pressed right after a change, before the change is saved", () => {
    const edited = { from: "эй ай", to: "A.I." };
    // The row sends its change on blur; the list it shows is still the old one when the trash is pressed.
    const change = (current: typeof list) => replaceEntry(current, rowEntries([], list[0]), edited);
    const remove = (current: typeof list) => removeEntry(current, rowEntries([edited], list[0]));
    // The saves go one after another: the change, then the delete.
    expect(remove(change(list))).toEqual([list[1], list[2]]);
    // A refused change leaves the saved entry, and the delete finds that one.
    expect(remove(list)).toEqual([list[1], list[2]]);
  });

  it("waits only for its own sends: what is saved goes, another entry in the row clears them", () => {
    const first = { from: "эй ай", to: "A.I." };
    const second = { from: "эй ай", to: "AI!" };
    expect(rowEntries([first, second], list[0])).toEqual([second, first, list[0]]);
    expect(sentAfter([first, second], { ...first })).toEqual([second]);
    expect(sentAfter([first, second], second)).toEqual([]);
    // The rows moved up after a delete above: this row shows another entry now.
    expect(sentAfter([first], list[1])).toEqual([]);
  });
});

describe("draftAfterRefusal", () => {
  const sent = { from: "эй ай", to: "AI" };

  it("puts a refused new entry back into the emptied fields", () => {
    expect(draftAfterRefusal({ from: "", to: "" }, sent)).toEqual(sent);
  });

  it("leaves the next entry the user began typing meanwhile", () => {
    expect(draftAfterRefusal({ from: "гугл", to: "" }, sent)).toEqual({ from: "гугл", to: "" });
    expect(draftAfterRefusal({ from: "", to: "Google" }, sent)).toEqual({ from: "", to: "Google" });
  });
});
