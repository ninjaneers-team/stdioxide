//! Cross-platform test utilities for spawning commands that work on both Unix and Windows.
//!
//! This module provides helper functions that abstract over platform-specific commands.
//! On Unix, commands use `bash`, `sleep`, `cat`, etc.
//! On Windows, commands use `cmd`, `powershell`, `ping` (for delays), etc.
//!
//! The goal is to make integration tests work on all platforms without #[cfg(not(windows))]
//! guards scattered throughout the test code.

#![allow(
    unreachable_pub,
    reason = "Private test module, but items need `pub` for parent access"
)]

// Acknowledge available dev-dependencies not used in this test file.
use anyhow as _;
use clap as _;
use lsp_types as _;
use serde_json as _;
use stdioxide as _;
use subprocess as _;
use tracing as _;
use tracing_subscriber as _;

/// Returns a command that sleeps for the specified number of seconds.
#[cfg(windows)]
#[must_use]
pub fn sleep_cmd(seconds: u32) -> (&'static str, Vec<String>) {
    // Use ping as a sleep alternative on Windows
    // Pings localhost N+1 times with 1-second intervals (approximately N seconds total)
    let pings = seconds.saturating_add(1);
    (
        "ping",
        vec!["-n".to_owned(), pings.to_string(), "127.0.0.1".to_owned()],
    )
}

/// Returns a command that sleeps for the specified number of seconds.
#[cfg(not(windows))]
#[must_use]
pub fn sleep_cmd(seconds: u32) -> (&'static str, Vec<String>) {
    ("sleep", vec![seconds.to_string()])
}

/// Returns a command that echoes text to `stdout`, then sleeps.
#[cfg(windows)]
#[must_use]
pub fn echo_with_sleep_cmd(text: &str, seconds: u32) -> (&'static str, Vec<String>) {
    let pings = seconds.saturating_add(1);
    (
        "cmd",
        vec![
            "/C".to_owned(),
            format!("echo {text} && ping -n {pings} 127.0.0.1 >nul"),
        ],
    )
}

/// Returns a command that echoes text to `stdout`, then sleeps.
#[cfg(not(windows))]
#[must_use]
pub fn echo_with_sleep_cmd(text: &str, seconds: u32) -> (&'static str, Vec<String>) {
    (
        "bash",
        vec!["-c".to_owned(), format!("echo '{text}' && sleep {seconds}")],
    )
}

/// Returns a command that echoes text to `stderr`, then sleeps.
#[cfg(windows)]
#[must_use]
pub fn stderr_echo_with_sleep_cmd(text: &str, seconds: u32) -> (&'static str, Vec<String>) {
    let pings = seconds.saturating_add(1);
    (
        "cmd",
        vec![
            "/C".to_owned(),
            format!("echo {text} 1>&2 && ping -n {pings} 127.0.0.1 >nul"),
        ],
    )
}

/// Returns a command that echoes text to `stderr`, then sleeps.
#[cfg(not(windows))]
#[must_use]
pub fn stderr_echo_with_sleep_cmd(text: &str, seconds: u32) -> (&'static str, Vec<String>) {
    (
        "bash",
        vec![
            "-c".to_owned(),
            format!("echo '{text}' >&2 && sleep {seconds}"),
        ],
    )
}

/// Returns a command that echoes to `stderr`, sleeps, echoes again, then sleeps more.
#[cfg(windows)]
#[must_use]
pub fn multi_echo_stderr_cmd(
    buffered: &str,
    sleep1: f32,
    realtime: &str,
    sleep2: u32,
) -> (&'static str, Vec<String>) {
    let pings = sleep2.saturating_add(1);
    (
        "powershell",
        vec![
            "-NoProfile".to_owned(),
            "-Command".to_owned(),
            format!(
                "[Console]::Error.WriteLine('{buffered}'); Start-Sleep -Seconds {sleep1}; [Console]::Error.WriteLine('{realtime}'); ping -n {pings} 127.0.0.1 >$null"
            ),
        ],
    )
}

