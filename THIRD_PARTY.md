# Bundled Tools

The Windows portable package redistributes the unmodified official scrcpy 5.0.1
Windows x64 release alongside Android platform-tools. These are separate
programs, not linked into the AndroidTool executable.
On Windows, AndroidTool can host the unmodified scrcpy renderer window as a
native child window inside its mirroring page; scrcpy remains a separate process.

## scrcpy

- Project and source: https://github.com/Genymobile/scrcpy
- Exact release: https://github.com/Genymobile/scrcpy/releases/tag/v5.0.1
- scrcpy license: Apache License 2.0, included as `scrcpy/LICENSE.txt`
- The embedded gamepad HID report descriptor is derived from scrcpy; its license is also included as `assets/scrcpy-APACHE-2.0.txt`.
- AndroidTool modifies the descriptor to use standard Z/Rz stick and Brake/Accelerator trigger usages, with Generic Android key layout and native L2/R2 button bits.
- Release archive SHA-256:
  `b12a2c4ee8be317422451fc7dcf8ee20a71b5ea7ef9ad73ddd825a25316227a5`

The official distribution also contains dependency libraries, including SDL,
FFmpeg and libusb. Their licenses and source information are available from
their respective upstream projects and the official scrcpy release/build
documentation:

- SDL: https://github.com/libsdl-org/SDL (zlib license)
- FFmpeg: https://ffmpeg.org/legal.html (LGPL/GPL depending on build options)
- libusb: https://github.com/libusb/libusb (LGPL 2.1 or later)
- scrcpy Windows build documentation:
  https://github.com/Genymobile/scrcpy/blob/v5.0.1/doc/build.md

## Android platform-tools

Official distribution: https://developer.android.com/tools/releases/platform-tools

The package preserves the platform-tools `NOTICE.txt` and `source.properties`.

## Optional downloads

AndroidTool can download official bundletool releases from Google and Eclipse
Temurin Java runtimes from Adoptium on demand. These are not included in the
portable package; their respective distributions and licenses are provided by
their upstream projects.
