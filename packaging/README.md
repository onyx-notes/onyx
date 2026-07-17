# Packaging channels

Onyx ships through these channels at beta. The release workflow builds the
installers; the manifests here are updated per release (version + hashes).

| Channel | File | Notes |
|---|---|---|
| Homebrew (macOS) | `homebrew/onyx.rb` | cask, `brew install --cask onyx` |
| winget (Windows) | `winget/Onyx.yaml` | `winget install Onyx` |
| AUR (Arch) | `aur/PKGBUILD` | `yay -S onyx-bin` |
| Flatpak | via Flathub manifest (submitted separately) |
| .deb / AppImage | built by the release workflow (Tauri) |

The build itself is in `.github/workflows/release.yml`; signing and
notarization activate when the maintainer adds the documented secrets.
