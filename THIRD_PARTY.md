# Bundled Tools

The installer progress/log presentation and parts of the AAB conversion behavior
(Universal APK extraction, per-conversion Java scratch directories and scratch
write-failure diagnosis) reference [TouMingQAQ/AAB2APK](https://github.com/TouMingQAQ/AAB2APK),
revision `057712be25571379f69436b6a1116ed067124698`.

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

## APK signature support

The application embeds the unmodified official Android `apksig` 8.13.0
library from Google's Maven repository (`assets/signing/apksig-8.13.0.jar`)
under Apache License 2.0; its license is included as
`assets/signing/apksig-LICENSE.txt`. SHA-256:
`c070ed1394629d74641aa0906f60b2ffa1ee77e6366a1f93437f59717b1aeb89`.

Official artifact: https://dl.google.com/dl/android/maven2/com/android/tools/build/apksig/8.13.0/apksig-8.13.0.jar

`assets/signing/androidtool-signing.jar` is the AndroidTool helper built from
`java/AndroidToolSigner.java` against that library. Rebuild it with JDK 17+
using `scripts/build-signing-helper.ps1`.
With JDK 17+ and Python 3, run `python scripts/test-signing-helper.py path/to/large.apk`
against an APK larger than 2 GiB to exercise the signing regression. It uses a
temporary test keystore, checks output signatures and alignment, and leaves the
input APK unchanged.