/// Returns a command that echoes to `stderr`, sleeps, echoes again, then sleeps more.
#[cfg(not(windows))]
#[must_use]
pub fn multi_echo_stderr_cmd(
    buffered: &str,
    sleep1: f32,
    realtime: &str,
    sleep2: u32,
) -> (&'static str, Vec<String>) {
    (
        "bash",
        vec![
            "-c".to_owned(),
            format!("echo '{buffered}' >&2; sleep {sleep1}; echo '{realtime}' >&2; sleep {sleep2}"),
        ],
    )
}

/// Returns a command that echoes to `stdout`, sleeps, echoes again, then sleeps more.
#[cfg(windows)]
#[must_use]
pub fn multi_echo_stdout_cmd(
    buffered: &str,
    sleep1: f32,
    realtime: &str,
    sleep2: u32,
) -> (&'static str, Vec<String>) {
    let pings = sleep2.saturating_add(1);
    (
        "powershell",
        vec![
            "-NoProfile".to_owned(),
            "-Command".to_owned(),
            format!(
                "Write-Output '{buffered}'; Start-Sleep -Seconds {sleep1}; Write-Output '{realtime}'; ping -n {pings} 127.0.0.1 >$null"
            ),
        ],
    )
}

/// Returns a command that echoes to `stdout`, sleeps, echoes again, then sleeps more.
#[cfg(not(windows))]
#[must_use]
pub fn multi_echo_stdout_cmd(
    buffered: &str,
    sleep1: f32,
    realtime: &str,
    sleep2: u32,
) -> (&'static str, Vec<String>) {
    (
        "bash",
        vec![
            "-c".to_owned(),
            format!("echo '{buffered}'; sleep {sleep1}; echo '{realtime}'; sleep {sleep2}",),
        ],
    )
}

/// Returns a command that reads from `stdin` and echoes to `stdout` (like `cat`).
#[cfg(windows)]
#[must_use]
pub fn cat_cmd() -> (&'static str, Vec<String>) {
    // Use Python for reliable line-by-line I/O on Windows.
    // `-u` flag disables buffering for immediate output.
    (
        python_cmd(),
        vec![
            "-u".to_owned(),
            "-c".to_owned(),
            "import sys; [print(line.rstrip()) for line in sys.stdin]".to_owned(),
        ],
    )
}

/// Returns a command that reads from `stdin` and echoes to `stdout` (like `cat`).
#[cfg(not(windows))]
#[must_use]
pub const fn cat_cmd() -> (&'static str, Vec<String>) {
    ("cat", vec![])
}

/// Returns a command that continuously reads from `stdin` and writes "response" to `stdout`.
#[cfg(windows)]
#[must_use]
pub fn loop_stdin_to_stdout_cmd() -> (&'static str, Vec<String>) {
    // PowerShell script that reads line by line and echoes
    // Use [Console]::In to read from `stdin` and [Console]::WriteLine() for immediate flushing
    (
        "powershell",
        vec![
            "-NoProfile".to_owned(),
            "-Command".to_owned(),
            "while($line = [Console]::In.ReadLine()) { [Console]::WriteLine('response') }"
                .to_owned(),
        ],
    )
}

/// Returns a command that continuously reads from `stdin` and writes "response" to `stdout`.
#[cfg(not(windows))]
#[must_use]
pub fn loop_stdin_to_stdout_cmd() -> (&'static str, Vec<String>) {
    (
        "bash",
        vec![
            "-c".to_owned(),
            "while true; do read line; echo response; done".to_owned(),
        ],
    )
}

/// Returns a command that continuously writes "error" to `stderr` in a loop.
#[cfg(windows)]
#[must_use]
pub fn continuous_stderr_loop_cmd() -> (&'static str, Vec<String>) {
    (
        "powershell",
        vec![
            "-NoProfile".to_owned(),
            "-Command".to_owned(),
            "while($true) { [Console]::Error.WriteLine('error'); Start-Sleep -Milliseconds 100 }"
                .to_owned(),
        ],
    )
}

