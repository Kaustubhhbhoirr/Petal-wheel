//! Flick core: hotkey, Explorer selection, wheel overlay and the worker supervisor.
//!
//! The core stays idle at ~0% CPU. All heavy work happens in worker processes
//! that the supervisor starts inside resource-limited Windows Job Objects.

#[cfg(windows)]
pub mod supervisor;
