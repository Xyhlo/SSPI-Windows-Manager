export type Settings = {
  ps5Host: string
  ps5Port: number
  resolverBaseUrl: string
  downloadDir: string
  onboardingComplete: boolean
  realDebridEnabled: boolean
  realDebridConfigured: boolean
  theme: string
  reduceMotion: boolean
  transferMode: string
  uploadLanes: number
}

export type Game = {
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

export type DeliveryJob = {
  jobId: string
  titleId?: string
  packageKind?: string
  packageLabel?: string
  packageVersion?: string
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

export type LocalPackage = {
  number: number
  path: string
  name: string
  kind: string
  size: number
  titleId?: string
}

export type ManualCandidate = {
  path: string
  name: string
  content: "pkg" | "dump" | "archive" | string
  detectedKind: string
  titleId?: string
  size: number
  needsTitle: boolean
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
  ps5Host: "",
  ps5Port: 9114,
  resolverBaseUrl: "",
  downloadDir: "",
  onboardingComplete: false,
  realDebridEnabled: false,
  realDebridConfigured: false,
  theme: "dark",
  reduceMotion: false,
  transferMode: "balanced",
  uploadLanes: 4,
}

export const isActiveJob = (stage: string) =>
  !["complete", "failed", "cancelled", "monitoring-ended"].includes(stage)
