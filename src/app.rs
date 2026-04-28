//! Application entry point that orchestrates the TCP servers and child process lifecycle.
//!
//! Spawns all the server threads (protocol, stderr, health) and manages the child process
//! coordination loop.

use std::{
    net::TcpListener,
    sync::{Arc, mpsc},
    thread::{self, JoinHandle},
};

use crate::{
    args::Args,
    child::StartedChild,
    control::{ControlMessage, run_child_coordinator},
    output::{NotifyableOutputState, pump_output_to_state},
    servers::{health::health_server, protocol::protocol_server, stderr::stderr_server},
};

/// Run the stdioxide forwarder with the given configuration.
///
/// This function:
/// 1. Starts the child process with the specified command and arguments
/// 2. Binds TCP listeners on the configured ports (protocol, stderr, health)
/// 3. Spawns threads to pump child process output into shared buffered state
/// 4. Spawns server threads to handle client connections on each port
/// 5. Coordinates child process lifecycle and graceful shutdown
///
/// The function blocks until the child process exits or is terminated.
///
/// # Errors
///
/// Returns an error if:
/// - Port binding fails (port already in use or insufficient permissions)
/// - Child process fails to start
/// - Child process coordinator thread panics
///
/// # Example
///
/// ```no_run
/// use stdioxide::{app, args::Args};
/// use clap::Parser;
///
/// let args = Args::parse();
/// app::run(&args).expect("Failed to run stdioxide");
/// ```
pub fn run(args: &Args) -> Result<(), anyhow::Error> {
    let protocol_listener = TcpListener::bind(("0.0.0.0", args.protocol_port))?;
    let stderr_listener = TcpListener::bind(("0.0.0.0", args.stderr_port))?;
    let health_listener = TcpListener::bind(("0.0.0.0", args.health_port))?;

    let child = StartedChild::start(&args.command, &args.args)?;

    let stdout_state = Arc::new(NotifyableOutputState::new());
    let stderr_state = Arc::new(NotifyableOutputState::new());

    let (control_tx, control_rx) = mpsc::channel::<ControlMessage>();

    {
        let stdout_state = Arc::clone(&stdout_state);
        thread::spawn(move || {
            drop(pump_output_to_state(child.stdout, &stdout_state, "stdout"));
        });
    }

    {
        let stderr_state = Arc::clone(&stderr_state);
        thread::spawn(move || {
            drop(pump_output_to_state(child.stderr, &stderr_state, "stderr"));
        });
    }

    {
        let stdout_state = Arc::clone(&stdout_state);
        let control_tx = control_tx.clone();
        thread::spawn(move || {
            drop(protocol_server(
                &protocol_listener,
                stdout_state,
                child.stdin,
                control_tx,
            ));
        });
    }

    {
        let stderr_state = Arc::clone(&stderr_state);
        let control_tx = control_tx.clone();
        thread::spawn(move || {
            drop(stderr_server(&stderr_listener, &stderr_state, &control_tx));
        });
    }

    // We drop the `control_tx` object here so that the main thread is no longer an owner of it
    // and thus is not taken into account when determining whether the channel is disconnected.
    drop(control_tx);

    {
        thread::spawn(move || {
            health_server(&health_listener);
        });
    }

    let coordinator_thread: JoinHandle<Result<(), anyhow::Error>> =
        thread::spawn(move || run_child_coordinator(&child.job, &control_rx));

    coordinator_thread
        .join()
        .map_err(|error| anyhow::anyhow!("Child coordinator thread panicked: {error:?}"))??;

    Ok(())
}
