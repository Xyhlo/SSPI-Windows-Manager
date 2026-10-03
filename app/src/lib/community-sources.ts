import { invoke } from "@tauri-apps/api/core"
import type { PackageSource } from "@/types"

export type CommunitySource = {
  id: string; source: string; name: string; description: string; tags: string[]
  size: number; date: string; revision: number; installed: boolean
}
export type CommunitySources = { entries: CommunitySource[]; warning: string | null }

export async function listCommunitySources(demo = false): Promise<CommunitySources> {
  if (demo) return {
    entries: [
      { id: "preview-ps4", source: "preview-ps4", name: "Community PS4", description: "Example catalog for the offline preview.", tags: ["PS4"], size: 1229034, date: "2026-10-01", revision: 2, installed: false },
      { id: "preview-ps5", source: "preview-ps5", name: "Community PS5", description: "Example catalog for the offline preview.", tags: ["PS5"], size: 2799573, date: "2026-10-01", revision: 2, installed: false },
    ], warning: "Offline preview. These are sample sources; nothing is downloaded or installed.",
  }
  return invoke("list_community_sources")
}

export async function installCommunitySource(id: string): Promise<PackageSource[]> {
  return invoke("install_community_source", { id })
}
