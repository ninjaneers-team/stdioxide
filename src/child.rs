use subprocess::{Exec, Job, Redirection};

pub struct StartedChild {
    pub job: Job,
    pub stdin: std::fs::File,
    pub stdout: std::fs::File,
    pub stderr: std::fs::File,
}

impl StartedChild {
    pub fn start(command: &str, args: &[String]) -> Result<Self, anyhow::Error> {
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
            .ok_or_else(|| anyhow::anyhow!("Failed to capture child stdin"))?;
        let child_stdout = job
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("Failed to capture child stdout"))?;
        let child_stderr = job
            .stderr
            .take()
            .ok_or_else(|| anyhow::anyhow!("Failed to capture child stderr"))?;
        Ok(Self {
            job,
            stdin: child_stdin,
            stdout: child_stdout,
            stderr: child_stderr,
        })
    }
}
