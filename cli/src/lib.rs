//! OwlMap: turn an undocumented codebase into a navigable map with Claude.
//!
//! Pipeline: [`repo_source`] fetches the code, [`scanner`] keeps the files worth
//! reading, [`grouper`] cuts them into modules, [`analyzer`] summarises every
//! module and writes the overview documents through an [`client::Llm`], and
//! [`writer`] puts the Markdown on disk.

pub mod analyzer;
pub mod client;
pub mod config;
pub mod grouper;
pub mod prompts;
pub mod repo_source;
pub mod scanner;
pub mod writer;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Rough token estimate: ~4 bytes per token for code and English prose.
/// Good enough for budgeting; the API reports exact usage afterwards.
pub fn estimate_tokens(bytes: u64) -> u64 {
    bytes.div_ceil(4)
}
