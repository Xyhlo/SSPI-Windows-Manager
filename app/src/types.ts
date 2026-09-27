export type ConsoleKind = "ps5" | "ps4"
export type Ps4Transport = "receiver" | "inbox"

export type Settings = {
  activeConsole: ConsoleKind
  ps5Host: string
  ps5Port: number
  ps4Host: string
  ps4Transport: Ps4Transport
  ps4ReceiverPort: number
  ps4LoaderPort: number
  /** PS5 ELF loader port for sending payloads (9021 by default). Older settings files don't have it. */
  ps5LoaderPort?: number
  ps4ServePort: number
  ps4FtpPort: number
  ps4FtpUser: string
  ps4FtpPasswordConfigured: boolean
  ps4ArchiveMode: "pc" | "ps4"
  ps4RemoveAfterInstall: boolean
  resolverBaseUrl: string
  downloadDir: string
  onboardingComplete: boolean
  realDebridEnabled: boolean
  realDebridConfigured: boolean
  torboxEnabled: boolean
  torboxConfigured: boolean
  alldebridEnabled: boolean
  alldebridConfigured: boolean
  theme: string
  reduceMotion: boolean
  transferMode: string
  uploadLanes: number
  packageDumps: boolean
  downloadPackageOnly: boolean
  keepArchives: boolean
  keepExtractions: boolean
  keepPackages: boolean
  fpkgPreset: string
  fpkgCompressionLevel?: number | null
  fpkgDoctor: boolean
  fpkgPfsVersion: number
  fpkgEnginePath: string
  targetFw: string
  fpkgCleanupSource: boolean
}

export type Game = {
  variants?: Game[]
  titleId: string
  name: string
  region: string
  icon?: string
  packages?: PackageCandidate[]
  sourceId?: string
  sourceName?: string
  sourceVersion?: string
}

export type PackageSource = {
  id: string
  name: string
  description: string
  version: string
  engineType: string
  enabled: boolean
  trust: string
  installUrl: string
}

export type CatalogSection = {
  id: string
  title: string
  games: Game[]
}

export type GameCatalog = {
  sections: CatalogSection[]
  cached: boolean
  source: string
}

export type PackageCandidate = {
  kind: string
  label: string
  url: string
  accessType?: string
  sourceId?: string
  sourceName?: string
  sourceVersion?: string
  candidateId?: string
  groupId?: string
  hoster?: string
  version?: string
  firmware?: string
  sourcePageUrl?: string
  expectedSize?: number
  expectedSha256?: string
  expectedContentId?: string
  archiveSetId?: string
  archivePartNumber?: number
  archivePartCount?: number
  archiveFileName?: string
  archiveFormatHint?: string
  archivePassword?: string
  mirrorId?: string
  intermediateUrl?: string
  referer?: string
  diagnostics?: string[]
}

export type DeliveryRequest = {
  target?: ConsoleKind
  package: PackageCandidate
  titleId: string
  titleName: string
  icon?: string
  archiveParts: PackageCandidate[]
  backport?: { package: PackageCandidate; parts: PackageCandidate[] }
  provider?: string
}

export type PackagingInfo = {
  preset: string; pfsVersion: number; threads: number; inputBytes: number; fileCount: number;
  outputBytes: number; outputPath: string; elapsedSeconds: number; log: string[];
  compressionLevel?: number; doctor?: DoctorReport | null; doctorApplied?: boolean;
  activity?: string; phaseProgress?: number | null;
  compressionInputBytes?: number | null; compressionOutputBytes?: number | null;
  speedBps?: number | null; lastActivitySeconds?: number | null;
}

export type DoctorReport = {
  scannedModules: number;
  issues: { path: string; severity: string; message: string }[];
  repairs: { relative: string; backup: string }[];
}

export type SpacePlan = {
  phase: string; directory: string; freeBytes?: number | null; requiredBytes: number;
  archiveBytes: number; extractedBytes: number; packageBytes: number; temporaryBytes: number;
  workspaceBytes: number; estimated: boolean; enough: boolean; message: string; reservedBytes?: number;
}

