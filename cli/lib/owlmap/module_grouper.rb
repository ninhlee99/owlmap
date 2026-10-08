# frozen_string_literal: true

module Owlmap
  # Groups files into "modules" — the units Claude summarises one at a time.
  #
  # 1. A module is usually a directory. Conventional containers (app/, src/,
  #    lib/…) are split one level deeper, so app/models and app/controllers
  #    become their own modules.
  # 2. A module larger than the character budget is split by the next directory
  #    level; a single directory that is still too large is cut into parts.
  # 3. Tiny modules are folded into their parent folder, and tiny top-level
  #    folders are batched together, so no API call is spent on two files.
  # 4. Tests are kept apart from the code they test, one module per top-level
  #    area, and are read in truncated form (see FileScanner::SourceFile).
  class ModuleGrouper
    Mod = Struct.new(:name, :files, keyword_init: true) do
      def bytes = files.sum(&:prompt_size)
      def slug = name.downcase.gsub(/[^a-z0-9]+/, "-").gsub(/\A-|-\z/, "").then { |s| s.empty? ? "root" : s }
    end

    CONTAINERS = %w[app src lib packages apps internal pkg cmd services modules components features].freeze
    ROOT = "(root)"
    SMALL_FOLDERS = "(small folders)"

    def initialize(files, char_budget:, min_chars: 3_000)
      @files = files
      @budget = char_budget
      @min = min_chars
    end

    def group
      code, tests = @files.partition { |f| !f.test }
      modules = code.group_by { |f| base_key(f.path) }
                    .flat_map { |key, files| split(key, files, depth_of(key)) }
      modules = fold_small(modules)
      modules += test_modules(tests)
      modules.sort_by(&:name)
    end

    private

    def base_key(path)
      parts = File.dirname(path).split("/")
      return ROOT if parts == ["."]
      return parts.first(2).join("/") if CONTAINERS.include?(parts.first) && parts.length >= 2

      parts.first
    end

    def depth_of(key)
      key == ROOT ? 0 : key.count("/") + 1
    end

    def split(key, files, depth)
      mod = Mod.new(name: key, files: files)
      return [mod] if mod.bytes <= @budget

      deeper = files.group_by { |f| key_at(f.path, depth + 1) }
      return deeper.flat_map { |k, fs| split(k, fs, depth + 1) } if deeper.size > 1

      # Everything sits in one deeper folder (e.g. lib/foo/bar/*): keep descending.
      only = deeper.keys.first
      return split(only, files, depth + 1) if only != key

      parts(key, files)
    end

    def parts(key, files)
      chunks = []
      current = []
      size = 0
      files.each do |f|
        if size + f.prompt_size > @budget && current.any?
          chunks << current
          current = []
          size = 0
        end
        current << f
        size += f.prompt_size
      end
      chunks << current if current.any?
      return [Mod.new(name: key, files: chunks.first)] if chunks.size == 1

      chunks.each_with_index.map { |fs, i| Mod.new(name: "#{key} (part #{i + 1})", files: fs) }
    end

    # Fold tiny nested modules into their parent folder (when that keeps the
    # parent within budget), then batch tiny top-level folders together.
    def fold_small(modules)
      loop do
        by_name = modules.to_h { |m| [m.name, m] }
        small = modules.find do |m|
          next false unless m.bytes < @min && m.name.include?("/") && !m.name.include?("(part")

          parent = by_name[File.dirname(m.name)]
          parent.nil? || parent.bytes + m.bytes <= @budget
        end
        break unless small

        parent_name = File.dirname(small.name)
        parent = by_name[parent_name]
        modules -= [small, parent].compact
        modules << Mod.new(name: parent_name, files: (parent&.files || []) + small.files)
      end

      tiny_top = modules.select { |m| m.bytes < @min && !m.name.include?("/") && m.name != ROOT }
      return modules if tiny_top.size < 2

      rest = modules - tiny_top
      rest + parts(SMALL_FOLDERS, tiny_top.flat_map(&:files))
    end

    def test_modules(tests)
      tests.group_by { |f| top_area(f.path) }
           .flat_map { |area, files| parts("#{area} (tests)", files) }
    end

    def top_area(path)
      first = path.split("/").first
      first.match?(/\A(test|tests|spec|specs|__tests__|e2e|cypress)\z/i) || !path.include?("/") ? "project" : first
    end

    def key_at(path, depth)
      parts = File.dirname(path).split("/")
      return ROOT if parts == ["."]

      parts.first([depth, parts.length].min).join("/")
    end
  end
end
