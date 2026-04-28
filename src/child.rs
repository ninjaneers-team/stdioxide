//! Child process spawning and stream capture.

use std::fs;

use subprocess::{Exec, Job, Redirection};

/// A spawned child process with captured `stdin`, `stdout`, and `stderr` streams.
#[expect(
    clippy::redundant_pub_crate,
    reason = "Linting conflict with `rustc::unreachable_pub`."
)]
pub(crate) struct StartedChild {
    /// The subprocess `Job` handle for process lifecycle management.
    pub job: Job,
    /// File handle for writing to the child’s `stdin`.
    pub stdin: fs::File,
    /// File handle for reading from the child’s `stdout`.
    pub stdout: fs::File,
    /// File handle for reading from the child’s `stderr`.
    pub stderr: fs::File,
}

impl StartedChild {
    /// Spawns a child process with the given command and arguments.
    ///
    /// All three standard streams (`stdin`, `stdout`, `stderr`) are captured as pipes.
    pub(crate) fn start(command: &str, args: &[String]) -> Result<Self, anyhow::Error> {
        let mut process = Exec::cmd(command);
        for arg in args {
            process = process.arg(arg);
        }
        let mut job = process
            .stdin(Redirection::Pipe)
            .stdout(Redirection::Pipe)
            .stderr(Redirection::Pipe)
            .start()?;
        let child_stdin = job
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("Failed to capture child `stdin`"))?;
        let child_stdout = job
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("Failed to capture child `stdout`"))?;
        let child_stderr = job
            .stderr
            .take()
            .ok_or_else(|| anyhow::anyhow!("Failed to capture child `stderr`"))?;
        Ok(Self {
            job,
            stdin: child_stdin,
            stdout: child_stdout,
            stderr: child_stderr,
        })
    }
}
