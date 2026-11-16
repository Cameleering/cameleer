//! HTTP/2 settings of the gRPC client and server.
//!
//! The engine is simoom (`simoom-client` for calls, `simoom` for the server): it implements
//! HTTP/2 with HPACK, flow control, GOAWAY and the stream state machine. This module only holds the
//! settings the `grpc` component and the byte-level client and server accept.

use std::time::Duration;

/// Connection settings for our side.
///
/// | field | client | server |
/// |---|---|---|
/// | `max_concurrent_streams` | | streams the server accepts at once per connection |
/// | `initial_window_size`, `connection_window_size`, `max_frame_size` | per-stream and connection receive windows, largest frame | simoom's own values (the server's windows are not configurable) |
/// | `header_table_size`, `max_header_list_size` | HPACK table and header list limits | the same |
/// | `connect_timeout` | TCP and TLS connect | |
/// | `write_timeout` | socket write | output that makes no progress closes the connection |
#[derive(Clone, Debug)]
pub struct H2Config {
    /// SETTINGS_MAX_CONCURRENT_STREAMS we allow the peer to open (server side).
    pub max_concurrent_streams: u32,
    /// SETTINGS_INITIAL_WINDOW_SIZE: per-stream receive window.
    pub initial_window_size: u32,
    /// Connection-level receive window (raised with a WINDOW_UPDATE after the preface).
    pub connection_window_size: u32,
    /// SETTINGS_MAX_FRAME_SIZE we accept.
    pub max_frame_size: u32,
    /// SETTINGS_HEADER_TABLE_SIZE for our HPACK decoder.
    pub header_table_size: u32,
    /// SETTINGS_MAX_HEADER_LIST_SIZE we accept (0 = the engine's default).
    pub max_header_list_size: u32,
    /// Timeout for the TCP connect and the TLS/preface handshake.
    pub connect_timeout: Duration,
    /// Socket write timeout.
    pub write_timeout: Duration,
}

impl Default for H2Config {
    fn default() -> Self {
        H2Config {
            max_concurrent_streams: 256,
            initial_window_size: 1 << 20,
            connection_window_size: 4 << 20,
            max_frame_size: 16_384,
            header_table_size: 4096,
            max_header_list_size: 64 * 1024,
            connect_timeout: Duration::from_secs(10),
            write_timeout: Duration::from_secs(30),
        }
    }
}
