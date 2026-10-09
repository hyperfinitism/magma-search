// SPDX-License-Identifier: Apache-2.0

use anyhow::{Context, Result, anyhow, bail, ensure};
use rustsat::{
    instances::Cnf,
    solvers::SolverResult,
    types::{Assignment, TernaryVal, Var},
};
use std::{
    ffi::OsString,
    fs,
    io::{self, BufWriter, Read, Seek, Write},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::process::CommandExt,
    },
    path::Path,
    process::{Child, Command, Output, Stdio},
    thread,
};

pub const MAX_THREADS: u32 = 128;

const DEFAULT_IMAGE: &str = "magma-mallob:4a3b8da";
const RUN_SCRIPT: &str = include_str!("../../mallob/run-mallob.sh");

struct ContainerConfig {
    engine: OsString,
    image: OsString,
}

impl ContainerConfig {
    fn from_env() -> Self {
        Self {
            engine: std::env::var_os("MAGMA_CONTAINER_ENGINE").unwrap_or_else(|| "docker".into()),
            image: std::env::var_os("MAGMA_MALLOB_IMAGE").unwrap_or_else(|| DEFAULT_IMAGE.into()),
        }
    }

    fn installation_context(&self) -> String {
        format!(
            "starting Mallob container; ensure {} is running with Linux containers and build the image with: {} build -t {} -f mallob/Dockerfile mallob",
            self.engine.to_string_lossy(),
            self.engine.to_string_lossy(),
            self.image.to_string_lossy(),
        )
    }
}

enum ExecutionConfig {
    Container(ContainerConfig),
    Native,
}

impl ExecutionConfig {
    fn from_env() -> Result<Self> {
        match std::env::var("MAGMA_MALLOB_MODE").as_deref() {
            Ok("container") | Err(std::env::VarError::NotPresent) => {
                Ok(Self::Container(ContainerConfig::from_env()))
            }
            Ok("native") => Ok(Self::Native),
            Ok(mode) => bail!("invalid MAGMA_MALLOB_MODE={mode:?}; expected native or container"),
            Err(error) => Err(anyhow!("reading MAGMA_MALLOB_MODE: {error}")),
        }
    }

    fn installation_context(&self) -> String {
        match self {
            Self::Container(config) => config.installation_context(),
            Self::Native => {
                "starting native Mallob; rebuild the devcontainer to install /mallob".to_owned()
            }
        }
    }
}

pub fn signature() -> Result<String> {
    let config = match ExecutionConfig::from_env()? {
        ExecutionConfig::Container(config) => config,
        ExecutionConfig::Native => {
            let context = ExecutionConfig::Native.installation_context();
            ensure!(Path::new("/mallob/build/mallob").is_file(), "{context}");
            let revision = fs::read_to_string("/mallob/REVISION").context(context)?;
            let revision = revision.trim();
            ensure!(!revision.is_empty(), "Mallob revision is empty");
            return Ok(format!("Mallob {revision}"));
        }
    };
    let output = Command::new(&config.engine)
        .args(["image", "inspect", "--format", "{{.Id}}"])
        .arg(&config.image)
        .output()
        .with_context(|| config.installation_context())?;
    ensure!(
        output.status.success(),
        "{}; inspecting image {} failed: {}",
        config.installation_context(),
        config.image.to_string_lossy(),
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let image_id = std::str::from_utf8(&output.stdout)
        .context("Mallob image ID is not UTF-8")?
        .trim();
    ensure!(!image_id.is_empty(), "Mallob image has an empty ID");
    Ok(format!("Mallob container {image_id}"))
}

pub struct Outcome<T> {
    pub status: SolverResult,
    pub model: Option<T>,
}

struct NativeWork {
    directory: tempfile::TempDir,
    run_id: String,
    _lock: fs::File,
}

impl NativeWork {
    fn new() -> Result<Self> {
        // Mallob's global shared-memory names require serializing native runs.
        let lock = fs::File::open("/mallob/REVISION").context("opening native Mallob lock")?;
        loop {
            if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } == 0 {
                break;
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error).context("locking native Mallob");
            }
        }
        let directory = tempfile::Builder::new().prefix("magma-mallob-").tempdir()?;
        let run_id = directory
            .path()
            .file_name()
            .context("creating native Mallob run ID")?
            .to_string_lossy()
            .into_owned();
        Ok(Self {
            directory,
            run_id,
            _lock: lock,
        })
    }

    fn path(&self) -> &Path {
        self.directory.path()
    }
}

