import { useMemo, useState, type ReactNode } from "react"
import { Edit, Loader2, Plus, Server, Trash2, Zap } from "lucide-react"
import { toast } from "sonner"

import { api } from "@/lib/api"
import { cn } from "@/lib/utils"
import type { ProxyRecord, TestResult } from "@/types"
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
  ["testing", "测试中"],
  ["disabled", "未启用"],
] as const

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
      .filter((proxy) => {
        if (filter === "all") return true
        if (filter === "enabled") return proxy.enabled === 1
        if (filter === "disabled") return proxy.enabled !== 1
        return proxy.status === filter
      })
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
        toast.success(`${proxy.name} 测试通过，响应 ${result.responseTime} ms`)
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
            <div className="grid gap-2.5">
              {filtered.map((proxy) => {
                const isTesting = testingIds.has(proxy.id)
                return (
                  <div
                    key={proxy.id}
                    className="group relative grid gap-3 rounded-md border bg-card/40 p-4 transition-colors hover:border-primary/40 hover:bg-card lg:grid-cols-[1fr_auto] lg:items-center"
                  >
                  <div className="min-w-0">
                    <div className="flex flex-wrap items-center gap-2">
                      <span className="truncate font-medium">{proxy.name}</span>
                      <StatusBadge proxy={proxy} />
                      <span className="rounded-sm border border-border bg-muted/50 px-1.5 py-0.5 font-mono text-[0.7rem] uppercase tracking-wider text-muted-foreground">
                        {proxy.type}
                      </span>
                      {proxy.enabled !== 1 && (
                        <Badge variant="secondary">未启用</Badge>
                      )}
                    </div>
                    <div className="mt-2.5 grid gap-x-6 gap-y-1.5 md:grid-cols-2 xl:grid-cols-[minmax(16rem,1.8fr)_minmax(6rem,0.7fr)_minmax(6rem,0.7fr)_minmax(6rem,0.7fr)]">
                      <Stat label="地址" value={`${proxy.host}:${proxy.port}`} />
                      <Stat
                        label="响应"
                        value={
                          proxy.response_time != null
                            ? `${proxy.response_time} ms`
                            : "—"
                        }
                      />
                      <Stat label="测活成功" value={proxy.success_count} tone="success" />
                      <Stat label="测活失败" value={proxy.fail_count} tone="danger" />
                    </div>
                  </div>
                  <div className="flex items-center gap-2">
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
                      variant="outline"
                      size="sm"
                      onClick={() => onEdit(proxy)}
                    >
                      <Edit />
                      编辑
                    </Button>
                    <Button
                      variant="ghost"
                      size="sm"
                      className="text-muted-foreground hover:bg-destructive/10 hover:text-destructive"
                      onClick={() => deleteProxy(proxy)}
                    >
                      <Trash2 />
                      删除
                    </Button>
                  </div>
                </div>
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

function Stat({
  label,
  value,
  tone,
}: {
  label: string
  value: ReactNode
  tone?: "success" | "danger"
}) {
  return (
    <span className="grid min-w-0 grid-cols-[2.75rem_minmax(0,1fr)] items-baseline gap-1.5 text-sm">
      <span className="whitespace-nowrap text-[0.7rem] uppercase tracking-wider text-muted-foreground/70">
        {label}
      </span>
      <span
        className={cn(
          "truncate font-mono tabular-nums text-foreground/90",
          tone === "success" && "text-success",
          tone === "danger" && "text-destructive"
        )}
      >
        {value}
      </span>
    </span>
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
  if (proxy.status === "active") return 0
  if (proxy.status === "testing") return 1
  if (proxy.status === "unknown" || !proxy.status) return 2
  if (proxy.status === "inactive") return 4
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
