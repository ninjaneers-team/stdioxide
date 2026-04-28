//! stdioxide binary entry point.
//!
//! Launches the TCP forwarder that exposes a child process’s standard streams over the network.

#![allow(
    unused_crate_dependencies,
    reason = "The binary depends on subprocess transitively through the stdioxide library"
)]

use std::io::stderr;

use stdioxide::{app, args::Args};

fn main() -> Result<(), anyhow::Error> {
    tracing_subscriber::fmt()
        .with_writer(stderr)
        .with_target(false)
        .init();

    let args = Args::parse();
    app::run(&args)
}
