import type { PackageCandidate } from "../types"

export function packageKind(candidate: PackageCandidate) {
  if (candidate.kind.toLowerCase() === "base") return "base"
  if (/\bback[ -]?port(?:ed)?\b/i.test(candidate.label)) return "backport"
  return candidate.kind.toLowerCase()
}

export function packageVersion(candidate: PackageCandidate) {
  const explicit = candidate.version?.trim().replace(/^v(?:ersion)?\s*/i, "")
  const value = explicit || candidate.label.match(/\bv(?:ersion)?\s*(\d+(?:\.\d+)+)/i)?.[1]
  if (!value || !/^\d+(?:\.\d+)*$/.test(value)) return ""
  const numbers = value.split(".").map(Number)
  while (numbers.length > 1 && numbers[numbers.length - 1] === 0) numbers.pop()
  return numbers.join(".")
}

export function packageKey(candidate: PackageCandidate) {
  return candidate.archiveSetId
    ? [candidate.archiveSetId, candidate.hoster || "", candidate.mirrorId || "", packageKind(candidate)].join("|")
    : candidate.candidateId || candidate.url
}

export function archivePartsFor(candidate: PackageCandidate, packages: PackageCandidate[]) {
  if (!candidate.archiveSetId) return []
  return packages.filter((item) => packageKey(item) === packageKey(candidate))
    .sort((left, right) => (left.archivePartNumber || 1) - (right.archivePartNumber || 1))
}

