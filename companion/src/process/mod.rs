//! Another process's memory and its child processes, per platform.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::{descendants, Process};

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{descendants, Process};

/// Writable mappings at least this large are candidates for guest RAM. Guest RAM is
/// gigabytes; everything else an emulator allocates read-write is far smaller.
const MIN_REGION: u64 = 64 << 20;
