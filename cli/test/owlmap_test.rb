# frozen_string_literal: true

require "minitest/autorun"
require "tmpdir"
require "fileutils"
require_relative "../lib/owlmap"

# Stands in for the Claude API: module calls get JSON, overview calls get Markdown.
class FakeClient
  attr_reader :calls, :usage

  def initialize(module_reply: nil, fail_on: nil)
    @module_reply = module_reply
    @fail_on = fail_on
    @calls = []
    @lock = Mutex.new
    @usage = Owlmap::ClaudeClient::Usage.new(0, 0, 0, 0, 0)
  end

  def complete(model:, system:, user:, max_tokens:)
    @lock.synchronize { @calls << { model: model, system: system, user: user } }
    raise Owlmap::Error, "boom" if @fail_on && user.include?(@fail_on)

    if system == Owlmap::Prompts::MODULE_SYSTEM
      @module_reply || %(```json\n{"purpose":"Handles things. More detail.","key_files":[{"path":"a.rb","role":"main"}],"public_interface":[],"depends_on":["Redis"],"used_by":[],"data":[],"risks":["Tax rates are duplicated"],"notes":""}\n```)
    else
      "```markdown\n# Doc for #{user[/Write (\w+)\.md/, 1]}\n\nBody.\n```"
    end
  end
end

module Fixture
  def make_repo(files)
    dir = Dir.mktmpdir("owlmap-test-")
    files.each do |path, body|
      full = File.join(dir, path)
      FileUtils.mkdir_p(File.dirname(full))
      File.binwrite(full, body)
    end
    dir
  end
end

class FileScannerTest < Minitest::Test
  include Fixture

  def test_keeps_source_and_skips_noise
    root = make_repo(
      "app/models/user.rb" => "class User; end\n",
      "README.md" => "# Hi\n",
      "Gemfile" => "source 'https://rubygems.org'\n",
      "Gemfile.lock" => "GEM\n",
      "node_modules/x/index.js" => "x\n",
      ".env" => "SECRET=1\n",
      "config/master.key" => "abc\n",
      "public/logo.png" => "\x89PNG\x00\x00",
      "assets/app.min.js" => "a\n",
      "lib/blob.rb" => "a\x00b",
      "lib/huge.rb" => "x" * 200_000,
      "lib/empty.rb" => "",
      "CHANGELOG.md" => "v1\n",
      "spec/user_spec.rb" => "describe User\n"
    )
    result = Owlmap::FileScanner.new(root, max_file_bytes: 100_000).scan
    assert_equal %w[Gemfile README.md app/models/user.rb spec/user_spec.rb], result.files.map(&:path)
    assert_equal [false, false, false, true], result.files.map(&:test)
    assert_equal 1, result.skipped[:boilerplate]
    assert_equal 1, result.skipped[:lockfile]
    assert_equal 2, result.skipped[:secret]
    assert_equal 1, result.skipped[:too_large]
    assert_equal 1, result.skipped[:binary]
    assert_equal 1, result.skipped[:minified]
  ensure
    FileUtils.rm_rf(root)
  end

  def test_does_not_follow_symlinks
    root = make_repo("lib/a.rb" => "x\n")
    outside = make_repo("secret.rb" => "TOP SECRET\n")
    File.symlink(outside, File.join(root, "lib", "linked"))
    File.symlink(File.join(outside, "secret.rb"), File.join(root, "lib", "s.rb"))
    paths = Owlmap::FileScanner.new(root, max_file_bytes: 100_000).scan.files.map(&:path)
    assert_equal ["lib/a.rb"], paths
  ensure
    FileUtils.rm_rf(root)
    FileUtils.rm_rf(outside)
  end
end

