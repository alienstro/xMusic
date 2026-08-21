//! Adapters: the concrete technology the application is reached through and
//! reaches out with. Everything Tauri-, HTTP- or filesystem-shaped lives under
//! here, and nothing under here decides policy.

pub mod http;
pub mod tauri_page;
