import type { Replacement } from "./settings";

/// The variants of an entry as the backend compares them: separated by commas or new lines,
/// single spaces, any case; empty variants left out. `""` when there is no phrase.
export function phraseKey(from: string): string {
  return from
    .split(/[,\n]/)
    .map((variant) => variant.split(/\s+/).filter(Boolean).join(" ").toLocaleLowerCase())
    .filter(Boolean)
    .join(", ");
}

/// The list with `added` put in: an entry for a phrase already there takes its place, a new one
/// goes to the end. Entries without a phrase are left out.
export function mergeReplacements(list: Replacement[], added: Replacement[]): Replacement[] {
  const result = list.filter((item) => phraseKey(item.from) !== "");
  for (const item of added) {
    const key = phraseKey(item.from);
    if (key === "") continue;
    const index = result.findIndex((existing) => phraseKey(existing.from) === key);
    if (index >= 0) result[index] = item;
    else result.push(item);
  }
  return result;
}

/// Whether `item` is the entry `shown`: rows are found by what they hold, not by their place, so
/// a change queued behind another one still finds its row.
const sameEntry = (item: Replacement, shown: Replacement) => item.from === shown.from && item.to === shown.to;

/// The place of a row's entry: the first of `shown` (what the row holds, the latest first) that
/// the list has; -1 when it has none of them.
function placeOf(list: Replacement[], shown: readonly Replacement[]): number {
  for (const entry of shown) {
    const index = list.findIndex((item) => sameEntry(item, entry));
    if (index >= 0) return index;
  }
  return -1;
}

/// The list with the row's entry (`shown`, see `placeOf`) changed to `next`; a list without it
/// stays as it is.
export function replaceEntry(list: Replacement[], shown: readonly Replacement[], next: Replacement): Replacement[] {
  const index = placeOf(list, shown);
  return index < 0 ? list : list.map((item, at) => (at === index ? next : item));
}

/// The list without the row's entry (`shown`, see `placeOf`); a second delete of it changes
/// nothing.
export function removeEntry(list: Replacement[], shown: readonly Replacement[]): Replacement[] {
  const index = placeOf(list, shown);
  return index < 0 ? list : list.filter((_, at) => at !== index);
}

/// What a row has sent and not yet seen saved (`sent`, the latest last), once the row shows the
/// new entry `item`: the sends up to `item` are saved. An `item` that is none of them is another
/// entry (the rows moved up, an import changed it): nothing sent is left to wait for.
export function sentAfter(sent: readonly Replacement[], item: Replacement): Replacement[] {
  const at = sent.findIndex((entry) => sameEntry(entry, item));
  return at < 0 ? [] : sent.slice(at + 1);
}

/// The entries a row's change or delete looks for: what it sent last first, then what it sent
/// before, then the saved entry it shows (`item`). A delete pressed right after a change, before
/// the change is saved, still finds the row; a refused change leaves the earlier entry to find.
export function rowEntries(sent: readonly Replacement[], item: Replacement): Replacement[] {
  return [...sent].reverse().concat(item);
}

/// What the fields of a new entry show once the save of the entry sent from them (`sent`) was
/// refused: the entry again while both are still empty, to be tried again; what the user began
/// typing since stays.
export function draftAfterRefusal(shown: Replacement, sent: Replacement): Replacement {
  return shown.from === "" && shown.to === "" ? sent : shown;
}
