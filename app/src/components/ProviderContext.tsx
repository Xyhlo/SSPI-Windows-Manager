import { createContext, useCallback, useContext, useEffect, useState, type ReactNode } from "react"
import { invoke } from "@tauri-apps/api/core"
import type { Settings } from "../types"
import type { ProviderHosts } from "../lib/providers"

const Context = createContext<{ inventories: ProviderHosts[]; loading: boolean; refresh: () => void }>({ inventories: [], loading: false, refresh: () => {} })
export const useProviders = () => useContext(Context)

export function ProviderContext({ settings, children }: { settings: Settings; children: ReactNode }) {
  const [inventories, setInventories] = useState<ProviderHosts[]>([])
  const [loading, setLoading] = useState(false)
  const [revision, setRevision] = useState(0)
  const refresh = useCallback(() => setRevision(old => old + 1), [])
  useEffect(() => {
    let active = true
    setLoading(true)
    setInventories([])
    invoke<ProviderHosts[]>("get_provider_hosts").then(value => { if (active) setInventories(value) })
      .catch(() => { if (active) setInventories([]) }).finally(() => { if (active) setLoading(false) })
    return () => { active = false }
  }, [settings, revision])
  return <Context.Provider value={{ inventories, loading, refresh }}>{children}</Context.Provider>
}
