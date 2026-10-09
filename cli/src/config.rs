use crate::i18n::Lang;
use crate::scanner::ScanOptions;

/// All tunables in one place. Every value can be overridden from the CLI.
#[derive(Clone, Debug)]
pub struct Config {
    /// Summarises individual modules.
    pub fast_model: String,
    /// Writes architecture, flows and onboarding.
    pub smart_model: String,
    /// Refuse a repository with more source files than this, after filtering.
    pub max_files: usize,
    /// Which files to read (size limit, tests, include/exclude, bulk).
    pub scan: ScanOptions,
    /// Split a module into parts above this many characters.
    pub module_char_budget: u64,
    /// Fold modules smaller than this into a neighbour.
    pub min_module_chars: u64,
    /// Abort before calling the API if the estimate exceeds this.
    pub max_input_tokens: u64,
    /// Parallel module summaries.
    pub concurrency: usize,
    /// Language of the generated prose.
    pub lang: Lang,
    /// Above this many modules, summaries are rolled up into areas first.
    pub rollup_threshold: usize,
    /// Largest area; bigger ones are split a folder level deeper.
    pub area_max_modules: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            fast_model: std::env::var("OWLMAP_FAST_MODEL").unwrap_or_else(|_| "claude-haiku-5-5".into()),
            smart_model: std::env::var("OWLMAP_SMART_MODEL").unwrap_or_else(|_| "claude-sonnet-5-5".into()),
            max_files: 20_000,
            scan: ScanOptions::default(),
            module_char_budget: 160_000,
            min_module_chars: 3_000,
            max_input_tokens: 20_000_000,
            concurrency: 4,
            lang: Lang::En,
            rollup_threshold: 30,
            area_max_modules: 40,
        }
    }
}
