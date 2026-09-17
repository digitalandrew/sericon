#![cfg(target_os = "linux")]
pub mod config;
pub mod detect;
pub mod devices;
pub mod files;
pub mod formula;
pub mod help;
pub mod ipc;
pub mod journal;
mod live;
pub mod mcp;
mod menu;
mod picker;
mod search;
pub mod session;
pub mod terminal;
mod terminal_input;
mod tui;
