// DeskUnify changes, 2026-10-01; derived from Lan Mouse, GPL-3.0-or-later.
mod capture;
pub mod capture_test;
pub mod client;
#[cfg(any(target_os = "macos", windows))]
mod clipboard;
pub mod config;
mod connect;
mod crypto;
mod discovery;
mod dns;
mod emulation;
pub mod emulation_test;
#[cfg(any(target_os = "macos", windows))]
pub mod files;
mod listen;
pub mod service;
