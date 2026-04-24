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

pub fn run(args: Args) -> Result<(), anyhow::Error> {
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
            let _ = pump_output_to_state(child.stdout, stdout_state, "stdout");
        });
    }

    {
        let stderr_state = Arc::clone(&stderr_state);
        thread::spawn(move || {
            let _ = pump_output_to_state(child.stderr, stderr_state, "stderr");
        });
    }

    {
        let stdout_state = Arc::clone(&stdout_state);
        let control_tx = control_tx.clone();
        thread::spawn(move || {
            let _ = protocol_server(protocol_listener, stdout_state, child.stdin, control_tx);
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

    let coordinator_thread: JoinHandle<Result<(), anyhow::Error>> =
        thread::spawn(move || run_child_coordinator(child.job, control_rx));

    coordinator_thread
        .join()
        .expect("Failed to join coordinator thread")?;

    Ok(())
}
