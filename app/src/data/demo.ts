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
  { jobId: "demo-dlc", titleId: "CUSA17419", packageKind: "dlc", packageLabel: "City of Echoes — Expansion.pkg", createdAt: 3000, stageHistory: ["queued"], stage: "queued", progress: 0, message: "Waiting to download", speedBps: 0, title: "Neon Divide", icon: demoGames[0].icon },
  { jobId: "demo-extract", titleId: "PPSA00002", packageKind: "base", packageLabel: "Ashfall Protocol.zip", createdAt: 2000, stageHistory: ["downloading", "extracting"], stage: "extracting", progress: 0.41, message: "Extracting game folder", speedBps: 89_128_960, bytesDone: 8_804_982_784, bytesTotal: 21_474_836_480, etaSeconds: 142, title: "Ashfall Protocol", icon: demoGames[1].icon },
  { jobId: "demo-complete", titleId: "PPSA00003", packageKind: "base", packageLabel: "Arctic Signal.pkg", createdAt: 500, stageHistory: ["uploading", "installing"], stage: "complete", progress: 1, message: "Install complete", speedBps: 0, bytesDone: 327_155_712, bytesTotal: 327_155_712, title: "Arctic Signal", icon: demoGames[2].icon },
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
