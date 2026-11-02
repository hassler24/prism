//! Prism — Windows file finder.
//! No GUI framework, no GPU process, no background service:
//! Win32 layered window + software compositor + memory-mapped index.
#![allow(non_snake_case, non_upper_case_globals, dead_code)]

pub mod fuzzy;
pub mod index;
pub mod interop;
pub mod refresh;
pub mod ui;
pub mod usn;
pub mod walk;
pub mod win32;
