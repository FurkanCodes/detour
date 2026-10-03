## Install

- **Windows 10 / 11:** download `Detour-Windows.zip`, unzip, run `Detour.exe` and accept the administrator prompt.
- **macOS 11+:** download `Detour-macOS.zip`, unzip and move `Detour.app` to Applications. The app is not signed, so macOS may say it cannot verify it. Run this once in Terminal, then open it normally:

  ```sh
  xattr -dr com.apple.quarantine /Applications/Detour.app
  ```

The builds are not code-signed yet, so Windows SmartScreen also warns on first launch ("More info", then "Run anyway"). Verify downloads against `SHA256SUMS.txt`.

## What's new

- **macOS: native window.** Detour now uses the standard macOS title bar with the traffic-light buttons, rounded corners and native resizing, with Detour's own title row drawn beneath it.
- **macOS: minimize to the menu bar.** The yellow button tucks Detour away in the menu bar (and removes the Dock icon) when the menu bar icon is available. Open it again from the menu bar icon.
- Windows is unchanged and keeps its custom window.

See the [README](https://github.com/FurkanCodes/detour#readme) for how it works and the FAQ.
