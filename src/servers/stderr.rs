use std::{
    io::Read,
    net::{TcpListener, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
};

use crate::{
    control::ControlMessage,
    output::{NotifyableOutputState, ServingBehavior, serve_output_on_stream},
};

/// Monitors a client connection for disconnection by attempting to read from the socket.
/// Since `stderr` clients should not send data, any readability indicates disconnection (EOF).
/// When disconnection is detected, the atomic flag is cleared to allow new connections.
fn monitor_stderr_client_connection(
    mut stream: TcpStream,
    has_active_connection: Arc<AtomicBool>,
) -> Result<(), anyhow::Error> {
    let mut read_buffer = [0u8; 1];
    loop {
        match stream.read(&mut read_buffer) {
            Ok(0) => {
                // EOF; client disconnected gracefully.
                eprintln!("[stderr] client disconnect detected");
                has_active_connection.store(false, Ordering::Release);
                return Ok(());
            }
            Ok(_) => {
                // Ignore any data sent by client (unexpected but harmless).
            }
            Err(e) => {
                // Error reading; treat as disconnection.
                eprintln!("[stderr] read error (client likely disconnected): {e}");
                has_active_connection.store(false, Ordering::Release);
                return Err(anyhow::anyhow!("Failed to read from stderr client: {e}"));
            }
        }
    }
}

/// Waits for clients to connect on the `stderr` port, and serves the child process’s `stderr` output
/// to the first client that connects. If that client disconnects, we wait for the next client to connect
/// and serve the current `stderr` output to them instead, and so on. The function spawns two threads
/// for each client connection: one for monitoring disconnection and one for writing output.
pub fn stderr_server(
    listener: TcpListener,
    stderr_state: Arc<NotifyableOutputState>,
    control_tx: mpsc::Sender<ControlMessage>,
) -> Result<(), anyhow::Error> {
    // We allow reconnects on the `stderr` port, but only one client at a time. When a client disconnects,
    // we simply wait for the next one to connect.
    let has_active_connection = Arc::new(AtomicBool::new(false));
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                if has_active_connection
                    .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok()
                {
                    // Atomic value has been successfully changed from `false` to `true`.
                    eprintln!("[stderr] client connected from {}", stream.peer_addr()?);

                    let connection_monitoring_stream = match stream.try_clone() {
                        Ok(s) => s,
                        Err(e) => {
                            eprintln!("[stderr] failed to clone stream: {e}");
                            has_active_connection.store(false, Ordering::Release);
                            continue;
                        }
                    };

                    let has_active_connection_clone = Arc::clone(&has_active_connection);
                    let has_active_connection_monitor = Arc::clone(&has_active_connection);
                    let stderr_state = Arc::clone(&stderr_state);
                    let control_tx = control_tx.clone();

                    // Spawn read monitoring thread to detect disconnection proactively.
                    thread::spawn(move || {
                        let _ = monitor_stderr_client_connection(
                            connection_monitoring_stream,
                            has_active_connection_monitor,
                        );
                    });

                    // Spawn write thread to serve stderr output.
                    thread::spawn(move || {
                        let _ = serve_output_on_stream(
                            stream,
                            stderr_state,
                            control_tx,
                            ServingBehavior::DoNotKillChildOnDisconnect,
                            "stderr",
                        );
                        // When the write thread finishes, also clear the connection flag
                        // (idempotent if the read thread already did this).
                        has_active_connection_clone.store(false, Ordering::Release);
                    });
                } else {
                    // Atomic value was already `true`, so there is already an active connection.
                    eprintln!(
                        "[stderr] client connected from {}, but another client is already connected; rejecting connection",
                        stream.peer_addr()?
                    );
                }
            }
            Err(e) => {
                eprintln!("[stderr] accept failed: {e}");
            }
        }
    }

    Ok(())
}
