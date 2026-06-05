//! `stp-client` — the host (client) side of the Stimma Tools Protocol.
//!
//! Embeds in `stp` (the CLI) and in the e2e test harness. Provides the wire types, the two
//! transports (WebSocket + stdio), the registration/execute session state machine, asset
//! transfer, and parameter coercion/validation.

pub mod assets;
pub mod descriptor;
pub mod error;
pub mod protocol;
pub mod schema;
pub mod session;
pub mod transport;

pub use descriptor::ToolDescriptor;
pub use error::{ExitCode, Result, StpError};
pub use session::Session;
pub use transport::{connect_websocket, spawn_stdio, Connection, StdioConfig};
