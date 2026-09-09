![SSPI Windows Manager](docs/banner.png)

# SSPI Windows Manager

**Super Simple Package Installer for Windows** brings package search, local imports, archive extraction and PS5 delivery into one desktop application. Browse on your PC, choose a package or local folder, and send supported content to your console through the SSPI receiver.

**Version 0.2.2 · Development build · Tauri 2 / React / Rust**

[**Source code**](https://github.com/Xyhlo/SSPI-Windows-Manager) · [**Report an issue**](https://github.com/Xyhlo/SSPI-Windows-Manager/issues) · [**Join Discord**](https://discord.gg/hF2vw7ybRs)

This repository currently publishes source code, not a downloadable Windows release. GitHub's source ZIP is not an installer. Use a complete development distribution to run the application; see the build requirements below if you are working from source.

Looking for the application that runs directly on PS4? Visit [SSPI PS4](https://github.com/Xyhlo/SSPI), its [public beta](https://github.com/Xyhlo/SSPI/releases/tag/v5.10-beta) and [setup guide](https://xyhlo.github.io/SSPI/). The [PS5 application](https://github.com/Xyhlo/SSPI-PS5) is also a separate project.

## What it does

- **Search enabled package sources.** Install a compatible `.gssource` from a file or URL, choose which sources participate, and browse titles and artwork.
- **Inspect packages and mirrors.** Review package type, title ID, version, firmware information and host alternatives when metadata is available.
- **Unlock supported host links with Real-Debrid.** Store the API token in Windows Credential Manager. This Windows build exposes Real-Debrid integration; the PS4 beta's TorBox support should not be assumed to exist here.
- **Import local content.** Use Import file, Scan folder or Manual folder in Downloads. Review detected title IDs and base/update/DLC/backport classifications before installing.
- **Download and extract archives.** Prepare supported archive sets on the PC, then deliver the resulting content to the PS5 receiver. Missing or damaged volumes can still stop extraction.
- **Manage delivery by game.** Follow file-level progress and use the available pause/cancel actions. Downloading, extraction and console delivery are separate stages.
- **Configure LAN upload performance.** Balanced and Max bandwidth modes control upload lanes to the PS5. They do not turn internet downloads into parallel range downloads.
- **Preview the interface offline.** Demo data lets you explore the UI without a source, account or console. Preview mode does not perform real downloads or installations.

## First setup

You need a Windows machine with WebView2, a compatible PS5 homebrew environment for console delivery, a complete Windows Manager distribution, and free space on both devices. Browsing and package inspection do not require a connected console.

1. Launch the Windows application from the complete distribution.
2. Open **Settings → Receiver**. Use **Download receiver ELF** to export the receiver that matches the application, then load it through your PS5's supported payload loader.
3. Enter your PS5's current IPv4 address or hostname and receiver port. The normal receiver port is **9114**; it is distinct from your payload loader's port. Use **Test receiver** and save the settings.
4. Open **Settings → Sources**. Install a compatible `.gssource` from a local file or its direct download URL, then enable it.
5. If the chosen host needs Real-Debrid, open **Settings → Debrid**, add your own token, verify it and save. Leave the token field blank to preserve an existing saved token.
6. In **Settings → App**, choose a download folder with space for archives and extracted files. Start with Balanced upload mode.
7. Search and inspect a package, or import your own local content from **Downloads**. Check the title ID and package kind before sending it to the receiver.

Reload the matching receiver after an application update or receiver-port change. Keep the PC and PS5 reachable on the same LAN. If Windows Firewall prompts, allow the necessary local-network access for the application.

## Downloads and storage

Internet transfer, extraction and LAN upload have different bottlenecks. Downloads remain one stream per volume. **Max bandwidth** adjusts PS5 upload lanes only; it does not remove host limits or guarantee a particular transfer speed. Return to Balanced if uploads or mounts become unreliable.

Allow room for archive volumes, extracted packages and working files on the PC, plus installed content on the console. Do not remove staging files while a job still owns them. A completed PC download is not confirmation that the console installation or mount succeeded.

For manual folders, review the detected package kind and title ID. The UI lets you correct them before queuing. An inferred label or matching title ID alone is not proof of base/update compatibility.

## Troubleshooting

| Problem | Check |
| --- | --- |
| Receiver is not reachable | Load the receiver supplied with this build; check the current PS5 address, matching port, firewall and LAN isolation. Use Test receiver. |
| Search returns nothing | Confirm a compatible source is installed and enabled. A source download URL must return the actual archive, not a website's HTML file-view page. |
| A host link fails | Verify your Real-Debrid account and the host's current support. Try another valid mirror; a listed host is not a guarantee of service support. |
| Extraction fails | Check all required archive volumes, free space, archive passwords where applicable and the exact error. Do not mix parts from different uploads. |
| Upload or mounting fails | Confirm the receiver matches the app, try Balanced mode and retain the package-level error. PS5 firmware/loader compatibility still needs testing. |
| The browser preview cannot install | Offline preview uses simulated data. Run the packaged Tauri application for native operations. |

## Building in the development workspace

**A plain clone is not a complete distribution build.** This export contains selected production source, manifests, lockfiles, notices and the README banner. Local build scripts, the receiver SDK, runtime artwork and other excluded inputs must be supplied separately.

The complete build environment uses:

- Windows with Node.js/npm and the Rust MSVC toolchain.
- Visual Studio C++ build tools, WebView2, LLVM and Python 3.
- The receiver SDK under the ignored `SDK/payload-sdk/ps5-payload-sdk/` directory.
- Local build scripts, runtime assets and source-package inputs.

From the complete product working directory:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\build.ps1
```

The wrapper builds the receiver, prepares the bundled inputs and builds the Windows application. Timestamped distributions and intermediates go under `../Build-Output/Windows Manager/`. Do not place output in the source tree or on the desktop.

| Path | Contents |
| --- | --- |
| [`app/src/`](app/src) | React interface, package details, settings and download groups |
| [`app/src-tauri/src/`](app/src-tauri/src) | Rust source engine, downloads, archives, credentials and console delivery |
| [`payload/main.c`](payload/main.c) | PS5 receiver implementation |
| [`app/package.json`](app/package.json) | Frontend scripts and dependency manifest |
| [`app/src-tauri/Cargo.toml`](app/src-tauri/Cargo.toml) | Native application dependencies |
| [`product.json`](product.json) | Product identity and output convention |

## Testing and bug reports

This is development software. A successful PC build does not prove compatibility with every PS5 firmware, loader, archive format or package combination.

Report problems in [GitHub Issues](https://github.com/Xyhlo/SSPI-Windows-Manager/issues) or the [SSPI Discord](https://discord.gg/hF2vw7ybRs). Include the application build, Windows version, PS5 firmware/loader and receiver version, the exact steps, the stage that failed and the full error. For speed reports, include file size, host, connection type and upload mode.

Remove API keys, signed download URLs and personal paths from public logs and screenshots.

## License

Original SSPI contributions use the [MIT license](LICENSE). Third-party code, dependencies, artwork and trademarks retain their own terms; see [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
