# Third-Party Notices

MicHost is licensed under the GNU Affero General Public License v3.0 or later (`LICENSE`). It depends on third-party components with their own terms.

## JUCE 8

- Project: https://juce.com
- Licence: dual AGPLv3 / commercial JUCE licence.
- Used by `cpp/` for audio devices, VST3 hosting, GUI and message loop.
- With the open-source path, the combined MicHost application is distributed under AGPLv3. A commercial JUCE licence can be used to distribute under different terms.
- JUCE's own notice is in the JUCE source tree (`LICENSE.md`).

## Steinberg VST3 SDK

- Used through JUCE's VST3 hosting support.
- Licence: GPLv3 or the Steinberg proprietary VST3 licence, at the user's option.
- VST3 is a trademark of Steinberg Media Technologies GmbH. MicHost is not affiliated with or endorsed by Steinberg.

## Steinberg ASIO SDK

- Path on the development machine: `C:\kilo\asio-sdk\sdk\ASIOSDK`.
- Licence: Steinberg ASIO licence (see the SDK's `LICENSE.txt` and `Steinberg ASIO Licensing Agreement.pdf`).
- The SDK is not redistributed by MicHost. Builders must obtain and comply with the SDK licence. ASIO is a trademark of Steinberg Media Technologies GmbH.

## Rust crates

Rust dependencies are declared in the workspace and crate `Cargo.toml` files. Run:

```powershell
cargo metadata --format-version 1
cargo deny check licenses
```

or use `cargo-license` to produce a full dependency licence report. Important dependencies include eframe/egui, tray-icon/muda, rtrb, libloading, serde, base64, rfd and winreg; their licences are compatible with this project's AGPLv3 choice.

## Plugins

VST3 plugins are loaded from the user's machine and are not distributed with MicHost. Users are responsible for plugin licences. FabFilter Pro-G and Pro-Q 4 were used for validation only.
