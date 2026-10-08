# frozen_string_literal: true

require "json"

module Owlmap
  # Runs the two-stage pipeline:
  #   1. Summarise each module in parallel with the fast model (structured JSON).
  #   2. Write ARCHITECTURE, FLOWS and ONBOARDING with the smart model, from the
  #      file tree, manifests and module summaries (never from raw code again).
  class Analyzer
    MANIFEST_NAMES = %w[
      README.md README readme.md package.json Gemfile pyproject.toml requirements.txt
      go.mod Cargo.toml composer.json pom.xml build.gradle mix.exs pubspec.yaml
      Dockerfile docker-compose.yml compose.yaml Makefile Procfile .tool-versions
      config/routes.rb config/database.yml app.json vercel.json
    ].freeze
    MANIFEST_MAX_CHARS = 8_000
    MANIFESTS_TOTAL_MAX = 40_000
    TREE_MAX_LINES = 1_500
    SUMMARY_TOKENS_PER_MODULE = 700 # used only for the up-front estimate

    Plan = Struct.new(:files, :skipped, :modules, :estimated_input_tokens, keyword_init: true)
    Result = Struct.new(:plan, :summaries, :documents, :failures, keyword_init: true)

    def initialize(root:, repo_name:, config:, client: nil, log: $stderr)
      @root = root
      @repo_name = repo_name
      @config = config
      @client = client
      @log = log
    end

    # Everything that can be known without calling the API.
    def plan
      @plan ||= begin
        scan = FileScanner.new(@root, max_file_bytes: @config.max_file_bytes).scan
        if scan.files.empty?
          raise Error, "No source files found to document."
        end
        if scan.files.size > @config.max_files
          raise Error, "#{scan.files.size} source files found; the beta limit is #{@config.max_files}. " \
                       "Use --max-files to raise it if you accept the cost."
        end

        modules = ModuleGrouper.new(scan.files, char_budget: @config.module_char_budget,
                                            min_chars: @config.min_module_chars).group
        Plan.new(files: scan.files, skipped: scan.skipped, modules: modules,
                 estimated_input_tokens: estimate(scan.files, modules))
      end
    end

    def run
      raise Error, "No Claude client configured." unless @client

      p = plan
      if p.estimated_input_tokens > @config.max_input_tokens
        raise Error, "Estimated #{p.estimated_input_tokens} input tokens exceeds the limit of " \
                     "#{@config.max_input_tokens}. Raise --max-input-tokens to proceed."
      end

      say "Summarising #{p.modules.size} modules with #{@config.fast_model}…"
      summaries, failures = summarise_modules(p.modules)

      say "Writing architecture, flows and onboarding with #{@config.smart_model}…"
      documents = synthesise(summaries)

      Result.new(plan: p, summaries: summaries, documents: documents, failures: failures)
    end

    # Public so tests can exercise it directly.
    def self.parse_json_object(text)
      cleaned = text.to_s.strip.sub(/\A```(?:json)?\s*/i, "").sub(/\s*```\z/, "")
      first = cleaned.index("{")
      last = cleaned.rindex("}")
      raise JSON::ParserError, "no JSON object in reply" unless first && last && last > first

      JSON.parse(cleaned[first..last])
    end

    private

    def summarise_modules(modules)
      queue = Queue.new
      modules.each_with_index { |m, i| queue << [m, i] }
      summaries = Array.new(modules.size)
      failures = []
      lock = Mutex.new
      done = 0

      workers = Array.new([@config.concurrency, modules.size].min) do
        Thread.new do
          loop do
            mod, idx = begin
              queue.pop(true)
            rescue ThreadError
              break
            end
            summary = summarise(mod)
            lock.synchronize do
              summaries[idx] = summary
              failures << mod.name if summary["error"]
              done += 1
              say "  [#{done}/#{modules.size}] #{mod.name}#{summary['error'] ? ' (failed)' : ''}"
            end
          end
        end
      end
      workers.each(&:join)
      [summaries, failures]
    end

    def summarise(mod)
      text = mod.files.map { |f| "=== #{f.path} (#{f.language}) ===\n#{f.read(@root)}" }.join("\n\n")
      reply = @client.complete(
        model: @config.fast_model,
        system: Prompts::MODULE_SYSTEM,
        user: Prompts.module_user(mod.name, text),
        max_tokens: 4_000
      )
      data = self.class.parse_json_object(reply)
      base_summary(mod).merge(data.slice(*%w[purpose key_files public_interface depends_on used_by data risks notes]))
    rescue JSON::ParserError
      base_summary(mod).merge("purpose" => "", "notes" => reply.to_s.strip, "parse_error" => true)
    rescue Error => e
      base_summary(mod).merge("error" => e.message)
    end

    def base_summary(mod)
      { "module" => mod.name, "slug" => mod.slug, "files" => mod.files.map(&:path) }
    end

    def synthesise(summaries)
      usable = summaries.reject { |s| s["error"] }
      shared = {
        repo_name: @repo_name,
        tree: tree_text,
        manifests: manifests_text,
        modules_json: JSON.pretty_generate(usable.map { |s| s.except("files") })
      }
      tasks = {
        "ARCHITECTURE.md" => Prompts::ARCHITECTURE_TASK,
        "FLOWS.md" => Prompts::FLOWS_TASK,
        "ONBOARDING.md" => Prompts::ONBOARDING_TASK
      }
      threads = tasks.map do |file, task|
        Thread.new do
          text = @client.complete(
            model: @config.smart_model,
            system: Prompts::SYNTHESIS_SYSTEM,
            user: Prompts.synthesis_user(**shared, task: task),
            max_tokens: 8_000
          )
          [file, strip_outer_fence(text)]
        rescue Error => e
          [file, "# #{file.delete_suffix('.md').capitalize}\n\nOwlMap could not write this document: #{e.message}\n"]
        end
      end
      threads.map(&:value).to_h
    end

    def strip_outer_fence(text)
      t = text.to_s.strip
      t = t.sub(/\A```(?:markdown|md)?\s*\n/i, "").sub(/\n```\s*\z/, "") if t.start_with?("```")
      "#{t}\n"
    end

    def tree_text
      lines = plan.files.map { |f| f.path }
      return lines.join("\n") if lines.size <= TREE_MAX_LINES

      (lines.first(TREE_MAX_LINES) + ["… #{lines.size - TREE_MAX_LINES} more files"]).join("\n")
    end

    def manifests_text
      total = 0
      found = plan.files.select { |f| MANIFEST_NAMES.include?(f.path) }
      parts = []
      found.each do |f|
        break if total >= MANIFESTS_TOTAL_MAX

        body = f.read(@root)[0, MANIFEST_MAX_CHARS]
        total += body.size
        parts << "=== #{f.path} ===\n#{body}"
      end
      parts.empty? ? "(none found)" : parts.join("\n\n")
    end

    def estimate(files, modules)
      prompt_overhead = Owlmap.estimate_tokens(Prompts::MODULE_SYSTEM) + 50
      module_tokens = Owlmap.estimate_tokens(files.sum(&:prompt_size)) + modules.size * prompt_overhead
      tree_tokens = Owlmap.estimate_tokens(files.sum { |f| f.path.size + 1 })
      synthesis_once = tree_tokens + Owlmap.estimate_tokens(MANIFESTS_TOTAL_MAX) / 2 +
                       modules.size * SUMMARY_TOKENS_PER_MODULE + Owlmap.estimate_tokens(Prompts::SYNTHESIS_SYSTEM)
      module_tokens + synthesis_once * 3
    end

    def say(msg)
      @log&.puts(msg)
    end
  end
end
