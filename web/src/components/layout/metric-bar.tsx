import { Cable, CheckCircle2, Gauge, XCircle, type LucideIcon } from "lucide-react"

import { cn } from "@/lib/utils"

function MetricTile({
  label,
  value,
  icon: Icon,
  accent,
}: {
  label: string
  value: string | number
  icon: LucideIcon
  accent: string
}) {
  return (
    <div className="relative overflow-hidden rounded-lg border bg-card p-4">
      <span
        className="absolute inset-x-0 top-0 h-px opacity-70"
        style={{ background: accent }}
      />
      <div className="flex items-center justify-between gap-2">
        <span className="text-[0.7rem] font-medium uppercase tracking-[0.14em] text-muted-foreground">
          {label}
        </span>
        <Icon className="size-4 text-muted-foreground" style={{ color: accent }} />
      </div>
      <div className="mt-2 font-mono text-2xl font-semibold tabular-nums tracking-tight">
        {value}
      </div>
    </div>
  )
}

export function MetricBar({
  activeCount,
  failedCount,
  totalRequests,
  avgResponseMs,
}: {
  activeCount: number
  failedCount: number
  totalRequests: number
  avgResponseMs: number
}) {
  return (
    <div className={cn("grid gap-3 md:grid-cols-2 xl:grid-cols-4")}>
      <MetricTile
        label="在线代理"
        value={activeCount}
        icon={CheckCircle2}
        accent="var(--success)"
      />
      <MetricTile
        label="离线代理"
        value={failedCount}
        icon={XCircle}
        accent="var(--destructive)"
      />
      <MetricTile
        label="24小时请求"
        value={totalRequests.toLocaleString()}
        icon={Cable}
        accent="var(--primary)"
      />
      <MetricTile
        label="平均建连"
        value={`${avgResponseMs} ms`}
        icon={Gauge}
        accent="var(--chart-4)"
      />
    </div>
  )
}
