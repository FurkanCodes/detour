## Install

- **Windows 10 / 11:** download `Detour-Windows.zip`, unzip, run `Detour.exe` and accept the administrator prompt.
- **macOS 11+:** download `Detour-macOS.zip`, unzip and move `Detour.app` to Applications. The app is not signed, so macOS may say it cannot verify it. Run this once in Terminal, then open it normally:

  ```sh
  xattr -dr com.apple.quarantine /Applications/Detour.app
  ```

The builds are not code-signed yet, so Windows SmartScreen also warns on first launch ("More info", then "Run anyway"). Verify downloads against `SHA256SUMS.txt`.

## What's new

- **Closing Detour now turns it off and cleans up.** Before, the close button only hid Detour in the tray while it kept running, and games whose login goes through the Windows proxy settings could not sign in. Closing now quits and puts your proxy settings back. To keep Detour running in the tray, turn on *Keep running in the tray* in Settings (it is now off by default).
- **Windows: signing out, restarting or shutting down** while connected now puts your proxy settings back too.
- **Windows: the packet driver is unloaded** when Detour turns off, instead of staying loaded until Windows restarts. Some games' anti-cheat refuses to run while it is loaded. Detour also unloads a driver left behind by an earlier version the next time it starts.
- **Quitting while Detour is still connecting** now waits for it and undoes the connection, instead of leaving it half set up.
- **WARP tunnel:** turning it off also removes Detour's profile from WireSock.

See the [README](https://github.com/FurkanCodes/detour#readme) for how it works and the FAQ.
