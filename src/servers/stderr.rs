use std::{
    net::TcpListener,
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

/// Waits for clients to connect on the `stderr` port, and serves the child process’s `stderr` output
/// to the first client that connects. If that client disconnects, we wait for the next client to connect
/// and serve the current `stderr` output to them instead, and so on. The function spawns a new
/// thread for each client connection, but only allows one active connection at a time
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
                    let has_active_connection = Arc::clone(&has_active_connection);
                    let stderr_state = Arc::clone(&stderr_state);
                    let control_tx = control_tx.clone();
                    thread::spawn(move || {
                        let _ = serve_output_on_stream(
                            stream,
                            stderr_state,
                            control_tx,
                            ServingBehavior::DoNotKillChildOnDisconnect,
                            "stderr",
                        );
                        has_active_connection.store(false, Ordering::Release);
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
