//! gitattributes evaluation and git's wildmatch, with no I/O: callers supply the `.gitattributes` text. Also the
//! `lockable` lint that compares the attribute with a policy's exclusive paths.

pub mod attributes;
pub mod lockable;
pub mod wildmatch;
