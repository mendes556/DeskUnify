// DeskUnify changes, 2026-10-01; derived from Lan Mouse, GPL-3.0-or-later.
mod capture;
pub mod capture_test;
pub mod client;
#[cfg(any(target_os = "macos", windows))]
mod clipboard;
pub mod config;
mod connect;
#[cfg(any(target_os = "macos", windows))]
mod control;
#[cfg(not(any(target_os = "macos", windows)))]
#[path = "control_unsupported.rs"]
mod control;
mod crypto;
mod discovery;
mod dns;
mod emulation;
pub mod emulation_test;
#[cfg(any(target_os = "macos", windows))]
pub mod files;
mod listen;
pub mod service;
mod sharing;