export function packageProblem(candidate: PackageCandidate, packages: PackageCandidate[], requireDump = false) {
  const parts = candidate.archiveSetId ? archivePartsFor(candidate, packages) : [candidate]
  if (!parts.length || !candidate.url) return "No usable download was listed."
  if (parts.some((part) => part.diagnostics?.some((note) => /incomplete|missing\s+part/i.test(note)))) {
    return "This archive set is incomplete. Choose another mirror."
  }
  if (!candidate.archiveSetId && (candidate.archivePartNumber || /\.part\d+/i.test(candidate.archiveFileName || ""))) {
    return "This split archive has no complete grouped set. Choose another mirror."
  }
  const expected = Math.max(...parts.map((part) => part.archivePartCount || 0))
  const numbers = parts.map((part) => part.archivePartNumber || 0)
  if (expected > 1 && (parts.length !== expected || new Set(numbers).size !== expected || numbers.some((number) => number < 1 || number > expected))) {
    return "This archive set is missing volumes or contains duplicate volumes."
  }
  if (parts.some((part) => /7z/i.test(part.archiveFormatHint || "") || /\.7z(?:\.\d+)?(?:$|[?#])/i.test(part.archiveFileName || part.url))) {
    return "7z extraction is unavailable in this build. Choose a RAR or ZIP mirror."
  }
  if (requireDump && parts.some((part) => /\.(?:pkg|ffpkg|ffpfs|ffpfsc|exfat|iso)(?:$|[?#])/i.test(part.archiveFileName || part.url) || /\bexfat\b/i.test(part.label))) {
    return "Backport packaging needs an archive containing a game folder, not an existing package or disk image."
  }
  return ""
}

function belongsToTitle(candidate: PackageCandidate, titleId: string) {
  const identifiers = `${candidate.expectedContentId || ""} ${candidate.label}`.match(/(?:CUSA|PPSA|SLUS|SLES|SCUS|SCES|SLPS|SLPM|SCPS|SCAJ|SLAJ|SLKA|SLKS|SCKA)\d{5}/gi) || []
  return identifiers.every((identifier) => identifier.toUpperCase() === titleId.toUpperCase())
}

export function backportPairProblem(base: PackageCandidate, backport: PackageCandidate, packages: PackageCandidate[], titleId: string) {
  if (packageKind(base) !== "base" || packageKind(backport) !== "backport") return "Choose a base game and a backport."
  if (!packages.includes(base) || !packages.includes(backport) || !belongsToTitle(base, titleId) || !belongsToTitle(backport, titleId)) {
    return "The base and backport must belong to this title."
  }
  const baseVersion = packageVersion(base)
  const backportVersion = packageVersion(backport)
  if (baseVersion && backportVersion && baseVersion !== backportVersion) return "The base game and backport versions do not match."
  return packageProblem(base, packages, true) || packageProblem(backport, packages, true)
}

export function matchingBackportBases(backport: PackageCandidate, packages: PackageCandidate[], titleId: string) {
  const seen = new Set<string>()
  return packages.filter((candidate) => {
    if (backportPairProblem(candidate, backport, packages, titleId)) return false
    const key = packageKey(candidate)
    if (seen.has(key)) return false
    seen.add(key)
    return true
  }).map((candidate) => archivePartsFor(candidate, packages)[0] || candidate)
}

export type PlannedPackage = { base: PackageCandidate; backport?: PackageCandidate }
export type PackagePlan = { items: PlannedPackage[]; added: PackageCandidate[]; problems: string[] }

export function packageReleaseKey(candidate: PackageCandidate) {
  return [packageKind(candidate), packageVersion(candidate), candidate.sourceId || "", candidate.groupId || candidate.label, candidate.firmware || ""].join("|")
}

function uniqueReleases(candidates: PackageCandidate[]) {
  return [...new Map(candidates.map(candidate => [packageReleaseKey(candidate), candidate])).values()]
}

function firmwareNumber(value = "") {
  const match = /^(?:FW\s*)?(\d+)\.(\d{1,2})(?:\s*\+)?$/i.exec(value.trim())
  return match ? Number(match[1]) * 100 + Number(match[2].padEnd(2, "0")) : null
}

export function matchingBackports(base: PackageCandidate, packages: PackageCandidate[], titleId: string, targetFw: string) {
  const target = firmwareNumber(targetFw)
  return uniqueReleases(packages.filter(candidate => {
    if (backportPairProblem(base, candidate, packages, titleId)) return false
    if (!packageVersion(base) || packageVersion(base) !== packageVersion(candidate)) return false
    if (base.sourceId && candidate.sourceId !== base.sourceId) return false
    const required = firmwareNumber(candidate.firmware)
    return target == null || required == null || required <= target
  }))
}

/** One base/backport pair is one job; other selections keep separate transfers. */
export function planPackages(selected: PackageCandidate[], available: PackageCandidate[], titleId: string,
  options: { packageDumps: boolean; autoBackports: boolean; targetFw: string; catalog?: PackageCandidate[] }): PackagePlan {
  const plan: PackagePlan = { items: [], added: [], problems: [] }
  const chosen = uniqueReleases(selected)
  const backports = options.packageDumps && titleId.startsWith("PPSA") ? chosen.filter(candidate => packageKind(candidate) === "backport") : []
  const bases = chosen.filter(candidate => !backports.includes(candidate))
  const paired = new Set<string>()
  for (const backport of backports) {
    const matches = bases.filter(base => !backportPairProblem(base, backport, available, titleId))
    if (matches.length === 0) {
      if (bases.some(base => packageKind(base) === "base")) {
        plan.problems.push(`${backport.label}: it does not match the selected base version.`)
        continue
      }
      const choices = uniqueReleases(matchingBackportBases(backport, available, titleId))
      if (choices.length === 1) { bases.push(choices[0]); plan.added.push(choices[0]) }
      else plan.problems.push(`${backport.label}: select one matching base game.`)
    } else if (matches.length > 1) plan.problems.push(`${backport.label}: more than one selected base matches.`)
  }
  const baseVersions = new Set<string>()
  for (const base of bases) {
    const problem = packageProblem(base, available)
    if (problem) { plan.problems.push(`${base.label}: ${problem}`); continue }
    if (packageKind(base) !== "base" || !options.packageDumps || !titleId.startsWith("PPSA")) { plan.items.push({ base }); continue }
    const version = packageVersion(base)
    if (baseVersions.has(version)) { plan.problems.push("Select one base archive per game version; the other rows are alternatives."); continue }
    baseVersions.add(version)
    let matches = backports.filter(backport => !backportPairProblem(base, backport, available, titleId))
    if (!matches.length && options.autoBackports && !packageProblem(base, available, true) && !/\bback[ -]?port\b/i.test(base.label)) {
      matches = matchingBackports(base, available, titleId, options.targetFw)
      if (matches.length === 1) plan.added.push(matches[0])
      if (!matches.length && (options.catalog || available).some(candidate => packageKind(candidate) === "backport"
        && belongsToTitle(candidate, titleId) && (!base.sourceId || candidate.sourceId === base.sourceId)
        && (!packageVersion(candidate) || !version || packageVersion(candidate) === version))) {
        plan.problems.push(`${base.label}: choose a matching backport and a supported mirror; automatic pairing could not verify one.`)
        continue
      }
    }
    if (matches.length > 1) { plan.problems.push(`${base.label}: select the backport you want; several releases match.`); continue }
    const backport = matches[0]
    const target = firmwareNumber(options.targetFw), baseFirmware = firmwareNumber(base.firmware)
    if (!backport && target != null && baseFirmware != null && baseFirmware > target) {
      plan.problems.push(`${base.label}: the base is listed for firmware ${base.firmware}; select a compatible backport for ${options.targetFw}.`)
      continue
    }
    if (backport) {
      const target = firmwareNumber(options.targetFw), required = firmwareNumber(backport.firmware)
      if (target != null && required != null && required > target) { plan.problems.push(`${backport.label}: its listed firmware is above ${options.targetFw}.`); continue }
      paired.add(packageReleaseKey(backport))
    }
    plan.items.push({ base, backport })
  }
  for (const backport of backports) if (!paired.has(packageReleaseKey(backport))) plan.problems.push(`${backport.label}: no single matching base was selected.`)
  plan.problems = [...new Set(plan.problems)]
  return plan
}
