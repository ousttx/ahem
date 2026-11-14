// Services offered by name, as "Services" lists them: one key per service, for speech
// recognition and language models where the service has both. DeepSeek and Groq speak the
// OpenAI API at their own address; the backend knows the same services (`domain/services.rs`).
import type { Connection, Mode, ProviderKind } from "./settings";

export type ServiceId = "openai" | "anthropic" | "groq" | "deepgram" | "deepseek";

export interface Service {
  id: ServiceId;
  monogram: string;
  provider: ProviderKind;
  /// `null`: the provider's own address.
  baseUrl: string | null;
  /// Recognizes speech (a cloud engine of the recognition settings).
  speech: boolean;
  /// Has language models for modes.
  text: boolean;
  /// Where the service hands out API keys.
  keysUrl: string;
}

export const SERVICES: Service[] = [
  { id: "openai", monogram: "OA", provider: "open_ai", baseUrl: null, speech: true, text: true, keysUrl: "https://platform.openai.com/api-keys" },
  { id: "anthropic", monogram: "AN", provider: "anthropic", baseUrl: null, speech: false, text: true, keysUrl: "https://platform.claude.com/settings/keys" },
  { id: "groq", monogram: "GQ", provider: "open_ai_compatible", baseUrl: "https://api.groq.com/openai/v1", speech: true, text: true, keysUrl: "https://console.groq.com/keys" },
  { id: "deepgram", monogram: "DG", provider: "deepgram", baseUrl: null, speech: true, text: false, keysUrl: "https://console.deepgram.com/" },
  { id: "deepseek", monogram: "DS", provider: "open_ai_compatible", baseUrl: "https://api.deepseek.com", speech: false, text: true, keysUrl: "https://platform.deepseek.com/api_keys" },
];

/// The named services that recognize speech or have language models, or all of them.
export function servicesFor(only?: "speech" | "text"): Service[] {
  return SERVICES.filter((service) => !only || service[only]);
}

/// Whether a mode can use the connection: a speech-only service has no language models.
export function hasLanguageModels(connection: Pick<Connection, "provider">): boolean {
  return connection.provider !== "deepgram";
}

const sameUrl = (a: string, b: string) => a.replace(/\/+$/, "").toLowerCase() === b.replace(/\/+$/, "").toLowerCase();

/// ChatGPT through the user's account: connected by signing in, on their plan.
export function isChatGptAccount(connection: Pick<Connection, "provider">): boolean {
  return connection.provider === "chatgpt_account";
}

/// A connection as the interface names it: a ChatGPT account by the service, not by its email.
export function connectionLabel(connection: Pick<Connection, "name" | "provider">): string {
  return isChatGptAccount(connection) ? "ChatGPT" : connection.name;
}

/// The named service of a connection, or `null` for an own server.
export function serviceOf(connection: Pick<Connection, "provider" | "base_url">): ServiceId | null {
  const service = SERVICES.find(
    (item) =>
      item.provider === connection.provider &&
      (item.baseUrl === null || (connection.base_url !== null && sameUrl(item.baseUrl, connection.base_url))),
  );
  return service?.id ?? null;
}

/// Each named service with its connection (the first one), the ChatGPT account, and the rest:
/// own servers and extra connections of a named service.
export function groupConnections(connections: Connection[]): {
  named: Record<ServiceId, Connection | undefined>;
  account?: Connection;
  own: Connection[];
} {
  const named = {} as Record<ServiceId, Connection | undefined>;
  const own: Connection[] = [];
  let account: Connection | undefined;
  for (const connection of connections) {
    if (isChatGptAccount(connection)) {
      account ??= connection;
      continue;
    }
    const id = serviceOf(connection);
    if (id && !named[id]) named[id] = connection;
    else own.push(connection);
  }
  return { named, account, own };
}

/// The named service whose key form stays open: `open` while the service has no key. A key
/// added meanwhile elsewhere (another page, the other list) closes the form, so a second key
/// never makes a second connection.
export function keyFormOpen(open: ServiceId | null, connections: Connection[]): ServiceId | null {
  return open !== null && groupConnections(connections).named[open] !== undefined ? null : open;
}

/// The host and port of an address, the default name of an own server.
export function hostOf(url: string): string {
  const match = /^[a-z]+:\/\/([^/?#]+)/i.exec(url.trim());
  return match ? match[1].toLowerCase() : url.trim();
}

/// Whether a service runs on this computer, so what is sent to it stays here: an own server at
/// `localhost` or a loopback address, as the backend counts it. A `.local` name is another
/// computer of the network; that and anything else counts as a service elsewhere.
export function isLocal(connection: Pick<Connection, "provider" | "base_url">): boolean {
  if (connection.provider !== "open_ai_compatible" || !connection.base_url) return false;
  const host = hostOf(connection.base_url).replace(/:\d+$/, "").replace(/^\[|\]$/g, "");
  // A loopback address, not a name that starts like one (`127.example.com`).
  const loopback = /^127(\.\d{1,3}){3}$/.test(host);
  return host === "localhost" || host === "::1" || loopback;
}

/// The first `count` characters of `word` as a reader sees them: an emoji, a flag or a letter
/// with a combining mark stays whole (code points without `Intl.Segmenter`).
function leading(word: string, count: number): string {
  if (typeof Intl.Segmenter !== "function") return Array.from(word).slice(0, count).join("");
  const parts = Array.from(new Intl.Segmenter(undefined, { granularity: "grapheme" }).segment(word), (part) => part.segment);
  return parts.slice(0, count).join("");
}

/// Two letters in the service's square: the named service's, or from the server's name.
export function monogram(connection: Pick<Connection, "name" | "provider" | "base_url">): string {
  const id = serviceOf(connection);
  const service = SERVICES.find((item) => item.id === id);
  if (service) return service.monogram;
  const words = connection.name.trim().split(/\s+/).filter(Boolean);
  const letters = words.length > 1 ? leading(words[0], 1) + leading(words[1], 1) : leading(words[0] ?? "", 2);
  return letters.toUpperCase() || "AI";
}

/// `connection id|model` preselected for a new mode: the model of the newest mode with a
/// language model, while its service is connected; otherwise none, and the user chooses.
export function defaultModel(modes: Mode[], connections: Connection[]): string {
  for (const mode of [...modes].reverse()) {
    if (mode.action.kind !== "process") continue;
    const { connection_id, model } = mode.action;
    const connection = connections.find((item) => item.id === connection_id);
    if (connection?.models.includes(model)) return `${connection_id}|${model}`;
  }
  return "";
}
