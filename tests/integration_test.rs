//! Integration tests for stdioxide TCP forwarder functionality.
#![cfg(test)]
#![allow(
    clippy::panic_in_result_fn,
    reason = "Using `assert!()`s is idiomatic, but we need to return `Result`s to be able to return I/O-related errors."
)]

use std::{
    collections::HashSet,
    env,
    fs::read_to_string,
    io::{self, Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    process::{Child, Command, Stdio},
    //string::String,
    sync::{LazyLock, Mutex},
    thread,
    time::{Duration, Instant},
};

// Acknowledge available dev-dependencies not used in this test file.
use clap as _;
use lsp_types as _;
use stdioxide as _;
use subprocess as _;
use tracing as _;
use tracing_subscriber as _;

use crate::lsp_client::LspClient;

mod test_utils;
use test_utils::*;

// Global registry tracking all currently allocated ports across all tests used so
// that tests can allocate ports in parallel as long as they don’t conflict.
static ALLOCATED_PORTS_REGISTRY: LazyLock<Mutex<HashSet<u16>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

/// Find an available port by binding to port 0 and letting the OS assign one.
/// Returns the port number that was assigned.
/// Never returns default ports (7000, 7001, 7002) to avoid conflicts with `test_default_port_values()`.
fn find_available_port() -> Result<u16, anyhow::Error> {
    loop {
        // Note: This function binds to a port to find out whether it’s available. But this exposes a race
        //       condition if actions happen in the following order:
        //       1. `find_available_port()` binds to a port and thus considers it as being available.
        //       2. `find_available_port()` drops the listener, making the port available again.
        //       3. Another test tries to find an available port. `find_available_port()` might bind to the same
        //          port again.
        //       4. While the port is bound, the first test tries to bind the `TestForwarder` to the same port.
        //          This will fail because the port is currently blocked by `find_available_port()` running
        //          in the context of the second test.
        //       5. The first test fails even though the second test is about to free that port again.
        //      To mitigate this, the `TestForwarder::start()` function has a retry mechanism when
        //      spawning the forwarder, so if it fails to bind to the port, it will try again with a different
        //      shortly after.
        let port = TcpListener::bind("127.0.0.1:0")
            .map_err(|error| anyhow::anyhow!("Failed to bind to find available port: {error}"))?
            .local_addr()
            .map_err(|error| anyhow::anyhow!("Failed to get local address: {error}"))?
            .port();

        // Skip default ports to avoid conflicts with `test_default_port_values()`.
        if !(7000..=7002).contains(&port) {
            return Ok(port);
        }
    }
}

/// RAII guard that removes ports from the global registry when dropped.
struct PortGuard {
    ports: [u16; 3],
}

impl Drop for PortGuard {
    fn drop(&mut self) {
        #[expect(
            clippy::unwrap_used,
            reason = "This is the destructor and we cannot really do something else if locking fails, so it’s acceptable to unwrap here."
        )]
        let mut registry = ALLOCATED_PORTS_REGISTRY.lock().unwrap();
        for port in &self.ports {
            registry.remove(port);
        }
    }
}

/// RAII guard for allocated ports.
/// Registers ports in the global registry to prevent other tests from allocating them.
/// Ports are automatically released when this struct is dropped.
/// Not `Copy` or `Clone`: ownership ensures exclusivity.
struct AllocatedPorts {
    protocol_port: u16,
    stderr_port: u16,
    health_port: u16,
    _guard: PortGuard,
}

impl AllocatedPorts {
    /// Allocate three available ports and register them globally.
    /// Retries if the OS-assigned ports are already allocated by another test.
    /// Multiple tests can allocate ports concurrently without blocking each other.
    fn new() -> Result<Self, anyhow::Error> {
        loop {
            // Find candidate ports from the OS
            let p1 = find_available_port()?;
            let p2 = find_available_port()?;
            let p3 = find_available_port()?;

            // `find_available_port()` could return a port value that was previously found out to
            // be free in the context of a different test, but has not been bound yet. However, it would
            // still be in the registry. Therefore, we have to check against the registry to ensure
            // that such a port is *really* available.

            // Try to reserve them in the global registry.
            let mut registry = ALLOCATED_PORTS_REGISTRY.lock().map_err(|error| {
                anyhow::anyhow!("Failed to lock allocated ports registry: {error}")
            })?;
            let ports_are_available = [p1, p2, p3].iter().all(|port| !registry.contains(port));
            if ports_are_available {
                registry.insert(p1);
                registry.insert(p2);
                registry.insert(p3);
                drop(registry); // Drop “early” to satisfy Clippy.

                return Ok(Self {
                    protocol_port: p1,
                    stderr_port: p2,
                    health_port: p3,
                    _guard: PortGuard {
                        ports: [p1, p2, p3],
                    },
                });
            }
            // If any port is already allocated, try again
        }
    }

    fn protocol_port(&self) -> u16 {
        self.protocol_port
    }

    fn stderr_port(&self) -> u16 {
        self.stderr_port
    }

    fn health_port(&self) -> u16 {
        self.health_port
    }
}

/// Helper struct to manage a `stdioxide` process for testing.
/// Automatically cleans up the process when dropped.
/// Owns the allocated ports to keep them reserved in the global registry
/// for the entire lifetime of the forwarder.
struct TestForwarder {
    process: Child,
    ports: AllocatedPorts,
}

impl TestForwarder {
    /// Start a new `stdioxide` forwarder with the given command and arguments.
    /// Automatically allocates unique available ports for this test.
    /// Retries if port binding fails (e.g., if a port was grabbed between discovery and binding).
    fn start(command: &str, args: &[&str]) -> Result<Self, anyhow::Error> {
        const MAX_RETRIES: usize = 3;
        let mut last_error = None;

        for attempt in 0..MAX_RETRIES {
            // Allocate ports--registered in global registry.
            let ports = AllocatedPorts::new()?;

            // Try to start with these ports.
            match Self::try_start_with_ports(command, args, ports) {
                Ok(forwarder) => return Ok(forwarder),
                Err(error) => {
                    last_error = Some(error);
                    if attempt < MAX_RETRIES - 1 {
                        // Retry with new ports
                        thread::sleep(Duration::from_millis(100));
                    }
                }
            }
        }

        let last_error = last_error.unwrap_or_else(|| "Unknown error".to_owned());
        Err(anyhow::anyhow!(
            "Failed to start forwarder after {MAX_RETRIES} attempts. Last error: {last_error}",
        ))
    }

