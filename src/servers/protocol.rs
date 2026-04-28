//! Protocol TCP server for bidirectional `stdin`/`stdout` forwarding.

use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{Arc, mpsc},
    thread,
};

use tracing::info;

use crate::{
    control::ControlMessage,
    output::{NotifyableOutputState, ServingBehavior, serve_output_on_stream},
};

/// Forwards data from a TCP client stream to the child process’s `stdin`.
///
/// Reads from the client and writes to child `stdin` until the client disconnects
/// or an error occurs. Sends a kill signal on disconnection.
fn forward_stream_data_to_child_process(
    mut stream: TcpStream,
    mut child_stdin: fs::File,
    control_tx: &mpsc::Sender<ControlMessage>,
) -> Result<(), anyhow::Error> {
    let mut read_buffer = [0_u8; 8192];
    loop {
        let num_bytes_read = match stream.read(&mut read_buffer) {
            Ok(0) => {
                info!("[protocol] client disconnected; terminating child process");
                let _result = control_tx.send(ControlMessage::KillChild);
                return Ok(());
            }
            Ok(n) => n,
            Err(error) => {
                let _result = control_tx.send(ControlMessage::KillChild);
                return Err(anyhow::anyhow!(
                    "Failed to read from protocol client: {error}"
                ));
            }
        };

        if let Err(error) =
            child_stdin.write_all(read_buffer.get(..num_bytes_read).unwrap_or_default())
        {
            let _result = control_tx.send(ControlMessage::KillChild);
            return Err(anyhow::anyhow!("Failed to write to child stdin: {error}"));
        }
        if let Err(error) = child_stdin.flush() {
            let _result = control_tx.send(ControlMessage::KillChild);
            return Err(anyhow::anyhow!("Failed to flush child stdin: {error}"));
        }
    }
}

/// Waits for the first client to connect on the protocol port, then forwards data between that
/// client and the child process. This function spawns two threads: one for forwarding data from
/// the client to the child process’s `stdin`, and another for forwarding data from the child
/// process’s `stdout` to the client.
pub(crate) fn protocol_server(
    listener: &TcpListener,
    stdout_state: Arc<NotifyableOutputState>,
    child_stdin: fs::File,
    control_tx: mpsc::Sender<ControlMessage>,
) -> Result<(), anyhow::Error> {
    // We only accept a single (i.e., the first) client connection on the protocol port.
    // When the client disconnects, we terminate the child process and exit the server.
    let (stdin_thread, stdout_thread) = match listener.accept() {
        Ok((stream, address)) => {
            info!("[protocol] client connected from {address}");
            let cloned_stream = stream.try_clone()?;
            let cloned_control_tx = control_tx.clone();
            (
                thread::spawn(move || {
                    drop(forward_stream_data_to_child_process(
                        cloned_stream,
                        child_stdin,
                        &cloned_control_tx,
                    ));
                }),
                thread::spawn(move || {
                    drop(serve_output_on_stream(
                        stream,
                        &stdout_state,
                        &control_tx,
                        &ServingBehavior::KillChildOnDisconnect,
                        "protocol",
                    ));
                }),
            )
        }
        Err(error) => {
            return Err(anyhow::anyhow!(
                "Failed to accept client connection: {error}"
            ));
        }
    };

    stdin_thread
        .join()
        .map_err(|error| anyhow::anyhow!("Stdin forwarding thread panicked: {error:?}"))?;
    stdout_thread
        .join()
        .map_err(|error| anyhow::anyhow!("Stdout forwarding thread panicked: {error:?}"))?;

    Ok(())
}
