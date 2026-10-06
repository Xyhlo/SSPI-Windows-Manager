# Third-party notices and redistribution status

This file describes the inputs observed in the active `build-sdl.ps1` package
staging path on 2026-08-27. It is an engineering inventory, not legal advice.
The repository's MIT license covers only original Game Search contributions; it
does not relicense third-party code, binaries, artwork, trademarks, or data.

**Public binary redistribution is currently blocked.** Do not publish the PKG
or a binary bundle until every item marked `BLOCKED` below has documented
redistribution authority and the required license/notice texts are shipped.
See `docs/PKG_PROVENANCE.json` for the machine-readable inventory and hashes.

## Reviewed components

### Microsoft Visual C++ 2022 runtime

- SSPI Windows x64 statically links the C/C++ runtime for the application and
  its native archive dependencies.
- The prebuilt publishing library under `resources/fpkg/` still imports the
  dynamic runtime. Its application-local x64 runtime DLLs come from the installed
  Visual Studio 2022 Build Tools redistributable directory, version 14.44.35112.
- Copyright Microsoft Corporation. These runtime files retain Microsoft's terms
  and are not covered by this repository's MIT license.
- The distribution includes `Microsoft-VC-Redist.txt` and
  `MSVC-RUNTIME-NOTICE.txt` beside these DLLs. Preserve those notices.
- Reference: https://learn.microsoft.com/en-us/cpp/windows/redistributing-visual-cpp-files

### suppaftp — Rust FTP client

- Version: 12.1.0, pinned in the application's Cargo lockfile.
- Source: https://github.com/veeso/suppaftp (crates.io package `suppaftp`).
- Author: Christian Visintin, as stated in the crate metadata.
- License: MIT OR Apache-2.0, verified from the crate metadata and README.
  Upstream license texts: `LICENSE-MIT` and `LICENSE-APACHE` in the source repository.
- Used by Windows Manager for plain passive FTP with the `tokio` feature; TLS features are disabled.
- Preserve the applicable license and copyright notices in binary distributions.

### SDL2# / SDL2-CS — notice present, binary origin incomplete

- Observed files: `SDL2-CS.dll` and source under
  `ps4/app/lib/SDL2-CS/`.
- Copyright: 2013-2021 Ethan Lee, as stated in the local license.
- License: zlib, verified from `ps4/app/lib/SDL2-CS/LICENSE`.
- Native `toolchain/ps4/pkg-tools/DotNetTemplate/binaries/Common/sce_module/libSDL2.sprx`: the local SDL2# README says SDL2
  uses zlib, but this binary has no version, source commit, or build record.
- Status: `BLOCKED` for the native binary until its exact source/revision and
  reproducible build provenance are recorded. The managed wrapper notice must
  remain with distributions.

### SixLabors.ImageSharp backport — conditional license path

- Observed file: `SixLabors.ImageSharp.dll` (assembly/file version `0.0.1.0`)
  and vendored source under `ps4/app/lib/SDL2-CS/ImageSharp/`.
- Copyright: Six Labors.
- Local license: Six Labors Split License 1.0, June 2022.
- The local license offers Apache-2.0 only when one of its listed qualification
  criteria is met. An MIT-licensed Game Search release appears to meet its
  open-source/source-available criterion, but that conclusion depends on the
  actual distributor and release remaining eligible.
- Status: `BLOCKED` until the vendored revision and modifications are recorded,
  the distributor confirms the applicable Split License criterion, and all
  Apache-2.0/Six Labors notice obligations are packaged. A commercial license
  may be required if the distributor does not qualify.

### Microsoft System compatibility packages — metadata found, notices incomplete

The ImageSharp project names these NuGet inputs, and the local NuGet `.nuspec`
files point to the dotnet/corefx license URL:

| Package | Package version | Staged assembly SHA-256 |
| --- | --- | --- |
| System.Buffers | 4.5.1 | `423200010a7684763451473a4fb206dfa074fc8249676621ef9d9a13417d364d` |
| System.Memory | 4.5.2 | `b078917b36fdaecbffee5fe54f3c274971f324fb775dd80f01e000b29c27e15e` |
| System.Numerics.Vectors | 4.5.0 | `671b682dde1d554d898bf34004c4dc2fef2da6985078c77311308b858b704828` |
| System.Runtime.CompilerServices.Unsafe | 4.5.2 | `a79e1c30e9308afe4d680f0bfb82de3e8c1fe94aeca453ec4092c3ed4789ae6b` |
| System.ValueTuple | 4.5.0 | `d6fb0dcfee1490a8168117ed1b55758f11db38475417b3668d19f89dcb55cbdd` |

Status: `BLOCKED` pending capture of the license/notice text applicable to
these exact package versions and confirmation that the staged DLLs came from
those packages rather than an unrecorded rebuild or substitution.

### PS4-OpenOrbis-Mono entrypoint and runtime bundle — unresolved

