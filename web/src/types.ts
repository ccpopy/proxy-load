export type ProxyStatus = "active" | "inactive" | "testing" | "unknown"

export type ProxyServiceState = "starting" | "running" | "failed"

export interface ProxyServiceStatus {
  state: ProxyServiceState
  running: boolean
  host: string
  port: number
  error?: string | null
}

export interface TestResult {
  success: boolean
  responseTime: number
  statusCode?: number | null
  error?: string | null
}

export interface ProxyRecord {
  id: number
  name: string
  type: "http" | "https" | "socks4" | "socks5"
  host: string
  port: number
  username?: string | null
  password?: string | null
  status?: ProxyStatus | null
  last_test?: string | null
  response_time?: number | null
  success_count: number
  fail_count: number
  priority: number
  enabled: number
  skip_cert_verify: number
  test_url?: string | null
  test_timeout?: number | null
  current_weight?: number | null
  _score?: number
  _activeConnections?: number
}

export interface DnsMapping {
  id: number
  domain: string
  ip: string
  description?: string | null
  enabled: number
  dynamic: number
  last_resolved?: string | null
  created_at?: string | null
  updated_at?: string | null
}

export interface ProxyGroupDomain {
  id: number
  group_id: number
  domain: string
}

export interface ProxyGroupMember {
  proxy_id: number
  name: string
  type: string
  host: string
  port: number
  status?: string | null
  enabled: number
}

export interface ProxyGroup {
  id: number
  name: string
  is_default: number
  enabled: number
  domains: ProxyGroupDomain[]
  members: ProxyGroupMember[]
}

export interface Overview {
  activeProxies: number
  totalRequests: number
  successRequests: number
  failedRequests: number
  avgResponseTime: number
  uptime: number
}

export interface TrafficLog {
  id: number
  proxy_id?: number | null
  proxy_name?: string | null
  proxy_type?: string | null
  proxy_host?: string | null
  proxy_port?: number | null
  target_host?: string | null
  target_port?: number | null
  success: number
  response_time?: number | null
  error_message?: string | null
  result_type?: string | null
  created_at?: string | null
}

export interface TrafficLogPage {
  items: TrafficLog[]
  page: number
  pageSize: number
  total: number
  totalPages: number
}

export interface HourlyStat {
  hour: string
  total_requests: number
  success_requests: number
  failed_requests: number
  avg_response_time?: number | null
}

export interface ProxyUsageStat {
  id: number
  name: string
  type: string
  total_requests: number
  success_requests: number
}

export interface TargetStat {
  target_host: string
  request_count: number
  success_count: number
  avg_response_time?: number | null
}

export interface VersionInfo {
  version: string
  runtime?: string
  platform?: string
  arch?: string
}

export interface UpdateArtifact {
  fileName: string
  path: string
  downloadUrl: string
  version: string
  kind: string
  isNewer: boolean
  size?: number | null
}

export interface UpdateInfo {
  currentVersion: string
  appDir: string
  downloadDir: string
  installMode: string
  source: string
  hasUpdate: boolean
  latest?: UpdateArtifact | null
  artifacts: UpdateArtifact[]
}

export interface TransferCounts {
  added: number
  skipped: number
}

export interface ImportSummary {
  proxies: TransferCounts
  dnsMappings: TransferCounts
  proxyGroups: TransferCounts
  unresolvedMembers: number
}

export interface ImportResult {
  canceled: boolean
  summary?: ImportSummary
}

export interface ExportResult {
  canceled: boolean
  path?: string
  counts?: {
    proxies: number
    dnsMappings: number
    proxyGroups: number
  }
}

export interface AdvancedConfig {
  proxy_port: number
  periodic_test_interval: number
  probe_recovery_interval: number
  probe_concurrency: number
  dns_refresh_interval: number
  background_run: boolean
  start_minimized: boolean
  log_retention_days: number
  stats_retention_days: number
  pool_max_size: number
  pool_idle_timeout: number
  pool_wait_timeout: number
  circuit_failure_threshold: number
  circuit_timeout: number
  circuit_half_open_attempts: number
  health_check_interval: number
  health_degrade_threshold: number
  health_recover_threshold: number
  failfast_enabled: boolean
  failfast_max_attempts: number
  failfast_attempt_timeout: number
  failfast_total_timeout: number
  algorithm_weights: {
    responseTime: number
    successRate: number
    connections: number
    stability: number
    recentPerf: number
  }
}
