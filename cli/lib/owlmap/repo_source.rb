# frozen_string_literal: true

require "open3"
require "tmpdir"
require "fileutils"

module Owlmap
  # Resolves the user's input to a directory on disk.
  #   - A public GitHub URL is shallow-cloned into a temp dir, removed by #cleanup.
  #   - A local path is used in place and never modified or removed.
  class RepoSource
    GITHUB_URL = %r{\Ahttps://github\.com/(?<owner>[A-Za-z0-9](?:[A-Za-z0-9-]{0,38}))/(?<repo>[A-Za-z0-9._-]{1,100}?)(?:\.git)?/?\z}

    attr_reader :path, :name, :commit, :url

    def self.resolve(input)
      new(input).tap(&:prepare)
    end

    def initialize(input)
      @input = input.to_s.strip
      @tmpdir = nil
    end

    def prepare
      if (m = GITHUB_URL.match(@input))
        @url = "https://github.com/#{m[:owner]}/#{m[:repo]}"
        @name = "#{m[:owner]}-#{m[:repo]}"
        clone!
      elsif File.directory?(@input)
        @path = File.expand_path(@input)
        @name = File.basename(@path)
        @commit = git_head(@path)
      else
        raise Error, "Not a public GitHub URL (https://github.com/owner/repo) or a local folder: #{@input}"
      end
      self
    end

    def cleanup
      FileUtils.rm_rf(@tmpdir) if @tmpdir
      @tmpdir = nil
    end

    private

    def clone!
      @tmpdir = Dir.mktmpdir("owlmap-")
      @path = File.join(@tmpdir, "repo")
      env = { "GIT_TERMINAL_PROMPT" => "0" } # never prompt for credentials: public repos only
      # Arguments are passed as an array, so nothing in the URL reaches a shell.
      _out, err, status = Open3.capture3(
        env, "git", "-c", "core.symlinks=false", "clone", "--depth", "1", "--quiet", "--", @url, @path
      )
      unless status.success?
        cleanup
        raise Error, "Could not clone #{@url}. Is it a public repository? (#{err.strip.lines.last&.strip})"
      end
      @commit = git_head(@path)
    end

    def git_head(dir)
      out, _err, status = Open3.capture3("git", "-C", dir, "rev-parse", "HEAD")
      status.success? ? out.strip : nil
    end
  end
end
