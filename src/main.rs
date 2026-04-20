use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
};

use clap::Parser;
use subprocess::{Exec, Redirection};

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// The port to use for forwarding stdin and stdout.
    #[arg(long, env = "STDIOXIDE_PROTOCOL_PORT", default_value_t = 7000)]
    protocol_port: u16,

    /// The port to use for forwarding stderr.
    #[arg(long, env = "STDIOXIDE_STDERR_PORT", default_value_t = 7001)]
    stderr_port: u16,

    /// The port to use for health checks.
    #[arg(long, env = "STDIOXIDE_HEALTH_PORT", default_value_t = 7002)]
    health_port: u16,

    /// The command to run as a subprocess.
    #[arg(required = true)]
    command: String,

    /// The arguments to pass to the command.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 0..)]
    args: Vec<String>,
}

struct OutputState {
    buffer: Vec<u8>,
    eof: bool,
}

struct NotifyableOutputState {
    state: Mutex<OutputState>,
    condition_variable: Condvar,
}

impl NotifyableOutputState {
    fn new() -> Self {
        Self {
            state: Mutex::new(OutputState {
                buffer: Vec::new(),
                eof: false,
            }),
            condition_variable: Condvar::new(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum ControlMessage {
    KillChild,
}

#[derive(Debug, Clone, Copy)]
enum ServingBehavior {
    KillChildOnDisconnect,
    DoNotKillChildOnDisconnect,
}

/// Pumps data from the given `source` (either `stdout` or `stderr` of the child process) into the shared `state`.
fn pump_output_to_state(
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

fn forward_stream_data_to_child_process(
    mut stream: TcpStream,
    mut child_stdin: std::fs::File,
    control_tx: mpsc::Sender<ControlMessage>,
) -> Result<(), anyhow::Error> {
    let mut read_buffer = [0u8; 8192];
    loop {
        let num_bytes_read = match stream.read(&mut read_buffer) {
            Ok(0) => {
                eprintln!("[protocol] client disconnected; terminating child process");
                let _ = control_tx.send(ControlMessage::KillChild);
                return Ok(());
            }
            Ok(n) => n,
            Err(e) => {
                let _ = control_tx.send(ControlMessage::KillChild);
                return Err(anyhow::anyhow!("Failed to read from protocol client: {e}"));
            }
        };

        if let Err(e) = child_stdin.write_all(&read_buffer[..num_bytes_read]) {
            let _ = control_tx.send(ControlMessage::KillChild);
            return Err(anyhow::anyhow!("Failed to write to child stdin: {e}"));
        }
        if let Err(e) = child_stdin.flush() {
            let _ = control_tx.send(ControlMessage::KillChild);
            return Err(anyhow::anyhow!("Failed to flush child stdin: {e}"));
        }
    }
}

fn serve_output_on_stream(
    mut stream: TcpStream,
    output_state: Arc<NotifyableOutputState>,
    control_tx: mpsc::Sender<ControlMessage>,
    serving_behavior: ServingBehavior,
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
                    "[protocol] EOF reached and no buffered output; closing client connection"
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
            if let ServingBehavior::KillChildOnDisconnect = serving_behavior {
                let _ = control_tx.send(ControlMessage::KillChild);
            }
            return Ok(());
        }

        if guard.eof && guard.buffer.is_empty() {
            eprintln!("[protocol] EOF reached; closing client connection");
            return Ok(());
        }
    }
}

/// Waits for the first client to connect on the protocol port, then forwards data between that
/// client and the child process. This function spawns two threads: one for forwarding data from
/// the client to the child process’s `stdin`, and another for forwarding data from the child
/// process’s `stdout` to the client.
fn protocol_server(
    listener: TcpListener,
    stdout_state: Arc<NotifyableOutputState>,
    child_stdin: std::fs::File,
    control_tx: mpsc::Sender<ControlMessage>,
) -> Result<(), anyhow::Error> {
    // We only accept a single (i.e., the first) client connection on the protocol port.
    // When the client disconnects, we terminate the child process and exit the server.
    let (stdin_thread, stdout_thread) = match listener.accept() {
        Ok((stream, address)) => {
            eprintln!("[protocol] client connected from {address}");
            let cloned_stream = stream.try_clone()?;
            let cloned_control_tx = control_tx.clone();
            (
                thread::spawn(move || {
                    let _ = forward_stream_data_to_child_process(
                        cloned_stream,
                        child_stdin,
                        cloned_control_tx,
                    );
                }),
                thread::spawn(move || {
                    let _ = serve_output_on_stream(
                        stream,
                        Arc::clone(&stdout_state),
                        control_tx,
                        ServingBehavior::KillChildOnDisconnect,
                    );
                }),
            )
        }
        Err(e) => {
            return Err(anyhow::anyhow!("Failed to accept client connection: {e}"));
        }
    };

    stdin_thread.join().expect("Failed to join stdin thread");
    stdout_thread.join().expect("Failed to join stdout thread");

    Ok(())
}

/// Waits for clients to connect on the `stderr` port, and serves the child process’s `stderr` output
/// to the first client that connects. If that client disconnects, we wait for the next client to connect
/// and serve the current `stderr` output to them instead, and so on. The function spawns a new
/// thread for each client connection, but only allows one active connection at a time
fn stderr_server(
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

/// Waits for clients to connect on the `health` port, and immediately drops any connections. The existence
/// of a successful connection is used by the client as a health check for whether the process is alive.
fn health_server(listener: TcpListener) -> Result<(), anyhow::Error> {
    for stream in listener.incoming() {
        match stream {
            Ok(_stream) => {
                // Immediately drop it; successful connect is enough.
            }
            Err(e) => {
                eprintln!("[health] accept failed: {e}");
            }
        }
    }

    Ok(())
}

fn main() -> Result<(), anyhow::Error> {
    let args = Args::parse();

    let protocol_listener = TcpListener::bind(("0.0.0.0", args.protocol_port))?;
    let stderr_listener = TcpListener::bind(("0.0.0.0", args.stderr_port))?;
    let health_listener = TcpListener::bind(("0.0.0.0", args.health_port))?;

    let mut process = Exec::cmd(args.command);
    for arg in &args.args {
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

    let stdout_state = Arc::new(NotifyableOutputState::new());
    let stderr_state = Arc::new(NotifyableOutputState::new());

    let (control_tx, control_rx) = mpsc::channel::<ControlMessage>();

    {
        let stdout_state = Arc::clone(&stdout_state);
        thread::spawn(move || {
            let _ = pump_output_to_state(child_stdout, stdout_state, "stdout");
        });
    }

    {
        let stderr_state = Arc::clone(&stderr_state);
        thread::spawn(move || {
            let _ = pump_output_to_state(child_stderr, stderr_state, "stderr");
        });
    }

    {
        let stdout_state = Arc::clone(&stdout_state);
        let control_tx = control_tx.clone();
        thread::spawn(move || {
            let _ = protocol_server(protocol_listener, stdout_state, child_stdin, control_tx);
        });
    }

    {
        let stderr_state = Arc::clone(&stderr_state);
        let control_tx = control_tx.clone();
        thread::spawn(move || {
            let _ = stderr_server(stderr_listener, stderr_state, control_tx);
        });
    }

    // We drop the `control_tx` object here so that the main thread is no longer an owner of it
    // and thus is not taken into account when determining whether the channel is disconnected.
    drop(control_tx);

    {
        thread::spawn(move || {
            let _ = health_server(health_listener);
        });
    }

    let coordinator_thread: JoinHandle<Result<(), anyhow::Error>> = thread::spawn(move || {
        let job = job; // Take ownership of the child process.

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
    });

    coordinator_thread
        .join()
        .expect("Failed to join coordinator thread")?;

    Ok(())
}
