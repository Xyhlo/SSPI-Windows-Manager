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
  /** Remove from list / Clear inactive never delete a finished package. */
  keepPackagesOnRemove?: boolean
  /** "fastest" | "balanced" | "smallest"; older builds saved fast / standard. */
  fpkgPreset: string
  fpkgCompressionLevel?: number | null
  fpkgDoctor: boolean
  fpkgPfsVersion: number
  fpkgEnginePath: string
  targetFw: string
  fpkgCleanupSource: boolean
  /** Dump packaging output: an installable FPKG or a ShadowMount Plus exFAT image. */
  packageFormat?: PackageFormat
  /** Lizard (AMPR/LZ4) asset packing inside exFAT images. Experimental. */
  lizardPacking?: boolean
  /** What adding game folders does: ask, or start one batch action right away. */
  folderAction?: FolderAction
  /** Games that download at once; the priority game downloads beside them. */
  downloadSlots?: number
  /** Games that extract at once. */
  extractionSlots?: number
  /** Connections shared by every download, split by the bytes each has left. */
  downloadConnections?: number
}
export type FolderAction = "ask" | "package" | "package-send" | "send"
export type PackageFormat = "fpkg" | "exfat"

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
  packageDumps?: boolean
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
  /** Engine telemetry (stages-io-v1); the last snapshot is the build summary. */
  engine?: PackEngine | null
  /** Where the builder's temporary inner image went (the app picks the drive). */
  tempPath?: string
  workspaceSeconds?: number | null
  totalSeconds?: number | null
  /** "exfat" for ShadowMount images; empty or "fpkg" for packages. */
  format?: string
  /** Lizard (AMPR/LZ4) packing summary for images built with it. */
  lizard?: LizardSummary | null
}
export type LizardSummary = {
  runtime: string; filesTotal: number; filesPacked: number; filesAutoLoose: number
  packedBytes: number; storedBytes: number; chunks: number; sharedChunks: number; packs: number; crcPath: string
}

export type PackStageState = "pending" | "active" | "done"
export type PackStage = { id: string; label: string; state: PackStageState; seconds: number; progress?: number | null; detail?: string | null }
/** Measured by the packaging engine: builder milestones and this process's own I/O counters. */
export type PackEngine = {
  stage: string; stageIndex: number; stageCount: number; stageProgress?: number | null
  stages: PackStage[]
  io: { readBytes: number; writeBytes: number; readBps?: number | null; writeBps?: number | null }
  inputBytes?: number | null; compressedBytes?: number | null
  files?: { done: number; total: number } | null
  currentFile?: string | null; currentFileProgress?: number | null
  etaSeconds?: number | null
  level: number; workers: number; pfs: string; blockKiB: number; elapsedSeconds: number
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
  providerPreparation?: { provider: string; state: string; progress: number | null; bytesDone: number; bytesTotal: number; speedBps: number; etaSeconds: number | null } | null
  jobId: string
  titleId?: string
  packageKind?: string
  packageLabel?: string
  packageVersion?: string
  localPkg?: boolean
  createdAt?: number
  stageHistory?: string[]
  paused?: boolean
  /** The one game that downloads, extracts and packages first. */
  priority?: boolean
  /** Connections a segmented download is using now (download events only). */
  connections?: number | null
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
  downloadPackageOnly: false, keepArchives: false, keepExtractions: false, keepPackages: true, keepPackagesOnRemove: true,
  fpkgPreset: "balanced",
  fpkgCompressionLevel: null,
  fpkgDoctor: false,
  fpkgPfsVersion: 2,
  fpkgEnginePath: "",
  targetFw: "",
  fpkgCleanupSource: false,
  packageFormat: "fpkg",
  lizardPacking: false,
  folderAction: "ask",
  downloadSlots: 4,
  extractionSlots: 2,
  downloadConnections: 16,
}

export const isActiveJob = (stage: string) =>
  !["complete", "delivered", "failed", "cancelled", "monitoring-ended"].includes(stage)
