import type { CSSProperties } from "react"

import { cn } from "@/lib/utils"
import { proxyHealthView, probeHealthDescription } from "@/lib/proxy-health"
import type { ProxyRecord, ProxyStatus } from "@/types"

const PROXY_STATUS: Record<
  ProxyStatus,
  { label: string; className: string; dot: string }
> = {
  active: {
    label: "在线",
    className: "border-success/30 bg-success/10 text-success",
    dot: "var(--success)",
  },
  inactive: {
    label: "离线",
    className: "border-destructive/30 bg-destructive/10 text-destructive",
    dot: "var(--destructive)",
  },
  testing: {
    label: "测试中",
    className: "border-warning/30 bg-warning/10 text-warning",
    dot: "var(--warning)",
  },
  degraded: {
    label: "测活异常",
    className: "border-warning/30 bg-warning/10 text-warning",
    dot: "var(--warning)",
  },
  unknown: {
    label: "未知",
    className: "border-border bg-muted/50 text-muted-foreground",
    dot: "var(--muted-foreground)",
  },
}

export function StatusBadge({ proxy }: { proxy: ProxyRecord }) {
  const { key, label } = proxyHealthView(proxy)
  const tone = PROXY_STATUS[key] ?? PROXY_STATUS.unknown
  return (
    <span
      title={probeHealthDescription(proxy)}
      className={cn(
        "inline-flex items-center gap-1.5 rounded-sm border px-1.5 py-0.5 text-xs font-medium",
        tone.className
      )}
    >
      <span className="size-1.5 rounded-full" style={{ background: tone.dot }} />
      {label}
    </span>
  )
}

const CONNECTION = {
  online: { label: "代理运行中", color: "var(--success)" },
  offline: { label: "代理未运行", color: "var(--destructive)" },
  connecting: { label: "检测代理端口", color: "var(--muted-foreground)" },
} as const

export function ConnectionBadge({
  state,
}: {
  state: "online" | "offline" | "connecting"
}) {
  const tone = CONNECTION[state]
  return (
    <span className="inline-flex items-center gap-2 text-xs text-muted-foreground">
      <span
        className="status-dot"
        data-pulse={state === "online"}
        style={{ "--dot-color": tone.color } as CSSProperties}
      />
      <span className={cn(state === "offline" && "text-destructive")}>
        {tone.label}
      </span>
    </span>
  )
}
