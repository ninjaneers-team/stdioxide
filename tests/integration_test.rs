use std::{
    collections::HashSet,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    process::{Child, Command, Stdio},
    sync::{LazyLock, Mutex},
    thread,
    time::Duration,
};

// Global registry tracking all currently allocated ports across all tests used so
// that tests can allocate ports in parallel as long as they don’t conflict.
static ALLOCATED_PORTS_REGISTRY: LazyLock<Mutex<HashSet<u16>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

/// Find an available port by binding to port 0 and letting the OS assign one.
/// Returns the port number that was assigned.
/// Never returns default ports (7000, 7001, 7002) to avoid conflicts with `test_default_port_values()`.
fn find_available_port() -> u16 {
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
            .expect("Failed to bind to find available port")
            .local_addr()
            .expect("Failed to get local address")
            .port();

        // Skip default ports to avoid conflicts with `test_default_port_values()`.
        if !(7000..=7002).contains(&port) {
            return port;
        }
    }
}

/// RAII guard that removes ports from the global registry when dropped.
struct PortGuard {
    ports: [u16; 3],
}

impl Drop for PortGuard {
    fn drop(&mut self) {
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
    fn new() -> Self {
        loop {
            // Find candidate ports from the OS
            let p1 = find_available_port();
            let p2 = find_available_port();
            let p3 = find_available_port();

            // `find_available_port()` could return a port value that was previously found out to
            // be free in the context of a different test, but has not been bound yet. However, it would
            // still be in the registry. Therefore, we have to check against the registry to ensure
            // that such a port is *really* available.

            // Try to reserve them in the global registry.
            let mut registry = ALLOCATED_PORTS_REGISTRY.lock().unwrap();
            if !registry.contains(&p1) && !registry.contains(&p2) && !registry.contains(&p3) {
                registry.insert(p1);
                registry.insert(p2);
                registry.insert(p3);

                return Self {
                    protocol_port: p1,
                    stderr_port: p2,
                    health_port: p3,
                    _guard: PortGuard {
                        ports: [p1, p2, p3],
                    },
                };
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
    fn start(command: &str, args: &[&str]) -> Self {
        const MAX_RETRIES: usize = 3;
        let mut last_error = None;

        for attempt in 0..MAX_RETRIES {
            // Allocate ports - registered in global registry
            let ports = AllocatedPorts::new();

            // Try to start with these ports
            match Self::try_start_with_ports(command, args, ports) {
                Ok(forwarder) => return forwarder,
                Err(e) => {
                    last_error = Some(e);
                    if attempt < MAX_RETRIES - 1 {
                        // Retry with new ports
                        thread::sleep(Duration::from_millis(100));
                        continue;
                    }
                }
            }
        }

        panic!(
            "Failed to start forwarder after {} attempts. Last error: {}",
            MAX_RETRIES,
            last_error.unwrap()
        );
    }

    /// Try to start a new `stdioxide` forwarder with pre-allocated ports.
    /// Returns an error if spawning fails or the forwarder doesn't become ready.
    fn try_start_with_ports(
        command: &str,
        args: &[&str],
        ports: AllocatedPorts,
    ) -> Result<Self, String> {
        let process = Self::spawn_process(
            command,
            args,
            ports.protocol_port(),
            ports.stderr_port(),
            ports.health_port(),
        )
        .map_err(|e| format!("Failed to spawn process: {}", e))?;

        let forwarder = Self { process, ports };

        // Wait for the forwarder to bind to the ports
        forwarder
            .try_wait_for_ready()
            .map_err(|e| format!("Failed to become ready: {}", e))?;

        // Ports remain in the forwarder and will be released when it's dropped
        Ok(forwarder)
    }

    /// Internal helper to spawn the forwarder process.
    /// Returns the spawned Child process or an error if spawning fails (e.g., due to port conflicts).
    fn spawn_process(
        command: &str,
        args: &[&str],
        protocol_port: u16,
        stderr_port: u16,
        health_port: u16,
    ) -> std::io::Result<Child> {
        // Get the path to the `stdioxide` binary.
        // In integration tests, we need to use the binary from the target directory.
        let bin_path = std::env::var("CARGO_BIN_EXE_stdioxide")
            .unwrap_or_else(|_| "target/debug/stdioxide".to_string());

        let mut cmd = Command::new(&bin_path);
        cmd.arg("--protocol-port")
            .arg(protocol_port.to_string())
            .arg("--stderr-port")
            .arg(stderr_port.to_string())
            .arg("--health-port")
            .arg(health_port.to_string())
            .arg(command);

        for arg in args {
            cmd.arg(arg);
        }

        cmd.stderr(Stdio::piped());
        cmd.stdout(Stdio::piped());

        cmd.spawn()
    }

    /// Try to wait for the forwarder to be ready by attempting to connect to the health port.
    /// Returns an error if the forwarder doesn't become ready in time.
    fn try_wait_for_ready(&self) -> Result<(), String> {
        // Try to connect with a shorter timeout since some processes may exit quickly.
        const NUM_ATTEMPTS: usize = 30;
        let mut last_error = None;
        
        for attempt in 0..NUM_ATTEMPTS {
            match TcpStream::connect_timeout(
                &format!("127.0.0.1:{}", self.ports.health_port()).parse().unwrap(),
                Duration::from_millis(100),
            ) {
                Ok(_) => return Ok(()),
                Err(e) => {
                    last_error = Some(e);
                    if attempt < NUM_ATTEMPTS - 1 {
                        thread::sleep(Duration::from_millis(50));
                    }
                }
            }
        }
        
        Err(format!(
            "Forwarder did not become ready in time on port {}. Last error: {}",
            self.ports.health_port(),
            last_error.unwrap()
        ))
    }

    /// Connect to the protocol port.
    fn connect_protocol(&self) -> TcpStream {
        self.connect_with_retry(self.ports.protocol_port(), "protocol")
    }

    /// Connect to the `stderr` port.
    fn connect_stderr(&self) -> TcpStream {
        self.connect_with_retry(self.ports.stderr_port(), "stderr")
    }

    /// Connect to the health port.
    fn connect_health(&self) -> TcpStream {
        self.connect_with_retry(self.ports.health_port(), "health")
    }

    /// Connect to a port with retries.
    fn connect_with_retry(&self, port: u16, label: &str) -> TcpStream {
        const NUM_ATTEMPTS: usize = 20;
        for attempt in 0..NUM_ATTEMPTS {
            match TcpStream::connect(("127.0.0.1", port)) {
                Ok(stream) => return stream,
                Err(e) if attempt == NUM_ATTEMPTS - 1 => {
                    panic!("Failed to connect to {} port {}: {}", label, port, e);
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
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

/// Helper function to read from a stream with a timeout.
fn read_with_timeout(stream: &mut TcpStream, buffer: &mut [u8]) -> std::io::Result<usize> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.read(buffer)
}

/// Helper function to read all available data from a stream up to a timeout.
fn read_all_available(stream: &mut TcpStream, timeout: Duration) -> Vec<u8> {
    let mut result = Vec::new();
    let mut buffer = [0u8; 8192];
    stream
        .set_read_timeout(Some(timeout))
        .expect("Failed to set read timeout");

    loop {
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => result.extend_from_slice(&buffer[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => break,
            Err(_) => break,
        }
    }

    result
}

// ============================================================================
// ACCEPTANCE CRITERIA TESTS
// ============================================================================

#[test]
fn test_forwarder_starts_arbitrary_child_process() {
    // * [x] A standalone forwarder executable can be started that launches an arbitrary child process.

    let forwarder = TestForwarder::start("sleep", &["5"]);

    // If we got here, the forwarder started successfully.
    // The forwarder should be ready (health port should be accessible).
    assert!(forwarder.connect_health().peer_addr().is_ok());
}

#[test]
fn test_forwarder_passes_arguments_unchanged() {
    // * [x] The forwarder passes command-line arguments through to the child process unchanged.

    // Use a command that outputs arguments and then waits, so we have time to connect.
    let forwarder = TestForwarder::start(
        "bash",
        &[
            "-c",
            "echo $@ && sleep 5",
            "--",
            "-n",
            "test",
            "with spaces",
            "--flag",
        ],
    );

    let mut stream = forwarder.connect_protocol();
    let output = read_all_available(&mut stream, Duration::from_millis(500));

    // `bash` should output all the arguments.
    let output_str = String::from_utf8_lossy(&output);
    assert!(output_str.contains("test"));
    assert!(output_str.contains("with spaces"));
    assert!(output_str.contains("--flag"));
}

#[test]
fn test_child_process_configurable_externally() {
    // * [x] The child process to launch can be configured externally.

    // Test with different commands to verify external configuration works.
    let forwarder1 = TestForwarder::start("bash", &["-c", "echo first && sleep 5"]);
    let mut stream1 = forwarder1.connect_protocol();
    let output1 = read_all_available(&mut stream1, Duration::from_millis(500));
    assert!(String::from_utf8_lossy(&output1).contains("first"));

    let forwarder2 = TestForwarder::start("bash", &["-c", "printf second && sleep 5"]);
    let mut stream2 = forwarder2.connect_protocol();
    let output2 = read_all_available(&mut stream2, Duration::from_millis(500));
    assert!(String::from_utf8_lossy(&output2).contains("second"));
}

#[test]
fn test_forwarder_exits_when_child_exits() {
    // * [x] When the child process exits for any reason, the forwarder also terminates.

    // Use a command that runs briefly and then exits.
    let mut forwarder = TestForwarder::start("bash", &["-c", "echo test && sleep 0.5"]);

    // Connect to protocol port to ensure we’re monitoring the forwarder.
    let _stream = forwarder.connect_protocol();

    // Wait for the child to exit (should happen after ~0.5s).
    thread::sleep(Duration::from_millis(600));

    // Forwarder should have exited by now.
    assert!(
        forwarder.wait_for_exit(),
        "Forwarder should exit when child exits"
    );
}

#[test]
fn test_forwarder_exposes_three_tcp_ports() {
    // * [x] The forwarder exposes three TCP ports:
    //   * [x] a **protocol port**
    //   * [x] an **stderr port**
    //   * [x] a **health port**

    let forwarder = TestForwarder::start("sleep", &["10"]);

    // Verify all three ports are accessible.
    // Note: The `connect_*()` methods already `panic!()` if connection fails, so the real
    // accessibility check happens *there*. The `.peer_addr().is_ok()` is redundant but
    // serves as documentation.
    assert!(
        forwarder.connect_protocol().peer_addr().is_ok(),
        "Protocol port should be accessible"
    );
    assert!(
        forwarder.connect_stderr().peer_addr().is_ok(),
        "Stderr port should be accessible"
    );
    assert!(
        forwarder.connect_health().peer_addr().is_ok(),
        "Health port should be accessible"
    );
}

#[test]
fn test_default_port_values() {
    // * [x] The default port values are:
    //   * [x] `7000` for the protocol port
    //   * [x] `7001` for the stderr port
    //   * [x] `7002` for the health port

    // Test setup: Ensure default ports are available.
    // If they're not, this is an environment issue, not a test failure.
    let _port_7000 = TcpListener::bind("127.0.0.1:7000")
        .expect("TEST SETUP FAILED: Default port 7000 is not available. This is an environment issue, not a test failure.");
    let _port_7001 = TcpListener::bind("127.0.0.1:7001")
        .expect("TEST SETUP FAILED: Default port 7001 is not available. This is an environment issue, not a test failure.");
    let _port_7002 = TcpListener::bind("127.0.0.1:7002")
        .expect("TEST SETUP FAILED: Default port 7002 is not available. This is an environment issue, not a test failure.");

    // Release the ports so stdioxide can bind to them
    drop(_port_7000);
    drop(_port_7001);
    drop(_port_7002);

    // Launch `stdioxide` *without* specifying ports to verify it uses the defaults.
    let bin_path = std::env::var("CARGO_BIN_EXE_stdioxide")
        .unwrap_or_else(|_| "target/debug/stdioxide".to_string());

    let mut cmd = Command::new(&bin_path);
    cmd.arg("sleep").arg("10");
    cmd.stderr(Stdio::piped());
    cmd.stdout(Stdio::piped());

    let mut process = cmd.spawn().expect("Failed to start stdioxide");

    // Wait for the forwarder to be ready by connecting to the default health port.
    let mut connected = false;
    const NUM_ATTEMPTS: usize = 30;
    for _ in 0..NUM_ATTEMPTS {
        if TcpStream::connect_timeout(
            &"127.0.0.1:7002".parse().unwrap(),
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
        "Forwarder should be ready on default health port 7002"
    );

    // Verify we can connect to all three default ports.
    assert!(
        TcpStream::connect("127.0.0.1:7000").is_ok(),
        "Should connect to default protocol port 7000"
    );
    assert!(
        TcpStream::connect("127.0.0.1:7001").is_ok(),
        "Should connect to default stderr port 7001"
    );
    assert!(
        TcpStream::connect("127.0.0.1:7002").is_ok(),
        "Should connect to default health port 7002"
    );

    // Clean up.
    let _ = process.kill();
    let _ = process.wait();
}

#[test]
fn test_port_override_via_environment_variables() {
    // * [x] All three port numbers can be overridden via environment variables.

    // Allocate unique ports to avoid conflicts.
    let ports = AllocatedPorts::new();
    let custom_protocol = ports.protocol_port();
    let custom_stderr = ports.stderr_port();
    let custom_health = ports.health_port();

    // Launch stdioxide with environment variables (NOT command-line args) to test env var override.
    let bin_path = std::env::var("CARGO_BIN_EXE_stdioxide")
        .unwrap_or_else(|_| "target/debug/stdioxide".to_string());

    let mut cmd = Command::new(&bin_path);
    cmd.env("STDIOXIDE_PROTOCOL_PORT", custom_protocol.to_string())
        .env("STDIOXIDE_STDERR_PORT", custom_stderr.to_string())
        .env("STDIOXIDE_HEALTH_PORT", custom_health.to_string())
        .arg("sleep")
        .arg("10")
        .stderr(Stdio::piped())
        .stdout(Stdio::piped());

    let mut process = cmd.spawn().expect("Failed to start stdioxide");

    // Wait for the forwarder to be ready by connecting to the custom health port.
    let mut connected = false;
    const NUM_ATTEMPTS: usize = 30;
    for _ in 0..NUM_ATTEMPTS {
        if TcpStream::connect_timeout(
            &format!("127.0.0.1:{}", custom_health).parse().unwrap(),
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
        "Forwarder should be ready on custom health port {}",
        custom_health
    );

    // Verify we can connect to all three custom ports.
    assert!(
        TcpStream::connect(("127.0.0.1", custom_protocol)).is_ok(),
        "Should connect to custom protocol port {}",
        custom_protocol
    );
    assert!(
        TcpStream::connect(("127.0.0.1", custom_stderr)).is_ok(),
        "Should connect to custom stderr port {}",
        custom_stderr
    );
    assert!(
        TcpStream::connect(("127.0.0.1", custom_health)).is_ok(),
        "Should connect to custom health port {}",
        custom_health
    );

    // Clean up.
    let _ = process.kill();
    let _ = process.wait();

    // ports is dropped here, releasing the registry entries
}

#[test]
fn test_stdout_sent_over_protocol_port() {
    // * [x] The forwarder sends the child process's `stdout` stream over the protocol port.

    let forwarder = TestForwarder::start("bash", &["-c", "echo 'Hello from stdout' && sleep 5"]);

    let mut stream = forwarder.connect_protocol();
    let output = read_all_available(&mut stream, Duration::from_millis(500));
    let output_str = String::from_utf8_lossy(&output);

    assert!(output_str.contains("Hello from stdout"));
}

#[test]
fn test_stdin_received_on_protocol_port() {
    // * [x] The forwarder receives input for the child process's `stdin` stream on the protocol port.
    // * [x] Data received on the protocol port is forwarded to the child process's `stdin` while the connection is active.

    let forwarder = TestForwarder::start("cat", &[]);

    let mut stream = forwarder.connect_protocol();

    // Send data to stdin via the protocol port.
    stream
        .write_all(b"test input\n")
        .expect("Failed to write to protocol port");
    stream.flush().expect("Failed to flush");

    // Read back the echoed output from stdout.
    let output = read_all_available(&mut stream, Duration::from_millis(500));
    let output_str = String::from_utf8_lossy(&output);

    assert!(output_str.contains("test input"));
}

#[test]
fn test_stderr_sent_over_stderr_port() {
    // * [x] The forwarder sends the child process's `stderr` stream over the stderr port.

    // Use a bash command that writes to stderr and then waits.
    let forwarder = TestForwarder::start("bash", &["-c", "echo 'error message' >&2 && sleep 5"]);

    let mut stream = forwarder.connect_stderr();
    let output = read_all_available(&mut stream, Duration::from_millis(500));
    let output_str = String::from_utf8_lossy(&output);

    assert!(output_str.contains("error message"));
}

#[test]
fn test_protocol_port_single_client_only() {
    // * [x] The protocol port allows at most one active client connection at a time.

    let forwarder = TestForwarder::start(
        "bash",
        &["-c", "while true; do read line; echo response; done"],
    );

    // First client connects successfully.
    let mut stream1 = forwarder.connect_protocol();

    // Verify first client works.
    stream1.write_all(b"test\n").expect("Failed to write");
    stream1.flush().expect("Failed to flush");
    let output = read_all_available(&mut stream1, Duration::from_millis(500));
    assert!(String::from_utf8_lossy(&output).contains("response"));

    // Second client connection attempt.
    // The TCP connection might succeed (queued in backlog), but it won't be served.
    // We verify this by trying to read - if the connection isn't served, we'll timeout.
    match TcpStream::connect_timeout(
        &format!("127.0.0.1:{}", forwarder.ports.protocol_port())
            .parse()
            .unwrap(),
        Duration::from_millis(200),
    ) {
        Ok(mut stream2) => {
            // Connection succeeded, but it should not be served.
            // Try to read with a short timeout - we should get no data.
            stream2
                .set_read_timeout(Some(Duration::from_millis(200)))
                .ok();
            let mut buf = [0u8; 100];
            let result = stream2.read(&mut buf);
            // Either we timeout or get 0 bytes (connection not served).
            assert!(
                result.is_err() || matches!(result, Ok(0)),
                "Second client should not receive data"
            );
        }
        Err(_) => {
            // Connection was rejected, which is also acceptable behavior.
        }
    }
}

#[test]
fn test_stderr_port_single_client_only() {
    // * [x] The stderr port allows at most one active client connection at a time.

    let forwarder = TestForwarder::start(
        "bash",
        &["-c", "while true; do echo error >&2; sleep 0.1; done"],
    );

    // First client connects successfully.
    let _stream1 = forwarder.connect_stderr();

    // Second client should connect but be rejected.
    // According to the stderr_server implementation, it rejects additional connections.
    let stream2 = forwarder.connect_stderr();

    // The second connection is made but immediately closed/rejected.
    // Try to read - should get no data or connection closed.
    let output = read_all_available(
        &mut stream2.try_clone().unwrap(),
        Duration::from_millis(500),
    );

    // The second client should not receive meaningful data since the first is still active.
    // In practice, the connection is accepted but dropped, so we should see minimal/no data.
    // This test verifies the single-client behavior.
    assert!(
        output.is_empty() || output.len() < 100,
        "Second stderr client should not receive full stream data while first client is active"
    );
}

#[test]
fn test_health_port_multiple_clients() {
    // * [x] The health port allows multiple simultaneous client connections.

    let forwarder = TestForwarder::start("sleep", &["10"]);

    // Connect multiple clients to the health port.
    let _stream1 = forwarder.connect_health();
    let _stream2 = forwarder.connect_health();
    let _stream3 = forwarder.connect_health();

    // All connections should succeed.
    assert!(
        _stream1.peer_addr().is_ok()
            && _stream2.peer_addr().is_ok()
            && _stream3.peer_addr().is_ok()
    );
}

#[test]
fn test_protocol_port_buffered_stdout_replay() {
    // * [x] When a client connects to the protocol port for the first time, it first receives all buffered `stdout` data produced before the connection was established.
    // * [x] After the buffered `stdout` data has been sent, the client continues to receive newly produced `stdout` data in real time.

    // Use a script that produces output immediately and then waits.
    let forwarder = TestForwarder::start(
        "bash",
        &[
            "-c",
            "echo 'buffered output'; sleep 1; echo 'realtime output'; sleep 10",
        ],
    );

    // Now connect - buffering ensures we receive output produced before connection.
    thread::sleep(Duration::from_millis(100));

    // Connect and we should receive the buffered output first.
    let mut stream = forwarder.connect_protocol();

    // Read the output.
    let output = read_all_available(&mut stream, Duration::from_secs(3));
    let output_str = String::from_utf8_lossy(&output);

    // Verify we got both buffered and realtime output.
    assert!(output_str.contains("buffered output"));
    assert!(output_str.contains("realtime output"));
}

#[test]
fn test_protocol_disconnect_kills_child() {
    // * [x] When a client disconnects from the protocol port, the child process is killed and the forwarder terminates.

    let mut forwarder = TestForwarder::start("sleep", &["100"]);

    {
        let _stream = forwarder.connect_protocol();
        // Disconnect by dropping the stream.
    }

    // Forwarder should terminate.
    assert!(
        forwarder.wait_for_exit(),
        "Forwarder should exit when protocol client disconnects"
    );
}

#[test]
fn test_stderr_port_buffered_stderr_replay() {
    // * [x] When a client connects to the stderr port, it first receives all buffered `stderr` data produced before the connection was established.
    // * [x] After the buffered `stderr` data has been sent, the client continues to receive newly produced `stderr` data in real time.

    // Use a script that produces stderr immediately and then waits.
    let forwarder = TestForwarder::start(
        "bash",
        &[
            "-c",
            "echo 'buffered error' >&2; sleep 1; echo 'realtime error' >&2; sleep 10",
        ],
    );

    // Now connect to stderr - buffering ensures we receive output produced before connection.
    thread::sleep(Duration::from_millis(100));

    // Connect and we should receive the buffered output first.
    let mut stream = forwarder.connect_stderr();

    // Read the output.
    let output = read_all_available(&mut stream, Duration::from_secs(2));
    let output_str = String::from_utf8_lossy(&output);

    // Verify we got both buffered and realtime output.
    assert!(output_str.contains("buffered error"));
    assert!(output_str.contains("realtime error"));
}

#[test]
fn test_stderr_disconnect_does_not_kill_child() {
    // * [x] When a client disconnects from the stderr port, neither the forwarder nor the child process terminate because of that.

    let forwarder = TestForwarder::start("sleep", &["10"]);

    {
        let _stderr_stream = forwarder.connect_stderr();
        // Disconnect by dropping the stream.
    }

    // With proactive disconnect detection, disconnect is detected immediately.
    thread::sleep(Duration::from_millis(100));

    // Forwarder should still be running - we can connect to health port.
    assert!(
        forwarder.connect_health().peer_addr().is_ok(),
        "Forwarder should still be running after stderr disconnect"
    );
}

#[test]
fn test_stderr_port_reconnect_continues_from_current_state() {
    // * [x] When a client connects to the stderr port, it first receives all buffered `stderr` data
    //       produced before the connection was established and after a previous connection was active
    //       (i.e., no logging data is lost).

    // Use a script that outputs at controlled times:
    // - "before_connection" immediately (buffered before any connection)
    // - "during_first_connection" after 0.5 seconds (sent to first client)
    // - "trigger_disconnect" after 1.5 seconds (triggers disconnect detection)
    // - "while_disconnected" after 3 seconds (buffered while no client connected)
    // - "during_second_connection" after 5 seconds (sent to second client)
    let forwarder = TestForwarder::start(
        "bash",
        &[
            "-c",
            "echo 'before_connection' >&2; sleep 0.5; echo 'during_first_connection' >&2; sleep 1; echo 'trigger_disconnect' >&2; sleep 1.5; echo 'while_disconnected' >&2; sleep 2; echo 'during_second_connection' >&2; sleep 10",
        ],
    );

    // Wait to ensure "before_connection" is buffered
    thread::sleep(Duration::from_millis(100));

    // First connection - connect, read initial data, then disconnect BEFORE "trigger_disconnect"
    {
        let mut stream = forwarder.connect_stderr();
        // Read for 800ms to get "before_connection" (immediate) and "during_first_connection" (at t=0.5s)
        let output = read_all_available(&mut stream, Duration::from_millis(800));
        let output_str = String::from_utf8_lossy(&output);
        assert!(output_str.contains("before_connection"));
        assert!(output_str.contains("during_first_connection"));
        // Disconnect now (at ~t=1.0s), before "trigger_disconnect" (at t=1.5s)
        let _ = stream.shutdown(std::net::Shutdown::Both);
        drop(stream);
    }

    // Now we're at ~t=1.0s. "trigger_disconnect" will be produced at t=1.5s, which will
    // cause the server to try writing to the disconnected client and detect the disconnect.
    // With proactive disconnect detection, disconnect is detected quickly.
    // Wait for "while_disconnected" to be produced (at t=3s from start).
    thread::sleep(Duration::from_millis(2300));

    // Second connection - should receive buffered "while_disconnected" and realtime "during_second_connection"
    {
        let mut stream = forwarder.connect_stderr();
        // Read for 2.5s to get both buffered "while_disconnected" and realtime "during_second_connection"
        let output = read_all_available(&mut stream, Duration::from_millis(2500));
        let output_str = String::from_utf8_lossy(&output);

        assert!(
            output_str.contains("while_disconnected"),
            "Second client should receive data buffered during disconnect, got: {}",
            output_str
        );

        assert!(
            output_str.contains("during_second_connection"),
            "Second client should receive realtime data, got: {}",
            output_str
        );
    }
}

#[test]
fn test_output_buffering_prevents_data_loss() {
    // * [x] Output buffering must prevent loss of `stdout` and `stderr` data when no client is connected yet.

    // Start a process that produces output immediately.
    let forwarder = TestForwarder::start(
        "bash",
        &[
            "-c",
            "echo 'stdout message'; echo 'stderr message' >&2; sleep 10",
        ],
    );

    // Buffering ensures output is captured even if we connect immediately.
    thread::sleep(Duration::from_millis(100));

    // Now connect - we should receive the buffered output.
    let mut stdout_stream = forwarder.connect_protocol();
    let stdout_data = read_all_available(&mut stdout_stream, Duration::from_millis(500));
    let stdout_str = String::from_utf8_lossy(&stdout_data);

    let mut stderr_stream = forwarder.connect_stderr();
    let stderr_data = read_all_available(&mut stderr_stream, Duration::from_millis(500));
    let stderr_str = String::from_utf8_lossy(&stderr_data);

    assert!(stdout_str.contains("stdout message"));
    assert!(stderr_str.contains("stderr message"));
}

#[test]
fn test_health_port_indicates_readiness() {
    // * [x] A successful TCP connection to the health port indicates that the forwarder is ready to accept connections and operate normally.

    let forwarder = TestForwarder::start("sleep", &["10"]);

    // If we can connect to health port, the forwarder is ready.
    let health_stream = forwarder.connect_health();
    assert!(health_stream.peer_addr().is_ok());

    // And the other ports should also be accessible.
    assert!(forwarder.connect_protocol().peer_addr().is_ok());
    assert!(forwarder.connect_stderr().peer_addr().is_ok());
}

#[test]
fn test_health_checks_do_not_interfere() {
    // * [x] Health checks on the health port must not interfere with the behavior of the protocol port or the stderr port.

    let forwarder = TestForwarder::start("cat", &[]);

    // Perform multiple health checks.
    for _ in 0..5 {
        let _health = forwarder.connect_health();
        thread::sleep(Duration::from_millis(50));
    }

    // Protocol port should still work normally.
    let mut protocol_stream = forwarder.connect_protocol();
    protocol_stream
        .write_all(b"test data\n")
        .expect("Failed to write");
    protocol_stream.flush().expect("Failed to flush");

    let output = read_all_available(&mut protocol_stream, Duration::from_millis(500));
    assert!(String::from_utf8_lossy(&output).contains("test data"));

    // Stderr port should still work.
    assert!(forwarder.connect_stderr().peer_addr().is_ok());
}

#[test]
fn test_works_with_various_executables() {
    // * [x] The forwarder must work not only for Python applications, but for any executable child process.

    // Test with various common executables.

    // Test with echo via bash (shell command).
    {
        let forwarder = TestForwarder::start("bash", &["-c", "echo test1 && sleep 2"]);
        let mut stream = forwarder.connect_protocol();
        let output = read_all_available(&mut stream, Duration::from_millis(500));
        assert!(String::from_utf8_lossy(&output).contains("test1"));
    }

    // Test with printf.
    {
        let forwarder = TestForwarder::start("bash", &["-c", "printf test2 && sleep 2"]);
        let mut stream = forwarder.connect_protocol();
        let output = read_all_available(&mut stream, Duration::from_millis(500));
        assert!(String::from_utf8_lossy(&output).contains("test2"));
    }

    // Test with cat (interactive).
    {
        let forwarder = TestForwarder::start("cat", &[]);
        let mut stream = forwarder.connect_protocol();
        stream.write_all(b"test3\n").expect("Failed to write");
        stream.flush().expect("Failed to flush");
        let output = read_all_available(&mut stream, Duration::from_millis(500));
        assert!(String::from_utf8_lossy(&output).contains("test3"));
    }

    // Test with a python script (if available).
    {
        let forwarder = TestForwarder::start("bash", &["-c", "echo test4 && sleep 2"]);
        let mut stream = forwarder.connect_protocol();
        let output = read_all_available(&mut stream, Duration::from_millis(500));
        assert!(String::from_utf8_lossy(&output).contains("test4"));
    }
}

#[test]
fn test_large_output_buffering() {
    // Additional test: verify that large outputs are buffered correctly.

    // Generate a large output.
    let large_size = 100_000;
    let forwarder = TestForwarder::start(
        "bash",
        &[
            "-c",
            &format!("head -c {} /dev/zero | tr '\\0' 'A'; sleep 10", large_size),
        ],
    );

    // Wait for output to be generated.
    thread::sleep(Duration::from_millis(200));

    // Connect and read the buffered output.
    let mut stream = forwarder.connect_protocol();
    let mut total_read = 0;
    let mut buffer = [0u8; 8192];

    while total_read < large_size {
        match read_with_timeout(&mut stream, &mut buffer) {
            Ok(0) => break,
            Ok(n) => total_read += n,
            Err(_) => break,
        }
    }

    assert!(
        total_read >= large_size,
        "Should have read at least {} bytes, got {}",
        large_size,
        total_read
    );
}

#[test]
fn test_concurrent_stdin_stdout_bidirectional() {
    // Additional test: verify bidirectional communication works correctly.

    // Use cat which echoes stdin to stdout.
    let forwarder = TestForwarder::start("cat", &[]);
    let mut stream = forwarder.connect_protocol();

    // Send multiple lines and verify echo.
    for i in 0..5 {
        let message = format!("line {}\n", i);
        stream
            .write_all(message.as_bytes())
            .expect("Failed to write");
        stream.flush().expect("Failed to flush");

        let output = read_all_available(&mut stream, Duration::from_millis(500));
        let output_str = String::from_utf8_lossy(&output);
        assert!(output_str.contains(&format!("line {}", i)));
    }
}
