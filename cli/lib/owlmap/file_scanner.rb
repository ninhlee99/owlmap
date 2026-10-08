# frozen_string_literal: true

require "find"

module Owlmap
  # Walks a repository and keeps the files worth reading: source code, config
  # and docs. Skips dependencies, build output, binaries, lockfiles, secrets
  # and anything too large to be useful to a reader.
  class FileScanner
    TEST_LINES = 60 # tests are summarised from their opening lines only

    SourceFile = Struct.new(:path, :size, :language, :test, keyword_init: true) do
      def read(root)
        text = File.read(File.join(root, path), mode: "r:UTF-8", invalid: :replace, undef: :replace)
        return text unless test

        lines = text.lines
        return text if lines.size <= TEST_LINES

        lines.first(TEST_LINES).join + "\n… (#{lines.size - TEST_LINES} more lines of tests not shown)\n"
      end

      # Characters this file contributes to a prompt (tests are truncated).
      def prompt_size = test ? [size, TEST_LINES * 60].min : size
    end

    TEST_PATH = %r{(\A|/)(test|tests|spec|specs|__tests__|e2e|cypress)/|(_test|_spec|\.test|\.spec)\.[a-z]+\z}i
    # Project boilerplate that says little about how the code works.
    BOILERPLATE = /\A(CHANGELOG|CHANGES|HISTORY|NEWS|LICENSE|LICENCE|COPYING|CODE_OF_CONDUCT|CONTRIBUTING|SECURITY|AUTHORS|CONTRIBUTORS)(\.[a-z]+)?\z/i

    Result = Struct.new(:files, :skipped, keyword_init: true) do
      def total_bytes = files.sum(&:size)
    end

    IGNORED_DIRS = %w[
      .git .hg .svn node_modules vendor bower_components .bundle
      dist build out target bin obj .next .nuxt .svelte-kit .turbo .cache coverage
      tmp log logs public/assets public/packs storage
      __pycache__ .venv venv env .tox .mypy_cache .pytest_cache
      .idea .vscode .gradle Pods DerivedData
    ].freeze

    IGNORED_FILES = %w[
      package-lock.json yarn.lock pnpm-lock.yaml bun.lockb Gemfile.lock Cargo.lock
      composer.lock poetry.lock Pipfile.lock go.sum mix.lock pubspec.lock
      .DS_Store
    ].freeze

    # Never read anything that commonly holds credentials.
    SECRET_PATTERNS = [/\A\.env(\..*)?\z/, /\.pem\z/, /\.key\z/, /\Aid_(rsa|ed25519)/, /credentials/i, /\.p12\z/].freeze

    LANGUAGES = {
      ".rb" => "Ruby", ".rake" => "Ruby", ".erb" => "ERB", ".haml" => "Haml", ".slim" => "Slim",
      ".js" => "JavaScript", ".jsx" => "JavaScript", ".mjs" => "JavaScript", ".cjs" => "JavaScript",
      ".ts" => "TypeScript", ".tsx" => "TypeScript", ".vue" => "Vue", ".svelte" => "Svelte",
      ".py" => "Python", ".go" => "Go", ".rs" => "Rust", ".java" => "Java", ".kt" => "Kotlin",
      ".swift" => "Swift", ".php" => "PHP", ".cs" => "C#", ".c" => "C", ".h" => "C", ".cpp" => "C++",
      ".hpp" => "C++", ".scala" => "Scala", ".ex" => "Elixir", ".exs" => "Elixir", ".dart" => "Dart",
      ".sql" => "SQL", ".graphql" => "GraphQL", ".proto" => "Protobuf",
      ".sh" => "Shell", ".yml" => "YAML", ".yaml" => "YAML", ".json" => "JSON", ".toml" => "TOML",
      ".md" => "Markdown", ".html" => "HTML", ".css" => "CSS", ".scss" => "SCSS"
    }.freeze

    # Extension-less files that still describe the project.
    NAMED_FILES = %w[Gemfile Rakefile Dockerfile Makefile Procfile Brewfile Podfile].freeze

    def initialize(root, max_file_bytes:)
      @root = root
      @max_file_bytes = max_file_bytes
    end

    def scan
      files = []
      skipped = Hash.new(0)

      Find.find(@root) do |abs|
        rel = abs.delete_prefix(@root).delete_prefix("/")
        next if rel.empty?

        if File.symlink?(abs)
          skipped[:symlink] += 1
          Find.prune if File.directory?(abs)
          next
        end

        if File.directory?(abs)
          Find.prune if ignored_dir?(rel)
          next
        end

        reason = skip_reason(abs, rel)
        if reason
          skipped[reason] += 1
        else
          files << SourceFile.new(path: rel, size: File.size(abs), language: language_for(rel),
                                  test: rel.match?(TEST_PATH))
        end
      end

      Result.new(files: files.sort_by(&:path), skipped: skipped)
    end

    private

    def ignored_dir?(rel)
      IGNORED_DIRS.include?(File.basename(rel)) || IGNORED_DIRS.include?(rel)
    end

    def skip_reason(abs, rel)
      base = File.basename(rel)
      return :lockfile if IGNORED_FILES.include?(base)
      return :boilerplate if base.match?(BOILERPLATE)
      return :secret if SECRET_PATTERNS.any? { |re| base.match?(re) }
      return :minified if base.match?(/\.min\.(js|css)\z/)
      return :unsupported unless language_for(rel)
      return :too_large if File.size(abs) > @max_file_bytes
      return :empty if File.zero?(abs)
      return :binary if binary?(abs)

      nil
    end

    def language_for(rel)
      base = File.basename(rel)
      return "Build/config" if NAMED_FILES.include?(base)

      LANGUAGES[File.extname(rel).downcase]
    end

    def binary?(abs)
      sample = File.binread(abs, 4096) || ""
      sample.include?("\x00")
    end
  end
end