impl Drop for NativeWork {
    fn drop(&mut self) {
        terminate_native_workers(&self.run_id);
        if let Ok(entries) = fs::read_dir("/dev/shm") {
            for entry in entries.flatten() {
                if entry
                    .file_name()
                    .as_encoded_bytes()
                    .starts_with(b"edu.kit.iti.mallob.")
                {
                    let _ = fs::remove_file(entry.path());
                }
            }
        }
    }
}

fn terminate_native_workers(run_id: &str) {
    let marker = format!("MAGMA_MALLOB_RUN_ID={run_id}");
    if let Ok(entries) = fs::read_dir("/proc") {
        for entry in entries.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<libc::pid_t>().ok())
                .filter(|&pid| pid > 0 && pid != std::process::id() as libc::pid_t)
            else {
                continue;
            };
            let descriptor = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
            let pidfd =
                (descriptor >= 0).then(|| unsafe { OwnedFd::from_raw_fd(descriptor as i32) });
            if let Err(error) = terminate_native_worker(&entry.path(), &marker, pidfd.as_ref()) {
                eprintln!("terminating Mallob worker {pid}: {error}");
            }
        }
    }
}

fn terminate_native_worker(path: &Path, marker: &str, pidfd: Option<&OwnedFd>) -> io::Result<()> {
    let Ok(process) = fs::File::open(path) else {
        return Ok(());
    };
    let process_path = format!("/proc/self/fd/{}", process.as_raw_fd());
    if !fs::read(format!("{process_path}/environ")).is_ok_and(|environment| {
        environment
            .split(|&byte| byte == 0)
            .any(|variable| variable == marker.as_bytes())
    }) {
        return Ok(());
    }
    // A proc directory FD also targets the original process without PID reuse races.
    let signal_fd = pidfd.map_or(process.as_raw_fd(), AsRawFd::as_raw_fd);
    if unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            signal_fd,
            libc::SIGKILL,
            std::ptr::null::<libc::siginfo_t>(),
            0,
        )
    } != 0
    {
        let error = io::Error::last_os_error();
        return if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(error)
        };
    }
    if let Some(pidfd) = pidfd {
        let mut ready = libc::pollfd {
            fd: pidfd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        unsafe { libc::poll(&mut ready, 1, 1000) };
    } else {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while std::time::Instant::now() < deadline {
            let Ok(stat) = fs::read_to_string(format!("{process_path}/stat")) else {
                break;
            };
            if stat
                .rsplit_once(')')
                .and_then(|(_, fields)| fields.split_whitespace().next())
                .is_some_and(|state| matches!(state, "Z" | "X"))
            {
                break;
            }
            thread::sleep(std::time::Duration::from_millis(20));
        }
    }
    Ok(())
}

struct RunningSolver {
    child: Child,
    container: Option<(OsString, String)>,
    native_run_id: Option<String>,
    reaped: bool,
    completed: bool,
}

