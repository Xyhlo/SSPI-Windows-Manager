import type { DeliveryJob, Game, MetadataResponse, PackageCandidate } from "@/types"

const escapeXml = (value: string) =>
  value.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;")

export const demoArt = (title: string, subtitle: string, from: string, to: string, accent: string) =>
  `data:image/svg+xml;charset=utf-8,${encodeURIComponent(`<svg xmlns="http://www.w3.org/2000/svg" width="720" height="1000" viewBox="0 0 720 1000"><defs><linearGradient id="bg" x2="1" y2="1"><stop stop-color="${from}"/><stop offset="1" stop-color="${to}"/></linearGradient><radialGradient id="glow"><stop stop-color="${accent}" stop-opacity=".72"/><stop offset="1" stop-color="${accent}" stop-opacity="0"/></radialGradient><filter id="grain"><feTurbulence baseFrequency=".8" numOctaves="4"/><feColorMatrix values="1 0 0 0 0 0 1 0 0 0 0 0 1 0 0 0 0 0 .1 0"/></filter></defs><rect width="720" height="1000" fill="url(#bg)"/><circle cx="560" cy="245" r="340" fill="url(#glow)"/><rect width="720" height="1000" filter="url(#grain)" opacity=".8"/><path d="M-80 675L640 85M20 820L760 210" fill="none" stroke="${accent}" stroke-opacity=".45" stroke-width="3"/><circle cx="540" cy="280" r="180" fill="none" stroke="white" stroke-opacity=".28" stroke-width="2"/><circle cx="540" cy="280" r="120" fill="none" stroke="white" stroke-opacity=".15" stroke-width="18"/><path d="M95 690h530" stroke="white" stroke-opacity=".34"/><text x="62" y="775" fill="white" font-family="Arial,sans-serif" font-weight="800" font-size="62" letter-spacing="-2">${escapeXml(title)}</text><text x="66" y="830" fill="white" fill-opacity=".68" font-family="Arial,sans-serif" font-size="21" letter-spacing="7">${escapeXml(subtitle)}</text><text x="65" y="925" fill="white" fill-opacity=".5" font-family="Arial,sans-serif" font-size="17" letter-spacing="5">GAME SEARCH / PREVIEW</text></svg>`)}`

export const demoGames: Game[] = [
  { titleId: "CUSA17419", name: "Neon Divide", region: "EU", icon: demoArt("NEON DIVIDE", "CITY OF ECHOES", "#071422", "#44204f", "#ff4fa8") },
  { titleId: "PPSA00002", name: "Ashfall Protocol", region: "US", icon: demoArt("ASHFALL", "PROTOCOL", "#1d0d08", "#733013", "#ffad4f") },
  { titleId: "PPSA00003", name: "Arctic Signal", region: "EU", icon: demoArt("ARCTIC SIGNAL", "BEYOND THE ICE", "#031b25", "#0b5361", "#61dcff") },
  { titleId: "CUSA00004", name: "Midnight Circuit", region: "JP", icon: demoArt("MIDNIGHT", "CIRCUIT", "#070912", "#151e5c", "#6e8bff") },
  { titleId: "CUSA00005", name: "Solar Run", region: "US", icon: demoArt("SOLAR RUN", "NO TURNING BACK", "#1a0c03", "#7a3405", "#ffd15a") },
  { titleId: "CUSA00006", name: "Glass Horizon", region: "EU", icon: demoArt("GLASS HORIZON", "A NEW WORLD", "#061817", "#1f5450", "#78e0c3") },
]

export const demoPackages: PackageCandidate[] = [
  { kind: "base", label: "Base game package - 38.4 GB", url: "demo://base", version: "1.00", firmware: "5.05+", expectedSize: 41_231_686_144 },
  { kind: "update", label: "Version 1.08 update - 4.7 GB", url: "demo://update", version: "1.08", firmware: "9.00+", expectedSize: 5_046_586_368 },
  { kind: "backport", label: "Backport package - 312 MB", url: "demo://backport", firmware: "5.05+", expectedSize: 327_155_712 },
]

