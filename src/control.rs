use std::sync::mpsc;

use subprocess::Job;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlMessage {
    KillChild,
}

pub fn run_child_coordinator(
    job: Job,
    control_rx: mpsc::Receiver<ControlMessage>,
) -> Result<(), anyhow::Error> {
    loop {
        if let Some(status) = job.poll() {
            eprintln!("Child process exited with status: {status}");
            return Ok(());
        }

        match control_rx.recv_timeout(std::time::Duration::from_millis(100)) {
            Ok(ControlMessage::KillChild) => {
                let _ = job.kill();
                let status = job.wait()?;
                eprintln!("Child process killed; exit status: {status}");
                return Ok(());
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // Just poll again.
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                // All senders are gone; we terminate the child process and exit.
                eprintln!("Control channel disconnected; terminating child process");
                let _ = job.kill();
                let status = job.wait()?;
                eprintln!("Child process killed; exit status: {status}");
                return Ok(());
            }
        }
    }
}
