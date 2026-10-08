# frozen_string_literal: true

module Owlmap
  VERSION = "0.1.0"

  class Error < StandardError; end

  # Rough token estimate: ~4 characters per token for code and English prose.
  # Good enough for budgeting; the API reports exact usage afterwards.
  def self.estimate_tokens(text_or_size)
    size = text_or_size.is_a?(Integer) ? text_or_size : text_or_size.to_s.bytesize
    (size / 4.0).ceil
  end
end

require_relative "owlmap/config"
require_relative "owlmap/repo_source"
require_relative "owlmap/file_scanner"
require_relative "owlmap/module_grouper"
require_relative "owlmap/claude_client"
require_relative "owlmap/claude_code_client"
require_relative "owlmap/prompts"
require_relative "owlmap/analyzer"
require_relative "owlmap/writer"
