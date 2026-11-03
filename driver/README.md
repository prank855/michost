# MicHost virtual microphone driver (capture-only)

This directory contains the source and packaging for a **capture-only** PortCls/WaveRT
virtual microphone. It exposes one microphone endpoint and a control device that MicHost
uses to publish PCM into a named shared section. There is no render endpoint and no
cable.

> **Build status:** this source has **not been compiled or loaded** in the MicHost
> development environment because no WDK is installed (`...\Windows Kits\10\Include\*\km`
> is absent). Treat the MSBuild/INF package as unverified until it has been built and
> installed on a VS 2022 + WDK 11 machine. Do not claim the driver works before the
> checklist at the end passes.

## Layout

| Path | Purpose |
|---|---|
| `inc/mh_vmic.h` | C99 shared control ABI, ring layout, IOCTLs, struct sizes/static asserts |
| `sys/driver.cpp` | `DriverEntry`, adapter/power management, control device, section/ring, IOCTLs |
| `sys/driver.h` | Internal driver state and helpers |
| `sys/minwavert.cpp/.h` | Capture WaveRT miniport and stream (`GetReadPacket`, notifications, timer) |
| `sys/mintopo.cpp/.h` | Topology filter, microphone node, jack description |
| `mh_vmic.inf` | Root-enumerated `Root\MicHostVirtualMic`, amd64 + arm64 |
| `mh_vmic.vcxproj`, `mh_vmic.sln` | WDK MSBuild driver project (x64/ARM64, Debug/Release) |
| `../scripts/build-driver.ps1` | Locates VS/WDK and builds into `target/driver/<Configuration>/<arch>/` |

## Prerequisites

- Windows 10 2004+ or Windows 11 development VM (recommended), x64 host. The driver uses
  `ExAllocatePool2`/`ExSetTimer`, so older Windows 10 builds are not targeted.
- Visual Studio 2022 with the **Desktop development with C++** workload.
- **Windows Driver Kit (WDK) 11** for the same Visual Studio version, including the
  Windows Driver Kit Visual Studio extension and the WDK MSBuild integration.
- Windows SDK matching the WDK (the build script passes the latest installed
  `Include\<version>\km` version to MSBuild).
- `devcon.exe` from the WDK (optional; for installation and removal).
- Administrator rights for signing and installation.

`build-driver.ps1` uses `vswhere.exe` to find VS 2022 (17.x) and searches the WDK
`Installed Roots` registry value plus `%ProgramFiles(x86)%\Windows Kits\10` for
`Include\<version>\km\wdm.h`.

## Build

```powershell
# One architecture (default), output target\driver\Release\x64\
pwsh -NoProfile -File scripts/build-driver.ps1 -Configuration Release -Architecture x64

# Both architectures
pwsh -NoProfile -File scripts/build-driver.ps1 -Configuration Release -Architecture x64,arm64

# Debug
pwsh -NoProfile -File scripts/build-driver.ps1 -Configuration Debug -Architecture x64
```

Output is expected at:

```
target\driver\<Configuration>\x64\mh_vmic.sys
target\driver\<Configuration>\x64\mh_vmic.inf
target\driver\<Configuration>\x64\mh_vmic.cat
target\driver\<Configuration>\ARM64\...
```

If Visual Studio 2022 or the WDK is missing, the script prints
`skipped: WDK not installed (...)` and exits 0. Pass `-RequireDriver` to make that a hard
failure. A driver compile failure is always an error when the script is run directly.

`scripts/build-driver.ps1` stamps `DriverVer` with `-DriverVersion` (default
`0.1.0.0`) and a fresh date, cleans the per-architecture output directory before each
build, and fails if Inf2Cat did not produce a catalog.

`scripts/build.ps1` has the same optional step:

```powershell
pwsh -NoProfile -File scripts/build.ps1                 # runs driver step only if WDK found
pwsh -NoProfile -File scripts/build.ps1 -Driver         # require the driver step
pwsh -NoProfile -File scripts/build.ps1 -RequireDriver  # require WDK and driver build
pwsh -NoProfile -File scripts/build.ps1 -DriverArchitectures x64,arm64
```

An automatically detected driver failure does not fail the application build; `-Driver`
or `-RequireDriver` does. The app build and package are unchanged when no driver output is
present.

