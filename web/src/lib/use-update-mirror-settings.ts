import { useEffect, useRef, useState } from "react"

import { command, commandErrorMessage } from "@/lib/api"

interface MirrorSettings {
  url: string
  defaultUrl: string
}

// 状态保留在 AboutDialog，而非随弹窗内容卸载，避免关闭/重开时丢失正在保存的结果。
export function useUpdateMirrorSettings(open: boolean, onSaved: () => void) {
  const [settings, setSettings] = useState<MirrorSettings | null>(null)
  const [draft, setDraft] = useState("")
  const [loading, setLoading] = useState(true)
  const [saving, setSaving] = useState(false)
  const [loadError, setLoadError] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [notice, setNotice] = useState<string | null>(null)
  const [reload, setReload] = useState(0)
  const savingRef = useRef(false)

  useEffect(() => {
    if (!open || savingRef.current) return
    let cancelled = false
    setLoading(true)
    setLoadError(null)
    setError(null)
    setNotice(null)
    command<MirrorSettings>("get_update_mirror_settings")
      .then((value) => {
        if (cancelled) return
        setSettings(value)
        setDraft(value.url)
      })
      .catch((cause) => {
        if (cancelled) return
        setLoadError(commandErrorMessage(cause, "加载加速地址失败"))
      })
      .finally(() => {
        if (!cancelled) setLoading(false)
      })
    return () => { cancelled = true }
  }, [open, reload])

  function edit(value: string) {
    setDraft(value)
    setError(null)
    setNotice(null)
  }

  function restoreDefault() {
    if (!settings) return
    edit(settings.defaultUrl)
    setNotice("已填入默认地址，点击保存后生效。")
  }

  async function save() {
    if (savingRef.current || loading || loadError || !settings) return
    savingRef.current = true
    setSaving(true)
    setError(null)
    setNotice(null)
    try {
      const value = await command<MirrorSettings>("save_update_mirror_url", { url: draft })
      setSettings(value)
      setDraft(value.url)
      setNotice("加速地址已保存。")
      onSaved()
    } catch (cause) {
      setError(commandErrorMessage(cause, "连通性测试或保存失败，原地址保持不变"))
    } finally {
      savingRef.current = false
      setSaving(false)
    }
  }

  return {
    draft, settings, loading, saving, loadError, error, notice,
    edit, restoreDefault, save,
    retryLoad: () => setReload((value) => value + 1),
  }
}