impl RunningSolver {
    fn terminate(&mut self) {
        if self.completed {
            return;
        }
        if let Some((engine, name)) = &self.container {
            let _ = Command::new(engine)
                .args(["rm", "--force", name])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        if !self.reaped && self.container.is_none() {
            unsafe { libc::kill(-(self.child.id() as i32), libc::SIGTERM) };
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while std::time::Instant::now() < deadline {
                match self.child.try_wait() {
                    Ok(Some(_)) => {
                        self.reaped = true;
                        break;
                    }
                    Ok(None) => thread::sleep(std::time::Duration::from_millis(20)),
                    Err(_) => break,
                }
            }
        }
        if !self.reaped {
            unsafe { libc::kill(-(self.child.id() as i32), libc::SIGKILL) };
            let _ = self.child.kill();
            let _ = self.child.wait();
            self.reaped = true;
        }
        if let Some(run_id) = &self.native_run_id {
            terminate_native_workers(run_id);
        }
        self.completed = true;
    }
}

impl Drop for RunningSolver {
    fn drop(&mut self) {
        self.terminate();
    }
}

fn read_all(mut reader: impl Read) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn container_command(config: &ContainerConfig, name: &str, threads: usize) -> Command {
    let mut launcher = Command::new(&config.engine);
    launcher
        .args([
            "run",
            "--rm",
            "--init",
            "--pull=never",
            "--sig-proxy=true",
            "--network",
            "none",
            "--ipc",
            "private",
            "--shm-size=32g",
            "-i",
            "--name",
        ])
        .arg(name)
        .args(["-w", "/mallob", "--entrypoint", "/bin/bash"])
        .arg(&config.image)
        .args(["-c", RUN_SCRIPT, "--"])
        .arg(threads.to_string());
    launcher
}

fn run_solver(cnf: &Cnf, n_vars: u32, threads: usize) -> Result<Output> {
    let input = tempfile::Builder::new()
        .prefix("magma-mallob-")
        .tempfile()?;
    let container_name = input
        .path()
        .file_name()
        .context("creating Mallob container name")?
        .to_string_lossy()
        .into_owned();
    let (mut input, input_path) = input.into_parts();
    input_path.close().context("removing Mallob input path")?;
    {
        let mut writer = BufWriter::new(&mut input);
        cnf.write_dimacs(&mut writer, n_vars)?;
        writer.flush()?;
    }
    input.rewind()?;
    let config = ExecutionConfig::from_env()?;
    let installation_context = config.installation_context();
    let native_work = match &config {
        ExecutionConfig::Native => Some(NativeWork::new()?),
        ExecutionConfig::Container(_) => None,
    };
    let mut launcher = match &config {
        ExecutionConfig::Container(config) => container_command(config, &container_name, threads),
        ExecutionConfig::Native => {
            let mut launcher = Command::new("/bin/bash");
            launcher
                .args(["-c", RUN_SCRIPT, "--"])
                .arg(threads.to_string());
            launcher
        }
    };
    if let Some(work) = &native_work {
        launcher
            .env("MAGMA_MALLOB_RUN_ID", &work.run_id)
            .env("MAGMA_MALLOB_WORK_DIR", work.path());
    }
    let stdio_file = input.try_clone().context("cloning Mallob input handle")?;
    launcher
        .stdin(Stdio::from(stdio_file))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    launcher.process_group(0);
    {
        let parent_pid = std::process::id() as libc::pid_t;
        let native_lock_fd = native_work.as_ref().map(|work| work._lock.as_raw_fd());
        unsafe {
            launcher.pre_exec(move || {
                if let Some(fd) = native_lock_fd {
                    // Inherit the lock so parent death cannot release it before Mallob exits.
                    let flags = libc::fcntl(fd, libc::F_GETFD);
                    if flags == -1
                        || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1
                    {
                        return Err(io::Error::last_os_error());
                    }
                }
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::getppid() != parent_pid {
                    return Err(io::Error::from_raw_os_error(libc::ESRCH));
                }
                Ok(())
            });
        }
    }
    let mut solver = RunningSolver {
        child: launcher
            .spawn()
            .with_context(|| installation_context.clone())?,
        container: match config {
            ExecutionConfig::Container(config) => Some((config.engine, container_name)),
            ExecutionConfig::Native => None,
        },
        native_run_id: native_work.as_ref().map(|work| work.run_id.clone()),
        reaped: false,
        completed: false,
    };
    let stdout = solver
        .child
        .stdout
        .take()
        .context("opening Mallob stdout")?;
    let stderr = solver
        .child
        .stderr
        .take()
        .context("opening Mallob stderr")?;

    thread::scope(|scope| {
        let reader_out = thread::Builder::new()
            .name("mallob-stdout".into())
            .spawn_scoped(scope, move || read_all(stdout))
            .inspect_err(|_| {
                solver.terminate();
            })
            .context("starting Mallob stdout reader")?;
        let reader_err = thread::Builder::new()
            .name("mallob-stderr".into())
            .spawn_scoped(scope, move || read_all(stderr))
            .inspect_err(|_| {
                solver.terminate();
            })
            .context("starting Mallob stderr reader")?;
        let status = solver
            .child
            .wait()
            .inspect_err(|_| {
                solver.terminate();
            })
            .context("waiting for Mallob")?;
        solver.reaped = true;
        if !status.success() && solver.native_run_id.is_some() {
            // MPI workers can keep output pipes open after the launcher exits.
            solver.terminate();
        }
        let stdout = reader_out
            .join()
            .map_err(|_| anyhow!("Mallob stdout reader panicked"))?
            .context("reading Mallob stdout")?;
        let stderr = reader_err
            .join()
            .map_err(|_| anyhow!("Mallob stderr reader panicked"))?
            .context("reading Mallob stderr")?;
        solver.completed = status.success();
        ensure!(
            status.success(),
            "{installation_context}; Mallob exited unexpectedly ({}); stderr: {}",
            status,
            String::from_utf8_lossy(&stderr).trim(),
        );
        Ok(Output {
            status,
            stdout,
            stderr,
        })
    })
}

fn parse_output(
    cnf: &Cnf,
    n_vars: u32,
    code: Option<i32>,
    stdout: &[u8],
) -> Result<Outcome<Assignment>> {
    ensure!(code == Some(0), "Mallob exited unexpectedly ({code:?})");
    let stdout = std::str::from_utf8(stdout).context("Mallob output is not UTF-8")?;
    let mut status = None;
    let mut assignment = Assignment::default();
    let mut model_started = false;
    let mut model_ended = false;
    for line in stdout.lines() {
        let mut words = line.split_whitespace();
        match words.next() {
            Some("s") => {
                ensure!(status.is_none(), "Mallob returned multiple results");
                status = Some(match words.next() {
                    Some("SATISFIABLE") => SolverResult::Sat,
                    Some("UNSATISFIABLE") => SolverResult::Unsat,
                    Some("UNKNOWN") => SolverResult::Interrupted,
                    _ => bail!("invalid Mallob result: {line}"),
                });
                ensure!(words.next().is_none(), "invalid Mallob result: {line}");
            }
            Some("v") => {
                ensure!(!model_ended, "Mallob model continues after its terminator");
                model_started = true;
                for word in words {
                    ensure!(!model_ended, "Mallob model continues after its terminator");
                    let value: i64 = word.parse().context("invalid Mallob model literal")?;
                    if value == 0 {
                        model_ended = true;
                        continue;
                    }
                    let variable = value.unsigned_abs();
                    ensure!(
                        variable <= u64::from(n_vars),
                        "Mallob model variable {variable} exceeds {n_vars}"
                    );
                    let variable = Var::new((variable - 1) as u32);
                    let value = if value > 0 {
                        TernaryVal::True
                    } else {
                        TernaryVal::False
                    };
                    ensure!(
                        matches!(assignment.var_value(variable), TernaryVal::DontCare)
                            || assignment.var_value(variable) == value,
                        "Mallob returned conflicting assignments for variable {}",
                        variable.idx32() + 1
                    );
                    assignment.assign_var(variable, value);
                }
            }
            _ => (),
        }
    }
    let status = status.context("Mallob returned no SAT result")?;
    if status != SolverResult::Sat {
        ensure!(
            !model_started,
            "Mallob returned a model without a SAT result"
        );
        return Ok(Outcome {
            status,
            model: None,
        });
    }
    ensure!(model_started, "Mallob SAT result has no model");
    ensure!(model_ended, "Mallob model has no terminating zero");
    for variable in 0..n_vars {
        ensure!(
            assignment.var_value(Var::new(variable)) != TernaryVal::DontCare,
            "Mallob model does not assign variable {}",
            variable + 1
        );
    }
    ensure!(
        cnf.evaluate(&assignment) == TernaryVal::True,
        "Mallob model does not satisfy the CNF"
    );
    Ok(Outcome {
        status,
        model: Some(assignment),
    })
}

pub fn solve_cnf<T, F>(cnf: &Cnf, threads: usize, decode: F) -> Result<Outcome<T>>
where
    F: FnOnce(&Assignment) -> Result<T>,
{
    ensure!(
        (1..=MAX_THREADS as usize).contains(&threads),
        "--threads must be between 1 and {MAX_THREADS}"
    );
    // Mallob preprocessing requires a nonempty formula with variables.
    if cnf.iter().any(|clause| clause.is_empty()) {
        return Ok(Outcome {
            status: SolverResult::Unsat,
            model: None,
        });
    }
    if cnf.is_empty() {
        return Ok(Outcome {
            status: SolverResult::Sat,
            model: Some(decode(&Assignment::default())?),
        });
    }
    let n_vars = cnf
        .iter()
        .flat_map(|clause| clause.iter())
        .map(|literal| literal.var().idx32() + 1)
        .max()
        .unwrap_or(0);
    let output = run_solver(cnf, n_vars, threads)?;
    let outcome =
        parse_output(cnf, n_vars, output.status.code(), &output.stdout).with_context(|| {
            format!(
                "reading Mallob result; stderr: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )
        })?;
    Ok(Outcome {
        status: outcome.status,
        model: outcome.model.as_ref().map(decode).transpose()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustsat::{lit, types::Clause};

    #[test]
    fn procfd_fallback_terminates_only_the_matching_run() -> Result<()> {
        use std::os::unix::process::ExitStatusExt;

        let namespace = tempfile::Builder::new()
            .prefix("magma-procfd-test-")
            .tempdir()?;
        let run_id = namespace.path().file_name().unwrap().to_string_lossy();
        let marker = format!("MAGMA_MALLOB_RUN_ID={run_id}");
        let sleeper = |id: &str| -> Result<RunningSolver> {
            Ok(RunningSolver {
                child: Command::new("sleep")
                    .arg("60")
                    .env("MAGMA_MALLOB_RUN_ID", id)
                    .process_group(0)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()?,
                container: None,
                native_run_id: None,
                reaped: false,
                completed: false,
            })
        };
        let mut matching = sleeper(&run_id)?;
        let mut unrelated = sleeper(&format!("{run_id}-other"))?;
        let unrelated_path = format!("/proc/{}", unrelated.child.id());
        terminate_native_worker(Path::new(&unrelated_path), &marker, None)?;
        ensure!(
            unrelated.child.try_wait()?.is_none(),
            "unrelated run was terminated"
        );

        let matching_path = format!("/proc/{}", matching.child.id());
        terminate_native_worker(Path::new(&matching_path), &marker, None)?;
        let status = matching
            .child
            .try_wait()?
            .context("procfd fallback returned before the matching child exited")?;
        matching.reaped = true;
        ensure!(
            status.signal() == Some(libc::SIGKILL),
            "matching run was not killed"
        );
        ensure!(
            unrelated.child.try_wait()?.is_none(),
            "unrelated run was terminated"
        );
        Ok(())
    }

    #[test]
    fn parses_multiline_model_and_checks_cnf() -> Result<()> {
        let mut cnf = Cnf::new();
        cnf.add_unit(lit![0]);
        cnf.add_unit(!lit![1]);
        let result = parse_output(&cnf, 2, Some(0), b"c comment\ns SATISFIABLE\nv 1\nv -2 0\n")?;
        assert_eq!(result.status, SolverResult::Sat);
        assert!(result.model.is_some());
        assert!(parse_output(&cnf, 2, Some(0), b"s SATISFIABLE\nv -1 -2 0\n").is_err());
        Ok(())
    }

    #[test]
    fn rejects_invalid_models_and_results() {
        let mut cnf = Cnf::new();
        cnf.add_binary(lit![0], lit![1]);
        for output in [
            "s SATISFIABLE\n",
            "s SATISFIABLE\nv 1 2\n",
            "s SATISFIABLE\nv 1 0\n",
            "s SATISFIABLE\nv 1 -1 2 0\n",
            "s SATISFIABLE\nv 1 3 0\n",
            "s SATISFIABLE\nv 1 2 0 3\n",
            "s SATISFIABLE\nv 1 2 0\nv 1\n",
            "s SATISFIABLE\nv invalid 0\n",
            "s SATISFIABLE\ns UNSATISFIABLE\n",
            "",
        ] {
            assert!(parse_output(&cnf, 2, Some(0), output.as_bytes()).is_err());
        }
        assert!(parse_output(&cnf, 2, Some(1), b"s UNKNOWN\n").is_err());
        assert!(parse_output(&cnf, 2, None, b"").is_err());
        assert!(parse_output(&cnf, 2, Some(10), b"s SATISFIABLE\nv 1 2 0\n").is_err());
        assert!(parse_output(&cnf, 2, Some(20), b"s UNSATISFIABLE\n").is_err());
        assert!(parse_output(&cnf, 2, Some(0), b"s UNSATISFIABLE\nv 1 2 0\n").is_err());
    }

    #[test]
    fn parses_unsat_and_unknown_without_models() -> Result<()> {
        let cnf = Cnf::new();
        for (code, output, status) in [
            (0, "s UNSATISFIABLE\n", SolverResult::Unsat),
            (0, "s UNKNOWN\n", SolverResult::Interrupted),
        ] {
            let result = parse_output(&cnf, 0, Some(code), output.as_bytes())?;
            assert_eq!(result.status, status);
            assert!(result.model.is_none());
        }
        Ok(())
    }

    #[test]
    fn mallob_solves_sat_and_unsat_with_one_or_two_threads() -> Result<()> {
        for threads in [1, 2] {
            let mut cnf = Cnf::new();
            cnf.add_unit(lit![0]);
            cnf.add_unit(!lit![1]);
            cnf.add_unit(lit![7]);
            let result = solve_cnf(&cnf, threads, |assignment| {
                assert_eq!(assignment.lit_value(lit![0]), TernaryVal::True);
                assert_eq!(assignment.lit_value(lit![1]), TernaryVal::False);
                assert_eq!(assignment.lit_value(lit![7]), TernaryVal::True);
                Ok(())
            })?;
            assert_eq!(result.status, SolverResult::Sat);
            assert!(result.model.is_some());
            cnf.add_unit(!lit![0]);
            let result = solve_cnf::<(), _>(&cnf, threads, |_| bail!("UNSAT must not be decoded"))?;
            assert_eq!(result.status, SolverResult::Unsat);
            assert!(result.model.is_none());
        }
        Ok(())
    }

    #[test]
    fn mallob_handles_empty_cnf_and_empty_clause() -> Result<()> {
        let mut cnf = Cnf::new();
        let result = solve_cnf(&cnf, 2, |assignment| {
            ensure!(
                assignment.is_empty(),
                "empty CNF should have an empty model"
            );
            Ok(())
        })?;
        assert_eq!(result.status, SolverResult::Sat);
        cnf.add_clause(Clause::default());
        let result = solve_cnf::<(), _>(&cnf, 2, |_| bail!("UNSAT must not be decoded"))?;
        assert_eq!(result.status, SolverResult::Unsat);
        Ok(())
    }

    #[test]
    fn mallob_solves_independent_cnfs_concurrently() -> Result<()> {
        thread::scope(|scope| {
            let mut workers = Vec::new();
            for (threads, positive) in [(1, true), (1, false), (2, true), (2, false)] {
                workers.push(scope.spawn(move || -> Result<()> {
                    for _ in 0..2 {
                        let mut cnf = Cnf::new();
                        let literal = if positive { lit![0] } else { !lit![0] };
                        cnf.add_unit(literal);
                        let result = solve_cnf(&cnf, threads, |assignment| {
                            ensure!(assignment.lit_value(literal) == TernaryVal::True);
                            Ok(())
                        })?;
                        ensure!(result.status == SolverResult::Sat);
                    }
                    Ok(())
                }));
            }
            for worker in workers {
                worker
                    .join()
                    .map_err(|_| anyhow!("concurrent SAT test panicked"))??;
            }
            Ok(())
        })
    }
}
