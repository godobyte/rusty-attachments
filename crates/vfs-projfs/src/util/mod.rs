//! Utility functions for ProjFS operations.

// Some utility functions are used only by callback code which the
// compiler cannot trace through function pointers.
#[allow(dead_code)]
pub mod compare;
#[allow(dead_code)]
pub mod filetime;
#[allow(dead_code)]
pub mod wstr;

pub use compare::prj_file_name_compare;
pub use wstr::string_to_wide;
