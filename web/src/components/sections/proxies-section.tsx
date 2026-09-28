import { useMemo, useState, type ReactNode } from "react"
import { ChevronDown, Edit, Loader2, Plus, Server, Trash2, Zap } from "lucide-react"
import { Collapsible } from "radix-ui"
import { toast } from "sonner"

import { api } from "@/lib/api"
import { cn } from "@/lib/utils"
import { proxyMatchesFilter, proxyHealthView, probeHealthDetails, entryHandshakeMillis } from "@/lib/proxy-health"
import type { ProxyRecord, ProxyStatus, TestResult } from "@/types"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card"
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs"
import { EmptyState } from "@/components/common/empty-state"
import { StatusBadge } from "@/components/common/status-badge"

const FILTERS = [
  ["all", "全部"],
  ["enabled", "已启用"],
  ["active", "在线"],
  ["inactive", "离线"],
  ["degraded", "测活异常"],
  ["unknown", "待验证"],
  ["testing", "测试中"],
  ["disabled", "未启用"],
] as const

const HEALTH_ACCENT: Record<ProxyStatus, string> = {
  active: "bg-success",
  inactive: "bg-destructive",
  degraded: "bg-warning",
  testing: "bg-warning",
  unknown: "bg-muted-foreground/40",
}

export function ProxiesSection({
  proxies,
  onCreate,
  onEdit,
  onChanged,
}: {
  proxies: ProxyRecord[]
  onCreate: () => void
  onEdit: (proxy: ProxyRecord) => void
  onChanged: () => Promise<void>
}) {
  const [filter, setFilter] = useState("all")
  const [testingIds, setTestingIds] = useState<Set<number>>(() => new Set())
  const filtered = useMemo(() => {
    return [...proxies]
      .filter((proxy) => proxyMatchesFilter(proxy, filter))
      .sort(compareProxyHealth)
  }, [filter, proxies])

  async function testProxy(proxy: ProxyRecord) {
    if (testingIds.has(proxy.id)) return

    setTestingIds((current) => {
      const next = new Set(current)
      next.add(proxy.id)
      return next
    })

    try {
      const result = await api<TestResult>(`/api/proxies/${proxy.id}/test`, {
        method: "POST",
      })
      if (result.success) {
        toast.success(`${proxy.name} 测试通过，总耗时 ${result.responseTime} ms`)
      } else {
        toast.error(`${proxy.name} 测试失败: ${result.error ?? "未返回错误详情"}`)
      }
    } catch (error) {
      toast.error(describeError(error))
    } finally {
      setTestingIds((current) => {
        const next = new Set(current)
        next.delete(proxy.id)
        return next
      })
      try {
        await onChanged()
      } catch (error) {
        toast.error(`刷新代理状态失败: ${describeError(error)}`)
      }
    }
  }

  async function deleteProxy(proxy: ProxyRecord) {
    await api(`/api/proxies/${proxy.id}`, { method: "DELETE" })
    toast.success("代理已删除")
    await onChanged()
  }

  return (
    <Card>
      <CardHeader className="flex flex-row items-start justify-between gap-4">
        <div>
          <CardTitle>代理配置</CardTitle>
          <CardDescription>上游代理、状态和连通性测试</CardDescription>
        </div>
        <Button onClick={onCreate}>
          <Plus />
          新增代理
        </Button>
      </CardHeader>
      <CardContent className="flex flex-col gap-4">
        <Tabs value={filter} onValueChange={setFilter}>
          <TabsList className="flex flex-wrap">
            {FILTERS.map(([value, label]) => (
              <TabsTrigger key={value} value={value}>
                {label}
              </TabsTrigger>
            ))}
          </TabsList>
          <TabsContent value={filter} className="mt-4">
            <div className="grid gap-2">
              {filtered.map((proxy) => {
                const isTesting = testingIds.has(proxy.id)
                const disabled = proxy.enabled !== 1
                const health = proxyHealthView(proxy)
                return (
                  <Collapsible.Root
                    key={proxy.id}
                    className="group/proxy relative overflow-hidden rounded-md border bg-card/40 transition-[border-color,background-color,box-shadow] duration-200 hover:border-primary/40 hover:bg-card data-[state=open]:border-primary/40 data-[state=open]:bg-card data-[state=open]:shadow-sm"
                  >
                    <span
                      aria-hidden="true"
                      className={cn(
                        "absolute inset-y-0 left-0 w-1",
                        disabled ? "bg-muted-foreground/25" : HEALTH_ACCENT[health.key],
                        isTesting && "animate-pulse"
                      )}
                    />
                    <div className="grid items-center gap-x-6 gap-y-3 py-3 pr-3 pl-5 sm:grid-cols-[minmax(0,1fr)_auto] lg:grid-cols-[minmax(0,1fr)_6.5rem_8rem_auto]">
                      <div className={cn("flex min-w-0 flex-col gap-1", disabled && "opacity-60")}>
                        <div className="flex min-w-0 items-center gap-2">
                          <span className="truncate font-medium">{proxy.name}</span>
                          <StatusBadge proxy={proxy} />
                          {disabled && <Badge variant="secondary">未启用</Badge>}
                        </div>
                        <div className="flex min-w-0 flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted-foreground">
                          <span className="rounded-sm bg-muted px-1 font-mono text-[0.65rem] uppercase tracking-wider">
                            {proxy.type}
                          </span>
                          <span className="truncate font-mono">{proxy.host}:{proxy.port}</span>
                          <EntryStatus proxy={proxy} label={health.entry} />
                        </div>
                      </div>
                      <div className="flex items-center gap-1 max-sm:justify-end sm:col-start-2 sm:row-start-1 lg:col-start-4">
                        <Button
                          variant="outline"
                          size="sm"
                          disabled={isTesting}
                          aria-busy={isTesting}
                          onClick={() => testProxy(proxy)}
                        >
                          {isTesting ? <Loader2 className="animate-spin" /> : <Zap />}
                          {isTesting ? "测试中" : "测试"}
                        </Button>
                        <Button
                          variant="ghost"
                          size="icon-sm"
                          aria-label="编辑"
                          title="编辑"
                          className="text-muted-foreground hover:text-foreground"
                          onClick={() => onEdit(proxy)}
                        >
                          <Edit />
                        </Button>
                        <Button
                          variant="ghost"
                          size="icon-sm"
                          aria-label="删除"
                          title="删除"
                          className="text-muted-foreground hover:bg-destructive/10 hover:text-destructive"
                          onClick={() => deleteProxy(proxy)}
                        >
                          <Trash2 />
                        </Button>
                        <Collapsible.Trigger asChild>
                          <Button
                            variant="ghost"
                            size="icon-sm"
                            aria-label="测活详情"
                            title="测活详情"
                            className="text-muted-foreground hover:text-foreground data-[state=open]:bg-muted data-[state=open]:text-foreground"
                          >
                            <ChevronDown className="transition-transform duration-200 group-data-[state=open]/proxy:rotate-180 motion-reduce:transition-none" />
                          </Button>
                        </Collapsible.Trigger>
                      </div>
                      <div className="flex flex-wrap items-center gap-x-6 gap-y-2 sm:col-span-2 lg:col-span-2 lg:col-start-2 lg:row-start-1 lg:grid lg:grid-cols-subgrid">
                        <HandshakeMetric value={entryHandshakeMillis(proxy)} dimmed={disabled} />
                        <ProbeMetric
                          success={proxy.probe_health?.probe_success_count ?? 0}
                          failure={proxy.probe_health?.probe_failure_count ?? 0}
                          dimmed={disabled}
                        />
                      </div>
                    </div>
                    <Collapsible.Content className="overflow-hidden data-[state=closed]:animate-collapsible-up data-[state=open]:animate-collapsible-down motion-reduce:animate-none">
                      <div className="border-t border-dashed bg-muted/25 py-3 pr-4 pl-5">
                        <ProbeDetails details={probeHealthDetails(proxy)} />
                      </div>
                    </Collapsible.Content>
                  </Collapsible.Root>
                )
              })}
              {filtered.length === 0 && (
                <EmptyState icon={Server} text="暂无代理配置" />
              )}
            </div>
          </TabsContent>
        </Tabs>
      </CardContent>
    </Card>
  )
}