    /// Try to start a new `stdioxide` forwarder with pre-allocated ports.
    /// Returns an error if spawning fails or the forwarder doesn’t become ready.
    fn try_start_with_ports(
        command: &str,
        args: &[&str],
        ports: AllocatedPorts,
    ) -> Result<Self, String> {
        let process = Self::spawn_process(command, args, &ports)
            .map_err(|error| format!("Failed to spawn process: {error}"))?;

        let forwarder = Self { process, ports };

        // Wait for the forwarder to bind to the ports
        forwarder
            .try_wait_for_ready()
            .map_err(|error| format!("Failed to become ready: {error}"))?;

        // Ports remain in the forwarder and will be released when it’s dropped.
        Ok(forwarder)
    }

    /// Internal helper to spawn the forwarder process.
    /// Returns the spawned Child process or an error if spawning fails (e.g., due to port conflicts).
    fn spawn_process(command: &str, args: &[&str], ports: &AllocatedPorts) -> io::Result<Child> {
        // Get the path to the `stdioxide` binary.
        // In integration tests, we need to use the binary from the target directory.
        let bin_path = env::var("CARGO_BIN_EXE_stdioxide")
            .unwrap_or_else(|_| "target/debug/stdioxide".to_owned());

        let mut cmd = Command::new(&bin_path);
        cmd.arg("--protocol-port")
            .arg(ports.protocol_port().to_string())
            .arg("--stderr-port")
            .arg(ports.stderr_port().to_string())
            .arg("--health-port")
            .arg(ports.health_port().to_string())
            .arg(command);

        for arg in args {
            cmd.arg(arg);
        }

        cmd.stderr(Stdio::piped());
        cmd.stdout(Stdio::piped());

        cmd.spawn()
    }

    /// Try to wait for the forwarder to be ready by attempting to connect to the health port.
    /// Returns an error if the forwarder doesn’t become ready in time.
    fn try_wait_for_ready(&self) -> Result<(), anyhow::Error> {
        const NUM_ATTEMPTS: usize = 30;
        let mut last_error = None;

        for attempt in 0..NUM_ATTEMPTS {
            match TcpStream::connect_timeout(
                &format!("127.0.0.1:{}", self.ports.health_port())
                    .parse()
                    .map_err(|error| {
                        anyhow::anyhow!("Failed to parse health port address: {error}")
                    })?,
                Duration::from_millis(100),
            ) {
                Ok(_) => return Ok(()),
                Err(error) => {
                    last_error = Some(error);
                    if attempt < NUM_ATTEMPTS - 1 {
                        thread::sleep(Duration::from_millis(50));
                    }
                }
            }
        }

        let last_error =
            last_error.map_or_else(|| "Unknown error".to_owned(), |error| format!("{error}"));
        Err(anyhow::anyhow!(
            "Forwarder did not become ready in time on port {}. Last error: {last_error}",
            self.ports.health_port(),
        ))
    }

    /// Connect to the protocol port.
    fn connect_protocol(&self) -> Result<TcpStream, anyhow::Error> {
        Self::connect_with_retry(self.ports.protocol_port(), "protocol")
    }

    /// Connect to the `stderr` port.
    fn connect_stderr(&self) -> Result<TcpStream, anyhow::Error> {
        Self::connect_with_retry(self.ports.stderr_port(), "stderr")
    }

    /// Connect to the health port.
    fn connect_health(&self) -> Result<TcpStream, anyhow::Error> {
        Self::connect_with_retry(self.ports.health_port(), "health")
    }

    /// Connect to a port with retries.
    fn connect_with_retry(port: u16, label: &str) -> Result<TcpStream, anyhow::Error> {
        const NUM_ATTEMPTS: usize = 20;
        for attempt in 0..NUM_ATTEMPTS {
            match TcpStream::connect(("127.0.0.1", port)) {
                Ok(stream) => return Ok(stream),
                Err(error) if attempt == NUM_ATTEMPTS - 1 => {
                    return Err(anyhow::anyhow!(
                        "Failed to connect to {label} port {port} after {NUM_ATTEMPTS} attempts: {error}"
                    ));
                }
                Err(_) => {
                    thread::sleep(Duration::from_millis(50));
                }
            }
        }
        unreachable!()
    }

    /// Wait for the forwarder process to exit.
    fn wait_for_exit(&mut self) -> bool {
        const NUM_ATTEMPTS: usize = 50;
        for _ in 0..NUM_ATTEMPTS {
            if let Ok(Some(_)) = self.process.try_wait() {
                return true;
            }
            thread::sleep(Duration::from_millis(100));
        }
        false
    }
}

impl Drop for TestForwarder {
    fn drop(&mut self) {
        // Clean up: kill the process if it’s still running.
        drop(self.process.kill());
        drop(self.process.wait());
    }
}

/// Helper function to read from a stream with a timeout.
fn read_with_timeout(stream: &mut TcpStream, buffer: &mut [u8]) -> io::Result<usize> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.read(buffer)
}

/// Helper function to read all available data from a stream up to a timeout.
fn read_all_available(stream: &mut TcpStream, timeout: Duration) -> Result<Vec<u8>, anyhow::Error> {
    let mut result = Vec::new();
    let mut buffer = [0_u8; 8192];
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|error| anyhow::anyhow!("Failed to set read timeout: {error}"))?;

    loop {
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => result.extend_from_slice(buffer.get(..n).unwrap_or_default()),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
            Err(error) if error.kind() == io::ErrorKind::TimedOut => break,
            Err(_) => break,
        }
    }

    Ok(result)
}

