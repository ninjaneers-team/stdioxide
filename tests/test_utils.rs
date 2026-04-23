//! Cross-platform test utilities for spawning commands that work on both Unix and Windows.
//!
//! This module provides helper functions that abstract over platform-specific commands.
//! On Unix, commands use `bash`, `sleep`, `cat`, etc.
//! On Windows, commands use `cmd`, `powershell`, `ping` (for delays), etc.
//!
//! The goal is to make integration tests work on all platforms without #[cfg(not(windows))]
//! guards scattered throughout the test code.

#[cfg(windows)]
pub fn sleep_cmd(seconds: u32) -> (&'static str, Vec<String>) {
    // Use ping as a sleep alternative on Windows
    // Pings localhost N+1 times with 1-second intervals (approximately N seconds total)
    let pings = seconds + 1;
    (
        "ping",
        vec!["-n".to_string(), pings.to_string(), "127.0.0.1".to_string()],
    )
}

#[cfg(not(windows))]
pub fn sleep_cmd(seconds: u32) -> (&'static str, Vec<String>) {
    ("sleep", vec![seconds.to_string()])
}

#[cfg(windows)]
pub fn echo_with_sleep_cmd(text: &str, seconds: u32) -> (&'static str, Vec<String>) {
    let pings = seconds + 1;
    (
        "cmd",
        vec![
            "/C".to_string(),
            format!("echo {} && ping -n {} 127.0.0.1 >nul", text, pings),
        ],
    )
}

#[cfg(not(windows))]
pub fn echo_with_sleep_cmd(text: &str, seconds: u32) -> (&'static str, Vec<String>) {
    (
        "bash",
        vec![
            "-c".to_string(),
            format!("echo '{}' && sleep {}", text, seconds),
        ],
    )
}

#[cfg(windows)]
pub fn stderr_echo_with_sleep_cmd(text: &str, seconds: u32) -> (&'static str, Vec<String>) {
    let pings = seconds + 1;
    (
        "cmd",
        vec![
            "/C".to_string(),
            format!("echo {} 1>&2 && ping -n {} 127.0.0.1 >nul", text, pings),
        ],
    )
}

#[cfg(not(windows))]
pub fn stderr_echo_with_sleep_cmd(text: &str, seconds: u32) -> (&'static str, Vec<String>) {
    (
        "bash",
        vec![
            "-c".to_string(),
            format!("echo '{}' >&2 && sleep {}", text, seconds),
        ],
    )
}

#[cfg(windows)]
pub fn multi_echo_stderr_cmd(
    buffered: &str,
    sleep1: f32,
    realtime: &str,
    sleep2: u32,
) -> (&'static str, Vec<String>) {
    let sleep1_ms = (sleep1 * 1000.0) as u32;
    let pings = sleep2 + 1;
    (
        "powershell",
        vec![
            "-NoProfile".to_string(),
            "-Command".to_string(),
            format!(
                "[Console]::Error.WriteLine('{}'); Start-Sleep -Milliseconds {}; [Console]::Error.WriteLine('{}'); ping -n {} 127.0.0.1 >$null",
                buffered, sleep1_ms, realtime, pings
            ),
        ],
    )
}

#[cfg(not(windows))]
pub fn multi_echo_stderr_cmd(
    buffered: &str,
    sleep1: f32,
    realtime: &str,
    sleep2: u32,
) -> (&'static str, Vec<String>) {
    (
        "bash",
        vec![
            "-c".to_string(),
            format!(
                "echo '{}' >&2; sleep {}; echo '{}' >&2; sleep {}",
                buffered, sleep1, realtime, sleep2
            ),
        ],
    )
}

#[cfg(windows)]
pub fn multi_echo_stdout_cmd(
    buffered: &str,
    sleep1: f32,
    realtime: &str,
    sleep2: u32,
) -> (&'static str, Vec<String>) {
    let sleep1_ms = (sleep1 * 1000.0) as u32;
    let pings = sleep2 + 1;
    (
        "powershell",
        vec![
            "-NoProfile".to_string(),
            "-Command".to_string(),
            format!(
                "Write-Output '{}'; Start-Sleep -Milliseconds {}; Write-Output '{}'; ping -n {} 127.0.0.1 >$null",
                buffered, sleep1_ms, realtime, pings
            ),
        ],
    )
}

