import type { Game, PackageCandidate } from "../types"

export type ProviderHosts = {
  provider: string; enabled: boolean; configured: boolean; state: string; detail: string;
  supported: string[]; unavailable: string[]; unknown?: string[];
}
export const providerName = (id: string) => ({ "real-debrid": "Real-Debrid", torbox: "TorBox", alldebrid: "AllDebrid" }[id] || id)

export function hostState(url: string, inventory: ProviderHosts): "supported" | "unavailable" | "unknown" {
  if (!inventory.enabled || !inventory.configured || inventory.state !== "ready") return "unknown"
  let hostname: string
  try { hostname = new URL(url).hostname.toLowerCase().replace(/^www\./, "") } catch { return "unknown" }
  const match = (domains: string[]) => domains.reduce((longest, domain) => {
    const normalized = domain.toLowerCase().replace(/^www\./, "").replace(/^\*\./, "")
    return (hostname === normalized || hostname.endsWith(`.${normalized}`)) ? Math.max(longest, normalized.length) : longest
  }, 0)
  const supported = match(inventory.supported), unavailable = match(inventory.unavailable), unknown = match(inventory.unknown || [])
  if (unknown && unknown >= Math.max(supported, unavailable)) return "unknown"
  return unavailable && unavailable >= supported ? "unavailable" : supported ? "supported" : "unknown"
}

export function compatibleProviders(candidate: PackageCandidate, inventories: ProviderHosts[], selected = "") {
  return inventories.filter(item => (!selected || item.provider === selected) && hostState(candidate.url, item) === "supported").map(item => item.provider)
}
export function directCandidate(candidate: PackageCandidate) {
  const access = candidate.accessType?.toLowerCase()
  if (access && access !== "unknown") return access === "direct"
  try { return new URL(candidate.url).pathname.toLowerCase().endsWith(".pkg") } catch { return false }
}
export const variantKey = (game: Game) => `${game.titleId}|${game.region}|${game.sourceId || ""}`
export function groupedGames(games: Game[]): Game[] {
  const groups = new Map<string, Game[]>()
  for (const game of games.flatMap(item => item.variants?.length ? item.variants : [item])) {
    const key = `${game.titleId.startsWith("PPSA") ? "ps5" : "ps4"}:${game.name.normalize("NFKC").trim().toLowerCase()}`
    const variants = groups.get(key) || []
    if (!variants.some(item => variantKey(item) === variantKey(game))) variants.push(game)
    groups.set(key, variants)
  }
  return [...groups.values()].map(variants => ({ ...variants[0], variants }))
}
