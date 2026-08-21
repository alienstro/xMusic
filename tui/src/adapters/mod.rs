//! Adapters: the concrete ways this client reaches the world. The daemon over
//! HTTP, the daemon as a process, and the user's browser as a source of a
//! YouTube session. Nothing above them knows any of it.

pub mod browser_session;
pub mod daemon_process;
pub mod http_client;
