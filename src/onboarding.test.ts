import { describe, expect, it } from "vitest";
import { onboardingComplete, onboardingSteps, type Permissions } from "./onboarding";
import type { Connection } from "./settings";

const noModel = { model_id: null, connections: [] as Connection[] };
const ready = { model_id: "turbo", connections: [] as Connection[] };
const mac = (microphone: "granted" | "missing", accessibility: "granted" | "missing"): Permissions => ({
  microphone,
  accessibility,
});
const windows: Permissions = { microphone: "granted", accessibility: "not_needed" };
const linux: Permissions = { microphone: "not_needed", accessibility: "not_needed" };

describe("the first-run checklist", () => {
  it("asks for the model, the permissions the OS wants, then the optional language model", () => {
    expect(onboardingSteps(noModel, mac("missing", "missing")).map((step) => step.key)).toEqual([
      "model",
      "microphone",
      "accessibility",
      "services",
    ]);
  });

  it("has no step for a permission the OS does not ask for", () => {
    expect(onboardingSteps(noModel, windows).map((step) => step.key)).toEqual(["model", "microphone", "services"]);
    expect(onboardingSteps(noModel, linux).map((step) => step.key)).toEqual(["model", "services"]);
  });

  it("leaves the permission steps out until the permissions are read", () => {
    expect(onboardingSteps(noModel, null).map((step) => step.key)).toEqual(["model", "services"]);
  });

  it("checks a step as soon as the state says so", () => {
    const steps = onboardingSteps(ready, mac("granted", "missing"));
    expect(steps.map((step) => step.done)).toEqual([true, true, false, false]);
    expect(steps.find((step) => step.key === "services")?.optional).toBe(true);
  });

  it("is complete only once every step is done, the optional one too", () => {
    const connected = { model_id: "turbo", connections: [{} as Connection] };
    const permissions = mac("granted", "granted");
    expect(onboardingComplete(onboardingSteps(ready, permissions), permissions)).toBe(false);
    expect(onboardingComplete(onboardingSteps(connected, permissions), permissions)).toBe(true);
    const missing = mac("granted", "missing");
    expect(onboardingComplete(onboardingSteps(connected, missing), missing)).toBe(false);
    expect(onboardingComplete(onboardingSteps(noModel, linux), linux)).toBe(false);
    expect(onboardingComplete(onboardingSteps(connected, linux), linux)).toBe(true);
  });

  it("is never complete before the permissions are read", () => {
    const connected = { model_id: "turbo", connections: [{} as Connection] };
    expect(onboardingComplete(onboardingSteps(connected, null), null)).toBe(false);
  });
});