export const demoJobs: DeliveryJob[] = [
  { jobId: "demo-base", titleId: "CUSA17419", packageKind: "base", packageLabel: "Base game package", packageVersion: "1.00", createdAt: 1000, stageHistory: ["downloading", "extracting", "uploading", "installing"], stage: "complete", progress: 1, message: "Installation complete", speedBps: 0, bytesDone: 21_474_836_480, bytesTotal: 21_474_836_480, title: "Neon Divide", icon: demoGames[0].icon },
  { jobId: "demo-update", titleId: "CUSA17419", packageKind: "update", packageLabel: "Neon Divide — Update 1.08.pkg", packageVersion: "1.08", createdAt: 4000, stageHistory: ["downloading", "extracting", "uploading"], stage: "uploading", progress: .63, message: "Uploading Update 1.08.pkg to PS5", speedBps: 67_108_864, bytesDone: 3_179_349_411, bytesTotal: 5_046_586_368, etaSeconds: 28, title: "Neon Divide", icon: demoGames[0].icon },
  { jobId: "demo-dlc", titleId: "CUSA17419", packageKind: "dlc", packageLabel: "City of Echoes — Expansion.pkg", createdAt: 3000, stageHistory: ["queued"], stage: "queued", progress: 0, message: "Waiting for a download slot · 1 ahead", speedBps: 0, title: "Neon Divide", icon: demoGames[0].icon },
  { jobId: "demo-download", titleId: "PPSA00011", packageKind: "base", packageLabel: "Glass Horizon.part1-4.rar", createdAt: 6000, stageHistory: ["downloading"], stage: "downloading", progress: .37, message: "Downloading Glass Horizon · part 2/4", speedBps: 94_371_840, bytesDone: 6_442_450_944, bytesTotal: 17_179_869_184, etaSeconds: 114, connections: 14, priority: true, title: "Glass Horizon", icon: demoArt("GLASS HORIZON", "ABOVE THE CLOUDLINE", "#06101a", "#163a5c", "#7fc8ff") },
  { jobId: "demo-extract", titleId: "PPSA00002", packageKind: "base", packageLabel: "Ashfall Protocol.zip", createdAt: 2000, stageHistory: ["downloading", "extracting"], stage: "extracting", progress: 0.41, message: "Extracting game folder", speedBps: 89_128_960, bytesDone: 8_804_982_784, bytesTotal: 21_474_836_480, etaSeconds: 142, title: "Ashfall Protocol", icon: demoGames[1].icon },
  { jobId: "demo-complete", titleId: "PPSA00003", packageKind: "base", packageLabel: "Arctic Signal.pkg", createdAt: 500, stageHistory: ["uploading", "installing"], stage: "complete", progress: 1, message: "Install complete", speedBps: 0, bytesDone: 327_155_712, bytesTotal: 327_155_712, title: "Arctic Signal", icon: demoGames[2].icon },
  // FPKG builds: stage timings, I/O and ratios recorded from a real 2 GB dump slice on engine 1.1.0.
  { jobId: "demo-pack", titleId: "PPSA00007", packageKind: "base", packageLabel: "Deep Current (dump)", packageVersion: "01.004.000", createdAt: 5000, stageHistory: ["packaging"], stage: "packaging", progress: 0, packageOnly: true, message: "Packaging", speedBps: 0, title: "Deep Current", icon: demoArt("DEEP CURRENT", "BELOW THE SURFACE", "#041318", "#0f3a4a", "#4fd1ff"),
    packaging: { preset: "standard", compressionLevel: 3, pfsVersion: 2, threads: 16, inputBytes: 2_109_893_060, fileCount: 62, outputBytes: 0, outputPath: "", elapsedSeconds: 2.02, log: [], workspaceSeconds: 0.24,
      engine: { stage: "compress", stageIndex: 2, stageCount: 6, stageProgress: 0.411, etaSeconds: 2.69, level: 3, workers: 16, pfs: "v2", blockKiB: 256, elapsedSeconds: 2.02,
        inputBytes: 2_060_136_013, compressedBytes: null, files: { done: 58, total: 62 }, currentFile: "/tom/content/paks/pakchunk4-ps5.pak", currentFileProgress: 0.6,
        io: { readBytes: 862_117_376, writeBytes: 560_922_624, readBps: 451_199_795, writeBps: 295_174_144 },
        stages: [
          { id: "prepare", label: "Prepare", state: "done", seconds: 0.14, progress: 1 },
          { id: "compress", label: "Compress · Kraken 3", state: "active", seconds: 1.88, progress: 0.411, detail: "58 of 62 files" },
          { id: "outer", label: "Outer PFS · write and hash", state: "pending", seconds: 0 },
          { id: "metadata", label: "Metadata and artwork", state: "pending", seconds: 0, detail: "pic1.png, pic2.png from DDS" },
          { id: "finalize", label: "Finalize · digests and integrity", state: "pending", seconds: 0 },
          { id: "verify", label: "Verify package", state: "pending", seconds: 0 },
        ] } } },
  { jobId: "demo-packed", titleId: "PPSA00008", packageKind: "base", packageLabel: "Paper Lanterns (dump)", packageVersion: "01.004.000", createdAt: 400, stageHistory: ["packaging"], stage: "complete", progress: 1, packageOnly: true, message: "FPKG ready", speedBps: 0, title: "Paper Lanterns", icon: demoArt("PAPER LANTERNS", "A QUIET FESTIVAL", "#1a0f06", "#5a2a10", "#ffb35c"),
    packaging: { preset: "standard", compressionLevel: 3, pfsVersion: 2, threads: 16, inputBytes: 2_109_893_060, fileCount: 62, outputBytes: 1_205_439_986, elapsedSeconds: 11.23, log: [], workspaceSeconds: 0.24, totalSeconds: 11.47,
      outputPath: "D:\\Games\\FPKG\\Paper Lanterns [PPSA00008]\\UP0000-PPSA00008_00-PAPERLANTERNS000-A0104-V0104.pkg",
      engine: { stage: "verify", stageIndex: 6, stageCount: 6, stageProgress: 1, etaSeconds: null, level: 3, workers: 16, pfs: "v2", blockKiB: 256, elapsedSeconds: 11.23,
        inputBytes: 2_060_136_013, compressedBytes: 1_135_149_056, files: { done: 62, total: 62 }, currentFile: null, currentFileProgress: null,
        io: { readBytes: 3_372_220_416, writeBytes: 2_342_004_736, readBps: null, writeBps: null },
        stages: [
          { id: "prepare", label: "Prepare", state: "done", seconds: 0.15, progress: 1 },
          { id: "compress", label: "Compress · Kraken 3", state: "done", seconds: 4.24, progress: 1, detail: "62 files · 1,965 → 1,083 MiB (55.1%)" },
          { id: "outer", label: "Outer PFS · write and hash", state: "done", seconds: 1.3, progress: 1, detail: "1,083 MiB image" },
          { id: "metadata", label: "Metadata and artwork", state: "done", seconds: 5.09, progress: 1, detail: "pic1.png, pic2.png converted from DDS" },
          { id: "finalize", label: "Finalize · digests and integrity", state: "done", seconds: 0.7, progress: 1 },
          { id: "verify", label: "Verify package", state: "done", seconds: 0.05, progress: 1 },
        ] } } },
  // ShadowMount image: stage timings and sizes measured imaging a real 47 GiB, 92-file dump on the host.
  { jobId: "demo-image", titleId: "PPSA00009", packageKind: "base", packageLabel: "Glass Harbor (dump)", packageVersion: "01.004.000", createdAt: 300, stageHistory: ["packaging"], stage: "complete", progress: 1, packageOnly: true, message: "ShadowMount image ready", speedBps: 0, title: "Glass Harbor", icon: demoArt("GLASS HARBOR", "THE TIDE REMEMBERS", "#06121a", "#123a52", "#7fd4ff"),
    packaging: { preset: "exfat", format: "exfat", pfsVersion: 0, threads: 16, inputBytes: 50_490_196_890, fileCount: 92, outputBytes: 50_500_468_736, elapsedSeconds: 84.8, log: [], workspaceSeconds: 0.3, totalSeconds: 85.1,
      outputPath: "D:\\Games\\ShadowMount\\Glass Harbor [PPSA00009]\\PPSA00009-v01.004.000.exfat",
      engine: { stage: "done", stageIndex: 3, stageCount: 3, stageProgress: 1, etaSeconds: null, level: 0, workers: 16, pfs: "exFAT", blockKiB: 64, elapsedSeconds: 84.8,
        inputBytes: 50_490_196_890, compressedBytes: null, files: { done: 92, total: 92 }, currentFile: null, currentFileProgress: null,
        io: { readBytes: 50_490_196_890, writeBytes: 50_500_468_736, readBps: null, writeBps: null },
        stages: [
          { id: "layout", label: "Plan the exFAT layout", state: "done", seconds: 0.01, detail: "92 files, 21 folders" },
          { id: "write", label: "Write the exFAT image", state: "done", seconds: 58.27, detail: "47.03 GiB" },
          { id: "verify", label: "Verify the image", state: "done", seconds: 26.5, detail: "112 entries, 47.02 GiB checked" },
        ] } } },
  // The same dump with Lizard asset packing (release build, 16 workers).
  { jobId: "demo-image-lizard", titleId: "PPSA00010", packageKind: "base", packageLabel: "Salt Meridian (dump)", packageVersion: "01.004.000", createdAt: 200, stageHistory: ["packaging"], stage: "complete", progress: 1, packageOnly: true, message: "ShadowMount image ready", speedBps: 0, title: "Salt Meridian", icon: demoArt("SALT MERIDIAN", "WHERE THE MAPS END", "#160d05", "#4a2c12", "#ffc27a"),
    packaging: { preset: "exfat", format: "exfat", pfsVersion: 0, threads: 16, inputBytes: 50_490_196_890, fileCount: 92, outputBytes: 41_009_807_360, elapsedSeconds: 227.1, log: [], workspaceSeconds: 0.4, totalSeconds: 227.5,
      outputPath: "D:\\Games\\ShadowMount\\Salt Meridian [PPSA00010]\\PPSA00010-v01.004.000.exfat",
      lizard: { runtime: "0.4.2.1", filesTotal: 91, filesPacked: 21, filesAutoLoose: 0, packedBytes: 50_047_784_413, storedBytes: 32_278_856_256, chunks: 763_681, sharedChunks: 22_109, packs: 9, crcPath: "D:\\Games\\ShadowMount\\Salt Meridian [PPSA00010]\\PPSA00010-v01.004.000.ampr_assets.index.crc" },
      engine: { stage: "done", stageIndex: 4, stageCount: 4, stageProgress: 1, etaSeconds: null, level: 0, workers: 16, pfs: "exFAT", blockKiB: 64, elapsedSeconds: 227.1,
        inputBytes: 41_000_354_279, compressedBytes: null, files: { done: 81, total: 81 }, currentFile: null, currentFileProgress: null,
        io: { readBytes: 91_048_138_666, writeBytes: 41_009_807_360, readBps: null, writeBps: null },
        stages: [
          { id: "lizard", label: "Lizard asset packing", state: "done", seconds: 107.5, detail: "21 of 91 files packed, 16.55 GiB saved" },
          { id: "layout", label: "Plan the exFAT layout", state: "done", seconds: 0.02, detail: "81 files, 21 folders" },
          { id: "write", label: "Write the exFAT image", state: "done", seconds: 90.3, detail: "38.19 GiB" },
          { id: "verify", label: "Verify the image", state: "done", seconds: 29.2, detail: "101 entries, 38.18 GiB checked" },
        ] } } },
]

export const demoMetadata = (game: Game): MetadataResponse => ({
  enabled: true,
  attribution: "Simulated preview data",
  game: {
    name: game.name,
    description_raw: "A cinematic action adventure set across a fractured city where every choice changes the route home. This preview demonstrates the complete title, metadata, package, and installation flow without making network calls.",
    released: "2026-10-18",
    rating: 4.6,
    metacritic: 88,
    background_image: game.icon,
    genres: [{ name: "Action" }, { name: "Adventure" }, { name: "RPG" }],
    esrb_rating: { name: "Teen" },
  },
})
