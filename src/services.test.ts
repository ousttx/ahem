import { describe, expect, it } from "vitest";
import { connectionLabel, defaultModel, groupConnections, hasLanguageModels, hostOf, isLocal, keyFormOpen, monogram, serviceOf, servicesFor } from "./services";
import type { Connection, Mode } from "./settings";

function connection(id: string, provider: Connection["provider"], base_url: string | null = null, models = ["m"]): Connection {
  return { id, name: id, provider, base_url, key_hint: null, added_at_ms: 0, models };
}

function mode(id: string, connection_id: string, model: string): Mode {
  return {
    id,
    name: id,
    action: { kind: "process", connection_id, model, instruction: "x", output_language: null },
    hotkey: null,
    selection_hotkey: null,
  };
}

describe("the key pages", () => {
  it("are the services' own HTTPS pages", () => {
    for (const service of servicesFor()) expect(service.keysUrl).toMatch(/^https:\/\/[a-z.]+\.(com|ai)\//);
  });
});

describe("servicesFor", () => {
  it("splits the services into speech recognition and text processing", () => {
    const ids = (only?: "speech" | "text") => servicesFor(only).map((service) => service.id);
    expect(ids("speech")).toEqual(["openai", "groq", "deepgram"]);
    expect(ids("text")).toEqual(["openai", "anthropic", "groq", "deepseek"]);
    expect(ids()).toEqual(["openai", "anthropic", "groq", "deepgram", "deepseek"]);
  });
});

describe("serviceOf", () => {
  it("knows the named services by provider and address", () => {
    expect(serviceOf(connection("a", "anthropic"))).toBe("anthropic");
    expect(serviceOf(connection("o", "open_ai"))).toBe("openai");
    expect(serviceOf(connection("d", "open_ai_compatible", "https://API.deepseek.com/"))).toBe("deepseek");
    expect(serviceOf(connection("g", "open_ai_compatible", "https://api.groq.com/openai/v1"))).toBe("groq");
    expect(serviceOf(connection("l", "open_ai_compatible", "http://localhost:11434/v1"))).toBeNull();
    expect(serviceOf(connection("dg", "deepgram", null, []))).toBe("deepgram");
  });

  it("knows that a speech-only service has no language models", () => {
    expect(hasLanguageModels(connection("dg", "deepgram", null, []))).toBe(false);
    expect(hasLanguageModels(connection("g", "open_ai_compatible", "https://api.groq.com/openai/v1"))).toBe(true);
  });
});

describe("groupConnections", () => {
  it("puts one connection per named service and the rest under own servers", () => {
    const first = connection("a1", "anthropic");
    const second = connection("a2", "anthropic");
    const local = connection("l", "open_ai_compatible", "http://localhost:11434/v1");
    const { named, own } = groupConnections([first, local, second]);
    expect(named.anthropic).toBe(first);
    expect(named.openai).toBeUndefined();
    expect(own).toEqual([local, second]);
  });

  it("keeps the ChatGPT account apart from keys and own servers", () => {
    const account = connection("c", "chatgpt_account");
    const { named, account: found, own } = groupConnections([account, connection("o", "open_ai")]);
    expect(found).toBe(account);
    expect(own).toEqual([]);
    expect(named.openai?.id).toBe("o");
    expect(hasLanguageModels(account)).toBe(true);
    expect(isLocal(account)).toBe(false);
  });
});

describe("hostOf and monogram", () => {
  it("name an own server by its address", () => {
    expect(hostOf("http://LocalHost:11434/v1")).toBe("localhost:11434");
    expect(hostOf("https://openrouter.ai/api/v1")).toBe("openrouter.ai");
    expect(hostOf("not a url")).toBe("not a url");
  });

  it("give two letters", () => {
    expect(monogram({ name: "Claude", provider: "anthropic", base_url: null })).toBe("AN");
    expect(monogram({ name: "x", provider: "open_ai_compatible", base_url: "https://api.groq.com/openai/v1" })).toBe("GQ");
    expect(monogram({ name: "Local Ollama", provider: "open_ai_compatible", base_url: "http://localhost:11434/v1" })).toBe("LO");
    expect(monogram({ name: " ", provider: "open_ai_compatible", base_url: "http://localhost:1/v1" })).toBe("AI");
    // Whole characters as a reader sees them, never half an emoji.
    const own = (name: string) => monogram({ name, provider: "open_ai_compatible", base_url: "http://localhost:11434/v1" });
    expect(own("🦙 Ollama")).toBe("🦙O");
    expect(own("🇷🇺 Server")).toBe("🇷🇺S");
    expect(own("👍🏽👨‍👩‍👧")).toBe("👍🏽👨‍👩‍👧");
    expect(own("и\u0306ога")).toBe("И\u0306О");
  });
});

describe("defaultModel", () => {
  it("takes the newest mode's model while its service is connected", () => {
    const connections = [connection("c1", "anthropic", null, ["old", "new"])];
    const modes = [mode("a", "c1", "old"), mode("b", "c1", "new")];
    expect(defaultModel(modes, connections)).toBe("c1|new");
    expect(defaultModel([mode("a", "gone", "old")], connections)).toBe("");
    expect(defaultModel([mode("a", "c1", "removed")], connections)).toBe("");
  });

  it("chooses nothing without an earlier mode", () => {
    expect(defaultModel([], [connection("c1", "open_ai", null, ["babbage-002", "gpt-5.5"])])).toBe("");
  });
});

describe("isLocal", () => {
  it("counts only a server on this computer as local", () => {
    const own = (base_url: string) => ({ provider: "open_ai_compatible" as const, base_url });
    expect(isLocal(own("http://localhost:11434/v1"))).toBe(true);
    expect(isLocal(own("http://127.0.0.1:1234/v1"))).toBe(true);
    expect(isLocal(own("http://[::1]:8080/v1"))).toBe(true);
    // A `.local` name is another computer of the network.
    expect(isLocal(own("https://gpu.local/v1"))).toBe(false);
    expect(isLocal(own("http://studio.local:1234/v1"))).toBe(false);
    expect(isLocal(own("https://api.groq.com/openai/v1"))).toBe(false);
    expect(isLocal(own("http://localhost.example.com/v1"))).toBe(false);
    // Names that only start like a loopback address.
    expect(isLocal(own("https://127.example.com/v1"))).toBe(false);
    expect(isLocal(own("https://127.0.0.1.example.com/v1"))).toBe(false);
    expect(isLocal({ provider: "anthropic", base_url: null })).toBe(false);
  });
});

describe("connectionLabel", () => {
  it("names a ChatGPT account by the service, not by its email", () => {
    expect(connectionLabel({ name: "me@example.com", provider: "chatgpt_account" })).toBe("ChatGPT");
    expect(connectionLabel({ name: "Ollama", provider: "open_ai_compatible" })).toBe("Ollama");
  });
});

describe("keyFormOpen", () => {
  it("keeps the key form open while its service has no key", () => {
    expect(keyFormOpen("groq", [])).toBe("groq");
    expect(keyFormOpen(null, [])).toBeNull();
    // Another service's key, or an own server, leaves it open.
    expect(keyFormOpen("groq", [connection("d", "deepgram"), connection("own", "open_ai_compatible", "http://localhost:11434/v1")])).toBe("groq");
  });

  it("closes it once the service was connected elsewhere, so a second key makes no second connection", () => {
    expect(keyFormOpen("groq", [connection("g", "open_ai_compatible", "https://api.groq.com/openai/v1")])).toBeNull();
    expect(keyFormOpen("openai", [connection("o", "open_ai")])).toBeNull();
  });
});
