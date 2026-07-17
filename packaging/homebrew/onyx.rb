# Homebrew cask (template — version/sha filled per release).
cask "onyx" do
  version "0.1.0"
  sha256 :no_check

  url "https://github.com/onyx-notes/onyx/releases/download/v#{version}/Onyx_#{version}_universal.dmg"
  name "Onyx"
  desc "Fast, private, self-hosted markdown notes"
  homepage "https://github.com/onyx-notes/onyx"

  app "Onyx.app"

  zap trash: [
    "~/Library/Application Support/dev.onyx.app",
    "~/Library/Caches/dev.onyx.app",
  ]
end
