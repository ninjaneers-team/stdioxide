//! Child process lifecycle coordination and control messages.

use std::{sync::mpsc, time::Duration};
use subprocess::Job;
use tracing::info;

/// Messages sent to the child process coordinator to control lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[expect(
    clippy::redundant_pub_crate,
    reason = "Linting conflict with `rustc::unreachable_pub`."
)]
pub(crate) enum ControlMessage {
    /// Terminate the child process immediately.
    KillChild,
}

/// Monitors the child process and handles termination requests.
///
/// Polls the child process for exit and listens for control messages to kill it.
/// Returns when the child process exits or is explicitly terminated.
#[expect(
    clippy::redundant_pub_crate,
    reason = "Linting conflict with `rustc::unreachable_pub`."
)]
pub(crate) fn run_child_coordinator(
    job: &Job,
    control_rx: &mpsc::Receiver<ControlMessage>,
) -> Result<(), anyhow::Error> {
    loop {
        if let Some(status) = job.poll() {
            info!("Child process exited with status: {status}");
            return Ok(());
        }

        match control_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(ControlMessage::KillChild) => {
                drop(job.kill());
                let status = job.wait()?;
                info!("Child process killed; exit status: {status}");
                return Ok(());
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // Just poll again.
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                // All senders are gone; we terminate the child process and exit.
                info!("Control channel disconnected; terminating child process");
                drop(job.kill());
                let status = job.wait()?;
                info!("Child process killed; exit status: {status}");
                return Ok(());
            }
        }
    }
}