class ModuleGrouperTest < Minitest::Test
  F = Owlmap::FileScanner::SourceFile

  def files(spec) = spec.map { |path, size| F.new(path: path, size: size, language: "Ruby") }

  def test_splits_containers_one_level_deeper
    mods = Owlmap::ModuleGrouper.new(
      files("app/models/user.rb" => 10, "app/controllers/a.rb" => 10, "config/routes.rb" => 10, "Gemfile" => 5),
      char_budget: 1_000, min_chars: 0
    ).group
    assert_equal ["(root)", "app/controllers", "app/models", "config"], mods.map(&:name)
  end

  def test_folds_tiny_modules
    spec = { "app/models/user.rb" => 5_000, "app/models/concerns/x.rb" => 100,
             "docs/a.md" => 100, "script/b.sh" => 100, "config/c.rb" => 5_000 }
    mods = Owlmap::ModuleGrouper.new(files(spec), char_budget: 50_000).group
    assert_equal ["(small folders)", "app/models", "config"], mods.map(&:name)
    assert_includes mods.find { |m| m.name == "app/models" }.files.map(&:path), "app/models/concerns/x.rb"
  end

  def test_tests_are_grouped_apart
    f = Owlmap::FileScanner::SourceFile
    list = [f.new(path: "lib/a.rb", size: 5_000, language: "Ruby", test: false),
            f.new(path: "test/a_test.rb", size: 90_000, language: "Ruby", test: true),
            f.new(path: "contrib/spec/b_spec.rb", size: 100, language: "Ruby", test: true)]
    mods = Owlmap::ModuleGrouper.new(list, char_budget: 50_000).group
    assert_equal ["contrib (tests)", "lib", "project (tests)"], mods.map(&:name)
    assert_equal 3_600, mods.last.bytes, "test files count at their truncated size"
  end

  def test_oversized_module_splits_by_subfolder_then_parts
    spec = { "lib/a/one.rb" => 600, "lib/a/two.rb" => 600, "lib/b/three.rb" => 300 }
    mods = Owlmap::ModuleGrouper.new(files(spec), char_budget: 1_000, min_chars: 0).group
    assert_equal ["lib/a (part 1)", "lib/a (part 2)", "lib/b"], mods.map(&:name)
  end

  def test_descends_through_single_child_folders
    spec = { "lib/deep/x/a.rb" => 600, "lib/deep/y/b.rb" => 600 }
    mods = Owlmap::ModuleGrouper.new(files(spec), char_budget: 1_000, min_chars: 0).group
    assert_equal ["lib/deep/x", "lib/deep/y"], mods.map(&:name)
  end

  def test_slug
    m = Owlmap::ModuleGrouper::Mod.new(name: "app/models (part 2)", files: [])
    assert_equal "app-models-part-2", m.slug
    assert_equal "root", Owlmap::ModuleGrouper::Mod.new(name: "(root)", files: []).slug
  end
end

