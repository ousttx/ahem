//! Speech engine lifecycle: started on demand, stopped by the unload policy.
//!
//! Work holds the engine: a dictation from the microphone's opening (a recording recognized again from
//! "Recognize again") until its transcript is ready (not until the language model answers), a
//! file task until its last window is transcribed (not through telling voices apart or the
//! language model), and a window of a file that ended while it was at the engine until its
//! answer.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::InvalidTransition;

const MACHINE: &str = "engine lifecycle";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum UnloadPolicy {
    /// Stop the engine as soon as no work holds it.
    #[default]
    Immediate,
    AfterIdle {
        minutes: u32,
    },
    Never,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineState {
    Unloaded,
    Loading,
    Ready,
    Unloading,
}

impl EngineState {
    fn name(self) -> &'static str {
        match self {
            Self::Unloaded => "unloaded",
            Self::Loading => "loading",
            Self::Ready => "ready",
            Self::Unloading => "unloading",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineInput {
    /// Work needs the engine.
    Acquire,
    /// That work no longer needs it.
    Release,
    Loaded,
    LoadFailed {
        detail: String,
    },
    IdleTimeout {
        token: u64,
    },
    /// The engine process exited after `StopEngine`.
    Stopped,
    /// The engine process exited on its own.
    Crashed {
        detail: String,
    },
    PolicyChanged(UnloadPolicy),
    /// The engine configuration (e.g. the model) changed: the running engine takes no new
    /// request (`restarting`: the shell holds them back) but the dictation of a stream open at
    /// it, which it finishes, and stops once none is at it (`Idle`, which the shell sends after
    /// that stream too) or no work holds it; work still holding it starts it again with the new
    /// configuration, so other audio goes only to the engine the user chose (a stream is only
    /// ever open at a model on this computer). After a failed load, work still holding the
    /// engine starts it with the new one at once.
    Restart,
    /// No request is at the engine: a restart waiting for one stops it now.
    Idle,
}

impl EngineInput {
    fn name(&self) -> &'static str {
        match self {
            Self::Acquire => "acquire",
            Self::Release => "release",
            Self::Loaded => "loaded",
            Self::LoadFailed { .. } => "load_failed",
            Self::IdleTimeout { .. } => "idle_timeout",
            Self::Stopped => "stopped",
            Self::Crashed { .. } => "crashed",
            Self::PolicyChanged(_) => "policy_changed",
            Self::Restart => "restart",
            Self::Idle => "idle",
        }
    }
}

/// Actions for the shell to perform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineEffect {
    StartEngine,
    StopEngine,
    /// Report `IdleTimeout { token }` after `after`.
    StartIdleTimer {
        token: u64,
        after: Duration,
    },
    CancelIdleTimer {
        token: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EngineEvent {
    StateChanged { state: EngineState },
    Failed { detail: String },
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Transition {
    pub effects: Vec<EngineEffect>,
    pub events: Vec<EngineEvent>,
}

#[derive(Debug)]
pub struct EngineLifecycle {
    state: EngineState,
    policy: UnloadPolicy,
    holds: u32,
    idle_timer: Option<u64>,
    next_token: u64,
    /// A restart waits for the request at the engine to finish (`Idle`).
    restart_pending: bool,
}

impl EngineLifecycle {
    pub fn new(policy: UnloadPolicy) -> Self {
        Self {
            state: EngineState::Unloaded,
            policy,
            holds: 0,
            idle_timer: None,
            next_token: 0,
            restart_pending: false,
        }
    }

    pub fn state(&self) -> EngineState {
        self.state
    }

    /// The engine runs the old configuration until the request and the stream at it finish: it
    /// must take no new one but the dictation of that stream.
    pub fn restarting(&self) -> bool {
        self.state == EngineState::Ready && self.restart_pending
    }

    #[cfg(test)]
    pub fn holds(&self) -> u32 {
        self.holds
    }

    pub fn handle(&mut self, input: EngineInput) -> Result<Transition, InvalidTransition> {
        use EngineInput as I;
        use EngineState as S;

        let before = self.state;
        let mut t = Transition::default();
        match (self.state, input) {
            (state, I::Acquire) => {
                self.holds += 1;
                match state {
                    S::Unloaded => self.start(&mut t),
                    S::Ready => self.disarm(&mut t),
                    // Unloading restarts the engine once it has stopped.
                    S::Loading | S::Unloading => {}
                }
            }
            (state, I::Release) => {
                if self.holds == 0 {
                    return Err(InvalidTransition::new(MACHINE, state.name(), "release"));
                }
                self.holds -= 1;
                if self.holds == 0 {
                    match state {
                        S::Ready if self.restart_pending => {
                            self.restart_pending = false;
                            self.stop(&mut t);
                        }
                        S::Ready => self.apply_policy(&mut t),
                        S::Loading if self.policy == UnloadPolicy::Immediate => self.stop(&mut t),
                        _ => {}
                    }
                }
            }
            (S::Loading, I::Loaded) => {
                self.state = S::Ready;
                if self.holds == 0 {
                    self.apply_policy(&mut t);
                }
            }
            (S::Loading, I::LoadFailed { detail }) => {
                self.state = S::Unloaded;
                t.events.push(EngineEvent::Failed { detail });
            }
            // A load that finished or failed while it was being stopped is stale.
            (S::Unloading, I::Loaded | I::LoadFailed { .. }) => {}
            (S::Ready, I::IdleTimeout { token }) if self.idle_timer == Some(token) => {
                self.idle_timer = None;
                self.stop(&mut t);
            }
            // The timer was cancelled or replaced before it fired.
            (_, I::IdleTimeout { .. }) => {}
            (S::Unloading, I::Stopped) => {
                self.state = S::Unloaded;
                self.restart_pending = false;
                if self.holds > 0 {
                    self.start(&mut t);
                }
            }
            // A crash under other work starts the engine again for it: the request that crashed
            // (it holds the engine too) fails on its own. Not for that request alone, nor while
            // loading, where a model that cannot load would loop.
            (S::Ready, I::Crashed { .. }) if self.holds > 1 => {
                self.disarm(&mut t);
                self.restart_pending = false;
                self.start(&mut t);
            }
            (S::Loading | S::Ready, I::Crashed { detail }) => {
                self.disarm(&mut t);
                self.state = S::Unloaded;
                self.restart_pending = false;
                t.events.push(EngineEvent::Failed { detail });
            }
            (state, I::PolicyChanged(policy)) => {
                self.policy = policy;
                if state == S::Ready && self.holds == 0 {
                    self.disarm(&mut t);
                    self.apply_policy(&mut t);
                }
            }
            // Loading the old configuration is pointless; work still holding the engine
            // restarts it with the new one once it has stopped.
            (S::Loading, I::Restart) => self.stop(&mut t),
            (S::Ready, I::Restart) => {
                if self.holds == 0 {
                    self.disarm(&mut t);
                    self.stop(&mut t);
                } else {
                    self.restart_pending = true;
                }
            }
            // Work still holds an engine that failed to load: the new configuration gets its try
            // now, not after that work failed with the old error.
            (S::Unloaded, I::Restart) if self.holds > 0 => self.start(&mut t),
            // Not running, or already stopping: the next start uses the new configuration.
            (S::Unloaded | S::Unloading, I::Restart) => {}
            (S::Ready, I::Idle) if self.restart_pending => {
                self.restart_pending = false;
                self.disarm(&mut t);
                self.stop(&mut t);
            }
            // No restart waits.
            (_, I::Idle) => {}
            (state, input) => {
                return Err(InvalidTransition::new(MACHINE, state.name(), input.name()));
            }
        }
        if self.state != before {
            t.events.insert(0, EngineEvent::StateChanged { state: self.state });
        }
        Ok(t)
    }

    fn start(&mut self, t: &mut Transition) {
        self.state = EngineState::Loading;
        t.effects.push(EngineEffect::StartEngine);
    }

    fn stop(&mut self, t: &mut Transition) {
        self.state = EngineState::Unloading;
        t.effects.push(EngineEffect::StopEngine);
    }

    fn disarm(&mut self, t: &mut Transition) {
        if let Some(token) = self.idle_timer.take() {
            t.effects.push(EngineEffect::CancelIdleTimer { token });
        }
    }

    /// Called when the engine is ready and nothing holds it.
    fn apply_policy(&mut self, t: &mut Transition) {
        match self.policy {
            UnloadPolicy::Immediate => self.stop(t),
            UnloadPolicy::AfterIdle { minutes } => {
                self.next_token += 1;
                self.idle_timer = Some(self.next_token);
                t.effects.push(EngineEffect::StartIdleTimer {
                    token: self.next_token,
                    after: Duration::from_secs(u64::from(minutes) * 60),
                });
            }
            UnloadPolicy::Never => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use EngineEffect as E;
    use EngineInput as I;

    fn run(engine: &mut EngineLifecycle, input: EngineInput) -> Transition {
        engine.handle(input).expect("valid transition")
    }

    fn ready(policy: UnloadPolicy) -> EngineLifecycle {
        let mut engine = EngineLifecycle::new(policy);
        run(&mut engine, I::Acquire);
        run(&mut engine, I::Loaded);
        engine
    }

    #[test]
    fn acquire_starts_the_engine() {
        let mut engine = EngineLifecycle::new(UnloadPolicy::Immediate);
        let t = run(&mut engine, I::Acquire);
        assert_eq!(t.effects, vec![E::StartEngine]);
        assert_eq!(t.events, vec![EngineEvent::StateChanged { state: EngineState::Loading }]);
        let t = run(&mut engine, I::Loaded);
        assert_eq!(engine.state(), EngineState::Ready);
        assert!(t.effects.is_empty(), "held engine stays loaded");
    }

    #[test]
    fn immediate_policy_stops_when_work_is_done() {
        let mut engine = ready(UnloadPolicy::Immediate);
        let t = run(&mut engine, I::Release);
        assert_eq!(t.effects, vec![E::StopEngine]);
        assert_eq!(engine.state(), EngineState::Unloading);
        run(&mut engine, I::Stopped);
        assert_eq!(engine.state(), EngineState::Unloaded);
    }

    #[test]
    fn immediate_policy_stops_a_load_nobody_needs() {
        let mut engine = EngineLifecycle::new(UnloadPolicy::Immediate);
        run(&mut engine, I::Acquire);
        let t = run(&mut engine, I::Release);
        assert_eq!(t.effects, vec![E::StopEngine]);
        let t = run(&mut engine, I::Loaded);
        assert!(t.effects.is_empty() && t.events.is_empty(), "stale load ignored");
        assert_eq!(engine.state(), EngineState::Unloading);
    }

    #[test]
    fn idle_policy_waits_for_the_timer() {
        let mut engine = EngineLifecycle::new(UnloadPolicy::AfterIdle { minutes: 10 });
        run(&mut engine, I::Acquire);
        run(&mut engine, I::Release);
        assert_eq!(engine.state(), EngineState::Loading, "loading continues");
        let t = run(&mut engine, I::Loaded);
        assert_eq!(
            t.effects,
            vec![E::StartIdleTimer { token: 1, after: Duration::from_secs(600) }]
        );
        let t = run(&mut engine, I::IdleTimeout { token: 1 });
        assert_eq!(t.effects, vec![E::StopEngine]);
    }

    #[test]
    fn new_work_cancels_the_idle_timer() {
        let mut engine = ready(UnloadPolicy::AfterIdle { minutes: 5 });
        run(&mut engine, I::Release);
        let t = run(&mut engine, I::Acquire);
        assert_eq!(t.effects, vec![E::CancelIdleTimer { token: 1 }]);
        let t = run(&mut engine, I::IdleTimeout { token: 1 });
        assert!(t.effects.is_empty(), "stale timer ignored");
        assert_eq!(engine.state(), EngineState::Ready);
        let t = run(&mut engine, I::Release);
        assert_eq!(
            t.effects,
            vec![E::StartIdleTimer { token: 2, after: Duration::from_secs(300) }]
        );
    }

    #[test]
    fn never_policy_keeps_the_engine() {
        let mut engine = ready(UnloadPolicy::Never);
        let t = run(&mut engine, I::Release);
        assert!(t.effects.is_empty());
        assert_eq!(engine.state(), EngineState::Ready);
    }

    #[test]
    fn work_during_unloading_restarts_the_engine() {
        let mut engine = ready(UnloadPolicy::Immediate);
        run(&mut engine, I::Release);
        let t = run(&mut engine, I::Acquire);
        assert!(t.effects.is_empty(), "wait until the process stops");
        let t = run(&mut engine, I::Stopped);
        assert_eq!(t.effects, vec![E::StartEngine]);
        assert_eq!(engine.state(), EngineState::Loading);
    }

    #[test]
    fn load_failure_reports_and_allows_retry() {
        let mut engine = EngineLifecycle::new(UnloadPolicy::Immediate);
        run(&mut engine, I::Acquire);
        let t = run(&mut engine, I::LoadFailed { detail: "no model".into() });
        assert_eq!(engine.state(), EngineState::Unloaded);
        assert!(t.events.contains(&EngineEvent::Failed { detail: "no model".into() }));
        let t = run(&mut engine, I::Acquire);
        assert_eq!(t.effects, vec![E::StartEngine]);
        assert_eq!(engine.holds(), 2);
    }

    #[test]
    fn crash_cancels_the_timer_and_reports() {
        let mut engine = ready(UnloadPolicy::AfterIdle { minutes: 5 });
        run(&mut engine, I::Release);
        let t = run(&mut engine, I::Crashed { detail: "exit 3".into() });
        assert_eq!(t.effects, vec![E::CancelIdleTimer { token: 1 }]);
        assert_eq!(engine.state(), EngineState::Unloaded);
        assert!(t.events.contains(&EngineEvent::Failed { detail: "exit 3".into() }));
    }

    #[test]
    fn a_crash_under_work_starts_the_engine_again_but_not_while_it_loads() {
        // Only the request that crashed holds it: it stays down.
        let mut engine = ready(UnloadPolicy::Never);
        let t = run(&mut engine, I::Crashed { detail: "exit 3".into() });
        assert_eq!(engine.state(), EngineState::Unloaded);
        assert!(t.events.contains(&EngineEvent::Failed { detail: "exit 3".into() }));
        // Another dictation waits for it too: it starts again.
        let mut engine = ready(UnloadPolicy::Never);
        run(&mut engine, I::Acquire);
        let t = run(&mut engine, I::Crashed { detail: "exit 3".into() });
        assert_eq!(t.effects, vec![E::StartEngine]);
        assert_eq!(engine.state(), EngineState::Loading);
        assert!(!t.events.iter().any(|event| matches!(event, EngineEvent::Failed { .. })));
        // Crashing again while loading: it stays down and says so.
        let t = run(&mut engine, I::Crashed { detail: "exit 3".into() });
        assert_eq!(engine.state(), EngineState::Unloaded);
        assert!(t.events.contains(&EngineEvent::Failed { detail: "exit 3".into() }));
    }

    #[test]
    fn policy_change_applies_to_an_idle_engine() {
        let mut engine = ready(UnloadPolicy::Never);
        run(&mut engine, I::Release);
        let t = run(&mut engine, I::PolicyChanged(UnloadPolicy::AfterIdle { minutes: 1 }));
        assert_eq!(t.effects, vec![E::StartIdleTimer { token: 1, after: Duration::from_secs(60) }]);
        let t = run(&mut engine, I::PolicyChanged(UnloadPolicy::Immediate));
        assert_eq!(t.effects, vec![E::CancelIdleTimer { token: 1 }, E::StopEngine]);
    }

    #[test]
    fn restart_stops_an_idle_engine() {
        let mut engine = ready(UnloadPolicy::AfterIdle { minutes: 5 });
        run(&mut engine, I::Release);
        let t = run(&mut engine, I::Restart);
        assert_eq!(t.effects, vec![E::CancelIdleTimer { token: 1 }, E::StopEngine]);
        run(&mut engine, I::Stopped);
        assert_eq!(engine.state(), EngineState::Unloaded, "next work loads the new model");
    }

    #[test]
    fn restart_waits_for_running_work() {
        let mut engine = ready(UnloadPolicy::Never);
        let t = run(&mut engine, I::Restart);
        assert!(t.effects.is_empty(), "work still uses the engine");
        let t = run(&mut engine, I::Release);
        assert_eq!(t.effects, vec![E::StopEngine], "stops despite the never policy");
        run(&mut engine, I::Stopped);
        run(&mut engine, I::Acquire);
        run(&mut engine, I::Loaded);
        let t = run(&mut engine, I::Release);
        assert!(t.effects.is_empty(), "the restart happened only once");
    }

    /// A restart under work: the engine takes no new request and stops once none is at it;
    /// the work still holding it starts it with the new configuration.
    #[test]
    fn a_restart_stops_the_engine_once_no_request_is_at_it() {
        let mut engine = ready(UnloadPolicy::Never);
        assert!(run(&mut engine, I::Idle).effects.is_empty(), "no restart waits");
        run(&mut engine, I::Restart);
        assert!(engine.restarting());
        let t = run(&mut engine, I::Idle);
        assert_eq!(t.effects, vec![E::StopEngine]);
        assert!(!engine.restarting());
        let t = run(&mut engine, I::Stopped);
        assert_eq!(t.effects, vec![E::StartEngine], "the work holding it starts the new one");
        run(&mut engine, I::Loaded);
        assert!(!engine.restarting());
    }

    #[test]
    fn restart_during_loading_loads_again() {
        let mut engine = EngineLifecycle::new(UnloadPolicy::Immediate);
        run(&mut engine, I::Acquire);
        let t = run(&mut engine, I::Restart);
        assert_eq!(t.effects, vec![E::StopEngine]);
        run(&mut engine, I::Loaded); // stale load of the old model
        let t = run(&mut engine, I::Stopped);
        assert_eq!(t.effects, vec![E::StartEngine], "the waiting work restarts it");
    }

    /// A recording holds an engine that failed to load; another model is chosen: it loads now.
    #[test]
    fn restart_after_a_failed_load_loads_the_new_model_for_waiting_work() {
        let mut engine = EngineLifecycle::new(UnloadPolicy::Immediate);
        run(&mut engine, I::Acquire);
        run(&mut engine, I::LoadFailed { detail: "broken".into() });
        assert_eq!(engine.state(), EngineState::Unloaded);
        let t = run(&mut engine, I::Restart);
        assert_eq!(t.effects, vec![E::StartEngine]);
    }

    #[test]
    fn restart_without_engine_does_nothing() {
        let mut engine = EngineLifecycle::new(UnloadPolicy::Immediate);
        let t = run(&mut engine, I::Restart);
        assert_eq!(t, Transition::default());
    }

    #[test]
    fn invalid_inputs_are_errors() {
        let mut engine = EngineLifecycle::new(UnloadPolicy::Immediate);
        assert_eq!(engine.handle(I::Release).unwrap_err().input, "release");
        assert_eq!(engine.handle(I::Loaded).unwrap_err().state, "unloaded");
        assert!(engine.handle(I::Stopped).is_err());
        let mut engine = ready(UnloadPolicy::Immediate);
        assert!(engine.handle(I::Stopped).is_err(), "use Crashed for an unexpected exit");
        assert_eq!(engine.state(), EngineState::Ready);
    }
}