function MetricLabel({ children }: { children: ReactNode }) {
  return <span className="whitespace-nowrap text-[0.7rem] text-muted-foreground/80">{children}</span>
}

function HandshakeMetric({ value, dimmed }: { value: number | null; dimmed: boolean }) {
  return (
    <div className={cn("flex items-baseline gap-2 lg:flex-col lg:gap-1", dimmed && "opacity-60")}>
      <MetricLabel>入口握手</MetricLabel>
      <span className="font-mono text-base tabular-nums leading-none">
        {value != null ? (
          <>
            {value}
            <span className="ml-0.5 text-xs text-muted-foreground">ms</span>
          </>
        ) : (
          <span className="text-muted-foreground/60">—</span>
        )}
      </span>
    </div>
  )
}

function ProbeMetric({ success, failure, dimmed }: { success: number; failure: number; dimmed: boolean }) {
  const total = success + failure
  return (
    <div className={cn("flex items-baseline gap-2 lg:flex-col lg:gap-1", dimmed && "opacity-60")}>
      <MetricLabel>测活 成功 / 失败</MetricLabel>
      <span className="font-mono text-sm tabular-nums leading-none">
        <span className={success > 0 ? "text-success" : "text-muted-foreground/60"}>{success}</span>
        <span className="px-1 text-muted-foreground/50">/</span>
        <span className={failure > 0 ? "text-destructive" : "text-muted-foreground/60"}>{failure}</span>
      </span>
      <span aria-hidden="true" className="hidden h-1 w-full overflow-hidden rounded-full bg-muted lg:flex">
        {total > 0 && (
          <>
            <span className="bg-success" style={{ width: `${(success / total) * 100}%` }} />
            <span className="bg-destructive/70" style={{ width: `${(failure / total) * 100}%` }} />
          </>
        )}
      </span>
    </div>
  )
}