export type DeliveryJob = {
  target?: ConsoleKind | ""
  packageOnly?: boolean
  components?: { kind: string; label: string; version: string; bytesTotal?: number | null; parts: { number: number; name: string; bytes?: number | null; downloaded: boolean }[] }[]
  retryable?: boolean
  space?: SpacePlan | null
  packaging?: PackagingInfo | null
  jobId: string
  titleId?: string
  packageKind?: string
  packageLabel?: string
  packageVersion?: string
  localPkg?: boolean
  createdAt?: number
  stageHistory?: string[]
  paused?: boolean
  stage: string
  progress: number
  bytesDone?: number
  bytesTotal?: number
  speedBps: number
  etaSeconds?: number | null
  message: string
  title?: string
  icon?: string
}

export type Ps4Probe = {
  root: string
  inbox: string
  worker: "ready" | "starting" | "stale" | "missing"
  workerBuild?: string
  message: string
}

export type Ps4InstalledTitle = {
  titleId: string
  name: string
  version?: string | null
  baseVersion?: string | null
  updateVersion?: string | null
  icon?: string | null
}

export type Ps4LibrarySnapshot = {
  entries: Ps4InstalledTitle[]
  complete: boolean
  truncated: boolean
  errors: string[]
  metadataWarnings: string[]
}

export type LocalPackage = {
  number: number
  path: string
  name: string
  fileName?: string
  kind: string
  size: number
  titleId?: string
  packageKind?: string
  version?: string
  icon?: string
}

export type ManualCandidate = {
  path: string
  name: string
  content: "pkg" | "dump" | "archive" | string
  detectedKind: string
  titleId?: string
  size: number
  needsTitle: boolean
  version?: string
  icon?: string
}

export type ManualItem = {
  path: string
  kind: string
  titleId?: string
}

export type MetadataGame = {
  id?: number
  name?: string
  description_raw?: string
  description?: string
  released?: string
  rating?: number
  metacritic?: number
  background_image?: string
  background_image_additional?: string
  genres?: Array<{ name?: string }>
  esrb_rating?: { name?: string }
  platforms?: Array<{ platform?: { name?: string } }>
  firmware?: string
  version?: string
  size?: string
}

export type MetadataResponse = {
  enabled?: boolean
  attribution?: string
  game?: MetadataGame
}

export type LoadState = "idle" | "loading" | "success" | "error"
export type Page = "Discover" | "Search" | "Downloads" | "Settings"

export const blankSettings: Settings = {
  activeConsole: "ps5",
  ps5Host: "",
  ps5Port: 9114,
  ps4Host: "",
  ps4Transport: "receiver",
  ps4ReceiverPort: 9114,
  ps4LoaderPort: 9090,
  ps5LoaderPort: 9021,
  ps4ServePort: 9115,
  ps4FtpPort: 2121,
  ps4FtpUser: "",
  ps4FtpPasswordConfigured: false,
  ps4ArchiveMode: "pc",
  ps4RemoveAfterInstall: true,
  resolverBaseUrl: "",
  downloadDir: "",
  onboardingComplete: false,
  realDebridEnabled: false,
  realDebridConfigured: false,
  torboxEnabled: false, torboxConfigured: false,
  alldebridEnabled: false, alldebridConfigured: false,
  theme: "dark",
  reduceMotion: false,
  transferMode: "balanced",
  uploadLanes: 4,
  packageDumps: false,
  downloadPackageOnly: false, keepArchives: false, keepExtractions: false, keepPackages: true,
  fpkgPreset: "fast",
  fpkgCompressionLevel: null,
  fpkgDoctor: false,
  fpkgPfsVersion: 2,
  fpkgEnginePath: "",
  targetFw: "",
  fpkgCleanupSource: false,
}

export const isActiveJob = (stage: string) =>
  !["complete", "delivered", "failed", "cancelled", "monitoring-ended"].includes(stage)
