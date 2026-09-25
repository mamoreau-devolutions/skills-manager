//! Subprocess execution with timeouts (stands in for `execFile`/`spawn`).

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub struct Output {
    pub status: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl Output {
    pub fn success(&self) -> bool {
        self.status == Some(0)
    }
    pub fn stdout_str(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }
    pub fn stderr_str(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }
}

#[derive(Debug)]
pub enum ProcError {
    /// The executable was not found (ENOENT).
    NotFound,
    /// The process exceeded its timeout and was killed.
    Timeout,
    /// Output exceeded the buffer limit and the process was killed.
    TooMuchOutput,
    Other(String),
}

impl std::fmt::Display for ProcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProcError::NotFound => write!(f, "command not found"),
            ProcError::Timeout => write!(f, "timed out"),
            ProcError::TooMuchOutput => write!(f, "output exceeded buffer"),
            ProcError::Other(s) => write!(f, "{}", s),
        }
    }
}

pub struct Run {
    cmd: Command,
    timeout: Option<Duration>,
    max_output: Option<usize>,
}

pub fn command(program: &str, args: &[&str]) -> Run {
    // Children share the parent's console, like Node's spawn/execFile default
    // (`windowsHide: false`). Use `hide_window` where TS passes `windowsHide`.
    let mut cmd = Command::new(program);
    cmd.args(args);
    Run {
        cmd,
        timeout: None,
        max_output: None,
    }
}

impl Run {
    pub fn env(mut self, k: &str, v: &str) -> Self {
        self.cmd.env(k, v);
        self
    }
    pub fn timeout(mut self, d: Duration) -> Self {
        self.timeout = Some(d);
        self
    }
    pub fn max_output(mut self, n: usize) -> Self {
        self.max_output = Some(n);
        self
    }
    /// Node's `windowsHide: true`: start the child without a console window.
    pub fn hide_window(mut self) -> Self {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            self.cmd.creation_flags(CREATE_NO_WINDOW);
        }
        self
    }

    /// Run with stdin closed and stdout/stderr captured.
    pub fn output(mut self) -> Result<Output, ProcError> {
        self.cmd
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = match self.cmd.spawn() {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(ProcError::NotFound),
            Err(e) => return Err(ProcError::Other(e.to_string())),
        };
        let mut out_pipe = child.stdout.take().unwrap();
        let mut err_pipe = child.stderr.take().unwrap();
        let out_t = std::thread::spawn(move || {
            let mut v = Vec::new();
            let _ = out_pipe.read_to_end(&mut v);
            v
        });
        let err_t = std::thread::spawn(move || {
            let mut v = Vec::new();
            let _ = err_pipe.read_to_end(&mut v);
            v
        });
        let start = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break s,
                Ok(None) => {}
                Err(e) => return Err(ProcError::Other(e.to_string())),
            }
            if let Some(t) = self.timeout {
                if start.elapsed() > t {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(ProcError::Timeout);
                }
            }
            std::thread::sleep(Duration::from_millis(15));
        };
        let stdout = out_t.join().unwrap_or_default();
        let stderr = err_t.join().unwrap_or_default();
        if let Some(max) = self.max_output {
            if stdout.len() + stderr.len() > max {
                return Err(ProcError::TooMuchOutput);
            }
        }
        Ok(Output {
            status: status.code(),
            stdout,
            stderr,
        })
    }

    /// Run with inherited stdio, returning the exit code.
    pub fn status_inherit(mut self) -> Result<Option<i32>, ProcError> {
        match self.cmd.status() {
            Ok(s) => Ok(s.code()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(ProcError::NotFound),
            Err(e) => Err(ProcError::Other(e.to_string())),
        }
    }

    /// Run with stdin inherited and stdout/stderr captured (spawnSync with
    /// `stdio: ['inherit', 'pipe', 'pipe']`).
    pub fn output_inherit_stdin(mut self) -> Result<Output, ProcError> {
        self.cmd
            .stdin(Stdio::inherit())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        match self.cmd.output() {
            Ok(o) => Ok(Output {
                status: o.status.code(),
                stdout: o.stdout,
                stderr: o.stderr,
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(ProcError::NotFound),
            Err(e) => Err(ProcError::Other(e.to_string())),
        }
    }
}
