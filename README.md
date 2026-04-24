# stdioxide 🚀

A TCP forwarder that exposes a child process’s `stdin`, `stdout`, and `stderr` streams over the network.

## Overview

stdioxide launches an arbitrary child process and forwards its standard streams over two TCP ports, allowing remote interaction with any command-line application. Output is buffered to prevent data loss when no clients are connected.  
In addition, it provides a third TCP port for health checks (e.g., to run it inside Kubernetes).

## Motivation

While several tools exist for TCP stream forwarding (such as `socat`, `netcat`, `tcpserver`, and `xinetd`), stdioxide addresses specific requirements for running processes in containerized environments:

**Health Check Integration**: Unlike general-purpose forwarding tools, stdioxide provides a dedicated health check port that container orchestrators (Kubernetes, Docker Compose) can use for readiness and liveness probes. Tools like `socat` would require additional wrapper scripts to provide this functionality.

**Lifecycle Coupling**: The forwarder automatically terminates when the child process exits, ensuring proper cleanup in container environments. Traditional tools like `xinetd` or `tcpserver` are designed to spawn processes on-demand but don’t couple their lifecycle to a single long-running child process. This coupling is essential for container orchestrators to correctly detect when the application has terminated.

**Reconnectable Stderr**: Most TCP forwarding solutions don’t provide separate, reconnectable access to `stderr` with buffering. This is particularly valuable for collecting diagnostic logs from applications that may have intermittent monitoring connections.

**Buffered Output**: stdioxide buffers both `stdout` and `stderr` to prevent data loss during client disconnections—a common scenario when running in environments with network instability or during rolling updates.

### Example Use Cases

- **Language Servers**: Run language servers (e.g., `rust-analyzer`, `pyright`, `typescript-language-server`) as network services. The protocol port provides `stdin`/`stdout` communication via the Language Server Protocol (LSP), the `stderr` port captures diagnostic logs, and the health port enables container orchestrators to monitor the language server’s availability.
- **Legacy CLI Tools**: Expose command-line applications that weren’t designed for network access as TCP services within containerized environments.
- **Batch Processors**: Wrap long-running data processing scripts with health monitoring and reconnectable log streaming.

## Features ✨

- **Universal compatibility**: Works with any executable child process
- **Three TCP ports**: Protocol, `stderr`, and health endpoints
- **Output buffering**: No data loss when clients disconnect and reconnect
- **Configurable ports**: Via command-line arguments or environment variables
- **Automatic cleanup**: Forwarder terminates when child process exits

## Installation

```bash
cargo build --release
```

The binary will be available at `target/release/stdioxide`.

## Usage

```bash
stdioxide [OPTIONS] <COMMAND> [ARGS...]
```

**Example:**

```bash
stdioxide --protocol-port 7000 --stderr-port 7001 --health-port 7002 python my_script.py
```

### Arguments

- `<COMMAND>`: The child process to launch
- `[ARGS...]`: Arguments passed through unchanged to the child process

### Options

- `--protocol-port <PORT>`: Protocol port (default: 7000)
- `--stderr-port <PORT>`: Stderr port (default: 7001)
- `--health-port <PORT>`: Health check port (default: 7002)

Ports can also be configured via environment variables:

- `STDIOXIDE_PROTOCOL_PORT`
- `STDIOXIDE_STDERR_PORT`
- `STDIOXIDE_HEALTH_PORT`

> [!NOTE]  
> Note: If both command-line arguments and environment variables are provided, command-line arguments take precedence.

## Port Behavior 🔌

### Protocol Port (default: 7000)

The bidirectional communication port for `stdin` and `stdout`:

- **Single client**: Only one active connection at a time
- **Bidirectional**: Receives `stdin` from client, sends `stdout` to client
- **Buffered replay**: New clients receive all buffered `stdout` before real-time data
- **Terminates on disconnect**: Child process is killed when the client disconnects

### Stderr Port (default: 7001)

The unidirectional port for `stderr` output:

- **Single client**: Only one active connection at a time
- **Reconnectable**: New clients can connect after previous ones disconnect
- **Buffered replay**: New clients receive all buffered `stderr`, including data produced while disconnected
- **No termination**: Child process continues running when clients disconnect

### Health Port (default: 7002)

The readiness check endpoint:

- **Immediately closed**: Connections are accepted and immediately closed
- **Readiness indicator**: Successful TCP connection means forwarder is ready
- **Non-interfering**: Health checks don’t affect protocol or `stderr` ports

## Testing

Run the full test suite:

```bash
cargo test
```

## Development

This project was developed with AI assistance using GitHub Copilot and Claude Sonnet 4.5.

## License

See LICENSE file for details.
