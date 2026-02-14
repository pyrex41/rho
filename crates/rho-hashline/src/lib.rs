pub mod apply;
pub mod edit;
pub mod error;
pub mod format;
pub mod hash;
pub mod heuristics;
pub mod parse;

pub use apply::apply_hashline_edits;
pub use edit::{HashlineEdit, InsertAfterOp, ReplaceLinesOp, ReplaceOp, SetLineOp};
pub use error::{HashlineError, HashlineMismatchError};
pub use format::format_hashlines;
pub use hash::compute_line_hash;
pub use parse::{parse_anchor, parse_line_ref, LineRef};