// ============================================================================
// ACCEPTANCE CRITERIA TESTS
// ============================================================================

#[test]
fn test_forwarder_starts_arbitrary_child_process() -> Result<(), anyhow::Error> {
    // * [x] A standalone forwarder executable can be started that launches an arbitrary child process.

    let (cmd, args) = sleep_cmd(5);
    let args_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let forwarder = TestForwarder::start(cmd, &args_refs)?;

    // If we got here, the forwarder started successfully.
    // The forwarder should be ready (health port should be accessible).
    forwarder.connect_health()?.peer_addr()?;
    Ok(())
}

#[test]
fn test_forwarder_passes_arguments_unchanged() -> Result<(), anyhow::Error> {
    // * [x] The forwarder passes command-line arguments through to the child process unchanged.

    // Use a command that outputs arguments and then waits, so we have time to connect.
    let (cmd, args) = echo_args_cmd(&["-n", "test", "with spaces", "--flag"]);
    let args_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let forwarder = TestForwarder::start(cmd, &args_refs)?;

    let mut stream = forwarder.connect_protocol()?;
    let output = read_all_available(&mut stream, Duration::from_millis(500))?;

    // `bash` should output all the arguments.
    let output_str = String::from_utf8_lossy(&output);
    assert!(output_str.contains("test"));
    assert!(output_str.contains("with spaces"));
    assert!(output_str.contains("--flag"));
    Ok(())
}

#[test]
fn test_child_process_configurable_externally() -> Result<(), anyhow::Error> {
    // * [x] The child process to launch can be configured externally.

    // Test with different commands to verify external configuration works.
    let (cmd1, args1) = echo_with_sleep_cmd("first", 5);
    let args1_refs: Vec<&str> = args1.iter().map(String::as_str).collect();
    let forwarder1 = TestForwarder::start(cmd1, &args1_refs)?;
    let mut stream1 = forwarder1.connect_protocol()?;
    let output1 = read_all_available(&mut stream1, Duration::from_millis(500))?;
    assert!(String::from_utf8_lossy(&output1).contains("first"));

    let (cmd2, args2) = echo_with_sleep_cmd("second", 5);
    let args2_refs: Vec<&str> = args2.iter().map(String::as_str).collect();
    let forwarder2 = TestForwarder::start(cmd2, &args2_refs)?;
    let mut stream2 = forwarder2.connect_protocol()?;
    let output2 = read_all_available(&mut stream2, Duration::from_millis(500))?;
    assert!(String::from_utf8_lossy(&output2).contains("second"));
    Ok(())
}

#[test]
fn test_forwarder_exits_when_child_exits() -> Result<(), anyhow::Error> {
    // * [x] When the child process exits for any reason, the forwarder also terminates.

    // Use a command that runs briefly and then exits.
    let (cmd, args) = short_lived_cmd("test", 0);
    let args_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut forwarder = TestForwarder::start(cmd, &args_refs)?;

    // Connect to protocol port to ensure we’re monitoring the forwarder.
    let _stream = forwarder.connect_protocol();

    // Wait for the child to exit (should happen after ~0.5s).
    thread::sleep(Duration::from_millis(600));

    // Forwarder should have exited by now.
    assert!(
        forwarder.wait_for_exit(),
        "Forwarder should exit when child exits"
    );
    Ok(())
}

#[test]
fn test_forwarder_exposes_three_tcp_ports() -> Result<(), anyhow::Error> {
    // * [x] The forwarder exposes three TCP ports:
    //   * [x] a **protocol port**
    //   * [x] an **stderr port**
    //   * [x] a **health port**

    let (cmd, args) = sleep_cmd(10);
    let args_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let forwarder = TestForwarder::start(cmd, &args_refs)?;

    // Verify all three ports are accessible.
    // Note: The `connect_*()` methods already return an `Err` if connection fails (which
    //       gets propagated), so the real accessibility check happens *there*. The
    //       `.peer_addr().is_ok()` is redundant but serves as documentation.
    assert!(
        forwarder.connect_protocol()?.peer_addr().is_ok(),
        "Protocol port should be accessible"
    );
    assert!(
        forwarder.connect_stderr()?.peer_addr().is_ok(),
        "Stderr port should be accessible"
    );
    assert!(
        forwarder.connect_health()?.peer_addr().is_ok(),
        "Health port should be accessible"
    );
    Ok(())
}

