import { invoke } from "@tauri-apps/api/core"
import { listen } from "@tauri-apps/api/event"

export interface ServiceInfo {
  proxy_port: number
  database_path: string
  started_at: number
}

export interface ServerEvent {
  type: string
  data?: unknown
  timestamp: number
}

type CommandArgs = Record<string, unknown>

export async function initServiceInfo() {
  return command<ServiceInfo>("get_service_info")
}

export async function command<T>(name: string, args?: CommandArgs): Promise<T> {
  ensureTauri()
  return invoke<T>(name, args)
}

export function commandErrorMessage(error: unknown, fallback: string) {
  if (error instanceof Error && error.message) return error.message
  if (typeof error === "string" && error.trim()) return error
  if (
    error &&
    typeof error === "object" &&
    "message" in error &&
    typeof error.message === "string" &&
    error.message.trim()
  ) {
    return error.message
  }
  return fallback
}

export function onServerEvent(handler: (message: ServerEvent) => void) {
  ensureTauri()
  return listen<ServerEvent>("server-event", (event) => handler(event.payload))
}

export async function api<T>(path: string, init?: RequestInit): Promise<T> {
  const route = new URL(path, "http://tauri.local")
  const method = (init?.method ?? "GET").toUpperCase()
  const body = parseJsonBody(init?.body)
  const parts = route.pathname.split("/").filter(Boolean)

  if (parts[0] !== "api") {
    throw new Error(`不支持的应用内接口路径: ${route.pathname}`)
  }

  if (method === "GET" && route.pathname === "/api/proxies") {
    return command<T>("list_proxies")
  }
  if (method === "GET" && route.pathname === "/api/proxy-service-status") {
    return command<T>("proxy_service_status")
  }
  if (method === "POST" && route.pathname === "/api/proxies") {
    return command<T>("create_proxy", { input: body })
  }
  if (method === "POST" && route.pathname === "/api/proxies/priorities") {
    return command<T>("update_proxy_priorities", {
      priorities: requireBodyField<Record<string, number>>(body, "priorities"),
    })
  }
  if (parts[1] === "proxies" && parts[2]) {
    const id = parseId(parts[2], "代理")
    if (method === "GET" && parts.length === 3) {
      return command<T>("get_proxy", { id })
    }
    if (method === "PUT" && parts.length === 3) {
      return command<T>("update_proxy", { id, input: body })
    }
    if (method === "DELETE" && parts.length === 3) {
      return command<T>("delete_proxy", { id })
    }
    if (method === "PUT" && parts[3] === "priority") {
      return command<T>("update_proxy_priority", {
        id,
        priority: requireBodyField<number>(body, "priority"),
      })
    }
    if (method === "POST" && parts[3] === "test") {
      return command<T>("test_proxy", { id })
    }
  }

  if (method === "GET" && route.pathname === "/api/dns-mappings") {
    return command<T>("list_dns_mappings")
  }
  if (method === "POST" && route.pathname === "/api/dns-mappings") {
    return command<T>("create_dns_mapping", { input: body })
  }
  if (parts[1] === "dns-mappings" && parts[2]) {
    const id = parseId(parts[2], "DNS 映射")
    if (method === "PUT" && parts.length === 3) {
      return command<T>("update_dns_mapping", { id, input: body })
    }
    if (method === "DELETE" && parts.length === 3) {
      return command<T>("delete_dns_mapping", { id })
    }
    if (method === "PUT" && parts[3] === "toggle") {
      return command<T>("toggle_dns_mapping", { id })
    }
  }

  if (method === "GET" && route.pathname === "/api/proxy-groups") {
    return command<T>("list_proxy_groups")
  }
  if (method === "POST" && route.pathname === "/api/proxy-groups") {
    return command<T>("create_proxy_group", { input: body })
  }
  if (parts[1] === "proxy-groups" && parts[2]) {
    const id = parseId(parts[2], "代理分组")
    if (method === "PUT" && parts.length === 3) {
      return command<T>("update_proxy_group", { id, input: body })
    }
    if (method === "DELETE" && parts.length === 3) {
      return command<T>("delete_proxy_group", { id })
    }
  }

  if (method === "GET" && route.pathname === "/api/settings") {
    return command<T>("get_settings")
  }
  if (method === "POST" && route.pathname === "/api/settings") {
    return command<T>("save_settings", { settings: body })
  }
  if (method === "GET" && route.pathname === "/api/advanced-config") {
    return command<T>("get_advanced_config")
  }
  if (method === "POST" && route.pathname === "/api/advanced-config") {
    return command<T>("save_advanced_config", { config: body })
  }
  if (method === "POST" && route.pathname === "/api/advanced-config/reset") {
    return command<T>("reset_advanced_config")
  }
  if (method === "GET" && route.pathname === "/api/advanced-config/export") {
    return command<T>("export_config")
  }
  if (method === "GET" && route.pathname === "/api/test-urls") {
    return command<T>("test_urls")
  }

  if (method === "GET" && route.pathname === "/api/stats/overview") {
    return command<T>("stats_overview")
  }
  if (method === "GET" && route.pathname === "/api/stats/hourly") {
    return command<T>("stats_hourly")
  }
  if (method === "GET" && route.pathname === "/api/stats/proxy-usage") {
    return command<T>("stats_proxy_usage")
  }
  if (method === "GET" && route.pathname === "/api/stats/targets") {
    return command<T>("stats_targets")
  }
  if (method === "GET" && route.pathname === "/api/stats/failed-targets") {
    return command<T>("stats_failed_targets")
  }
  if (method === "GET" && route.pathname === "/api/stats/circuit-breakers") {
    return command<T>("stats_circuit_breakers")
  }
  if (method === "GET" && route.pathname === "/api/stats/connection-pools") {
    return command<T>("stats_connection_pools")
  }

  if (method === "GET" && route.pathname === "/api/traffic-logs") {
    return command<T>("traffic_logs", {
      page: Number(route.searchParams.get("page") ?? "1"),
      pageSize: Number(route.searchParams.get("page_size") ?? "50"),
    })
  }
  if (method === "DELETE" && route.pathname === "/api/traffic-logs") {
    return command<T>("clear_traffic_logs")
  }
  if (method === "GET" && route.pathname === "/api/version") {
    return command<T>("version_info")
  }

  throw new Error(`不支持的应用内接口: ${method} ${route.pathname}`)
}

export function jsonBody(value: unknown): RequestInit {
  return {
    method: "POST",
    body: JSON.stringify(value),
  }
}

function ensureTauri() {
  if (!("__TAURI_INTERNALS__" in window)) {
    throw new Error("请通过 Tauri 应用窗口打开本系统，当前版本不支持普通浏览器直连管理 API。")
  }
}

function parseJsonBody(body: BodyInit | null | undefined) {
  if (body === undefined || body === null) return undefined
  if (typeof body !== "string") {
    throw new Error("应用内接口只接受 JSON 字符串请求体")
  }
  return JSON.parse(body) as Record<string, unknown>
}

function requireBodyField<T>(body: unknown, field: string): T {
  if (!body || typeof body !== "object" || !(field in body)) {
    throw new Error(`缺少请求字段: ${field}`)
  }
  return (body as Record<string, T>)[field]
}

function parseId(value: string, label: string) {
  const id = Number(value)
  if (!Number.isInteger(id) || id < 1) {
    throw new Error(`${label} ID 无效: ${value}`)
  }
  return id
}
