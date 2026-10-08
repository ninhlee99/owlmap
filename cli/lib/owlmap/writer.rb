# frozen_string_literal: true

require "fileutils"
require "json"
require "time"

module Owlmap
  # Turns an Analyzer::Result into a folder of Markdown files.
  class Writer
    def initialize(out_dir:, source:, config:, usage: nil)
      @out = out_dir
      @source = source
      @config = config
      @usage = usage
    end

    def write(result)
      FileUtils.mkdir_p(File.join(@out, "modules"))
      result.documents.each { |name, text| File.write(File.join(@out, name), text) }
      result.summaries.each do |s|
        File.write(File.join(@out, "modules", "#{s['slug']}.md"), module_markdown(s))
      end
      File.write(File.join(@out, "README.md"), index_markdown(result))
      File.write(File.join(@out, "owlmap.json"), JSON.pretty_generate(metadata(result)))
      @out
    end

    def module_markdown(s)
      out = +"# #{s['module']}\n\n"
      if s["error"]
        out << "OwlMap could not summarise this module: #{s['error']}\n\n"
      elsif s["parse_error"]
        out << "#{s['notes']}\n\n"
      else
        out << "#{s['purpose']}\n\n" unless blank?(s["purpose"])
        out << section("Key files", Array(s["key_files"]).map { |k| "`#{k['path']}` — #{k['role']}" if k.is_a?(Hash) }.compact)
        out << section("Public interface", s["public_interface"])
        out << section("Depends on", s["depends_on"])
        out << section("Used by", s["used_by"])
        out << section("Data", s["data"])
        out << section("Handle with care", s["risks"])
        out << "## Notes\n\n#{s['notes']}\n\n" unless blank?(s["notes"])
      end
      out << "<details><summary>All files in this module (#{s['files'].size})</summary>\n\n"
      out << s["files"].map { |f| "- `#{f}`" }.join("\n")
      out << "\n\n</details>\n"
    end

    private

    def section(title, items)
      items = Array(items).map(&:to_s).reject { |i| blank?(i) }
      return "" if items.empty?

      "## #{title}\n\n#{items.map { |i| "- #{i}" }.join("\n")}\n\n"
    end

    def index_markdown(result)
      plan = result.plan
      langs = plan.files.group_by(&:language).transform_values(&:size).sort_by { |_, n| -n }.first(6)
      rows = result.summaries.map do |s|
        purpose = s["error"] ? "_summary failed_" : first_sentence(s["purpose"])
        "| [#{s['module']}](modules/#{s['slug']}.md) | #{s['files'].size} | #{purpose} |"
      end

      <<~MD
        # #{@source.name} — OwlMap

        Generated documentation for #{@source.url ? "[#{@source.url}](#{@source.url})" : "`#{@source.name}`"}#{@source.commit ? " at commit `#{@source.commit[0, 12]}`" : ""}.

        | Document | What it answers |
        |---|---|
        | [Architecture](ARCHITECTURE.md) | How does the system fit together? |
        | [Key flows](FLOWS.md) | What happens when…? |
        | [Onboarding](ONBOARDING.md) | Where do I start? |

        **#{plan.files.size} files** in **#{plan.modules.size} modules** · #{langs.map { |l, n| "#{l} #{n}" }.join(', ')}

        ## Modules

        | Module | Files | Purpose |
        |---|---|---|
        #{rows.join("\n")}

        ---
        Written by OwlMap with Claude. Review before relying on it: statements marked "Unverified:" are inferences.
      MD
    end

    def metadata(result)
      {
        "owlmap_version" => VERSION,
        "generated_at" => Time.now.utc.iso8601,
        "source" => { "name" => @source.name, "url" => @source.url, "commit" => @source.commit },
        "models" => { "fast" => @config.fast_model, "smart" => @config.smart_model },
        "files" => result.plan.files.size,
        "modules" => result.plan.modules.size,
        "skipped" => result.plan.skipped.transform_keys(&:to_s),
        "estimated_input_tokens" => result.plan.estimated_input_tokens,
        "usage" => @usage&.to_h,
        "failed_modules" => result.failures
      }
    end

    def first_sentence(text)
      t = text.to_s.strip.gsub(/\s+/, " ").gsub("|", "\\|")
      s = t[/\A.+?[.!?](\s|\z)/] || t
      s.strip
    end

    def blank?(v) = v.nil? || v.to_s.strip.empty?
  end
end
