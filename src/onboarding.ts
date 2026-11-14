import type { Settings } from "./settings";

/// Whether the OS lets ahem use something it needs (`platform::Permission`).
export type Permission = "granted" | "missing" | "not_needed";
export type PermissionKind = "microphone" | "accessibility";
export interface Permissions {
  microphone: Permission;
  accessibility: Permission;
}

export type StepKey = "model" | PermissionKind | "services";
export interface Step {
  key: StepKey;
  done: boolean;
  optional: boolean;
}

/// How often the open checklist asks the OS again, so a permission granted in the system
/// settings gets its check mark without a restart.
export const PERMISSIONS_POLL_MS = 2000;
/// The last check mark stays in sight this long before the checklist hides itself.
export const HIDE_DELAY_MS = 1500;

/// First-run steps in order. A permission the OS does not ask for has no step; until the
/// permissions are read, their steps are left out.
export function onboardingSteps(
  settings: Pick<Settings, "model_id" | "connections">,
  permissions: Permissions | null,
): Step[] {
  const steps: Step[] = [{ key: "model", done: settings.model_id !== null, optional: false }];
  for (const kind of ["microphone", "accessibility"] as const) {
    const permission = permissions?.[kind];
    if (permission !== undefined && permission !== "not_needed") {
      steps.push({ key: kind, done: permission === "granted", optional: false });
    }
  }
  steps.push({ key: "services", done: settings.connections.length > 0, optional: true });
  return steps;
}

/// Every step is done, the optional one too: a step still open keeps the checklist in sight
/// until the user hides it. Never before the permissions are read.
export function onboardingComplete(steps: Step[], permissions: Permissions | null): boolean {
  return permissions !== null && steps.every((step) => step.done);
}
