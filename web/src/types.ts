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
  failureScope?: "proxy" | "target" | "network" | "local_resource" | "configuration" | "unknown"
  diagnostics?: {
    evidence: Record<"proxyTcp" | "proxyAuth" | "targetTunnel" | "targetTls",
      "not_observed" | "not_applicable" | "established" | "accepted" | "rejected" | "failed">
    phase?: string | null
    code?: { kind: string; value?: string | number } | null
    scope?: string | null
    rawOsError?: number | null
    timings: { totalUs: number; [phase: string]: number | null }
    redirects: number
    finalOrigin?: string | null
  }
  responseTime: number
  statusCode?: number | null
  error?: string | null
}

export interface ProxyRecord {
  id: number
  name: string
  type: "http" | "socks4" | "socks5"
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
  algorithm_override?: string | null
  sticky_failover_seconds?: number
  domains: ProxyGroupDomain[]
  members: ProxyGroupMember[]
}

export interface Overview {
  databaseQueue?: {
    queueLength: number
    capacity: number
    droppedLogs: number
    statusQueueLength?: number
    droppedStatus?: number
    coalescedStatus?: number
    retriedStatus?: number
    databaseErrors: number
    writtenLogs: number
    maxBatchDurationMs: number
  }
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
  snapshotId?: number
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
  hasManifest: boolean
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
  automaticInstallAvailable: boolean
  manualReason?: string | null
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
  target_quality_mode: "off" | "observe" | "adaptive"
  max_connections: number
  max_handshakes: number
  max_global_dials: number
  max_proxy_dials: number
  effective_concurrency?: Pick<AdvancedConfig, "max_connections" | "max_handshakes" | "max_global_dials" | "max_proxy_dials">
  proxy_port: number
  allow_lan: boolean
  inbound_auth_enabled: boolean
  inbound_auth_username: string
  inbound_auth_password: string
  periodic_test_interval: number
  probe_recovery_interval: number
  probe_concurrency: number
  probe_failure_threshold: number
  startup_probe_enabled: boolean
  dns_refresh_interval: number
  background_run: boolean
  start_minimized: boolean
  log_retention_days: number
  circuit_failure_threshold: number
  circuit_timeout: number
  failfast_enabled: boolean
  failfast_max_attempts: number
  failfast_attempt_timeout: number
  failfast_total_timeout: number
}
