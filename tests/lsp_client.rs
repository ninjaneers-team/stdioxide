//! Test utilities for LSP client communication.

#![allow(
    unreachable_pub,
    reason = "Private test module, but items need `pub` for parent access"
)]

use std::{
    io::{self, Read, Write},
    net::TcpStream,
    thread,
    time::Duration,
};

// Acknowledge available dev-dependencies not used in this test file.
use anyhow as _;
use clap as _;
use lsp_types as _;
use stdioxide as _;
use subprocess as _;
use tracing as _;
use tracing_subscriber as _;

/// RAII wrapper for LSP communication over a TCP stream.
/// Automatically sends the exit notification when dropped.
pub struct LspClient {
    stream: TcpStream,
    next_request_id: i32,
}

impl LspClient {
    /// Create a new LSP client from a TCP stream.
    ///
    /// # Errors
    ///
    /// Returns an error if setting read or write timeouts on the stream fails.
    pub fn new(stream: TcpStream) -> Result<Self, anyhow::Error> {
        // Set reasonable timeouts for LSP communication.
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .map_err(|error| anyhow::anyhow!("Failed to set read timeout: {error}"))?;
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .map_err(|error| anyhow::anyhow!("Failed to set write timeout: {error}"))?;

        Ok(Self {
            stream,
            next_request_id: 1,
        })
    }

    /// Send an LSP message over the stream.
    /// LSP uses JSON-RPC 2.0 with a Content-Length header.
    fn send_message(&mut self, message: &serde_json::Value) -> io::Result<()> {
        let json_str = serde_json::to_string(message)?;
        let content = format!("Content-Length: {}\r\n\r\n{}", json_str.len(), json_str);
        self.stream.write_all(content.as_bytes())?;
        self.stream.flush()?;
        Ok(())
    }

    /// Read an LSP message from the stream.
    /// Returns the parsed JSON value.
    fn read_message(&mut self) -> anyhow::Result<serde_json::Value> {
        // Read the Content-Length header.
        let mut header = String::new();
        let mut buffer = [0_u8; 1];

        // Read until we find "\r\n\r\n"
        loop {
            self.stream.read_exact(&mut buffer)?;
            header.push(buffer[0].into());
            if header.ends_with("\r\n\r\n") {
                break;
            }
            // Prevent infinite loops on malformed headers
            if header.len() > 1000 {
                return Err(anyhow::anyhow!("Header too long"));
            }
        }

        // Parse Content-Length
        let content_length = header
            .lines()
            .find(|line| line.starts_with("Content-Length:"))
            .and_then(|line| line.strip_prefix("Content-Length:"))
            .and_then(|len_str| len_str.trim().parse::<usize>().ok())
            .ok_or_else(|| anyhow::anyhow!("Missing Content-Length"))?;

        // Read the JSON content.
        let mut content = vec![0_u8; content_length];
        self.stream.read_exact(&mut content)?;

        // Parse JSON.
        let json: serde_json::Value = serde_json::from_slice(&content)?;
        Ok(json)
    }

    /// Send an LSP request and return the next request ID to use.
    fn send_request(
        &mut self,
        method: &str,
        params: &serde_json::Value,
    ) -> Result<i32, anyhow::Error> {
        let request_id = self.next_request_id;
        self.next_request_id = self.next_request_id.wrapping_add(1_i32);

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": method,
            "params": params
        });

        self.send_message(&request)
            .map_err(|error| anyhow::anyhow!("Failed to send LSP request: {error}"))?;
        Ok(request_id)
    }

    /// Send an LSP notification (no response expected).
    fn send_notification(
        &mut self,
        method: &str,
        params: &serde_json::Value,
    ) -> Result<(), anyhow::Error> {
        let notification = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        });

        self.send_message(&notification)
            .map_err(|error| anyhow::anyhow!("Failed to send LSP notification: {error}"))?;
        Ok(())
    }

    /// Read responses until we get a response with the specified ID.
    /// Skips notifications that may arrive in between.
    fn read_response(&mut self, expected_id: i32) -> anyhow::Result<serde_json::Value> {
        for _ in 0_usize..20_usize {
            match self.read_message() {
                Ok(msg) => {
                    // Check if this is our response.
                    if msg.get("id") == Some(&serde_json::json!(expected_id)) {
                        return Ok(msg);
                    }
                    // Otherwise, it's a notification, keep reading.
                }
                Err(error)
                    if error
                        .downcast_ref::<io::Error>()
                        .is_some_and(|io_error| io_error.kind() == io::ErrorKind::TimedOut) =>
                {
                    thread::sleep(Duration::from_millis(100));
                }
                Err(error) => {
                    return Err(anyhow::anyhow!("Failed to read LSP response: {error}"));
                }
            }
        }
        Err(anyhow::anyhow!(
            "Did not receive response with id {expected_id}"
        ))
    }

    /// Initialize the LSP server with the given workspace root.
    ///
    /// # Errors
    ///
    /// Returns an error if the initialize request fails to send or the response cannot be read.
    pub fn initialize(&mut self, root_uri: &str) -> anyhow::Result<serde_json::Value> {
        let params = serde_json::json!({
            "processId": null,
            "rootUri": root_uri,
            "capabilities": {
                "textDocument": {
                    "hover": {
                        "contentFormat": ["plaintext", "markdown"]
                    }
                }
            }
        });

        let request_id = self.send_request("initialize", &params)?;
        self.read_response(request_id)
    }

    /// Send the initialized notification.
    pub fn initialized(&mut self) {
        drop(self.send_notification("initialized", &serde_json::json!({})));
    }

    /// Open a document.
    pub fn did_open(&mut self, uri: &str, language_id: &str, text: &str) {
        let params = serde_json::json!({
            "textDocument": {
                "uri": uri,
                "languageId": language_id,
                "version": 1_i32,
                "text": text
            }
        });

        drop(self.send_notification("textDocument/didOpen", &params));
    }

    /// Request document symbols for a file.
    ///
    /// # Errors
    ///
    /// Returns an error if the document symbol request fails to send or the response cannot be read.
    pub fn document_symbol(&mut self, uri: &str) -> anyhow::Result<serde_json::Value> {
        let params = serde_json::json!({
            "textDocument": {
                "uri": uri
            }
        });

        let request_id = self.send_request("textDocument/documentSymbol", &params)?;
        self.read_response(request_id)
    }

    /// Shutdown the LSP server.
    ///
    /// # Errors
    ///
    /// Returns an error if the shutdown request fails to send or the response cannot be read.
    pub fn shutdown(&mut self) -> anyhow::Result<serde_json::Value> {
        let request_id = self.send_request("shutdown", &serde_json::json!(null))?;
        self.read_response(request_id)
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        // Automatically send exit notification when the client is dropped.
        drop(self.send_message(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": "exit"
        })));
    }
}