/// Returns a command that continuously writes "error" to `stderr` in a loop.
#[cfg(not(windows))]
#[must_use]
pub fn continuous_stderr_loop_cmd() -> (&'static str, Vec<String>) {
    (
        "bash",
        vec![
            "-c".to_owned(),
            "while true; do echo error >&2; sleep 0.1; done".to_owned(),
        ],
    )
}

/// Returns a command that generates a large block of output (repeated 'A' characters).
#[cfg(windows)]
#[must_use]
pub fn generate_large_output_cmd(size: usize) -> (&'static str, Vec<String>) {
    // Generate large output using PowerShell
    (
        "powershell",
        vec![
            "-NoProfile".to_owned(),
            "-Command".to_owned(),
            format!("'A' * {size}; ping -n 11 127.0.0.1 >$null"),
        ],
    )
}

/// Returns a command that generates a large block of output (repeated 'A' characters).
#[cfg(not(windows))]
#[must_use]
pub fn generate_large_output_cmd(size: usize) -> (&'static str, Vec<String>) {
    (
        "bash",
        vec![
            "-c".to_owned(),
            format!("head -c {size} /dev/zero | tr '\\0' 'A'; sleep 10"),
        ],
    )
}

/// Returns a command that outputs numbered lines to both `stdout` and `stderr` with delays.
#[cfg(windows)]
#[must_use]
pub fn numbered_output_loop_cmd(count: u32, interval_ms: u32) -> (&'static str, Vec<String>) {
    (
        "powershell",
        vec![
            "-NoProfile".to_owned(),
            "-Command".to_owned(),
            format!(
                "1..{count} | ForEach-Object {{ Write-Output \"stdout_line_$_\"; [Console]::Error.WriteLine(\"stderr_line_$_\"); Start-Sleep -Milliseconds {interval_ms} }}"
            ),
        ],
    )
}

/// Returns a command that outputs numbered lines to both `stdout` and `stderr` with delays.
#[cfg(not(windows))]
#[must_use]
pub fn numbered_output_loop_cmd(count: u32, interval_ms: u32) -> (&'static str, Vec<String>) {
    #[expect(
        clippy::integer_division,
        reason = "Intentional conversion of milliseconds to seconds with fractional part"
    )]
    let seconds = interval_ms / 1000;
    let millis = interval_ms % 1000;
    let interval_sec = format!("{seconds}.{millis:03}");
    (
        "bash",
        vec![
            "-c".to_owned(),
            format!(
                "for i in {{1..{count}}}; do echo \"stdout_line_$i\"; echo \"stderr_line_$i\" >&2; sleep {interval_sec}; done"
            ),
        ],
    )
}

/// Returns a command that emits timed `stderr` output for testing reconnection scenarios.
#[cfg(windows)]
#[must_use]
pub fn complex_stderr_reconnect_cmd() -> (&'static str, Vec<String>) {
    (
        "powershell",
        vec![
            "-NoProfile".to_owned(),
            "-Command".to_owned(),
            concat!(
                "[Console]::Error.WriteLine('before_connection'); Start-Sleep -Milliseconds 500; ",
                "[Console]::Error.WriteLine('during_first_connection'); Start-Sleep -Milliseconds 1000; ",
                "[Console]::Error.WriteLine('trigger_disconnect'); Start-Sleep -Milliseconds 1500; ",
                "[Console]::Error.WriteLine('while_disconnected'); Start-Sleep -Milliseconds 2000; ",
                "[Console]::Error.WriteLine('during_second_connection'); Start-Sleep -Seconds 10"
            ).to_owned(),
        ],
    )
}

