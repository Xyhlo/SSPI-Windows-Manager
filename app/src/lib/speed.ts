/* Recent transfer speed per job, sampled twice a second while the app runs (Stats for nerds). */
import type { DeliveryJob } from "@/types"
import { activeTransfer, transferStats } from "./downloads"

export type SpeedSample = { t: number; v: number; stage: string }

const WINDOW_S = 90
const history = new Map<string, SpeedSample[]>()
let latest: DeliveryJob[] = []
let timer: number | null = null

function sample() {
  const now = performance.now() / 1000
  const seen = new Set<string>()
  for (const job of latest) {
    if (!activeTransfer(job)) continue
    seen.add(job.jobId)
    const { speedBps } = transferStats(job)
    const list = history.get(job.jobId) || []
    list.push({ t: now, v: speedBps ?? 0, stage: job.stage })
    while (list.length && now - list[0].t > WINDOW_S) list.shift()
    history.set(job.jobId, list)
  }
  for (const id of [...history.keys()]) if (!seen.has(id) && !latest.some(job => job.jobId === id)) history.delete(id)
}

/** Keeps the sampler pointed at the current job list. */
export function trackSpeeds(jobs: DeliveryJob[]) {
  latest = jobs
  if (timer == null) timer = window.setInterval(sample, 500)
}

export const speedSamples = (jobId: string) => history.get(jobId) || []
export const speedNow = () => performance.now() / 1000
