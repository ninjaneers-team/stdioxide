use std::net::TcpListener;

/// Waits for clients to connect on the `health` port, and immediately drops any connections. The existence
/// of a successful connection is used by the client as a health check for whether the process is alive.
pub fn health_server(listener: TcpListener) -> Result<(), anyhow::Error> {
    for stream in listener.incoming() {
        match stream {
            Ok(_stream) => {
                // Immediately drop it; successful connect is enough.
            }
            Err(e) => {
                eprintln!("[health] accept failed: {e}");
            }
        }
    }

    Ok(())
}
