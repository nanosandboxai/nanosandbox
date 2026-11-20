# frozen_string_literal: true

Gem::Specification.new do |spec|
  spec.name          = "nanosandbox-sdk"
  spec.version       = "0.4.0"
  spec.authors       = ["DevDone Labs"]
  spec.summary       = "Ruby SDK for the nanosandbox protocol helper"
  spec.description   = "Stdlib-only Ruby protocol client for the nanosandbox helper (nanosb-protocol v1.0.0)."
  spec.homepage      = "https://github.com/nanosandboxai/nanosandbox"
  spec.license       = "Apache-2.0"
  spec.required_ruby_version = ">= 2.6"

  spec.files         = Dir["lib/**/*.rb"] + ["README.md"]
  spec.require_paths = ["lib"]

  spec.metadata = {
    "homepage_uri"    => "https://github.com/nanosandboxai/nanosandbox",
    "source_code_uri" => "https://github.com/nanosandboxai/nanosandbox/tree/main/sdk/ruby",
    "github_repo"     => "https://github.com/nanosandboxai/nanosandbox",
  }
end