- `PS4-OpenOrbis-Mono/LICENSE` dedicates that project's own work to the public
  domain under the Unlicense.
- The active package stages `eboot.bin`, a 196-file `mono/` tree, and native
  modules copied from `toolchain/ps4/pkg-tools/DotNetTemplate/binaries/Common` / `binaries/Release`.
- That upstream project's own README explicitly warns that shipping its known
  PS4 Mono runtime may share a Sony binary and "can be a license issue."
- The `mono/4.5` tree is heterogeneous. It includes Mono assemblies plus
  Microsoft, Newtonsoft, ReactNative/Vsh, RabbitMQ, SharpZipLib, websocket-sharp
  and other assemblies. A single Mono license cannot clear the whole tree.
- Status: `BLOCKED`. Exact source commits, build recipes, per-file licenses,
  required notices, and redistribution authority are not recorded.

### PS4 native/system modules — no redistribution grant found

The active staging tree contains `libc.prx`, `libSceFios2.prx`,
`libmonosgen-2.0.prx`, `libSDL2.sprx`, and `sce_sys/about/right.sprx`.
No file in the reviewed tree establishes redistribution permission for the
Sony-named modules or `right.sprx`. Status: `BLOCKED`; remove them from any
public artifact unless the distributor can document a valid redistribution
grant. A jailbreak/homebrew use case does not itself supply such a grant.

### Artwork and other assets — authorship unknown

The package stages 21 files from `assets/` plus `icon0.png`, `pic0.png`
and `pic1.png` under `sce_sys/`. No authorship, source, or asset license record
was found. Status: `BLOCKED` pending first-party authorship confirmation or a
third-party license record for every asset.

### Packaging tools — repository-only, provenance unresolved

`create-gp4.exe`, `PkgTool.Core.*`, and `LibOrbisPkg.Core.dll` are build-time
tools and are not copied into the PKG by `build-sdl.ps1`. They are nevertheless
present in the repository. Their redistribution status was not established in
this pass. Status: `BLOCKED` for inclusion in a public source archive; omit them
or add verified provenance and notices.

### OpenOrbis PS4 Toolchain — PS4 receiver build input

- Local input: `SDK/openorbis-sdk/PS4Toolchain/` (not published in this repository).
- Source: https://github.com/OpenOrbis/OpenOrbis-PS4-Toolchain.
- License: GPL-3.0, as stated in the local toolchain `LICENSE`.
- Used to compile and link the standalone `sspi_ps4_receiver.elf` payload,
  including the toolchain headers, startup object and C runtime.
- Preserve the applicable license notices and satisfy the source obligations
  for the components included in any distributed payload.

### ps4-libjbc — PS4 receiver credential support, terms unconfirmed

- Local input: `SDK/ps4-libjbc/` (not published in this repository).
- Its C sources are compiled into the PS4 receiver for the startup credential
  probe and conditional jailbreak operation. Generated build copies adapt GNU
  assembly spelling and give its raw open/close syscalls private symbol names.
- No license file is present in the local copy. No redistribution permission
  is inferred from its availability or its use by another local product.
- Status: `BLOCKED` for publication until the upstream revision, license terms,
  and redistribution obligations have been confirmed.

### image — Rust artwork decoding and normalization

- Version: 0.25.10, pinned in the application's Cargo lockfile.
- Source: https://github.com/image-rs/image (crates.io package `image`).
- License: MIT OR Apache-2.0. Upstream license texts are `LICENSE-MIT`
  and `LICENSE-APACHE` in the source repository.
- Windows Manager enables PNG, JPEG and WebP decoders only to normalize
  game artwork for PS4 notifications.
- Preserve the applicable license and copyright notices in binary distributions.

### three.js — interface rendering

- Version: 0.170.0, pinned in the application's npm lockfile.
- Source: https://github.com/mrdoob/three.js (npm package `three`).
- Copyright © 2010-2024 three.js authors. License: MIT, verified from the
  package's `LICENSE` file.
- Windows Manager 2.20 uses it for the backdrop, the 3D game cases and install
  animations. It is bundled into the frontend build.
- Preserve the MIT copyright and permission notice in binary distributions.

### Geist — interface typeface

- Version: 5.3.0 of `@fontsource-variable/geist`, pinned in the application's npm lockfile.
- Source: https://github.com/vercel/geist-font, packaged by Fontsource.
- Copyright 2024 The Geist Project Authors. License: SIL Open Font License 1.1,
  verified from the package's `LICENSE` file.
- The variable font files are bundled into the frontend build. The OFL allows
  bundling with software; the font may not be sold on its own, and the license
  text must accompany redistributed copies.

### lz4 / lz4-sys — Lizard asset-pack compression

- Versions: lz4 1.28.1 and lz4-sys 1.11.1+lz4-1.10.0, pinned in the application's Cargo lockfile.
- Source: https://github.com/10xGenomics/lz4-rs. License: MIT (crate metadata and `LICENSE`).
- lz4-sys compiles the bundled LZ4 library 1.10.0, Copyright (c) 2011-2020 Yann Collet,
  BSD 2-Clause (`liblz4/LICENSE`). Ship both notices with binary distributions.

