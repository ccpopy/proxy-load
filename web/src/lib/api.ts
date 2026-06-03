import { invoke } from "@tauri-apps/api/core"

interface ServiceInfo {
  api_port: number
  proxy_port: number
  database_path: string
  started_at: number
}

let apiBase = "http://127.0.0.1:3333"

export async function initApiBase() {
  if (!("__TAURI_INTERNALS__" in window)) {
    return apiBase
  }

  const info = await invoke<ServiceInfo>("get_service_info")
  apiBase = `http://127.0.0.1:${info.api_port}`
  return apiBase
}

export function getApiBase() {
  return apiBase
}

export function getWsUrl() {
  const url = new URL(apiBase)
  url.protocol = url.protocol === "https:" ? "wss:" : "ws:"
  url.pathname = "/ws"
  return url.toString()
}

export async function api<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(`${apiBase}${path}`, {
    ...init,
    headers: {
      "Content-Type": "application/json",
      ...init?.headers,
    },
  })
  const payload = await response.json().catch(() => null)
  if (!response.ok) {
    throw new Error(payload?.error || `请求失败: ${response.status}`)
  }
  return payload as T
}

export function jsonBody(value: unknown): RequestInit {
  return {
    method: "POST",
    body: JSON.stringify(value),
  }
}
