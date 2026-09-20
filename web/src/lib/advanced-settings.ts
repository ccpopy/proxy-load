import type { AdvancedConfig } from "../types"

export const businessLimits = [
  { key: "max_connections", label: "业务连接上限", max: 16384 },
  { key: "max_handshakes", label: "入站握手并发", max: 4096 },
  { key: "max_global_dials", label: "全局业务拨号并发", max: 2048 },
  { key: "max_proxy_dials", label: "单节点业务拨号并发", max: 512 },
] as const

export function advancedSettingsPayload(config: AdvancedConfig) {
  const saved = { ...config }
  delete saved.effective_concurrency
  return saved
}

export function businessLimitError(config: AdvancedConfig): string | null {
  for (const { key, label, max } of businessLimits) {
    if (!Number.isInteger(config[key]) || config[key] < 1 || config[key] > max) {
      return `${label}必须为 1 到 ${max} 之间的整数`
    }
  }
  if (config.max_proxy_dials > config.max_global_dials || config.max_global_dials > config.max_connections
      || config.max_handshakes > config.max_connections) {
    return "单节点拨号 ≤ 全局业务拨号 ≤ 业务连接；入站握手 ≤ 业务连接"
  }
  return null
}
