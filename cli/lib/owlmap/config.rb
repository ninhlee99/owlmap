# frozen_string_literal: true

module Owlmap
  # All tunables in one place. Every value can be overridden from the CLI.
  Config = Struct.new(
    :fast_model,          # summarises individual modules
    :smart_model,         # writes architecture, flows and onboarding
    :max_files,           # refuse repositories larger than this (beta limit)
    :max_file_bytes,      # skip single files larger than this
    :module_char_budget,  # split a module into parts above this many characters
    :min_module_chars,    # fold modules smaller than this into a neighbour
    :max_input_tokens,    # abort before calling the API if the estimate exceeds this
    :concurrency,         # parallel module summaries
    :out_dir,
    keyword_init: true
  ) do
    def self.default
      new(
        fast_model: ENV.fetch("OWLMAP_FAST_MODEL", "claude-haiku-5-5"),
        smart_model: ENV.fetch("OWLMAP_SMART_MODEL", "claude-sonnet-5-5"),
        max_files: 500,
        max_file_bytes: 100_000,
        module_char_budget: 120_000,
        min_module_chars: 3_000,
        max_input_tokens: 600_000,
        concurrency: 4,
        out_dir: "owlmap-docs"
      )
    end
  end
end
