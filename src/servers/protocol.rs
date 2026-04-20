use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{Arc, mpsc},
    thread,
};

use crate::{
    control::ControlMessage,
    output::{NotifyableOutputState, ServingBehavior, serve_output_on_stream},
};

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

/// Waits for the first client to connect on the protocol port, then forwards data between that
/// client and the child process. This function spawns two threads: one for forwarding data from
/// the client to the child process’s `stdin`, and another for forwarding data from the child
/// process’s `stdout` to the client.
pub fn protocol_server(
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
                        "protocol",
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