`scripts/package.ps1 -IncludeDriver` stages complete architecture folders into
`target\dist\MicHost-<version>[-<Configuration>]\driver\<arch>\` plus
`driver\README.md`. An architecture is staged only when `.sys`, `.inf` and `.cat` are all
present. Driver staging is opt-in (`-IncludeDriver`) so a normal package does not ship an
unsigned test driver. No driver output means no `driver\` folder and an otherwise identical
package.

## Control ABI

The public header is `driver/inc/mh_vmic.h`. It is standalone C99 (no WDK types) and has
static assertions for every public structure.

- Device: `\\.\MicHostVirtualMic` (`\Device\MicHostVirtualMic`, symlink
  `\DosDevices\MicHostVirtualMic`).
- `FILE_DEVICE_UNKNOWN` = `0x22`, `METHOD_BUFFERED`, `FILE_ANY_ACCESS`.
- IOCTL values (defined with `CTL_CODE` in the header):

| IOCTL | Value | Input / output |
|---|---:|---|
| `MH_VMIC_IOCTL_GET_VERSION` | `0x222000` | out `uint32_t` version (1) |
| `MH_VMIC_IOCTL_OPEN_WRITER` | `0x222004` | in `MH_VMIC_OPEN_WRITER` (12 B) |
| `MH_VMIC_IOCTL_CLOSE_WRITER` | `0x222008` | none |
| `MH_VMIC_IOCTL_GET_STATS` | `0x22200C` | out `MH_VMIC_STATS` (32 B) |
| `MH_VMIC_IOCTL_SET_PACKET_FRAMES` | `0x222010` | in `MH_VMIC_SET_PACKET_FRAMES` (4 B) |
| `MH_VMIC_IOCTL_GET_SECTION_INFO` | `0x222014` | out `MH_VMIC_SECTION_INFO` (24 B) + UTF-16LE name, no NUL |

`MH_VMIC_RING` is exactly 80 bytes: 8 x `uint32_t` (`magic` `0x4D48564D`, version 1,
sample rate, channels, bytes per sample = 4, packet frames, ring frames, writer active)
followed by 6 x `uint64_t` (`write_index`, `read_index`, `overruns`, `underruns`,
`qpc_frequency`, `last_write_qpc`), then interleaved float32 samples.

Section rules:

- Created on `OPEN_WRITER` in the calling session at
  `\Sessions\<id>\BaseNamedObjects\<name>` with a DACL restricted to SYSTEM, Builtin
  Administrators and INTERACTIVE (`S-1-5-4`). The IOCTL returns the bare `<name>` (no
  session or `BaseNamedObjects` prefix), and the name is generated from 128 random bits
  per driver load. `GET_SECTION_INFO` returns the name only to the handle that owns
  `OPEN_WRITER`; other processes cannot obtain it and therefore cannot map the ring. The
  section is re-created if a writer in another session opens the device.
- Capacity is `max(4 * packet_frames, 2048)` rounded up to a power of two; the ring must
  hold at least two packets.
- Defaults: 48 kHz, 2 channels, 480-frame packets, float32.
- MicHost writes `write_index`, `overruns`, `last_write_qpc`. The driver writes
  `read_index`, `underruns`, `qpc_frequency` and `writer_active`.
- The first `OPEN_WRITER` wins; later opens fail with `STATUS_DEVICE_BUSY`.
- On `IRP_MJ_CLEANUP` the driver releases the writer, zeroes the data area, sets
  `writer_active = 0`, keeps the endpoint present and silent, and keeps the section
  alive for a later open.
- `GET_STATS.ConsumerActive` is 1 while a WaveRT capture stream is running.

## Driver behavior

- WaveRT filter: one capture host pin, a bridge pin, and an ADC node. The topology filter
  has one integrated microphone pin, a `KSNODETYPE_MICROPHONE` node, and a bridge pin.
  The INF sets `PKEY_AudioEndpoint_FormFactor = 4` (Microphone) and jack description.
- `AllocateBufferWithNotification` requests a double buffer (notification count 2) and
  maps the PortCls DMA pages. The miniport also accepts notification counts 1-4.
- The driver owns timing. An `ExTimer` DPC fills one notification packet per interval,
  copies from the shared ring, and fills silence when the ring is empty. It increments
  `underruns` and always keeps the endpoint alive.
- `GetReadPacket` returns the last completed packet number, the QPC of the first sample
  in that packet, and `MoreData`. Packet completion uses interlocked release ordering.
  The streaming/DPC path performs no allocation, locking, logging or blocking.
- `GetPosition` returns the cyclic byte offset. `GetHWLatency` reports the driver ring
  fill plus one packet as the FIFO.
- `AllocateBufferWithNotification` publishes the engine's actual notification packet size
  into the ring header; the host pump and underrun reporting follow that value.
- D3 stops the timer and stream; D0 lets PortCls resume the pin. The control device is
  recreated with the adapter and removed with it.
- Endpoint format is fixed to 48 kHz / 1-2 channels / float32. The writer's rate and
  channels must match when it opens; the ring header still carries the negotiated
  values.

## Test signing (development only)

Use a disposable VM. Test signing reduces boot security and is not for production.

```powershell
bcdedit /set testsigning on
# Reboot. If Secure Boot is enabled, disable it in firmware first; Windows will not
# enable test signing while Secure Boot is on.
```

Sign the built package with a test certificate (WDK `MakeCert`/`New-SelfSignedCertificate`
and `signtool sign /fd sha256 /a /ac <ca.cer> mh_vmic.sys`, then generate/re-sign the
catalog with `Inf2Cat`/`signtool`). Keep the certificate in the test VM only.

## Install

Root-enumerated devices need a device node, not just a driver package. As Administrator:

```powershell
# Creates Root\MicHostVirtualMic and installs the driver
devcon install mh_vmic.inf Root\MicHostVirtualMic

