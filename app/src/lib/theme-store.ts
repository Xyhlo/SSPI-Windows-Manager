/* The theme being edited: its settings in local storage, its images (custom icons and the
   wallpaper, as data URLs) in IndexedDB, which has room for them. Both fall back to memory. */
import { DEFAULT_SPEC, type ThemeSpec } from "./ps4-theme"

const SPEC_KEY = "sspi.ps4-theme.v2"
const memory = new Map<string, string>()

export function loadSpec(): ThemeSpec {
  try {
    const saved = JSON.parse(window.localStorage.getItem(SPEC_KEY) || "null") as Partial<ThemeSpec> | null
    return saved ? { ...DEFAULT_SPEC, ...saved, fit: { ...(saved.fit || {}) } } : DEFAULT_SPEC
  } catch { return DEFAULT_SPEC }
}
export function saveSpec(spec: ThemeSpec) {
  try { window.localStorage.setItem(SPEC_KEY, JSON.stringify(spec)) } catch { /* storage unavailable: the session keeps it */ }
}

let opening: Promise<IDBDatabase | null> | null = null
function database() {
  opening ??= new Promise(resolve => {
    try {
      const request = indexedDB.open("sspi-themes", 1)
      request.onupgradeneeded = () => request.result.createObjectStore("images")
      request.onsuccess = () => resolve(request.result)
      request.onerror = () => resolve(null)
    } catch { resolve(null) }
  })
  return opening
}
function run<T>(mode: IDBTransactionMode, work: (store: IDBObjectStore) => IDBRequest<T>) {
  return database().then(db => db && new Promise<T | null>(resolve => {
    try {
      const request = work(db.transaction("images", mode).objectStore("images"))
      request.onsuccess = () => resolve(request.result)
      request.onerror = () => resolve(null)
    } catch { resolve(null) }
  }))
}

/** Every stored image, keyed by slot (or "wallpaper"). */
export async function loadImages(): Promise<Record<string, string>> {
  const out: Record<string, string> = Object.fromEntries(memory)
  const keys = await run("readonly", store => store.getAllKeys())
  const values = await run("readonly", store => store.getAll())
  if (keys && values) keys.forEach((key, i) => { if (typeof values[i] === "string") out[String(key)] = values[i] as string })
  return out
}
export async function putImage(key: string, value: string | null) {
  if (value) memory.set(key, value); else memory.delete(key)
  await run<unknown>("readwrite", store => (value ? store.put(value, key) : store.delete(key)) as IDBRequest<unknown>)
}
