//! Output buffering and stream serving logic.

use std::{
    io::{self, Read, Write},
    net::TcpStream,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
};

use tracing::{debug, info};

use crate::control::ControlMessage;

/// Defines how output serving should handle client disconnection.
#[derive(Debug, Clone)]
#[expect(
    clippy::redundant_pub_crate,
    reason = "Linting conflict with `rustc::unreachable_pub`."
)]
pub(crate) enum ServingBehavior {
    /// Kill the child process when the client disconnects (protocol port behavior).
    KillChildOnDisconnect,
    /// Keep child alive on disconnect, allow reconnection (`stderr` port behavior).
    ///
    /// The `Arc<AtomicBool>` tracks whether a connection is currently active.
    DoNotKillChildOnDisconnect(Arc<AtomicBool>),
}

/// Buffered output state from a child process stream.
#[expect(
    clippy::redundant_pub_crate,
    reason = "Linting conflict with `rustc::unreachable_pub`."
)]
pub(crate) struct OutputState {
    /// Accumulated output bytes not yet sent to clients.
    pub buffer: Vec<u8>,
    /// Whether EOF has been reached on the source stream.
    pub eof: bool,
}

/// Thread-safe output state with condition variable for synchronization.
#[expect(
    clippy::redundant_pub_crate,
    reason = "Linting conflict with `rustc::unreachable_pub`."
)]
pub(crate) struct NotifyableOutputState {
    /// Protected output buffer and EOF flag.
    pub state: Mutex<OutputState>,
    /// Condition variable to notify waiters when new output arrives.
    pub condition_variable: Condvar,
}

impl NotifyableOutputState {
    /// Creates a new empty output state.
    pub(crate) fn new() -> Self {
        Self::default()
    }
}

impl Default for NotifyableOutputState {
    fn default() -> Self {
        Self {
            state: Mutex::new(OutputState {
                buffer: Vec::new(),
                eof: false,
            }),
            condition_variable: Condvar::new(),
        }
    }
}

/// Pumps data from the given `source` (either `stdout` or `stderr` of the child process) into the shared `state`.
///
/// Continuously reads from the source and appends to the buffer, notifying waiters on each read.
#[expect(
    clippy::redundant_pub_crate,
    reason = "Linting conflict with `rustc::unreachable_pub`."
)]
pub(crate) fn pump_output_to_state(
    mut source: impl Read,
    output_state: &Arc<NotifyableOutputState>,
    label: &'static str,
) -> Result<(), anyhow::Error> {
    loop {
        let mut buffer = [0_u8; 8192];
        let num_bytes_read = source.read(&mut buffer)?;
        {
            let mut guard = output_state.state.lock().map_err(|error| {
                anyhow::anyhow!("Failed to lock output state for {label}: {error}")
            })?;

            if num_bytes_read == 0 {
                debug!("[{label}] EOF reached");
                guard.eof = true;
                output_state.condition_variable.notify_all();
                break;
            }

            let chunk = buffer.get(..num_bytes_read).unwrap_or_default();
            guard.buffer.extend_from_slice(chunk);
        }
        output_state.condition_variable.notify_all();
    }

    Ok(())
}

