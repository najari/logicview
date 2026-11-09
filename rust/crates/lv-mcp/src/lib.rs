//! MCP server built into logicview (AUT-3..5): the tools of Saleae Logic 2's
//! MCP server (same names, arguments and result shapes, like the Python
//! server in `mcp/`), plus `read_analyzer_frames` and `query_frames`.
//!
//! Captures, decoding (including Saleae HLAs) and storage use the
//! logicview crates; nothing calls sigrok-cli.

pub mod schema;
mod tools;
pub mod transport;

pub use tools::{Config, DeviceSlot, McpError, Server, default_data_dir, find_hla_script, prepare};
pub use transport::{HttpHandle, handle_text, serve_stdio, start_http};