# If devcon is unavailable, use the SetupDi path:
#   SetupDiCreateDeviceInfo + SetupDiRegisterDeviceInfo for Root\MicHostVirtualMic,
#   then UpdateDriverForPlugAndPlayDevicesW with the INF path.
```

`pnputil /add-driver mh_vmic.inf /install` installs the package but does not create the
root device node by itself. Reboot or restart the audio service if the endpoint does not
appear immediately. Do not change the system default microphone automatically.

## Uninstall

```powershell
# Close MicHost and stop all capture streams first.
devcon remove Root\MicHostVirtualMic
# Resolve the published driver-store package name (oemNN.inf) first:
pnputil /enum-drivers
pnputil /delete-driver oemNN.inf /uninstall /force
```

The control device and section disappear when the driver unloads. If a writer or a
capture stream is still active, stop it before removing the device.

## Verification required on a WDK machine

1. **Build:** `scripts/build-driver.ps1 -RequireDriver -Architecture x64,arm64` produces
   `.sys`, `.inf` and `.cat` in `target\driver\Release\<arch>\`. Check for compile/INF
   errors, WDK platform-toolset errors and `Inf2Cat` catalog output.
2. **Sign/install:** test-sign the package, enable test signing (Secure Boot caveat),
   `devcon install`, and confirm Device Manager has no error code.
3. **Endpoint:** Windows Sound settings shows "MicHost Virtual Microphone" as a
   **microphone** (form factor 4), not line-in/speaker. The topology node is
   `KSNODETYPE_MICROPHONE` and a jack description is present.
4. **IOCTLs:** `GET_VERSION` = 1, `OPEN_WRITER` succeeds once and a second open returns
   `STATUS_DEVICE_BUSY`, `GET_SECTION_INFO` returns the bare name and 80-byte ring, and
   `GET_STATS`/`SET_PACKET_FRAMES` behave as documented.
5. **Tone/silence:** map the section, write a known float32 tone, confirm a WASAPI shared
   capture client receives it; stop writing and confirm the endpoint outputs **silence**
   without disappearing.
6. **Writer cleanup:** kill MicHost while a stream is capturing. The endpoint must stay
   present and silent, `writer_active` must be 0, and a new `OPEN_WRITER` must succeed.
   Confirm `read_index`/`underruns`/QPC stats remain sane.
7. **Packets/latency:** verify `GetReadPacket` packet numbers, QPC values and `MoreData`
   are monotonic across a 60-minute run; check for underruns and drift.
8. **Power:** D3 stops the stream, D0 resumes; no BSOD, no dangling event/timer. Run
   Driver Verifier and suspend/resume stress.
9. **Format/edge cases:** 1- and 2-channel writers; invalid packet sizes; ring reuse;
   section DACL from a non-admin interactive process; simultaneous open/close races.
10. **Packaging:** `scripts/package.ps1` stages `driver\<arch>\` with `.sys`, `.inf`,
    `.cat` and this README; without driver output the package is unchanged.

## Known gaps

- Not compiled or test-loaded here (no WDK). No production or attestation signature.
- Capture-only, one endpoint. No render side, no loopback, no third-party driver bundle.
- Fixed 48 kHz float32 endpoint. The writer can choose 1-2 channels, but a format change
  while a stream exists is rejected; recreate the stream for a new format.
- One writer. Shared-mode capture is the target; exclusive mode and multiple simultaneous
  clients are not hardened.
- The section is created for the driver instance and survives writer cleanup; it is
  destroyed on adapter removal/driver unload, not per MicHost process.
- Timing uses a kernel high-resolution timer DPC, not a hardware clock. QPC is nominal
  packet timing; the capture engine still contributes its own period.
- No runtime plugin/crash isolation, installer, auto-update, or "set as default" action.
- Windows microphone privacy indicator will light up when capturing; document it.
- Test signing cannot coexist with Secure Boot; enterprise WDAC/HVCI policy may block the
  driver even when signed.

## Licensing note

The MicHost application is AGPL-3.0-or-later. This driver package is a separate program
that communicates through the documented IOCTL ABI and does not link with the application.
If any Microsoft `Windows-driver-samples` code (MS-PL) is copied into this driver, keep it
as a separate MS-PL-licensed program/source tree and do not merge it into the AGPL app.
The FSF treats MS-PL as GPL-incompatible; obtain legal review before combining code.
VST3 and ASIO SDK terms are unchanged by this directory.