/// Returns a command that emits timed `stderr` output for testing reconnection scenarios.
#[cfg(not(windows))]
#[must_use]
pub fn complex_stderr_reconnect_cmd() -> (&'static str, Vec<String>) {
    (
        "bash",
        vec![
            "-c".to_owned(),
            concat!(
                "echo 'before_connection' >&2; sleep 0.5; ",
                "echo 'during_first_connection' >&2; sleep 1; ",
                "echo 'trigger_disconnect' >&2; sleep 1.5; ",
                "echo 'while_disconnected' >&2; sleep 2; ",
                "echo 'during_second_connection' >&2; sleep 10",
            )
            .to_owned(),
        ],
    )
}

/// Returns a command that outputs to both `stdout` and `stderr`, then sleeps.
#[cfg(windows)]
#[must_use]
pub fn combined_output_cmd(
    stdout_msg: &str,
    stderr_msg: &str,
    sleep_sec: u32,
) -> (&'static str, Vec<String>) {
    let pings = sleep_sec.saturating_add(1);
    (
        "powershell",
        vec![
            "-NoProfile".to_owned(),
            "-Command".to_owned(),
            format!(
                "Write-Output '{stdout_msg}'; [Console]::Error.WriteLine('{stderr_msg}'); ping -n {pings} 127.0.0.1 >$null"
            ),
        ],
    )
}

/// Returns a command that outputs to both `stdout` and `stderr`, then sleeps.
#[cfg(not(windows))]
#[must_use]
pub fn combined_output_cmd(
    stdout_msg: &str,
    stderr_msg: &str,
    sleep_sec: u32,
) -> (&'static str, Vec<String>) {
    (
        "bash",
        vec![
            "-c".to_owned(),
            format!("echo '{stdout_msg}'; echo '{stderr_msg}' >&2; sleep {sleep_sec}"),
        ],
    )
}

/// Returns a command that echoes all provided arguments to `stdout`.
#[cfg(windows)]
#[must_use]
pub fn echo_args_cmd(args: &[&str]) -> (&'static str, Vec<String>) {
    let mut cmd_args = vec!["/C".to_owned()];
    // Use echo %* to print all arguments on Windows (requires a batch context)
    // Alternative: build the echo command with all args
    let echo_str = args.join(" ");
    cmd_args.push(format!("echo {echo_str} && ping -n 6 127.0.0.1 >nul"));
    ("cmd", cmd_args)
}

/// Returns a command that echoes all provided arguments to `stdout`.
#[cfg(not(windows))]
#[must_use]
pub fn echo_args_cmd(args: &[&str]) -> (&'static str, Vec<String>) {
    let mut script_args = vec![
        "-c".to_owned(),
        "echo $@ && sleep 5".to_owned(),
        "--".to_owned(),
    ];
    script_args.extend(args.iter().map(ToString::to_string));
    ("bash", script_args)
}

/// Returns a command that outputs a message and exits quickly after a brief delay.
#[cfg(windows)]
#[must_use]
pub fn short_lived_cmd(msg: &str, sleep_ms: u32) -> (&'static str, Vec<String>) {
    (
        "powershell",
        vec![
            "-NoProfile".to_owned(),
            "-Command".to_owned(),
            format!("Write-Output '{msg}'; Start-Sleep -Milliseconds {sleep_ms}"),
        ],
    )
}

/// Returns a command that outputs a message and exits quickly after a brief delay.
#[cfg(not(windows))]
#[must_use]
pub fn short_lived_cmd(msg: &str, sleep_ms: u32) -> (&'static str, Vec<String>) {
    #[expect(
        clippy::integer_division,
        reason = "Intentional conversion of milliseconds to seconds with fractional part"
    )]
    let seconds = sleep_ms / 1000;
    let millis = sleep_ms % 1000;
    let sleep_arg = format!("{seconds}.{millis:03}");
    (
        "bash",
        vec!["-c".to_owned(), format!("echo {msg} && sleep {sleep_arg}")],
    )
}

/// Returns the platform-specific Python command name.
#[cfg(windows)]
#[must_use]
pub const fn python_cmd() -> &'static str {
    "python"
}

/// Returns the platform-specific Python command name.
#[cfg(not(windows))]
#[must_use]
pub const fn python_cmd() -> &'static str {
    "python3"
}