function EntryStatus({ proxy, label }: { proxy: ProxyRecord; label: string }) {
  const status = proxy.probe_health?.transport_status
  return (
    <span className="inline-flex items-center gap-1.5 whitespace-nowrap">
      <span
        aria-hidden="true"
        className={cn(
          "size-1.5 rounded-full",
          status === "reachable" ? "bg-success" : status === "unreachable" ? "bg-destructive" : "bg-muted-foreground/50"
        )}
      />
      {label}
    </span>
  )
}

function ProbeDetails({ details }: { details: string[] }) {
  return (
    <dl aria-label="测活详情" className="grid gap-x-8 gap-y-3 text-xs sm:grid-cols-2 xl:grid-cols-3">
      {details.map((detail, index) => {
        const separator = detail.indexOf("：")
        if (separator < 0) {
          return (
            <dd key={index} className="rounded-sm bg-muted/60 px-2 py-1 text-muted-foreground [overflow-wrap:anywhere] sm:col-span-full">
              {detail}
            </dd>
          )
        }
        return (
          <div key={index} className="flex min-w-0 flex-col gap-0.5">
            <dt className="text-[0.7rem] text-muted-foreground/80">{detail.slice(0, separator)}</dt>
            <dd className="text-foreground/85 [overflow-wrap:anywhere]">{detail.slice(separator + 1)}</dd>
          </div>
        )
      })}
    </dl>
  )
}

function compareProxyHealth(left: ProxyRecord, right: ProxyRecord) {
  const rankDiff = proxyHealthRank(left) - proxyHealthRank(right)
  if (rankDiff !== 0) return rankDiff

  const leftTime = left.response_time ?? Number.MAX_SAFE_INTEGER
  const rightTime = right.response_time ?? Number.MAX_SAFE_INTEGER
  if (leftTime !== rightTime) return leftTime - rightTime

  return left.name.localeCompare(right.name, "zh-CN")
}

function proxyHealthRank(proxy: ProxyRecord) {
  if (proxy.enabled !== 1) return 5
  const { key } = proxyHealthView(proxy)
  if (key === "active") return 0
  if (key === "testing") return 1
  if (key === "unknown") return 2
  if (key === "inactive") return 4
  return 3
}

function describeError(error: unknown) {
  if (error instanceof Error && error.message) return error.message
  if (typeof error === "string" && error.trim()) return error

  try {
    const serialized = JSON.stringify(error)
    if (serialized && serialized !== "undefined") return serialized
  } catch {
    // The raw value is not JSON serializable; expose that instead of hiding it.
  }

  return "未返回错误详情"
}