#[test]
fn test_default_port_values() -> Result<(), anyhow::Error> {
    // * [x] The default port values are:
    //   * [x] `7000` for the protocol port
    //   * [x] `7001` for the stderr port
    //   * [x] `7002` for the health port
    const NUM_ATTEMPTS: usize = 30;

    // Test setup: Ensure default ports are available.
    // If they’re not, this is an environment issue, not a test failure.
    for port in 7000_u16..=7002_u16 {
        let error_message = format!(
            "TEST SETUP FAILED: Default port {port} is not available. This is an environment issue, not a test failure."
        );
        drop(
            TcpListener::bind(format!("127.0.0.1:{port}"))
                .map_err(|error| anyhow::anyhow!("{error_message}: {error}"))?,
        );
        // Port is immediately released here.
    }

    // Launch `stdioxide` *without* specifying ports to verify it uses the defaults.
    let bin_path =
        env::var("CARGO_BIN_EXE_stdioxide").unwrap_or_else(|_| "target/debug/stdioxide".to_owned());

    let (sleep_command, sleep_args) = sleep_cmd(10);
    let mut cmd = Command::new(&bin_path);
    cmd.arg(sleep_command);
    for arg in sleep_args {
        cmd.arg(arg);
    }
    cmd.stderr(Stdio::piped());
    cmd.stdout(Stdio::piped());

    let mut process = cmd
        .spawn()
        .map_err(|error| anyhow::anyhow!("Failed to start stdioxide: {error}"))?;

    // Wait for the forwarder to be ready by connecting to the default health port.
    let mut connected = false;
    for _ in 0..NUM_ATTEMPTS {
        if TcpStream::connect_timeout(&"127.0.0.1:7002".parse()?, Duration::from_millis(100))
            .is_ok()
        {
            connected = true;
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    assert!(
        connected,
        "Forwarder should be ready on default health port 7002"
    );

    // Verify we can connect to all three default ports.
    for port in 7000_u16..=7002_u16 {
        assert!(
            TcpStream::connect(format!("127.0.0.1:{port}")).is_ok(),
            "Should connect to default port {port}"
        );
    }

    // Clean up.
    drop(process.kill());
    drop(process.wait());
    Ok(())
}

#[test]
fn test_port_override_via_environment_variables() -> Result<(), anyhow::Error> {
    // * [x] All three port numbers can be overridden via environment variables.
    const NUM_ATTEMPTS: usize = 30;

    // Allocate unique ports to avoid conflicts.
    let ports = AllocatedPorts::new()?;
    let custom_protocol = ports.protocol_port();
    let custom_stderr = ports.stderr_port();
    let custom_health = ports.health_port();

    // Launch `stdioxide` with environment variables (NOT command-line args) to test env var override.
    let bin_path =
        env::var("CARGO_BIN_EXE_stdioxide").unwrap_or_else(|_| "target/debug/stdioxide".to_owned());

    let (sleep_command, sleep_args) = sleep_cmd(10);
    let mut cmd = Command::new(&bin_path);
    cmd.env("STDIOXIDE_PROTOCOL_PORT", custom_protocol.to_string())
        .env("STDIOXIDE_STDERR_PORT", custom_stderr.to_string())
        .env("STDIOXIDE_HEALTH_PORT", custom_health.to_string())
        .arg(sleep_command);
    for arg in sleep_args {
        cmd.arg(arg);
    }
    cmd.stderr(Stdio::piped()).stdout(Stdio::piped());

    let mut process = cmd
        .spawn()
        .map_err(|error| anyhow::anyhow!("Failed to start stdioxide: {error}"))?;

    // Wait for the forwarder to be ready by connecting to the custom health port.
    let mut connected = false;
    for _ in 0..NUM_ATTEMPTS {
        if TcpStream::connect_timeout(
            &format!("127.0.0.1:{custom_health}").parse()?,
            Duration::from_millis(100),
        )
        .is_ok()
        {
            connected = true;
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    assert!(
        connected,
        "Forwarder should be ready on custom health port {custom_health}",
    );

    // Verify we can connect to all three custom ports.
    assert!(
        TcpStream::connect(("127.0.0.1", custom_protocol)).is_ok(),
        "Should connect to custom protocol port {custom_protocol}",
    );
    assert!(
        TcpStream::connect(("127.0.0.1", custom_stderr)).is_ok(),
        "Should connect to custom stderr port {custom_stderr}",
    );
    assert!(
        TcpStream::connect(("127.0.0.1", custom_health)).is_ok(),
        "Should connect to custom health port {custom_health}",
    );

    // Clean up.
    drop(process.kill());
    drop(process.wait());
    Ok(())
}

#[test]
fn test_stdout_sent_over_protocol_port() -> Result<(), anyhow::Error> {
    // * [x] The forwarder sends the child process’s `stdout` stream over the protocol port.

    let (cmd, args) = echo_with_sleep_cmd("Hello from stdout", 5);
    let args_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let forwarder = TestForwarder::start(cmd, &args_refs)?;

    let mut stream = forwarder.connect_protocol()?;
    let output = read_all_available(&mut stream, Duration::from_millis(500))?;
    let output_str = String::from_utf8_lossy(&output);

    assert!(output_str.contains("Hello from stdout"));
    Ok(())
}

#[test]
fn test_stdin_received_on_protocol_port() -> Result<(), anyhow::Error> {
    // * [x] The forwarder receives input for the child process’s `stdin` stream on the protocol port.
    // * [x] Data received on the protocol port is forwarded to the child process’s `stdin` while the connection is active.

    let (cmd, args) = cat_cmd();
    let args_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let forwarder = TestForwarder::start(cmd, &args_refs)?;

    let mut stream = forwarder.connect_protocol()?;

    // Send data to `stdin` via the protocol port.
    stream
        .write_all(b"test input\n")
        .map_err(|error| anyhow::anyhow!("Failed to write to protocol port: {error}"))?;
    stream
        .flush()
        .map_err(|error| anyhow::anyhow!("Failed to flush protocol port: {error}"))?;

    // Read back the echoed output from `stdout`.
    // Increased timeout to account for PowerShell startup on Windows
    let output = read_all_available(&mut stream, Duration::from_millis(1500))?;
    let output_str = String::from_utf8_lossy(&output);

    assert!(output_str.contains("test input"));
    Ok(())
}

#[test]
fn test_stderr_sent_over_stderr_port() -> Result<(), anyhow::Error> {
    // * [x] The forwarder sends the child process’s `stderr` stream over the `stderr` port.

    // Use a `bash` command that writes to `stderr` and then waits.
    let (cmd, args) = stderr_echo_with_sleep_cmd("error message", 5);
    let args_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let forwarder = TestForwarder::start(cmd, &args_refs)?;
    let mut stream = forwarder.connect_stderr()?;
    let output = read_all_available(&mut stream, Duration::from_millis(500))?;
    let output_str = String::from_utf8_lossy(&output);

    assert!(output_str.contains("error message"));
    Ok(())
}

#[test]
fn test_protocol_port_single_client_only() -> Result<(), anyhow::Error> {
    // * [x] The protocol port allows at most one active client connection at a time.

    let (cmd, args) = loop_stdin_to_stdout_cmd();
    let args_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let forwarder = TestForwarder::start(cmd, &args_refs)?;

    // First client connects successfully.
    let mut stream1 = forwarder.connect_protocol()?;

    // Verify first client works.
    stream1
        .write_all(b"test\n")
        .map_err(|error| anyhow::anyhow!("Failed to write to protocol port: {error}"))?;
    stream1
        .flush()
        .map_err(|error| anyhow::anyhow!("Failed to flush protocol port: {error}"))?;
    // Increased timeout to account for PowerShell startup and buffering on Windows
    let output = read_all_available(&mut stream1, Duration::from_millis(1500))?;
    assert!(String::from_utf8_lossy(&output).contains("response"));

    // Second client connection attempt.
    // The protocol server only calls `accept()` once, so the second connection
    // succeeds at the TCP level (queued in backlog) but is never accepted/served.
    let mut stream2 = TcpStream::connect_timeout(
        &format!("127.0.0.1:{}", forwarder.ports.protocol_port()).parse()?,
        Duration::from_millis(200),
    )
    .map_err(|error| {
        anyhow::anyhow!(
            "Second client should connect successfully (TCP handshake completes): {error}"
        )
    })?;

    // The connection is established but never served--reading should timeout.
    stream2
        .set_read_timeout(Some(Duration::from_millis(200)))
        .map_err(|error| anyhow::anyhow!("Should set read timeout: {error}"))?;

    let mut buf = [0_u8; 100];
    let result = stream2.read(&mut buf);

    if let Err(error) = &result {
        assert!(
            error.kind() == io::ErrorKind::WouldBlock || error.kind() == io::ErrorKind::TimedOut,
            "Second client should timeout reading (connection never served by protocol server)"
        );
        Ok(())
    } else {
        anyhow::bail!(
            "Second client should not receive data (connection never served by protocol server)"
        );
    }
}

#[test]
fn test_stderr_port_single_client_only() -> Result<(), anyhow::Error> {
    // * [x] The `stderr` port allows at most one active client connection at a time.

    let (cmd, args) = continuous_stderr_loop_cmd();
    let args_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let forwarder = TestForwarder::start(cmd, &args_refs)?;

    // First client connects successfully.
    let _stream1 = forwarder.connect_stderr()?;

    // Second client should connect but be rejected.
    // According to the `stderr_server` implementation, it rejects additional connections.
    let stream2 = forwarder.connect_stderr()?;

    // The second connection is made but immediately closed/rejected.
    // Try to read--should get no data or connection closed.
    let output = read_all_available(
        &mut stream2
            .try_clone()
            .map_err(|error| anyhow::anyhow!("Failed to clone stream: {error}"))?,
        Duration::from_millis(500),
    )?;

    // The second client should not receive meaningful data since the first is still active.
    // In practice, the connection is accepted but dropped, so we should see minimal/no data.
    // This test verifies the single-client behavior.
    assert!(
        output.is_empty() || output.len() < 100,
        "Second stderr client should not receive full stream data while first client is active"
    );
    Ok(())
}

#[test]
fn test_health_port_multiple_clients() -> Result<(), anyhow::Error> {
    // * [x] The health port allows multiple simultaneous client connections.

    let (cmd, args) = sleep_cmd(10);
    let args_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let forwarder = TestForwarder::start(cmd, &args_refs)?;

    // Connect multiple clients to the health port.
    let stream1 = forwarder.connect_health()?;
    let stream2 = forwarder.connect_health()?;
    let stream3 = forwarder.connect_health()?;

    // All connections should succeed.
    assert!(
        stream1.peer_addr().is_ok() && stream2.peer_addr().is_ok() && stream3.peer_addr().is_ok()
    );
    Ok(())
}

#[test]
fn test_protocol_port_buffered_stdout_replay() -> Result<(), anyhow::Error> {
    // * [x] When a client connects to the protocol port for the first time, it first receives all buffered `stdout` data
    //       produced before the connection was established.
    // * [x] After the buffered `stdout` data has been sent, the client continues to receive newly produced `stdout` data
    //       in real time.

    // Use a script that produces output immediately and then waits.
    let (cmd, args) = multi_echo_stdout_cmd("buffered output", 1.0, "realtime output", 10);
    let args_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let forwarder = TestForwarder::start(cmd, &args_refs)?;

    // Now connect - buffering ensures we receive output produced before connection.
    thread::sleep(Duration::from_millis(100));

    // Connect and we should receive the buffered output first.
    let mut stream = forwarder.connect_protocol()?;

    // Read the output.
    let read_timeout = Duration::from_secs(5);
    let output = read_all_available(&mut stream, read_timeout)?;
    let output_str = String::from_utf8_lossy(&output);

    // Verify we got both buffered and realtime output.
    assert!(output_str.contains("buffered output"));
    assert!(output_str.contains("realtime output"));
    Ok(())
}

#[test]
fn test_protocol_disconnect_kills_child() -> Result<(), anyhow::Error> {
    // * [x] When a client disconnects from the protocol port, the child process is killed and the forwarder terminates.

    let (cmd, args) = sleep_cmd(100);
    let args_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut forwarder = TestForwarder::start(cmd, &args_refs)?;

    {
        let _stream = forwarder.connect_protocol();
        // Disconnect by dropping the stream.
    }

    // Forwarder should terminate.
    assert!(
        forwarder.wait_for_exit(),
        "Forwarder should exit when protocol client disconnects"
    );
    Ok(())
}

#[test]
fn test_stderr_port_buffered_stderr_replay() -> Result<(), anyhow::Error> {
    // * [x] When a client connects to the `stderr` port, it first receives all buffered `stderr` data
    //       produced before the connection was established.
    // * [x] After the buffered `stderr` data has been sent, the client continues to receive newly produced
    //       `stderr` data in real time.

    // Use a script that produces `stderr` immediately and then waits.
    let (cmd, args) = multi_echo_stderr_cmd("buffered error", 1.0, "realtime error", 10);
    let args_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let forwarder = TestForwarder::start(cmd, &args_refs)?;

    // Now connect to `stderr`--buffering ensures we receive output produced before connection.
    thread::sleep(Duration::from_millis(100));

    // Connect and we should receive the buffered output first.
    let mut stream = forwarder.connect_stderr()?;

    // Read the output.
    let read_timeout = Duration::from_secs(5);
    let output = read_all_available(&mut stream, read_timeout)?;
    let output_str = String::from_utf8_lossy(&output);

    // Verify we got both buffered and realtime output.
    assert!(output_str.contains("buffered error"));
    assert!(output_str.contains("realtime error"));
    Ok(())
}

#[test]
fn test_stderr_disconnect_does_not_kill_child() -> Result<(), anyhow::Error> {
    // * [x] When a client disconnects from the `stderr` port, neither the forwarder nor the child
    //       process terminate because of that.

    let (cmd, args) = sleep_cmd(10);
    let args_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let forwarder = TestForwarder::start(cmd, &args_refs)?;
    {
        let _stderr_stream = forwarder.connect_stderr();
        // Disconnect by dropping the stream.
    }

    // With proactive disconnect detection, disconnect is detected immediately.
    thread::sleep(Duration::from_millis(100));

    // Forwarder should still be running--we can connect to health port.
    assert!(
        forwarder.connect_health()?.peer_addr().is_ok(),
        "Forwarder should still be running after stderr disconnect"
    );
    Ok(())
}

#[test]
fn test_stderr_port_reconnect_continues_from_current_state() -> Result<(), anyhow::Error> {
    // * [x] When a client connects to the `stderr` port, it first receives all buffered `stderr` data
    //       produced before the connection was established and after a previous connection was active
    //       (i.e., no logging data is lost).

    // Use a script that outputs at controlled times:
    // - "before_connection" immediately (buffered before any connection)
    // - "during_first_connection" after 0.5 seconds (sent to first client)
    // - "trigger_disconnect" after 1.5 seconds (triggers disconnect detection)
    // - "while_disconnected" after 3 seconds (buffered while no client connected)
    // - "during_second_connection" after 5 seconds (sent to second client)
    let (cmd, args) = complex_stderr_reconnect_cmd();
    let args_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let forwarder = TestForwarder::start(cmd, &args_refs)?;

    // Wait to ensure "before_connection" is buffered.
    thread::sleep(Duration::from_millis(100));

    // First connection--connect, read initial data, then disconnect BEFORE "trigger_disconnect".
    {
        let mut stream = forwarder.connect_stderr()?;
        // Read for 800ms to get "before_connection" (immediate) and "during_first_connection" (at t=0.5s)
        let output = read_all_available(&mut stream, Duration::from_millis(800))?;
        let output_str = String::from_utf8_lossy(&output);
        assert!(output_str.contains("before_connection"));
        assert!(output_str.contains("during_first_connection"));
        // Disconnect now (at ~t=1.0s), before "trigger_disconnect" (at t=1.5s)
        drop(stream.shutdown(Shutdown::Both));
        drop(stream);
        assert!(
            !output_str.contains("trigger_disconnect"),
            "Should not receive 'trigger_disconnect' in first connection"
        );
    }

    // Now we’re at ~t=1.0s. "trigger_disconnect" will be produced at t=1.5s, which will
    // cause the server to try writing to the disconnected client and detect the disconnect.
    // With proactive disconnect detection, disconnect is detected quickly.
    // Wait for "while_disconnected" to be produced (at t=3s from start).
    thread::sleep(Duration::from_millis(2300));

    // Second connection--should receive all buffered data (trigger_disconnect, while_disconnected)
    // and realtime data (during_second_connection). No logging data must be lost.
    {
        let mut stream = forwarder.connect_stderr()?;
        // Read for 2.5s to get buffered and realtime data
        let output = read_all_available(&mut stream, Duration::from_millis(2500))?;
        let output_str = String::from_utf8_lossy(&output);

        assert!(
            output_str.contains("trigger_disconnect"),
            "Second client should receive 'trigger_disconnect' (buffered during disconnect), got: {output_str}",
        );

        assert!(
            output_str.contains("while_disconnected"),
            "Second client should receive 'while_disconnected' (buffered during disconnect), got: {output_str}",
        );

        assert!(
            output_str.contains("during_second_connection"),
            "Second client should receive realtime data, got: {output_str}",
        );
    }
    Ok(())
}

#[test]
fn test_output_buffering_prevents_data_loss() -> Result<(), anyhow::Error> {
    // * [x] Output buffering must prevent loss of `stdout` and `stderr` data when no client is connected yet.

    // Start a process that produces output immediately.
    let (cmd, args) = combined_output_cmd("stdout message", "stderr message", 10);
    let args_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let forwarder = TestForwarder::start(cmd, &args_refs)?;

    // Buffering ensures output is captured even if we connect immediately.
    thread::sleep(Duration::from_millis(100));

    // Now connect--we should receive the buffered output.
    let mut stdout_stream = forwarder.connect_protocol()?;
    let stdout_data = read_all_available(&mut stdout_stream, Duration::from_millis(500))?;
    let stdout_str = String::from_utf8_lossy(&stdout_data);

    let mut stderr_stream = forwarder.connect_stderr()?;
    let stderr_data = read_all_available(&mut stderr_stream, Duration::from_millis(500))?;
    let stderr_str = String::from_utf8_lossy(&stderr_data);

    assert!(stdout_str.contains("stdout message"));
    assert!(stderr_str.contains("stderr message"));
    Ok(())
}

#[test]
fn test_health_port_indicates_readiness() -> Result<(), anyhow::Error> {
    // * [x] A successful TCP connection to the health port indicates that the forwarder is ready to accept connections and operate normally.

    let (cmd, args) = sleep_cmd(10);
    let args_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let forwarder = TestForwarder::start(cmd, &args_refs)?;

    // If we can connect to health port, the forwarder is ready.
    let health_stream = forwarder.connect_health()?;
    health_stream.peer_addr()?;

    // And the other ports should also be accessible.
    forwarder.connect_protocol()?.peer_addr()?;
    forwarder.connect_stderr()?.peer_addr()?;
    Ok(())
}

#[test]
fn test_health_checks_do_not_interfere() -> Result<(), anyhow::Error> {
    // * [x] Health checks on the health port must not interfere with the behavior of the protocol port or the `stderr` port.

    // Use a process that produces high-volume output on both `stdout` and `stderr`.
    // Output a unique numbered line every 10ms for 3 seconds (300 lines on each stream).
    let (cmd, args) = numbered_output_loop_cmd(300, 10);
    let args_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let forwarder = TestForwarder::start(cmd, &args_refs)?;

    // Spawn a thread to continuously perform health checks for 3.5 seconds.
    let health_port = forwarder.ports.health_port();
    let health_check_handle = thread::spawn(move || {
        let start = Instant::now();
        let mut check_count = 0_usize;
        while start.elapsed() < Duration::from_millis(3500) {
            if TcpStream::connect(("127.0.0.1", health_port)).is_ok() {
                check_count += 1_usize;
            }
            thread::sleep(Duration::from_millis(20));
        }
        check_count
    });

    // Spawn a thread to read from the protocol port (`stdout`).
    let mut protocol_stream = forwarder.connect_protocol()?;
    let protocol_handle = thread::spawn(move || {
        let mut all_output = Vec::new();
        let mut buffer = [0_u8; 8192];
        protocol_stream
            .set_read_timeout(Some(Duration::from_secs(4)))
            .ok();

        loop {
            match protocol_stream.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => all_output.extend_from_slice(buffer.get(..n).unwrap_or_default()),
                Err(error) if error.kind() == io::ErrorKind::TimedOut => break,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(_) => break,
            }
        }
        all_output
    });

    // Spawn a thread to read from the stderr port (`stderr`).
    let mut stderr_stream = forwarder.connect_stderr()?;
    let stderr_handle = thread::spawn(move || {
        let mut all_output = Vec::new();
        let mut buffer = [0_u8; 8192];
        stderr_stream
            .set_read_timeout(Some(Duration::from_secs(4)))
            .ok();

        loop {
            match stderr_stream.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => all_output.extend_from_slice(buffer.get(..n).unwrap_or_default()),
                Err(error) if error.kind() == io::ErrorKind::TimedOut => break,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(_) => break,
            }
        }
        all_output
    });

    // Wait for all threads to complete.
    let health_check_count = health_check_handle
        .join()
        .map_err(|error| anyhow::anyhow!("Health check thread panicked: {error:?}"))?;
    let protocol_output = protocol_handle
        .join()
        .map_err(|error| anyhow::anyhow!("Protocol thread panicked: {error:?}"))?;
    let stderr_output = stderr_handle
        .join()
        .map_err(|error| anyhow::anyhow!("Stderr thread panicked: {error:?}"))?;

    // Verify that health checks were performed successfully.
    // Note: The count varies significantly by platform (Linux/Windows: ~150+, macOS: ~35-40)
    // due to differences in TCP connection establishment speed. We just verify that
    // a reasonable number of health checks occurred without interfering with data transfer.
    assert!(
        health_check_count > 20,
        "Should have performed multiple health checks (got {health_check_count})",
    );

    // Verify that we received substantial data on both ports despite constant health checks.
    let protocol_str = String::from_utf8_lossy(&protocol_output);
    let stderr_str = String::from_utf8_lossy(&stderr_output);

    // Should have received most of the lines (allowing for some buffering delays at the end).
    let protocol_line_count = protocol_str.matches("stdout_line_").count();
    let stderr_line_count = stderr_str.matches("stderr_line_").count();

    assert!(
        protocol_line_count >= 250,
        "Should have received most stdout lines despite health checks (got {protocol_line_count})",
    );

    assert!(
        stderr_line_count >= 250,
        "Should have received most stderr lines despite health checks (got {stderr_line_count})",
    );

    // Verify data integrity: check for a few specific lines.
    assert!(protocol_str.contains("stdout_line_1"));
    assert!(protocol_str.contains("stdout_line_100"));
    assert!(stderr_str.contains("stderr_line_1"));
    assert!(stderr_str.contains("stderr_line_100"));

    Ok(())
}

#[test]
fn test_works_with_various_executables() -> Result<(), anyhow::Error> {
    // * [x] The forwarder must work not only for Python applications, but for any executable child process.

    // Test with various common executables.

    // Test with `echo` via shell command.
    {
        let (cmd, args) = echo_with_sleep_cmd("test1", 2);
        let args_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let forwarder = TestForwarder::start(cmd, &args_refs)?;
        let mut stream = forwarder.connect_protocol()?;
        let output = read_all_available(&mut stream, Duration::from_millis(500))?;
        assert!(String::from_utf8_lossy(&output).contains("test1"));
    }

    // Test with another echo.
    {
        let (cmd, args) = echo_with_sleep_cmd("test2", 2);
        let args_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let forwarder = TestForwarder::start(cmd, &args_refs)?;
        let mut stream = forwarder.connect_protocol()?;
        let output = read_all_available(&mut stream, Duration::from_millis(500))?;
        assert!(String::from_utf8_lossy(&output).contains("test2"));
    }

    // Test with `cat` (interactive).
    {
        let (cmd, args) = cat_cmd();
        let args_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let forwarder = TestForwarder::start(cmd, &args_refs)?;
        let mut stream = forwarder.connect_protocol()?;
        stream
            .write_all(b"test3\n")
            .map_err(|error| anyhow::anyhow!("Failed to write to protocol port: {error}"))?;
        stream
            .flush()
            .map_err(|error| anyhow::anyhow!("Failed to flush protocol port: {error}"))?;
        let output = read_all_available(&mut stream, Duration::from_millis(500))?;
        assert!(String::from_utf8_lossy(&output).contains("test3"));
    }

    // Test with a Python script (if available).
    // Note: Python startup can be slow on Windows, so we use a longer timeout.
    {
        let python = python_cmd();
        let forwarder = TestForwarder::start(
            python,
            &["-u", "-c", "import time; print('test4'); time.sleep(2)"],
        )?;
        let mut stream = forwarder.connect_protocol()?;

        let read_timeout = Duration::from_secs(5); // Python startup (especially slow on Windows).
        let output = read_all_available(&mut stream, read_timeout)?;
        assert!(
            String::from_utf8_lossy(&output).contains("test4"),
            "Expected 'test4' in output, got: {:?}",
            String::from_utf8_lossy(&output)
        );
    }

    Ok(())
}

#[test]
fn test_large_output_buffering() -> Result<(), anyhow::Error> {
    // Additional test: verify that large outputs are buffered correctly.

    // Generate a large output.
    let large_size = 100_000;
    let (cmd, args) = generate_large_output_cmd(large_size);
    let args_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let forwarder = TestForwarder::start(cmd, &args_refs)?;

    // Wait for output to be generated.
    thread::sleep(Duration::from_millis(200));

    // Connect and read the buffered output.
    let mut stream = forwarder.connect_protocol()?;
    let mut total_read = 0_usize;
    let mut buffer = [0_u8; 8192];

    while total_read < large_size {
        match read_with_timeout(&mut stream, &mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(n) => total_read += n,
        }
    }

    assert!(
        total_read >= large_size,
        "Should have read at least {large_size} bytes, got {total_read}",
    );
    Ok(())
}

#[test]
fn test_concurrent_stdin_stdout_bidirectional() -> Result<(), anyhow::Error> {
    // Additional test: verify bidirectional communication works correctly.

    // Use `cat` which echoes `stdin` to `stdout`.
    let (cmd, args) = cat_cmd();
    let args_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let forwarder = TestForwarder::start(cmd, &args_refs)?;
    let mut stream = forwarder.connect_protocol()?;

    // Send multiple lines and verify echo.
    for i in 0_usize..5_usize {
        let message = format!("line {i}\n");
        stream
            .write_all(message.as_bytes())
            .map_err(|error| anyhow::anyhow!("Failed to write to protocol port: {error}"))?;
        stream
            .flush()
            .map_err(|error| anyhow::anyhow!("Failed to flush protocol port: {error}"))?;

        // Increased timeout for Python startup on Windows.
        let read_timeout = Duration::from_secs(5);
        let output = read_all_available(&mut stream, read_timeout)?;
        let output_str = String::from_utf8_lossy(&output);
        assert!(
            output_str.contains(&format!("line {i}")),
            "Expected 'line {i}' in output, got: {output_str:?}"
        );
    }

    Ok(())
}

// ============================================================================
// LSP INTEGRATION TEST
// ============================================================================

mod lsp_client;

#[test]
fn test_lsp_rust_analyzer_integration() -> Result<(), anyhow::Error> {
    // Test that stdioxide can successfully tunnel LSP communication with `rust-analyzer`.
    // This validates the real-world use case of running a language server through the forwarder.

    let mut forwarder = TestForwarder::start("rust-analyzer", &[])?;
    let stream = forwarder.connect_protocol()?;

    let mut lsp = LspClient::new(stream)?;

    // Initialize the LSP server.
    let workspace_path = env::current_dir()
        .map_err(|error| anyhow::anyhow!("Failed to get current directory: {error}"))?
        .to_string_lossy()
        .to_string();
    let root_uri = format!("file://{workspace_path}");

    let init_response = lsp.initialize(&root_uri)?;

    assert_eq!(
        init_response
            .get("jsonrpc")
            .ok_or_else(|| anyhow::anyhow!("Expected 'jsonrpc' field in initialize response"))?,
        "2.0"
    );
    assert!(
        init_response
            .get("result")
            .and_then(|result| result.get("capabilities"))
            .is_some_and(serde_json::Value::is_object),
        "Should receive server capabilities"
    );

    // Send initialized notification.
    lsp.initialized();

    // Open a document (src/main.rs).
    let main_rs_path = format!("{workspace_path}/src/main.rs");
    let main_rs_uri = format!("file://{main_rs_path}");
    let main_rs_content = read_to_string(&main_rs_path)
        .map_err(|error| anyhow::anyhow!("Failed to read src/main.rs: {error}"))?;

    lsp.did_open(&main_rs_uri, "rust", &main_rs_content);

    // Request document symbols.
    let symbols_response = lsp.document_symbol(&main_rs_uri)?;

    assert_eq!(
        symbols_response
            .get("jsonrpc")
            .ok_or_else(|| anyhow::anyhow!(
                "Expected 'jsonrpc' field in document symbol response"
            ))?,
        "2.0"
    );

    // Verify we got some symbols (src/main.rs should have at least the main function).
    let symbols = symbols_response
        .get("result")
        .and_then(|result| result.as_array())
        .ok_or_else(|| anyhow::anyhow!("Expected array of symbols"))?;

    assert!(
        !symbols.is_empty(),
        "Should have received symbols for src/main.rs"
    );

    // Verify at least one symbol has a name (e.g., "main").
    let has_named_symbol = symbols.iter().any(|sym| sym["name"].is_string());
    assert!(
        has_named_symbol,
        "Should have at least one named symbol in src/main.rs"
    );

    // Shutdown the LSP server.
    let shutdown_response = lsp.shutdown()?;
    assert_eq!(
        shutdown_response
            .get("result")
            .ok_or_else(|| anyhow::anyhow!("Expected 'result' field in shutdown response"))?,
        &serde_json::Value::Null
    );

    // Exit notification is sent automatically when lsp is dropped.
    drop(lsp);

    // Forwarder should terminate after LSP client disconnects from protocol port
    // or because the LSP server process exits on shutdown (either reason is okay).
    assert!(
        forwarder.wait_for_exit(),
        "Forwarder should exit after LSP client disconnects from protocol port"
    );
    Ok(())
}
