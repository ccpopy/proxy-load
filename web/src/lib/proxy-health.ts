import type { ProxyRecord, ProxyStatus } from "../types"

export function proxyHealthView(proxy: ProxyRecord): { key: ProxyStatus; label: string; entry: string } {
  const health = proxy.probe_health
  const required = proxy.health_policy?.mode === "required_probe"
  const entry = health?.transport_status === "reachable" ? "代理入口可达" :
    health?.transport_status === "unreachable" ? "代理入口不可达" : "代理入口待验证"
  if (required) {
    if (!health?.fresh) return { key: "unknown", label: "业务待验证", entry }
    if (health.readiness_status === "not_ready") return { key: "inactive", label: "业务不可用", entry }
    if (health.last_probe_result?.outcome !== "success") {
      return { key: "degraded", label: health.last_probe_result?.outcome === "failure" ? "业务测活失败" : "业务测活未完成", entry }
    }
    if (health.readiness_status !== "ready") return { key: "degraded", label: "业务恢复验证中", entry }
    return { key: "active", label: "业务就绪", entry }
  }
  if (health?.last_probe_result) {
    if (!health.fresh) return { key: "unknown", label: "待重新测活", entry }
    if (health.last_probe_result.outcome !== "success") return {
      key: health.transport_status === "unreachable" ? "inactive" : "degraded",
      label: health.last_probe_result.outcome === "failure" ? "最近测活失败" : "最近测活未完成", entry,
    }
  }
  const key = proxy.status ?? "unknown"
  return { key, label: { active: "在线", inactive: "离线", testing: "测试中", unknown: "未知", degraded: "测活异常" }[key], entry }
}

export function readinessAllowsNewConnections(proxy: ProxyRecord): boolean {
  if (proxy.enabled !== 1) return false
  if (proxy.health_policy?.mode !== "required_probe") return true
  return !!proxy.probe_health?.fresh && ["ready", "degraded"].includes(proxy.probe_health.readiness_status)
}

export function probeHealthDescription(proxy: ProxyRecord, now = Date.now()): string {
  const health = proxy.probe_health
  const last = health?.last_probe_result
  const policy = proxy.health_policy
  const lines = [policy?.mode === "required_probe" ? "专用节点：业务探测控制全部新连接" : "通用代理：目标失败不作节点级隔离"]
  if (last) {
    lines.push(`最近探测：${Math.max(0, Math.floor((now - last.observed_at) / 1000))} 秒前${health?.fresh ? "" : "（旧配置或上次运行，仅作历史）"}`)
    lines.push(`探测地址（${last.url_source === "node" ? "节点独立" : "继承全局"}）：${last.probe_url}`)
    lines.push(`阶段：${last.diagnostics.phase ?? "完成"}；范围：${last.diagnostics.scope ?? "无"}`)
    if (last.diagnostics.code) lines.push(`结果：${last.diagnostics.code.kind}${last.diagnostics.code.value == null ? "" : ` / ${last.diagnostics.code.value}`}`)
  }
  lines.push(`最近完整成功：${health?.last_success_at ? new Date(health.last_success_at).toLocaleString() : "尚无"}`)
  if (policy?.mode === "required_probe") {
    lines.push(`业务就绪有效期：${policy.max_age_seconds} 秒`)
    lines.push(`连续失败 ${health?.consecutive_failures ?? 0}/${policy.failure_threshold}；连续恢复 ${health?.consecutive_successes ?? 0}/${policy.recovery_threshold}`)
    lines.push(readinessAllowsNewConnections(proxy) ? "就绪门槛通过；仍受分组、熔断和容量限制" : "已阻止该节点全部新业务连接；后台继续探测")
  }
  lines.push(`新测活统计起始：${health?.statistics_started_at ? new Date(health.statistics_started_at).toLocaleString() : "尚未开始"}；未计入失败的探测：${health?.probe_excluded_count ?? 0}`)
  lines.push(`旧版混合历史：成功 ${proxy.success_count} / 失败 ${proxy.fail_count}（不并入新统计）`)
  return lines.join("\n")
}

export function entryHandshakeMillis(proxy: ProxyRecord): number | null {
  const last = proxy.probe_health?.last_probe_result
  if (!last) return proxy.response_time ?? null
  if (!proxy.probe_health?.fresh) return null
  const timings = last.diagnostics.timings
  if (timings.proxyTcpUs == null || timings.proxyAuthUs == null) return null
  return Math.floor((timings.proxyTcpUs + timings.proxyAuthUs) / 1000)
}
