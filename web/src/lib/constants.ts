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
  proxy_port: 5678,
  periodic_test_interval: 300000,
  log_retention_days: 7,
  stats_retention_days: 30,
  pool_max_size: 50,
  pool_idle_timeout: 30000,
  pool_wait_timeout: 10000,
  circuit_failure_threshold: 5,
  circuit_timeout: 60000,
  circuit_half_open_attempts: 2,
  health_check_interval: 30000,
  health_degrade_threshold: 0.5,
  health_recover_threshold: 0.8,
  failfast_enabled: true,
  failfast_max_attempts: 3,
  failfast_attempt_timeout: 10000,
  failfast_total_timeout: 30000,
  algorithm_weights: {
    responseTime: 0.3,
    successRate: 0.25,
    connections: 0.2,
    stability: 0.15,
    recentPerf: 0.1,
  },
}

export const trafficChartConfig = {
  total: { label: "总请求", color: "var(--chart-1)" },
  success: { label: "成功", color: "var(--chart-2)" },
  failed: { label: "失败", color: "var(--chart-3)" },
} satisfies ChartConfig

export const latencyChartConfig = {
  avg: { label: "平均响应", color: "var(--chart-1)" },
} satisfies ChartConfig

export const barChartConfig = {
  requests: { label: "请求数", color: "var(--chart-1)" },
} satisfies ChartConfig

export const INITIAL_TRAFFIC_PAGE_SIZE = 25
