# frozen_string_literal: true

require "net/http"
require "json"
require "uri"

module Owlmap
  # Minimal Claude Messages API client using only the Ruby standard library.
  # Retries rate limits, overloads and server errors with exponential backoff,
  # and keeps a running total of token usage for the run report.
  class ClaudeClient
    ENDPOINT = URI("https://api.anthropic.com/v1/messages")
    API_VERSION = "2023-06-01"
    RETRYABLE = [408, 429, 500, 502, 503, 504, 529].freeze

    Usage = Struct.new(:input_tokens, :output_tokens, :cache_read_tokens, :cache_write_tokens, :calls) do
      def add(u)
        self.input_tokens += u.fetch("input_tokens", 0).to_i
        self.output_tokens += u.fetch("output_tokens", 0).to_i
        self.cache_read_tokens += u.fetch("cache_read_input_tokens", 0).to_i
        self.cache_write_tokens += u.fetch("cache_creation_input_tokens", 0).to_i
        self.calls += 1
      end

      def to_h = super.transform_keys(&:to_s)
    end

    attr_reader :usage

    def initialize(api_key: ENV["ANTHROPIC_API_KEY"], max_retries: 5, timeout: 300)
      raise Error, "Set ANTHROPIC_API_KEY (create one in the Claude Console)." if api_key.to_s.empty?

      @api_key = api_key
      @max_retries = max_retries
      @timeout = timeout
      @usage = Usage.new(0, 0, 0, 0, 0)
      @usage_lock = Mutex.new
    end

    # Returns the text of Claude's reply.
    # The system prompt is marked cacheable: it is identical across every module
    # call in a run, so repeated calls read it from the prompt cache.
    def complete(model:, system:, user:, max_tokens:)
      body = {
        model: model,
        max_tokens: max_tokens,
        system: [{ type: "text", text: system, cache_control: { type: "ephemeral" } }],
        messages: [{ role: "user", content: user }]
      }
      data = post_with_retries(body)
      @usage_lock.synchronize { @usage.add(data.fetch("usage", {})) }

      if data["stop_reason"] == "max_tokens"
        warn "owlmap: a reply was cut off at max_tokens (#{model}); output may be incomplete."
      end
      data.fetch("content", []).select { |c| c["type"] == "text" }.map { |c| c["text"] }.join
    end

    private

    def post_with_retries(body)
      attempt = 0
      begin
        attempt += 1
        response = post(body)
        code = response.code.to_i
        return JSON.parse(response.body) if code == 200

        if RETRYABLE.include?(code) && attempt <= @max_retries
          wait = retry_after(response) || backoff(attempt)
          sleep(wait)
          raise RetryNow
        end
        raise Error, "Claude API error #{code}: #{error_message(response.body)}"
      rescue RetryNow
        retry
      rescue Net::OpenTimeout, Net::ReadTimeout, Errno::ECONNRESET, EOFError => e
        raise Error, "Network error talking to the Claude API: #{e.class}" if attempt > @max_retries

        sleep(backoff(attempt))
        retry
      end
    end

    class RetryNow < StandardError; end

    def post(body)
      http = Net::HTTP.new(ENDPOINT.host, ENDPOINT.port)
      http.use_ssl = true
      http.open_timeout = 20
      http.read_timeout = @timeout
      req = Net::HTTP::Post.new(ENDPOINT)
      req["x-api-key"] = @api_key
      req["anthropic-version"] = API_VERSION
      req["content-type"] = "application/json"
      req.body = JSON.generate(body)
      http.request(req)
    end

    def retry_after(response)
      v = response["retry-after"]
      v && v.to_f.positive? ? [v.to_f, 60].min : nil
    end

    def backoff(attempt)
      [2**attempt + rand, 60].min
    end

    def error_message(body)
      JSON.parse(body).dig("error", "message") || body[0, 300]
    rescue JSON::ParserError
      body.to_s[0, 300]
    end
  end
end
