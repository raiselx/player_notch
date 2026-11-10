# NotchPlayer (`player_notch`)

A frameless always-on-top mini media player for Windows 10/11 shaped like a MacBook notch / Dynamic Island. Connects to active Windows System Media Transport Controls (SMTC) sessions (Apple Music, Spotify, browsers, etc.).

## Features

- **Auto-hide behavior:** smoothly slides up and hides when nothing is playing or playback is stopped (including idle placeholders like Apple Music's `Not Playing`). Remains visible for `10s` after pausing (`PAUSE_LINGER`) or while hovered.
- **Compact mode (`360×40`):** square album art (`26×26`, `4 px` radius), truncated track title, and `⏮` `⏯` `⏭` controls.
- **Expanded mode on hover (`420×102`):** smoothly expands to show larger cover art (`40×40`, `6 px` radius), scrolling marquee for long titles, artist name, interactive seek/progress bar with elapsed/total time, and a large circular Play/Pause button.
- **Click-to-open source app:** clicking the album art or title in expanded mode brings the source application window to the foreground (supports both standard `.exe` apps and Microsoft Store / UWP apps via `ApplicationFrameHost` + `GetApplicationUserModelId`, falling back to `explorer.exe shell:AppsFolder\<AUMID>`).
- **Adaptive background color:** extracts a dominant saturated color from the cover art (`MEDIANCUT` quantization + averaging + darkening) and smoothly animates vertical gradients on track change.
- **System tray & single instance:** tray icon with **Autostart with Windows** (`HKCU\Software\Microsoft\Windows\CurrentVersion\Run\FloatingNotchPlayer`) and **Quit**, plus single-instance mutex protection (`Local\FloatingNotchPlayerSingleInstance`).

## Project Structure

- `player.py` — main application (`Media` SMTC poller + `Notch` Qt widget + tray integration)
- `NotchPlayer.spec` — PyInstaller `--onefile --windowed` build specification
- `requirements.txt` — Python dependencies
- `dist/NotchPlayer.exe` — prebuilt standalone Windows executable (~53.6 MiB)

## Quick Start

### Run the standalone `.exe`
Download and launch `dist/NotchPlayer.exe` (no Python installation required).

### Run from source
Tested on Windows 11 with Python `3.14.2`:

```powershell
pip install -r requirements.txt
python player.py
```

CLI flags for autostart:
```powershell
python player.py --enable-autostart
python player.py --disable-autostart
```

Debug environment variables:
- `CLOCK_DEBUG_SHOT=<path>` — saves a widget screenshot after 3.5s and exits
- `CLOCK_DEBUG_HOVER=1` — forces expanded hover state

## Build Executable

```powershell
pip install pyinstaller==6.22.3
pyinstaller --clean NotchPlayer.spec
```

The standalone binary will be generated at `dist/NotchPlayer.exe`.

## Known Notes / TODO

- Standalone `dist/NotchPlayer.exe` has not yet been extensively tested across different machines (including `winrt` inside onefile packaging).
- Live UI interactions (button clicks, timeline seeking support in Apple Music, clicking cover/title to focus source app) need full end-to-end verification.
- For browser media sessions, clicking the cover brings the top browser window to focus rather than a specific tab.
- Near-white / grayscale covers currently produce a gray background tint.