class RepoSourceTest < Minitest::Test
  def test_rejects_non_github_and_odd_urls
    ["http://github.com/a/b", "https://gitlab.com/a/b", "https://github.com/a", "https://github.com/a/b;rm -rf",
     "https://github.com/-x/b", "/definitely/not/here"].each do |input|
      assert_raises(Owlmap::Error, input) { Owlmap::RepoSource.resolve(input) }
    end
  end

  def test_accepts_github_url_forms
    re = Owlmap::RepoSource::GITHUB_URL
    %w[https://github.com/rails/rails https://github.com/rails/rails/ https://github.com/rails/rails.git].each do |u|
      m = re.match(u)
      assert m, u
      assert_equal "rails", m[:repo]
    end
  end

  def test_local_folder_is_used_in_place
    dir = Dir.mktmpdir
    src = Owlmap::RepoSource.resolve(dir)
    assert_equal File.expand_path(dir), src.path
    src.cleanup
    assert File.directory?(dir), "cleanup must never delete a local folder"
  ensure
    FileUtils.rm_rf(dir)
  end
end

class AnalyzerTest < Minitest::Test
  include Fixture

  def setup
    @root = make_repo(
      "README.md" => "# Shop\nRun with bin/dev\n",
      "Gemfile" => "gem 'rails'\n",
      "app/models/user.rb" => "class User; end\n",
      "app/services/auth_service.rb" => "class AuthService; end\n",
      "config/routes.rb" => "Rails.application.routes.draw {}\n"
    )
    @config = Owlmap::Config.default
    @config.concurrency = 2
    @config.min_module_chars = 0
  end

  def teardown = FileUtils.rm_rf(@root)

  def test_end_to_end_with_fake_client
    client = FakeClient.new
    result = Owlmap::Analyzer.new(root: @root, repo_name: "shop", config: @config, client: client, log: nil).run

    assert_equal 4, result.summaries.size
    assert_empty result.failures
    assert_equal "Handles things. More detail.", result.summaries.first["purpose"]
    assert_equal %w[ARCHITECTURE.md FLOWS.md ONBOARDING.md], result.documents.keys.sort
    assert result.documents["FLOWS.md"].start_with?("# Doc for FLOWS"), "outer ``` fence should be stripped"

    module_calls = client.calls.count { |c| c[:system] == Owlmap::Prompts::MODULE_SYSTEM }
    assert_equal 4, module_calls
    assert client.calls.select { |c| c[:system] == Owlmap::Prompts::MODULE_SYSTEM }.all? { |c| c[:model] == @config.fast_model }
    synth = client.calls.reject { |c| c[:system] == Owlmap::Prompts::MODULE_SYSTEM }
    assert_equal 3, synth.size
    assert synth.all? { |c| c[:model] == @config.smart_model }
    assert synth.first[:user].include?("=== README.md ==="), "manifests should be included"
    refute synth.first[:user].include?("class AuthService"), "overview calls must not resend raw code"
  end

  def test_module_failure_is_isolated
    client = FakeClient.new(fail_on: "Module: app/models")
    result = Owlmap::Analyzer.new(root: @root, repo_name: "shop", config: @config, client: client, log: nil).run
    assert_equal ["app/models"], result.failures
    assert_equal 3, result.documents.size
  end

  def test_unparseable_reply_is_kept_as_notes
    client = FakeClient.new(module_reply: "Sorry, here is prose instead of JSON.")
    result = Owlmap::Analyzer.new(root: @root, repo_name: "shop", config: @config, client: client, log: nil).run
    s = result.summaries.first
    assert s["parse_error"]
    assert_includes s["notes"], "prose instead"
  end

  def test_refuses_over_budget_before_calling_api
    @config.max_input_tokens = 10
    client = FakeClient.new
    err = assert_raises(Owlmap::Error) do
      Owlmap::Analyzer.new(root: @root, repo_name: "shop", config: @config, client: client, log: nil).run
    end
    assert_match(/exceeds the limit/, err.message)
    assert_empty client.calls
  end

  def test_refuses_too_many_files
    @config.max_files = 2
    assert_raises(Owlmap::Error) do
      Owlmap::Analyzer.new(root: @root, repo_name: "shop", config: @config, log: nil).plan
    end
  end

  def test_parse_json_object_tolerates_fences_and_chatter
    obj = Owlmap::Analyzer.parse_json_object("Here you go:\n```json\n{\"a\": 1}\n```\nThanks")
    assert_equal({ "a" => 1 }, obj)
  end
end

class WriterTest < Minitest::Test
  include Fixture

  def test_writes_full_doc_set
    root = make_repo("app/models/user.rb" => "class User; end\n", "lib/tax.rb" => "RATE = 0.1\n")
    out = Dir.mktmpdir
    config = Owlmap::Config.default
    config.min_module_chars = 0
    client = FakeClient.new(fail_on: "Module: lib")
    result = Owlmap::Analyzer.new(root: root, repo_name: "demo", config: config, client: client, log: nil).run
    source = Struct.new(:name, :url, :commit).new("demo", "https://github.com/acme/demo", "abcdef1234567890")
    Owlmap::Writer.new(out_dir: out, source: source, config: config, usage: client.usage).write(result)

    assert_equal %w[ARCHITECTURE.md FLOWS.md ONBOARDING.md README.md modules owlmap.json],
                 Dir.children(out).sort
    assert_equal %w[app-models.md lib.md], Dir.children(File.join(out, "modules")).sort
    readme = File.read(File.join(out, "README.md"), encoding: "UTF-8")
    assert_includes readme, "[app/models](modules/app-models.md)"
    assert_includes readme, "_summary failed_"
    assert_includes readme, "`abcdef123456`"
    mod = File.read(File.join(out, "modules", "app-models.md"), encoding: "UTF-8")
    assert_includes mod, "## Handle with care"
    assert_includes mod, "`a.rb` — main"
    meta = JSON.parse(File.read(File.join(out, "owlmap.json")))
    assert_equal ["lib"], meta["failed_modules"]
  ensure
    FileUtils.rm_rf(root)
    FileUtils.rm_rf(out)
  end
end