#[cfg(not(windows))]
pub fn multi_echo_stdout_cmd(
    buffered: &str,
    sleep1: f32,
    realtime: &str,
    sleep2: u32,
) -> (&'static str, Vec<String>) {
    (
        "bash",
        vec![
            "-c".to_string(),
            format!(
                "echo '{}'; sleep {}; echo '{}'; sleep {}",
                buffered, sleep1, realtime, sleep2
            ),
        ],
    )
}

#[cfg(windows)]
pub fn cat_cmd() -> (&'static str, Vec<String>) {
    // PowerShell that reads from stdin and writes to stdout with immediate flushing
    // Use [Console]::In to read from stdin (not console keyboard)
    (
        "powershell",
        vec![
            "-NoProfile".to_string(),
            "-Command".to_string(),
            "while($line = [Console]::In.ReadLine()) { [Console]::WriteLine($line) }".to_string(),
        ],
    )
}

#[cfg(not(windows))]
pub fn cat_cmd() -> (&'static str, Vec<String>) {
    ("cat", vec![])
}

#[cfg(windows)]
pub fn loop_stdin_to_stdout_cmd() -> (&'static str, Vec<String>) {
    // PowerShell script that reads line by line and echoes
    // Use [Console]::In to read from stdin and [Console]::WriteLine() for immediate flushing
    (
        "powershell",
        vec![
            "-NoProfile".to_string(),
            "-Command".to_string(),
            "while($line = [Console]::In.ReadLine()) { [Console]::WriteLine('response') }".to_string(),
        ],
    )
}

#[cfg(not(windows))]
pub fn loop_stdin_to_stdout_cmd() -> (&'static str, Vec<String>) {
    (
        "bash",
        vec![
            "-c".to_string(),
            "while true; do read line; echo response; done".to_string(),
        ],
    )
}

#[cfg(windows)]
pub fn continuous_stderr_loop_cmd() -> (&'static str, Vec<String>) {
    (
        "powershell",
        vec![
            "-NoProfile".to_string(),
            "-Command".to_string(),
            "while($true) { [Console]::Error.WriteLine('error'); Start-Sleep -Milliseconds 100 }"
                .to_string(),
        ],
    )
}

#[cfg(not(windows))]
pub fn continuous_stderr_loop_cmd() -> (&'static str, Vec<String>) {
    (
        "bash",
        vec![
            "-c".to_string(),
            "while true; do echo error >&2; sleep 0.1; done".to_string(),
        ],
    )
}

#[cfg(windows)]
pub fn generate_large_output_cmd(size: usize) -> (&'static str, Vec<String>) {
    // Generate large output using PowerShell
    (
        "powershell",
        vec![
            "-NoProfile".to_string(),
            "-Command".to_string(),
            format!("'A' * {}; ping -n 11 127.0.0.1 >$null", size),
        ],
    )
}

#[cfg(not(windows))]
pub fn generate_large_output_cmd(size: usize) -> (&'static str, Vec<String>) {
    (
        "bash",
        vec![
            "-c".to_string(),
            format!("head -c {} /dev/zero | tr '\\0' 'A'; sleep 10", size),
        ],
    )
}

#[cfg(windows)]
pub fn numbered_output_loop_cmd(count: u32, interval_ms: u32) -> (&'static str, Vec<String>) {
    (
        "powershell",
        vec![
            "-NoProfile".to_string(),
            "-Command".to_string(),
            format!(
                "1..{} | ForEach-Object {{ Write-Output \"stdout_line_$_\"; [Console]::Error.WriteLine(\"stderr_line_$_\"); Start-Sleep -Milliseconds {} }}",
                count, interval_ms
            ),
        ],
    )
}

#[cfg(not(windows))]
pub fn numbered_output_loop_cmd(count: u32, interval_ms: u32) -> (&'static str, Vec<String>) {
    let interval_sec = interval_ms as f32 / 1000.0;
    (
        "bash",
        vec![
            "-c".to_string(),
            format!(
                "for i in {{1..{count}}}; do echo \"stdout_line_$i\"; echo \"stderr_line_$i\" >&2; sleep {interval_sec}; done"
            ),
        ],
    )
}

