import { invoke } from "@tauri-apps/api/core";
import { Fragment, useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import type { Target } from "../App";
import {
  HIDE_DELAY_MS,
  onboardingComplete,
  onboardingSteps,
  PERMISSIONS_POLL_MS,
  type PermissionKind,
  type Permissions,
  type StepKey,
} from "../onboarding";
import { commandFailure } from "../present";
import type { SaveError, Settings } from "../settings";
import { Card, Icon, readOnFocus, shownTitle, useFocusAfter } from "../ui";

interface Props {
  settings: Settings;
  update: (change: Partial<Settings>) => Promise<SaveError | null>;
  navigate: (target: Target) => void;
}

/// Where the button of a step leads: a page of the app, or the OS's permission request.
const ACTIONS: Record<StepKey, { target: Target } | { permission: PermissionKind }> = {
  model: { target: "models" },
  microphone: { permission: "microphone" },
  accessibility: { permission: "accessibility" },
  services: { target: "services" },
};

/// First-run steps; each one is checked from the app's and the OS's state, not by the user.
/// The checklist hides itself once every step is done, otherwise when the user hides it.
export function OnboardingPanel({ settings, update, navigate }: Props) {
  const { t } = useTranslation();
  const [permissions, setPermissions] = useState<Permissions | null>(null);
  // The step whose system settings did not open, and why.
  const [failure, setFailure] = useState<{ key: StepKey; text: string } | null>(null);
  const hidden = settings.onboarding_done;

  const read = useCallback(() => {
    invoke<Permissions>("get_permissions")
      .then((now) =>
        setPermissions((before) =>
          before?.microphone === now.microphone && before.accessibility === now.accessibility ? before : now,
        ),
      )
      .catch(() => {});
  }, []);

  useEffect(() => {
    if (hidden) return;
    const stop = readOnFocus(read);
    // Only while the window is seen: a hidden one (in the tray) has nothing to update.
    const timer = window.setInterval(() => {
      if (document.visibilityState === "visible") read();
    }, PERMISSIONS_POLL_MS);
    return () => {
      window.clearInterval(timer);
      stop();
    };
  }, [hidden, read]);

  const steps = onboardingSteps(settings, permissions);
  const complete = onboardingComplete(steps, permissions);
  const hide = useRef(update);
  hide.current = update;
  // "Hide" goes with the checklist: the focus goes to the page's title. A step allowed takes its
  // button away: the focus goes to the button of a step still to do, else to the title.
  const follow = useFocusAfter();
  useEffect(() => {
    if (hidden || !complete) return;
    const timer = window.setTimeout(() => void hide.current({ onboarding_done: true }), HIDE_DELAY_MS);
    return () => window.clearTimeout(timer);
  }, [hidden, complete]);

  if (hidden) return null;

  const act = (key: StepKey, button: HTMLElement) => {
    const action = ACTIONS[key];
    setFailure(null);
    if ("target" in action) return navigate(action.target);
    const list = button.closest(".onboarding");
    const page = button.closest(".page");
    follow(() => list?.querySelector<HTMLElement>("button") ?? (page && shownTitle(page)));
    const request = invoke("request_permission", { kind: action.permission });
    void commandFailure(request, (detail) => t("onboarding.allow_failed", { detail })).then((text) => {
      if (text) setFailure({ key, text });
      else read();
    });
  };

  return (
    <Card
      title={t("onboarding.title")}
      action={
        <button
          type="button"
          className="btn link"
          onClick={(event) => {
            const page = event.currentTarget.closest(".page");
            follow(() => page && shownTitle(page));
            void update({ onboarding_done: true });
          }}
        >
          {t("onboarding.hide")}
        </button>
      }
    >
      <div className="onboarding">
        {steps.map((step) => (
          <Fragment key={step.key}>
            <div className={step.done ? "onboarding-step done" : "onboarding-step"}>
              {/* A step still to do asks for attention; a done or optional one does not. */}
              <Icon name={step.done ? "check" : "circle"} size={15} tone={step.done ? "text" : step.optional ? "faint" : "danger"} />
              <span className="onboarding-text">
                {t(`onboarding.${step.key}`)}
                {step.optional && <span className="caption"> · {t("onboarding.optional")}</span>}
              </span>
              {!step.done && (
                <button type="button" className={step.optional ? "btn link" : "btn link attention"} onClick={(event) => act(step.key, event.currentTarget)}>
                  {t("target" in ACTIONS[step.key] ? "onboarding.open" : "onboarding.allow")}
                </button>
              )}
            </div>
            {failure?.key === step.key && !step.done && (
              <span className="caption warning-text" role="alert">
                {failure.text}
              </span>
            )}
          </Fragment>
        ))}
      </div>
    </Card>
  );
}
