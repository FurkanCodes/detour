## Install

- **Windows 10 / 11:** download `Detour-Windows.zip`, unzip, run `Detour.exe` and accept the administrator prompt.
- **macOS 11+:** download `Detour-macOS.zip`, unzip and move `Detour.app` to Applications. The app is not signed, so macOS may say it cannot verify it. Run this once in Terminal, then open it normally:

  ```sh
  xattr -dr com.apple.quarantine /Applications/Detour.app
  ```

The builds are not code-signed yet, so Windows SmartScreen also warns on first launch ("More info", then "Run anyway"). Verify downloads against `SHA256SUMS.txt`.

## What's new

- **No more SSL errors on Turkish bank and government sites.** Akbank, İşbank, Ziraat, PTT and e-Devlet failed with SSL errors because Detour split the TLS handshake into two records, which they refuse. That trick is gone.
- **Windows now works the way [BypaxDPI](https://github.com/BypaxDPI/BypaxDPI-Windows) does.** The local proxy sends the whole TLS handshake one byte per packet, which gets Discord and Roblox through Türk Telekom while every site still accepts it. Names resolve through Cloudflare over HTTPS first and never fall back to the provider's poisoned DNS.
- **Windows: the WinHTTP proxy is set too**, so system services and many native programs use Detour, with BypaxDPI's list of hosts that stay direct (connectivity checks, Windows Update, game launchers). Your own settings are saved and restored.
- **Windows: no more packet rewriting or decoy packets** in the default method, so other programs' connections are left alone.
- **Turbo, Balanced and Strong** now match BypaxDPI's modes. On Türk Telekom keep *Use provider profile* or *Strong*.
- **New, experimental: WARP tunnel (Windows).** Settings → *Connection method* → *WARP tunnel* sends Discord and Roblox, voice included, through a free Cloudflare WARP tunnel run by WireSock. The first connect downloads and installs WireSock.
- macOS keeps the native window and menu bar from 0.2.2.

See the [README](https://github.com/FurkanCodes/detour#readme) for how it works and the FAQ.
