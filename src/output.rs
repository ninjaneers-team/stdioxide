use std::{
    io::{Read, Write},
    net::TcpStream,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
};

use crate::control::ControlMessage;

#[derive(Debug, Clone)]
pub enum ServingBehavior {
    KillChildOnDisconnect,
    DoNotKillChildOnDisconnect(Arc<AtomicBool>),
}

pub struct OutputState {
    pub buffer: Vec<u8>,
    pub eof: bool,
}

pub struct NotifyableOutputState {
    pub state: Mutex<OutputState>,
    pub condition_variable: Condvar,
}

impl NotifyableOutputState {
    pub fn new() -> Self {
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
pub fn pump_output_to_state(
    mut source: impl Read,
    output_state: Arc<NotifyableOutputState>,
    label: &'static str,
) -> Result<(), anyhow::Error> {
    loop {
        let mut buffer = [0u8; 8192];
        let num_bytes_read = source.read(&mut buffer)?;
        let mut guard = output_state
            .state
            .lock()
            .expect("Failed to lock output state");

        if num_bytes_read == 0 {
            eprintln!("[{label}] EOF reached");
            guard.eof = true;
            output_state.condition_variable.notify_all();
            break;
        }

        let chunk = &buffer[..num_bytes_read];
        guard.buffer.extend_from_slice(chunk);
        output_state.condition_variable.notify_all();
    }

    Ok(())
}

pub fn serve_output_on_stream(
    mut stream: TcpStream,
    output_state: Arc<NotifyableOutputState>,
    control_tx: mpsc::Sender<ControlMessage>,
    serving_behavior: ServingBehavior,
    label: &'static str,
) -> Result<(), anyhow::Error> {
    loop {
        let buffered_data = {
            let mut guard = output_state
                .state
                .lock()
                .expect("Failed to lock stdout state");

            while guard.buffer.is_empty() && !guard.eof {
                // Wait until there’s either new output to send or we’ve reached EOF.
                guard = output_state
                    .condition_variable
                    .wait(guard)
                    .expect("Failed to wait on condition variable");
            }

            if guard.buffer.is_empty() && guard.eof {
                eprintln!(
                    "[{label}] EOF reached and no buffered output; closing client connection"
                );
                return Ok(());
            }

            // Clone the buffered data to avoid holding the lock while writing to the stream,
            // which could potentially block for a long time if the client is slow to read.
            guard.buffer.clone()
        };

        let mut num_bytes_written = 0;

        while num_bytes_written < buffered_data.len() {
            match stream.write(&buffered_data[num_bytes_written..]) {
                Ok(0) => {
                    // Treat as connection no longer writable.
                    break;
                }
                Ok(n) => {
                    num_bytes_written += n;
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                    // Interrupted by a signal, just retry.
                    continue;
                }
                Err(_) => {
                    // Any other error is treated as the connection being no longer writable.
                    break;
                }
            }
        }

        // Before draining the buffer, check if the connection is still active (for `stderr` reconnect support).
        // If the read monitoring thread detected a disconnect, we should NOT drain the buffer to prevent data loss.
        if let ServingBehavior::DoNotKillChildOnDisconnect(ref active) = serving_behavior
            && !active.load(Ordering::Acquire)
        {
            eprintln!(
                "[{label}] Connection no longer active (detected by monitoring thread); exiting without draining buffer to prevent data loss"
            );
            return Ok(());
        }

        let mut guard = output_state
            .state
            .lock()
            .expect("Failed to lock stdout state");

        // Since we copied the buffer, there may have been new output produced while we were writing to the stream. We
        // only remove the number of bytes that we successfully wrote, so that any new output will still be in the buffer
        // for the next iteration.
        guard.buffer.drain(..num_bytes_written);

        if num_bytes_written < buffered_data.len() {
            // Something went wrong while writing to the stream, and we weren’t able to write all the buffered data.
            // We treat this as the connection being no longer writable and exit the loop (and potentially kill the
            // child process, depending on the serving behavior).
            if matches!(serving_behavior, ServingBehavior::KillChildOnDisconnect) {
                let _ = control_tx.send(ControlMessage::KillChild);
            }
            return Ok(());
        }

        if guard.eof && guard.buffer.is_empty() {
            eprintln!("[{label}] EOF reached; closing client connection");
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_pump_output_to_state_empty_input() {
        let state = Arc::new(NotifyableOutputState::new());
        let input = Cursor::new(Vec::<u8>::new());

        let result = pump_output_to_state(input, Arc::clone(&state), "test");
        assert!(result.is_ok());

        let guard = state.state.lock().unwrap();
        assert!(guard.buffer.is_empty());
        assert!(guard.eof);
    }

    #[test]
    fn test_pump_output_to_state_single_chunk() {
        let state = Arc::new(NotifyableOutputState::new());
        let data = b"Hello, World!";
        let input = Cursor::new(data.to_vec());

        let result = pump_output_to_state(input, Arc::clone(&state), "test");
        assert!(result.is_ok());

        let guard = state.state.lock().unwrap();
        assert_eq!(guard.buffer, data);
        assert!(guard.eof);
    }

    #[test]
    fn test_pump_output_to_state_multiple_chunks() {
        let state = Arc::new(NotifyableOutputState::new());
        let data = vec![0u8; 16384]; // Larger than buffer size (8192).
        let input = Cursor::new(data.clone());

        let result = pump_output_to_state(input, Arc::clone(&state), "test");
        assert!(result.is_ok());

        let guard = state.state.lock().unwrap();
        assert_eq!(guard.buffer, data);
        assert!(guard.eof);
    }
}