### crc32fast — Lizard block checksums

- Version: 1.5.1, pinned in the application's Cargo lockfile.
- Source: https://github.com/srijs/rust-crc32fast. License: MIT OR Apache-2.0 (crate metadata).

### ampr_emu — Lizard pack format and pack-capable runtime

- `app/src-tauri/src/ampr_pack.rs` is a port of the pack tool (`ampr_pack.py` 4.0 and
  `ampr_pack_format.py`) from drakmor's ampr_emu 0.4.2.1 pack tools. The release copies the
  locally supplied `libSceAmpr.sprx` 0.4.2.1 "test-pack" build (SHA-256
  `69e6c4d5e4f5fb83c9e01815db5861c4c75734acbf4595cafa50d4c218116d1a`) from `SDK/ampr/`
  into `resources/ampr/`.
- Status: `BLOCKED`. The local pack tools carry only the LZ4 license; ampr_emu's own license
  and redistribution terms are not recorded. Confirm them before publishing the port or
  distributing the runtime, and ship the required notice.

### .NET runtime — packaging engine runtime

- Shipped unmodified in `resources/dotnet`: the Microsoft.NETCore.App shared framework and its
  `hostfxr`, version 9.0.20, copied from the locally supplied .NET SDK (`SDK/dotnet`).
- SSPI starts its framework-dependent packaging engines (`fpkg-cli`, `themepack-cli`) with
  `DOTNET_ROOT` pointing at this folder, so no separate .NET install is needed.
- Source: https://github.com/dotnet/runtime. License: MIT. Microsoft's `LICENSE.txt` and
  `ThirdPartyNotices.txt` are copied beside the runtime.

### exFAT up-case table

- `app/src-tauri/src/exfat/upcase_table.rs` holds the recommended up-case table defined in
  Microsoft's exFAT specification (section 7.2.5.1; 5,836 bytes, checksum `0xE619D30D`).
  The values were transcribed from exfatprogs 1.2.0 `mkfs/upcase.c` and checked against the
  specification's checksum. No exfatprogs code is used.

### LibOrbisPkg — PS4 theme packaging library

- Used by Tools > Themes, restored on 2026-10-01.
- Observed file: `LibOrbisPkg.Core.dll` (assembly version 0.2.0.0), supplied locally
  under `SDK/liborbispkg/` and shipped unmodified in `resources/themepack/`.
- Source: https://github.com/maxton/LibOrbisPkg. License: GNU LGPL version 3,
  as stated in the project README. It stays a separate, replaceable assembly;
  SSPI's packager only calls it.
- Status: the local build's exact source revision is not recorded. Record it
  before any public binary distribution, and ship the LGPL text with it.

### PS4 Ultimate Theme Creator — theme format reference

- Used by Tools > Themes, restored on 2026-10-01.
- Source: PS4 Ultimate Theme Creator, Copyright (c) 2026 Imxnxl. License: MIT.
- SSPI's PS4 theme builder follows its console-verified theme layout, animated
  scene structure and limits, and embeds the 420-byte plane model its generator
  writes. Keep the MIT notice with distributions.

### SSPI console interfaces — WebKit Autoloader, Payload Manager and ELF loader

- Based on PS5 WebKit Autoloader (https://github.com/itsPLK/ps5-webkit-autoloader),
  Payload Manager (https://github.com/itsPLK/ps5-payload-manager), unified autoloader
  and their pinned ELF loader dependencies. The original GPL license and source
  notices remain applicable. SSPI's repository MIT license does not replace them.
- SSPI modifies the browser layouts, branding, startup handoff, Manager edition
  checks and the shared ELF loader's network binding/readiness reporting.
  Upstream firmware selection and exploit-chain source/assets remain unchanged.
- The pinned original v0.5.2 host has SHA-256
  `6421f167d01ff1c0f3c8c2df1bf0e656691837b938aa924b3d2426c05ec2b7bf`.
  Its chain assets and public HTTPS identity are reused. Windows reads the host's
  embedded archive; it does not execute the Python server.
- The distribution includes the GPL text under `resources/web-launcher/LICENSE`.
  The accompanying host embeds `sspi-launcher-sources.zip` with tracked upstream
  inputs, original licenses, pinned revisions, SSPI overrides and build recipe,
  plus `sspi-build.json` with source and native payload hashes. These are also
  downloadable from the local SSPI host at paths matching those filenames.
- Runtime payloads downloaded from configured repositories retain their own
  attribution and licensing and are not included in the public source export.

## Release rule

A clean-room/public release should be generated from an explicit allowlist.
Absence from this notice is not approval. Unknown or unreviewed inputs default
to `BLOCKED`, and package-source results/content are not bundled merely because
the application can discover them.
