import {
  Activity,
  ArrowLeftRight,
  Globe2,
  Layers3,
  Server,
  SlidersHorizontal,
} from "lucide-react"

import type { ChartConfig } from "@/components/ui/chart"
import type { AdvancedConfig, Overview } from "@/types"

export type SectionKey =
  | "proxies"
  | "dns"
  | "settings"
  | "groups"
  | "transfer"
  | "status"
export type ThemeMode = "light" | "dark"

export const navItems = [
  { key: "proxies", label: "代理配置", icon: Server },
  { key: "dns", label: "DNS映射", icon: Globe2 },
  { key: "settings", label: "负载设置", icon: SlidersHorizontal },
  { key: "groups", label: "代理分组", icon: Layers3 },
  { key: "transfer", label: "导入导出", icon: ArrowLeftRight },
  { key: "status", label: "系统状态", icon: Activity },
] satisfies Array<{ key: SectionKey; label: string; icon: typeof Server }>

export const emptyOverview: Overview = {
  activeProxies: 0,
  totalRequests: 0,
  successRequests: 0,
  failedRequests: 0,
  avgResponseTime: 0,
  uptime: 0,
}

export const defaultAdvanced: AdvancedConfig = {
  target_quality_mode: "off",
  max_connections: 1024,
  max_handshakes: 128,
  max_global_dials: 64,
  max_proxy_dials: 32,
  proxy_port: 5678,
  allow_lan: false,
  inbound_auth_enabled: false,
  inbound_auth_username: "",
  inbound_auth_password: "",
  periodic_test_interval: 180000,
  probe_recovery_interval: 180000,
  probe_concurrency: 8,
  probe_failure_threshold: 2,
  startup_probe_enabled: true,
  dns_refresh_interval: 300000,
  background_run: false,
  start_minimized: false,
  log_retention_days: 7,
  circuit_failure_threshold: 5,
  circuit_timeout: 60000,
  failfast_enabled: true,
  failfast_max_attempts: 3,
  failfast_attempt_timeout: 5000,
  failfast_total_timeout: 15000,
}

export const trafficChartConfig = {
  total: { label: "总请求", color: "var(--chart-1)" },
  success: { label: "建连成功", color: "var(--chart-2)" },
  failed: { label: "建连失败", color: "var(--chart-3)" },
} satisfies ChartConfig

export const latencyChartConfig = {
  avg: { label: "平均建连", color: "var(--chart-1)" },
} satisfies ChartConfig

export const barChartConfig = {
  requests: { label: "请求数", color: "var(--chart-1)" },
} satisfies ChartConfig

export const TRAFFIC_PAGE_SIZES = [10, 20, 30, 40, 50] as const
export const INITIAL_TRAFFIC_PAGE_SIZE: number = TRAFFIC_PAGE_SIZES[0]
