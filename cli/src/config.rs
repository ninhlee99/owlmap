/// All tunables in one place. Every value can be overridden from the CLI.
#[derive(Clone, Debug)]
pub struct Config {
    /// Summarises individual modules.
    pub fast_model: String,
    /// Writes architecture, flows and onboarding.
    pub smart_model: String,
    /// Refuse repositories with more source files than this (beta limit).
    pub max_files: usize,
    /// Skip single files larger than this.
    pub max_file_bytes: u64,
    /// Split a module into parts above this many characters.
    pub module_char_budget: u64,
    /// Fold modules smaller than this into a neighbour.
    pub min_module_chars: u64,
    /// Abort before calling the API if the estimate exceeds this.
    pub max_input_tokens: u64,
    /// Parallel module summaries.
    pub concurrency: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            fast_model: std::env::var("OWLMAP_FAST_MODEL").unwrap_or_else(|_| "claude-haiku-5-5".into()),
            smart_model: std::env::var("OWLMAP_SMART_MODEL").unwrap_or_else(|_| "claude-sonnet-5-5".into()),
            max_files: 500,
            max_file_bytes: 100_000,
            module_char_budget: 120_000,
            min_module_chars: 3_000,
            max_input_tokens: 600_000,
            concurrency: 4,
        }
    }
}
