# frozen_string_literal: true

require "open3"
require "json"
require "tmpdir"

module Owlmap
  # Runs prompts through the locally installed Claude Code CLI in print mode,
  # using whatever account `claude` is signed in with (a Claude subscription or
  # a Console API key).
  #
  # Intended for the developer's own runs on their own machine. A hosted OwlMap
  # service must use ClaudeClient with an API key: Anthropic does not allow
  # routing other people's requests through a Free/Pro/Max plan login.
  #
  # Same #complete / #usage interface as ClaudeClient, so the Analyzer does not
  # care which one it gets.
  class ClaudeCodeClient
    attr_reader :usage

    def initialize(bin: ENV.fetch("OWLMAP_CLAUDE_BIN", "claude"), timeout: 600)
      @bin = bin
      @timeout = timeout
      @usage = ClaudeClient::Usage.new(0, 0, 0, 0, 0)
      @lock = Mutex.new
      @safe_mode = true
      check_installed!
    end

    def complete(model:, system:, user:, max_tokens: nil) # max_tokens is decided by Claude Code
      Dir.mktmpdir("owlmap-cc-") do |dir|
        # Empty working directory: no project CLAUDE.md, settings or files for Claude to pick up.
        prompt_file = File.join(dir, "system.txt")
        File.write(prompt_file, system)
        out, err, status = run(dir, model, prompt_file, user)

        if !status.success? && @safe_mode && err.match?(/unknown option.*safe-mode/i)
          @safe_mode = false # older Claude Code without --safe-mode
          out, err, status = run(dir, model, prompt_file, user)
        end

        data = parse(out)
        if data.nil? || !status.success? || data["is_error"]
          detail = data&.dig("result") || err.strip.lines.last || "exit #{status.exitstatus}"
          raise Error, "Claude Code call failed: #{detail.to_s.strip[0, 300]}"
        end

        @lock.synchronize { @usage.add(data["usage"] || {}) }
        data["result"].to_s
      end
    end

    private

    def run(dir, model, prompt_file, user)
      args = [
        @bin, "-p",
        "--output-format", "json",
        "--model", model,
        "--system-prompt-file", prompt_file,
        "--tools", "",                # no tools: Claude only reads what we send
        "--strict-mcp-config",        # and no MCP servers either
        "--max-turns", "1",
        "--no-session-persistence"
      ]
      args << "--safe-mode" if @safe_mode
      Open3.capture3(*args, stdin_data: user, chdir: dir)
    end

    def parse(out)
      JSON.parse(out)
    rescue JSON::ParserError
      nil
    end

    def check_installed!
      _o, _e, st = Open3.capture3(@bin, "--version")
      raise Error, "`#{@bin} --version` failed." unless st.success?
    rescue Errno::ENOENT
      raise Error, "Claude Code (`#{@bin}`) is not installed. See https://code.claude.com/docs/en/setup"
    end
  end
end
