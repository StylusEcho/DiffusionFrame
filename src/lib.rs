//! The parts of DiffusionFrame that do not touch a webview.
//!
//! Keeping configuration, argument parsing, the reachability probe, the
//! WebView2 command line and the offline page here means they are testable on
//! any host, while `main.rs` stays a thin shell around the platform webview.

pub mod browser_args;
pub mod cli;
pub mod config;
pub mod menu;
pub mod net;
pub mod platform;
pub mod ui;
