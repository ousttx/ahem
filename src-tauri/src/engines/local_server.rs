//! What whisper-server and llama-server share: a child process with its output in a log, waited
//! for until it answers `/health` on this computer, and killed when it is dropped.
//!
//! The server runs under a guard: the app itself started with [`GUARD_ARG`], which starts the
//! server and ends it as soon as its input closes. The input is a pipe from the app, so it
//! closes when the app ends in any way (a crash, a forced quit): a model of gigabytes never
//! stays in memory without its app. The same holds on every system, and no process is looked
//! for by its number.

use std::fs::{self, File};
use std::io::Read as _;
use std::net::TcpListener;
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use std::{io, thread};

use serde::Deserialize;
use ureq::Agent;

use super::whisper_server::EngineError;
use crate::platform;

/// A model may take this long to load.
pub const START_TIMEOUT: Duration = Duration::from_secs(180);
/// One recognition request may take this long.
pub const INFERENCE_TIMEOUT: Duration = Duration::from_secs(600);
const HEALTH_TIMEOUT: Duration = Duration::from_millis(500);
/// How long a failed request waits to see whether its process died: a request fails as soon as
/// the process closes its end, a little before it can be seen to have ended (and a guard
/// notices its server's end within a tenth of a second).
const DEATH_NOTICE: Duration = Duration::from_millis(400);

/// Opens an engine's log for a new start. The last start's log is kept as `<name>.previous.log`:
/// after a crash the next dictation starts the engine again, and the reason must not go with it.
pub fn open_log(path: &Path) -> io::Result<File> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let _ = fs::rename(path, path.with_extension("previous.log"));
    File::create(path)
}

/// A port nobody listens on now, for the server to take.
pub fn free_port() -> io::Result<u16> {
    Ok(TcpListener::bind(("127.0.0.1", 0))?.local_addr()?.port())
}

/// The argument that starts the app as the guard of a server: `ahem --server-guard <program>
/// <arguments>…`.
pub const GUARD_ARG: &str = "--server-guard";
/// How long the guard gets to end its server after its input closes.
const GUARD_WAIT: Duration = Duration::from_secs(2);

/// A running local server: the guard, or in tests the server itself.
pub struct ServerProcess {
    child: Child,
    /// The guard's input; dropping it tells the guard to end the server.
    guard: Option<ChildStdin>,
}

impl ServerProcess {
    /// Starts `command` (the program at `executable` with its arguments and environment) under
    /// a guard: its output in `log`, its own folder as the working directory, no console window.
    pub fn spawn(command: Command, executable: &Path, log: &Path) -> Result<Self, EngineError> {
        // Under `cargo test` the running program is the test binary, which has no guard.
        if cfg!(test) {
            return Self::spawn_under(None, command, executable, log);
        }
        let app = std::env::current_exe()?;
        Self::spawn_under(Some(&app), command, executable, log)
    }

    /// `guard`: the app binary that runs the guard; `None` starts the server itself (tests).
    fn spawn_under(
        guard: Option<&Path>,
        command: Command,
        executable: &Path,
        log: &Path,
    ) -> Result<Self, EngineError> {
        let log = open_log(log)?;
        let mut command = match guard {
            Some(app) => guarded(app, &command),
            None => command,
        };
        let input = if guard.is_some() { Stdio::piped() } else { Stdio::null() };
        command.stdin(input).stdout(log.try_clone()?).stderr(log);
        if let Some(dir) = executable.parent() {
            command.current_dir(dir);
        }
        platform::hide_console(&mut command);
        let mut child = command.spawn().map_err(|error| EngineError::Start(error.to_string()))?;
        let guard = child.stdin.take();
        Ok(Self { child, guard })
    }

    /// Waits until `healthy` says so; an error if the process ends first, `cancel` is set or
    /// the model does not load in time.
    pub fn wait_ready(
        &mut self,
        cancel: &AtomicBool,
        healthy: impl Fn() -> bool,
    ) -> Result<(), EngineError> {
        let deadline = Instant::now() + START_TIMEOUT;
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(EngineError::Cancelled);
            }
            if let Some(status) = self.child.try_wait()? {
                return Err(EngineError::Exited(status.code()));
            }
            if healthy() {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(EngineError::Timeout);
            }
            thread::sleep(Duration::from_millis(200));
        }
    }

    /// After a failed request: whether the server died (or cannot be asked).
    pub fn died(&mut self) -> bool {
        died(&mut self.child)
    }

    pub fn stop(mut self) -> io::Result<()> {
        self.kill()
    }

    fn kill(&mut self) -> io::Result<()> {
        // The guard ends its server once its input closes; killing the guard itself would
        // leave the server running.
        if self.guard.take().is_some() {
            let until = Instant::now() + GUARD_WAIT;
            while Instant::now() < until {
                if self.child.try_wait()?.is_some() {
                    return Ok(());
                }
                thread::sleep(Duration::from_millis(20));
            }
        }
        if self.child.try_wait()?.is_none() {
            self.child.kill()?;
        }
        self.child.wait()?;
        Ok(())
    }
}

