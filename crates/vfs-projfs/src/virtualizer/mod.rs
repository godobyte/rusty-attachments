//! ProjFS virtualizer implementation.
//!
//! This module provides the ProjFS callback implementations and
//! manages the virtualization lifecycle.

// Callback functions are used via function pointers passed to ProjFS,
// which the compiler cannot trace as "used" code.
#[allow(dead_code)]
mod callbacks;
#[allow(dead_code)]
mod enumeration;
mod projfs;
#[allow(dead_code)]
mod sendable;

pub use projfs::WritableProjFs;
