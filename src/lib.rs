//! A TCP forwarder that exposes a child process’s stdin, stdout, and stderr streams over the network.
//!
//! stdioxide launches an arbitrary child process and forwards its standard streams over two TCP ports,
//! allowing remote interaction with any command-line application. Output is buffered to prevent data
//! loss when no clients are connected. A third TCP port provides health check functionality for
//! container orchestrators.
//!
//! # Architecture
//!
//! - **Protocol Port**: Bidirectional communication for stdin/stdout (single client, kills child on disconnect)
//! - **Stderr Port**: Reconnectable stderr streaming with buffering (single client, child continues on disconnect)
//! - **Health Port**: Simple readiness check endpoint
//!
//! # Example
//!
//! ```no_run
//! use stdioxide::{app, args::Args};
//! use clap::Parser;
//!
//! let args = Args::parse();
//! app::run(&args).expect("Failed to run stdioxide");
//! ```

#![allow(
    unused_crate_dependencies,
    reason = "dev-dependencies available to lib tests"
)]
#![cfg_attr(
    test,
    allow(
        clippy::panic_in_result_fn,
        reason = "Using `assert!()`s is idiomatic, but we need to return `Result`s to be able to return I/O-related errors."
    )
)]

pub mod app;

/// Command-line argument parsing and configuration.
///
/// Defines the `Args` struct with port configurations and child process command.
pub mod args;

pub(crate) mod child;
pub(crate) mod control;
pub(crate) mod output;
pub(crate) mod servers;
