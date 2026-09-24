//! Target-specific agent implementations.
//!
//! Exactly one specialist module is compiled for a supported target. Shared
//! protocol and task code calls this boundary instead of importing OS APIs.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub(crate) use linux::*;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub(crate) use windows::*;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub(crate) use macos::*;

#[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
compile_error!("nezha-agent-rust supports Linux, Windows, and macOS targets only");