/// Serves output from the shared `state` to the given `stream`.
///
/// Waits for output to become available, then writes it to the TCP stream.
/// Handles disconnection according to the specified `serving_behavior`.
#[expect(
    clippy::redundant_pub_crate,
    reason = "Linting conflict with `rustc::unreachable_pub`."
)]
pub(crate) fn serve_output_on_stream(
    mut stream: TcpStream,
    output_state: &Arc<NotifyableOutputState>,
    control_tx: &mpsc::Sender<ControlMessage>,
    serving_behavior: &ServingBehavior,
    label: &'static str,
) -> Result<(), anyhow::Error> {
    loop {
        let buffered_data = {
            let mut guard = output_state.state.lock().map_err(|error| {
                anyhow::anyhow!("Failed to lock stdout state for {label}: {error}")
            })?;

            while guard.buffer.is_empty() && !guard.eof {
                // Wait until there’s either new output to send or we’ve reached EOF.
                guard = output_state
                    .condition_variable
                    .wait(guard)
                    .map_err(|error| {
                        anyhow::anyhow!("Failed to wait on condition variable for {label}: {error}")
                    })?;
            }

            if guard.buffer.is_empty() && guard.eof {
                debug!("[{label}] EOF reached and no buffered output; closing client connection");
                return Ok(());
            }

            // Clone the buffered data to avoid holding the lock while writing to the stream,
            // which could potentially block for a long time if the client is slow to read.
            guard.buffer.clone()
        };

        let mut num_bytes_written = 0;

        while num_bytes_written < buffered_data.len() {
            match stream.write(buffered_data.get(num_bytes_written..).unwrap_or_default()) {
                Ok(0) => {
                    // Treat as connection no longer writable.
                    break;
                }
                Ok(n) => {
                    num_bytes_written = num_bytes_written.saturating_add(n);
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                    // Interrupted by a signal, just retry.
                }
                Err(_) => {
                    // Any other error is treated as the connection being no longer writable.
                    break;
                }
            }
        }

        // Before draining the buffer, check if the connection is still active (for `stderr` reconnect support).
        // If the read monitoring thread detected a disconnect, we should NOT drain the buffer to prevent data loss.
        if let ServingBehavior::DoNotKillChildOnDisconnect(active) = serving_behavior
            && !active.load(Ordering::Acquire)
        {
            info!(
                "[{label}] Connection no longer active (detected by monitoring thread); exiting without draining buffer to prevent data loss"
            );
            return Ok(());
        }

        let mut guard = output_state
            .state
            .lock()
            .map_err(|error| anyhow::anyhow!("Failed to lock stdout state for {label}: {error}"))?;

        // Since we copied the buffer, there may have been new output produced while we were writing to the stream. We
        // only remove the number of bytes that we successfully wrote, so that any new output will still be in the buffer
        // for the next iteration.
        guard.buffer.drain(..num_bytes_written);

        if num_bytes_written < buffered_data.len() {
            // Something went wrong while writing to the stream, and we weren’t able to write all the buffered data.
            // We treat this as the connection being no longer writable and exit the loop (and potentially kill the
            // child process, depending on the serving behavior).
            if matches!(serving_behavior, ServingBehavior::KillChildOnDisconnect) {
                let _result = control_tx.send(ControlMessage::KillChild);
            }
            return Ok(());
        }

        if guard.eof && guard.buffer.is_empty() {
            debug!("[{label}] EOF reached; closing client connection");
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_pump_output_to_state_empty_input() -> Result<(), anyhow::Error> {
        let state = Arc::new(NotifyableOutputState::new());
        let input = Cursor::new(Vec::<u8>::new());

        pump_output_to_state(input, &state, "test")?;

        let guard = state
            .state
            .lock()
            .map_err(|error| anyhow::anyhow!("Failed to lock output state for test: {error}"))?;
        assert!(guard.buffer.is_empty());
        assert!(guard.eof);
        drop(guard); // Only needed to satisfy Clippy ¯\_(ツ)_/¯
        Ok(())
    }

    #[test]
    fn test_pump_output_to_state_single_chunk() -> Result<(), anyhow::Error> {
        let state = Arc::new(NotifyableOutputState::new());
        let data = b"Hello, World!";
        let input = Cursor::new(data.to_vec());

        pump_output_to_state(input, &state, "test")?;

        let guard = state
            .state
            .lock()
            .map_err(|error| anyhow::anyhow!("Failed to lock output state for test: {error}"))?;
        assert_eq!(guard.buffer, data);
        assert!(guard.eof);
        drop(guard); // Only needed to satisfy Clippy ¯\_(ツ)_/¯
        Ok(())
    }

    #[test]
    fn test_pump_output_to_state_multiple_chunks() -> Result<(), anyhow::Error> {
        let state = Arc::new(NotifyableOutputState::new());
        let data = vec![0_u8; 16384]; // Larger than buffer size (8192).
        let input = Cursor::new(data.clone());

        pump_output_to_state(input, &state, "test")?;

        let guard = state
            .state
            .lock()
            .map_err(|error| anyhow::anyhow!("Failed to lock output state for test: {error}"))?;
        assert_eq!(guard.buffer, data);
        assert!(guard.eof);
        drop(guard); // Only needed to satisfy Clippy ¯\_(ツ)_/¯
        Ok(())
    }
}
