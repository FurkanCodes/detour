## Install

- **Windows 10 / 11:** download `Detour-Windows.zip`, unzip, run `Detour.exe` and accept the administrator prompt.
- **macOS 11+:** download `Detour-macOS.zip`, unzip, move `Detour.app` to Applications, then right-click it and choose **Open** the first time.

The builds are not code-signed yet, so Windows SmartScreen and macOS Gatekeeper warn on first launch. Verify downloads against `SHA256SUMS.txt`.

## What's new

- **Windows now works like a proxy-based bypass for browsers.** A local proxy is set as the system proxy while Detour is on, alongside the packet engine for apps that ignore proxies.
- **Turning Detour off cuts everything it was carrying at once**, and turning it on again works immediately. Browsers no longer keep poisoned DNS answers or stale connections across a toggle.
- Your own proxy settings are saved and restored, with a logon safety net if Windows shuts down or Detour is killed while connected.
- The proxy keeps handshakes split even when a browser opens a connection long before using it, and falls back to encrypted DNS when the provider resolver has no answer for a name.

See the [README](https://github.com/FurkanCodes/detour#readme) for how it works and the FAQ.