#[cfg(windows)]
pub fn complex_stderr_reconnect_cmd() -> (&'static str, Vec<String>) {
    (
        "powershell",
        vec![
            "-NoProfile".to_string(),
            "-Command".to_string(),
            concat!(
                "[Console]::Error.WriteLine('before_connection'); Start-Sleep -Milliseconds 500; ",
                "[Console]::Error.WriteLine('during_first_connection'); Start-Sleep -Milliseconds 1000; ",
                "[Console]::Error.WriteLine('trigger_disconnect'); Start-Sleep -Milliseconds 1500; ",
                "[Console]::Error.WriteLine('while_disconnected'); Start-Sleep -Milliseconds 2000; ",
                "[Console]::Error.WriteLine('during_second_connection'); Start-Sleep -Seconds 10"
            ).to_string(),
        ],
    )
}

#[cfg(not(windows))]
pub fn complex_stderr_reconnect_cmd() -> (&'static str, Vec<String>) {
    (
        "bash",
        vec![
            "-c".to_string(),
            concat!(
                "echo 'before_connection' >&2; sleep 0.5; ",
                "echo 'during_first_connection' >&2; sleep 1; ",
                "echo 'trigger_disconnect' >&2; sleep 1.5; ",
                "echo 'while_disconnected' >&2; sleep 2; ",
                "echo 'during_second_connection' >&2; sleep 10",
            )
            .to_string(),
        ],
    )
}

#[cfg(windows)]
pub fn combined_output_cmd(
    stdout_msg: &str,
    stderr_msg: &str,
    sleep_sec: u32,
) -> (&'static str, Vec<String>) {
    let pings = sleep_sec + 1;
    (
        "powershell",
        vec![
            "-NoProfile".to_string(),
            "-Command".to_string(),
            format!(
                "Write-Output '{}'; [Console]::Error.WriteLine('{}'); ping -n {} 127.0.0.1 >$null",
                stdout_msg, stderr_msg, pings
            ),
        ],
    )
}

#[cfg(not(windows))]
pub fn combined_output_cmd(
    stdout_msg: &str,
    stderr_msg: &str,
    sleep_sec: u32,
) -> (&'static str, Vec<String>) {
    (
        "bash",
        vec![
            "-c".to_string(),
            format!(
                "echo '{}'; echo '{}' >&2; sleep {}",
                stdout_msg, stderr_msg, sleep_sec
            ),
        ],
    )
}

#[cfg(windows)]
pub fn echo_args_cmd(args: &[&str]) -> (&'static str, Vec<String>) {
    let mut cmd_args = vec!["/C".to_string()];
    // Use echo %* to print all arguments on Windows (requires a batch context)
    // Alternative: build the echo command with all args
    let echo_str = args.join(" ");
    cmd_args.push(format!("echo {} && ping -n 6 127.0.0.1 >nul", echo_str));
    ("cmd", cmd_args)
}

#[cfg(not(windows))]
pub fn echo_args_cmd(args: &[&str]) -> (&'static str, Vec<String>) {
    let mut script_args = vec![
        "-c".to_string(),
        "echo $@ && sleep 5".to_string(),
        "--".to_string(),
    ];
    script_args.extend(args.iter().map(|s| s.to_string()));
    ("bash", script_args)
}

#[cfg(windows)]
pub fn short_lived_cmd(msg: &str, sleep_ms: u32) -> (&'static str, Vec<String>) {
    (
        "powershell",
        vec![
            "-NoProfile".to_string(),
            "-Command".to_string(),
            format!(
                "Write-Output '{}'; Start-Sleep -Milliseconds {}",
                msg, sleep_ms
            ),
        ],
    )
}

#[cfg(not(windows))]
pub fn short_lived_cmd(msg: &str, sleep_ms: u32) -> (&'static str, Vec<String>) {
    let sleep_sec = sleep_ms as f32 / 1000.0;
    (
        "bash",
        vec![
            "-c".to_string(),
            format!("echo {} && sleep {}", msg, sleep_sec),
        ],
    )
}

#[cfg(windows)]
pub fn python_cmd() -> &'static str {
    "python"
}

#[cfg(not(windows))]
pub fn python_cmd() -> &'static str {
    "python3"
}