/// After a failed request: whether the process died (or cannot be asked). A request fails the
/// moment the process closes its end, a little before the process can be seen to have ended.
pub fn died(child: &mut Child) -> bool {
    let until = Instant::now() + DEATH_NOTICE;
    loop {
        if !matches!(child.try_wait(), Ok(None)) {
            return true;
        }
        if Instant::now() >= until {
            return false;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// The app started as the guard of `command`'s program, with its arguments, environment and
/// working directory.
fn guarded(app: &Path, command: &Command) -> Command {
    let mut guard = Command::new(app);
    guard.arg(GUARD_ARG).arg(command.get_program()).args(command.get_args());
    for (name, value) in command.get_envs() {
        match value {
            Some(value) => guard.env(name, value),
            None => guard.env_remove(name),
        };
    }
    if let Some(dir) = command.get_current_dir() {
        guard.current_dir(dir);
    }
    guard
}

/// Runs the guard when the app was started as one: call first in `main`. `true` means the guard
/// ran and the process should end. The server's output goes where the guard's does (the log);
/// the guard ends with the server's exit code.
pub fn run_guard_if_asked() -> bool {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new(GUARD_ARG)) {
        return false;
    }
    let Some(program) = args.next() else { std::process::exit(2) };
    let mut command = Command::new(program);
    command.args(args).stdin(Stdio::null());
    platform::hide_console(&mut command);
    let mut server = match command.spawn() {
        Ok(server) => server,
        Err(error) => {
            // The guard's output is the engine's log: the reason stays there.
            log_line!("cannot start {}: {error}", Path::new(command.get_program()).display());
            std::process::exit(127)
        }
    };
    // A thread reads until the app's end of the pipe closes; this one watches both.
    let app_gone = std::sync::Arc::new(AtomicBool::new(false));
    let gone = std::sync::Arc::clone(&app_gone);
    thread::spawn(move || {
        let _ = io::stdin().read_to_end(&mut Vec::new());
        gone.store(true, Ordering::Relaxed);
    });
    loop {
        if let Ok(Some(status)) = server.try_wait() {
            std::process::exit(guard_exit(&status));
        }
        if app_gone.load(Ordering::Relaxed) {
            let _ = server.kill();
            let _ = server.wait();
            std::process::exit(0);
        }
        thread::sleep(Duration::from_millis(100));
    }
}

/// The guard's exit code for the server's end: its own code, or `SIGNALLED` when the system ended
/// it (a crash), said in the log by the status, which names the signal.
fn guard_exit(status: &std::process::ExitStatus) -> i32 {
    status.code().unwrap_or_else(|| {
        log_line!("the engine ended: {status}");
        SIGNALLED
    })
}

/// The shells' code for a process the system ended, less the signal's number.
const SIGNALLED: i32 = 128;

impl Drop for ServerProcess {
    fn drop(&mut self) {
        let _ = self.kill();
    }
}

/// The text of a local server's answer. Any status but 200 is an error that quotes the start of
/// the answer: it comes from the engine on this computer, not from a service.
pub fn answer_text(
    response: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
) -> Result<String, EngineError> {
    let mut response = response.map_err(|error| EngineError::Http(error.to_string()))?;
    let status = response.status().as_u16();
    let text = response
        .body_mut()
        .read_to_string()
        .map_err(|error| EngineError::Http(error.to_string()))?;
    if status != 200 {
        let detail: String = text.chars().take(250).collect();
        return Err(EngineError::Http(format!("HTTP {status}: {detail}")));
    }
    Ok(text)
}

/// The two clients of a local server: a quick one for `/health`, a patient one for requests.
pub struct Agents {
    pub health: Agent,
    pub inference: Agent,
}

impl Default for Agents {
    fn default() -> Self {
        let agent = |timeout| {
            let config = Agent::config_builder()
                .timeout_global(Some(timeout))
                .http_status_as_error(false)
                // This computer only: never through a proxy of the environment.
                .proxy(None)
                .build();
            Agent::new_with_config(config)
        };
        Self { health: agent(HEALTH_TIMEOUT), inference: agent(INFERENCE_TIMEOUT) }
    }
}

impl Agents {
    /// Whether the server at `base` answers `/health` with `{"status": "ok"}`: both servers do
    /// once their model is loaded.
    pub fn healthy(&self, base: &str) -> bool {
        #[derive(Deserialize)]
        struct Health {
            status: String,
        }
        let Ok(mut response) = self.health.get(format!("{base}/health")).call() else {
            return false;
        };
        response.status().as_u16() == 200
            && response.body_mut().read_to_string().is_ok_and(|text| {
                serde_json::from_str::<Health>(&text).is_ok_and(|health| health.status == "ok")
            })
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn log(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/test-artifacts")
            .join(format!("local-server-{name}-{}.log", std::process::id()))
    }

    fn alive(pid: u32) -> bool {
        Command::new("kill").args(["-0", &pid.to_string()]).status().is_ok_and(|s| s.success())
    }

    #[test]
    fn a_server_that_ends_before_it_is_ready_is_reported_with_its_code() {
        let mut command = Command::new("sh");
        command.args(["-c", "echo no model; exit 3"]);
        let path = log("exits");
        let mut process =
            ServerProcess::spawn_under(None, command, Path::new("/bin/sh"), &path).unwrap();
        let never = AtomicBool::new(false);
        let result = process.wait_ready(&never, || false);
        assert!(matches!(result, Err(EngineError::Exited(Some(3)))), "{result:?}");
        // Its output went to the log.
        assert_eq!(fs::read_to_string(&path).unwrap(), "no model\n");
    }

    /// A server the system ended (a crash) is told from one that exited with an error.
    #[test]
    fn a_crash_has_a_code_of_its_own() {
        let crashed = Command::new("sh").args(["-c", "kill -KILL $$"]).status().unwrap();
        assert_eq!(guard_exit(&crashed), SIGNALLED);
        let failed = Command::new("sh").args(["-c", "exit 1"]).status().unwrap();
        assert_eq!(guard_exit(&failed), 1);
    }

    #[test]
    fn the_last_log_is_kept_at_a_new_start() {
        let path = log("rotates");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "why it crashed").unwrap();
        drop(open_log(&path).unwrap());
        let previous = path.with_extension("previous.log");
        assert_eq!(fs::read_to_string(&previous).unwrap(), "why it crashed");
        assert_eq!(fs::read_to_string(&path).unwrap(), "");
    }

    #[test]
    fn a_server_that_died_is_noticed_after_a_failed_request() {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 0.1; exit 1"]);
        let mut process =
            ServerProcess::spawn_under(None, command, Path::new("/bin/sh"), &log("dies")).unwrap();
        // The request failed a moment before the process was gone.
        assert!(process.died());
    }

    #[test]
    fn a_running_server_is_killed_when_dropped_and_a_stop_ends_the_wait() {
        let mut command = Command::new("sleep");
        command.arg("60");
        let mut process =
            ServerProcess::spawn_under(None, command, Path::new("/bin/sleep"), &log("sleeps"))
                .unwrap();
        let pid = process.child.id();
        assert!(alive(pid));
        assert!(!process.died());
        // Ready as soon as it says so; stopped while waiting when asked.
        assert!(process.wait_ready(&AtomicBool::new(false), || true).is_ok());
        let stop = AtomicBool::new(true);
        assert!(matches!(process.wait_ready(&stop, || false), Err(EngineError::Cancelled)));
        drop(process);
        assert!(!alive(pid), "the server outlived its owner");
    }
}

/// The guard in the built app ends its server when the app's end of the pipe closes, also when
/// the app is killed outright. Build the app first (`cargo build`).
#[cfg(all(test, unix))]
#[test]
#[ignore = "needs the built app: cargo test -- --ignored the_guard_ends_its_server"]
fn the_guard_ends_its_server_with_the_app() {
    let app = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/debug/ahem");
    let alive = |pid: &str| {
        Command::new("kill").args(["-0", pid]).status().is_ok_and(|status| status.success())
    };
    // The server once the guard has started it; a freshly built app can take a while to start.
    let server_of = |guard: u32| {
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            let out = Command::new("pgrep").args(["-P", &guard.to_string()]).output().unwrap();
            let pid = String::from_utf8(out.stdout).unwrap().trim().to_string();
            if !pid.is_empty() || Instant::now() > until {
                return pid;
            }
            thread::sleep(Duration::from_millis(50));
        }
    };
    let log = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-artifacts/guard.log");
    // Dropped: the guard ends the server.
    let mut command = Command::new("sleep");
    command.arg("60");
    let process =
        ServerProcess::spawn_under(Some(&app), command, Path::new("/bin/sleep"), &log).unwrap();
    let server = server_of(process.child.id());
    assert!(!server.is_empty() && alive(&server), "the server runs under the guard");
    drop(process);
    // A server that crashes under its guard is noticed by the next failed request.
    let mut command = Command::new("sleep");
    command.arg("60");
    let mut crashing =
        ServerProcess::spawn_under(Some(&app), command, Path::new("/bin/sleep"), &log).unwrap();
    let crashed = server_of(crashing.child.id());
    Command::new("kill").args(["-9", &crashed]).status().unwrap();
    assert!(crashing.died(), "the guard did not follow its server");
    // Ended by the system: the guard says so in the log and exits with a code of its own.
    let code = crashing.child.try_wait().unwrap().and_then(|status| status.code());
    assert_eq!(code, Some(SIGNALLED));
    assert!(fs::read_to_string(&log).unwrap().contains("the engine ended: signal"));
    thread::sleep(Duration::from_millis(300));
    assert!(!alive(&server), "the server outlived its guard");
    // The guard's input closed without a word (the app was killed): the same.
    let mut command = Command::new("sleep");
    command.arg("60");
    let mut process =
        ServerProcess::spawn_under(Some(&app), command, Path::new("/bin/sleep"), &log).unwrap();
    let server = server_of(process.child.id());
    drop(process.guard.take());
    thread::sleep(Duration::from_millis(500));
    assert!(!alive(&server), "the server outlived the app");
}
