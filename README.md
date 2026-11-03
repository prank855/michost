<h1><img src="assets/icon.png" width="44" height="44" alt="MicHost icon"> MicHost</h1>

MicHost gives any Windows app a better microphone. It takes your mic, runs it through a chain of VST3 plugins, and sends the processed audio to a virtual cable or another output device.

Use it with Discord, Teams, Zoom, OBS, games, or any app that lets you choose a microphone.

The signal path is simple:

```text
microphone -> MicHost -> plugins and effects -> virtual cable or output device -> Discord, Teams, OBS, ...
```

## Download

**[Download the latest MicHost release](https://github.com/prank855/michost/releases/latest)**

Windows 10 or 11, 64-bit. No installer.

1. Download `MicHost-<version>.zip` from the release page.
2. Extract the zip.
3. Run `michost.exe`.

Keep `michost.exe`, `michost-probe.exe` and `mic_host.dll` in the same folder. Release builds include both ASIO and WASAPI device support.

Windows SmartScreen may warn because the app is not code-signed. Choose **More info**, then **Run anyway** only if you trust the source.

## Quick start

1. **Start `michost.exe`.** The main window opens and audio starts automatically.
2. **Choose your microphone** under **Input**.
3. **Choose where the processed audio goes** under **Output**. To use MicHost in Discord, Teams, OBS or another app, select a virtual cable such as VB-CABLE.
4. **Add effects** with **+ Add Plugin**.
5. **Select the cable's recording side** as the microphone in the app that should receive your voice.
6. **Speak** and watch the **IN** and **OUT** meters in the header.

If you are unsure which API to use, leave it on **Automatic**. Enable **Advanced Mode** to choose WASAPI shared, WASAPI low-latency, WASAPI exclusive or ASIO explicitly.

## What MicHost can do

- Route input and output through different audio APIs, including ASIO and WASAPI.
- Use mono or stereo input; mono can take the left or right hardware channel.
- Run VST3 plugins in real time with bypass, reorder, add and remove while audio keeps running.
- Add a sidechain input where a plugin supports it.
- Monitor the processed voice on headphones without changing the main output.
- Measure the live chain and cable latency instead of relying on an estimate.
- Save projects automatically and reopen the last one.

## Using MicHost

### The plugin chain

- **Reorder**: drag the row grip (the `::` handle), or use Up/Down.
- **Bypass**: compare the plugin with it switched out.
- **Edit**: open the plugin's own window. Audio must be running.
- **Remove**: take the plugin out of the chain; audio keeps running.
- **Sidechain**: choose an auxiliary input where the plugin supports it.

Each row shows the plugin's latency, CPU use and approximate memory use. **+ Add Plugin** stays pinned below the list, so it remains visible in long chains.

### Monitoring and latency

- The headphone icon in the header also plays the chain on your default playback device. Use headphones to avoid feedback.
- The header shows a host latency estimate. **Measure** in the Format section runs a short marker through the live chain and the output cable, then reports a measured p50/p95 latency.
- The receiving app's buffer is not included in the measurement.
- Input and output buffers are independent. A value of 0 uses the device's default.

### Projects and data

MicHost saves automatically and reopens the last project.

- Project: `%APPDATA%\MicHost\project.json`
- Last project path: `%APPDATA%\MicHost\last-project.txt`
- Device and plugin caches, and the log: `%LOCALAPPDATA%\MicHost\`

**Save project as...** and **Open project...** start in `%APPDATA%\MicHost`.

### Plugin scanning

Open **Scan VST3** to find installed VST3 plugins. Scanning uses plugin metadata only and does not open plugin windows.

**Folders** lists the default VST3 locations and any custom folders. Uncheck a default location to skip it during scans; the location stays in the list. Add custom folders for plugins installed elsewhere. Press **Rescan now** after changing folders.

### Tray and window

- The tray icon is always available. Right-click it for **Open MicHost**, **Start Engine**, **Stop Engine** and **Quit**.
- **Close to tray** controls what the window X does: minimize to the tray or quit.
- Launching `michost.exe` again brings the running window to the front.
- The taskbar button is shown while the main window is open.

## Requirements

- Windows 10 or 11, 64-bit.
- A microphone and an output device (speakers, an audio interface, or a virtual cable).
- 64-bit VST3 plugins for effects.
- Microsoft Visual C++ 2015-2022 Redistributable (x64) if the release package does not include the runtime DLLs.

## Troubleshooting

- **No audio**: confirm the input and output devices, the selected API, and that the window shows **Running**. Check that the output is not muted. For ASIO, close other apps that may be using the driver.
- **A plugin is missing**: run **Scan VST3**, check **Folders**, and confirm the plugin is a 64-bit VST3. Rescan after installing new plugins.
- **High latency**: try ASIO or WASAPI low-latency/exclusive, lower the buffers, and use **Measure**. Virtual cables and the receiving app add latency outside MicHost.
- **Howling or feedback**: use headphones or a virtual cable instead of speakers.
- **Startup is slow**: some plugins take several seconds to initialize; the window shows which plugin is loading.
- **Diagnostics**: the log is at `%LOCALAPPDATA%\MicHost\michost.log`.

## Building from source

See [BUILDING.md](BUILDING.md). Release builds are produced from the tagged source and published on the repository's Releases page.

## License

MicHost is licensed under AGPL-3.0-or-later. See `LICENSE` and `THIRD_PARTY_NOTICES.md`.
