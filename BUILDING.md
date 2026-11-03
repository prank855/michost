# Building MicHost

## Prerequisites

- Windows 10 or 11, 64-bit.
- Git.
- Rust stable with the MSVC target. The repository's `rust-toolchain.toml` selects stable; install it with [rustup](https://rustup.rs/).
- Visual Studio 2022 (or newer) Build Tools with the **Desktop development with C++** workload.
- CMake 3.22 or newer on `PATH` (or installed with Visual Studio).
- Python is not required.

CMake fetches JUCE 8.0.7 automatically when `MICHOST_JUCE_DIR` is not set. The first configure therefore needs network access and may take a few minutes.

## Build without ASIO

ASIO support requires the Steinberg ASIO SDK, which is not redistributed here. Omit it for a WASAPI-only build:

```powershell
pwsh -NoProfile -File scripts/build.ps1 -Configuration Release -NoAsio
```

The build writes `michost.exe`, `michost-probe.exe` and `mic_host.dll` to `target\release\`. Keep the three files together.

## Build with ASIO

Obtain the Steinberg ASIO SDK and point the build at the directory that contains `common/`:

```powershell
pwsh -NoProfile -File scripts/build.ps1 -Configuration Release -AsioDir C:\path\to\ASIOSDK
```

You can also set the `ASIO_SDK_DIR` environment variable or `MICHOST_JUCE_DIR` for an existing JUCE checkout. The SDK is not redistributed by MicHost; comply with the Steinberg ASIO licence.

## Package

Build first, then stage the release zip:

```powershell
pwsh -NoProfile -File scripts/build.ps1 -Configuration Release -NoAsio
pwsh -NoProfile -File scripts/package.ps1 -Configuration Release -Version 0.1.0 -SkipBuild
```

Without `-SkipBuild`, `package.ps1` runs `build.ps1` without `-NoAsio`, so it requires an ASIO SDK. The staged folder and zip are written to `target\dist\`.

## Tests

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

The headless native checks and their requirements are listed in `docs/testing.md` in the development repository; they are not required to build a release.

## Virtual microphone driver

The virtual microphone driver in `driver/` is a separate WDK/MSBuild product. It is not built by `build.ps1` unless a Windows Driver Kit is installed or `-Driver` is passed. Builds are unsigned by default. See `driver/README.md`.

## Release builds

The release workflow runs only when the workspace version in `Cargo.toml` changes on `main`. It compares the current version with the previous commit and does nothing if the version is unchanged or the matching tag already exists. When the version is new, it downloads the official Steinberg ASIO SDK 2.3.4, verifies its SHA-512, builds with `-AsioDir`, packages the binaries, writes a SHA-256 checksum, and creates the `v<version>` tag and GitHub Release. Documentation-only changes do not build or release. The SDK is fetched at build time and is not stored in this repository.

To reproduce the release locally, download the same SDK from `https://www.steinberg.net/asiosdk`, extract it, and pass the directory that contains `common/`:

```powershell
pwsh -NoProfile -File scripts/build.ps1 -Configuration Release -AsioDir C:\path\to\ASIOSDK
pwsh -NoProfile -File scripts/package.ps1 -Configuration Release -Version 0.1.0 -SkipBuild
```
