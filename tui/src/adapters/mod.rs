//! Adapters: the concrete ways this client reaches the world. The daemon over
//! HTTP, the daemon as a process, the user's browser as a source of a YouTube
//! session, and the installer that owns both binaries. Nothing above them knows
//! any of it.

pub mod browser_session;
pub mod daemon_process;
pub mod http_client;
pub mod installation;
