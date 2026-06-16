import { useMemo, type ReactNode } from "react"
import { ListChecks, Trash2 } from "lucide-react"
import {
  Area,
  AreaChart,
  Bar,
  BarChart,
  CartesianGrid,
  XAxis,
  YAxis,
} from "recharts"
import { toast } from "sonner"

import { api } from "@/lib/api"
import { cn } from "@/lib/utils"
import {
  barChartConfig,
  latencyChartConfig,
  trafficChartConfig,
} from "@/lib/constants"
import {
  formatDate,
  formatDuration,
  formatHour,
  paginationPages,
  truncateLabel,
} from "@/lib/format"
import type {
  HourlyStat,
  Overview,
  ProxyUsageStat,
  TargetStat,
  TrafficLogPage,
} from "@/types"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card"
import {
  ChartContainer,
  ChartTooltip,
  ChartTooltipContent,
} from "@/components/ui/chart"
import {
  Pagination,
  PaginationContent,
  PaginationItem,
  PaginationLink,
  PaginationNext,
  PaginationPrevious,
} from "@/components/ui/pagination"
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table"
import { EmptyState } from "@/components/common/empty-state"
import { Input } from "@/components/ui/input"

const rankingChartMargin = { top: 8, right: 12, bottom: 32, left: 0 } as const

export function StatusSection({
  overview,
  hourlyStats,
  proxyUsage,
  targetStats,
  logs,
  searchValue,
  onSearchChange,
  onPageChange,
  onPageSizeChange,
  onChanged,
}: {
  overview: Overview
  hourlyStats: HourlyStat[]
  proxyUsage: ProxyUsageStat[]
  targetStats: TargetStat[]
  logs: TrafficLogPage
  searchValue: string
  onSearchChange: (value: string) => void
  onPageChange: (page: number) => void
  onPageSizeChange: (pageSize: number) => void
  onChanged: () => Promise<void>
}) {
  const hourlyData = useMemo(
    () =>
      hourlyStats
        .slice()
        .reverse()
        .map((item) => ({
          hour: formatHour(item.hour),
          total: Number(item.total_requests || 0),
          success: Number(item.success_requests || 0),
          failed: Number(item.failed_requests || 0),
          avg: Math.round(Number(item.avg_response_time || 0)),
        })),
    [hourlyStats]
  )
  const usageData = useMemo(
    () =>
      proxyUsage.slice(0, 8).map((item) => ({
        name: item.name,
        requests: Number(item.total_requests || 0),
      })),
    [proxyUsage]
  )
  const targetData = useMemo(
    () =>
      targetStats.slice(0, 8).map((item) => ({
        name: item.target_host,
        requests: Number(item.request_count || 0),
      })),
    [targetStats]
  )

  return (
    <div className="grid gap-6">
      <Card>
        <CardHeader>
          <CardTitle>运行状态</CardTitle>
          <CardDescription>24小时请求、失败和响应概览</CardDescription>
        </CardHeader>
        <CardContent className="grid gap-4 md:grid-cols-4">
          <MetricTile label="活跃代理" value={overview.activeProxies} />
          <MetricTile label="成功请求" value={overview.successRequests} />
          <MetricTile label="失败请求" value={overview.failedRequests} />
          <MetricTile label="运行时长" value={formatDuration(overview.uptime)} />
        </CardContent>
      </Card>

      <div className="grid gap-6 xl:grid-cols-2">
        <ChartCard title="请求趋势" description="最近24小时成功和失败请求">
          <ChartContainer config={trafficChartConfig} className="h-64 w-full">
            <AreaChart data={hourlyData}>
              <CartesianGrid vertical={false} />
              <XAxis dataKey="hour" tickLine={false} axisLine={false} />
              <YAxis tickLine={false} axisLine={false} width={36} />
              <ChartTooltip content={<ChartTooltipContent />} />
              <Area dataKey="success" type="monotone" fill="var(--color-success)" stroke="var(--color-success)" stackId="1" />
              <Area dataKey="failed" type="monotone" fill="var(--color-failed)" stroke="var(--color-failed)" stackId="1" />
            </AreaChart>
          </ChartContainer>
        </ChartCard>

        <ChartCard title="响应耗时" description="最近24小时平均响应时间">
          <ChartContainer config={latencyChartConfig} className="h-64 w-full">
            <AreaChart data={hourlyData}>
              <CartesianGrid vertical={false} />
              <XAxis dataKey="hour" tickLine={false} axisLine={false} />
              <YAxis tickLine={false} axisLine={false} width={42} />
              <ChartTooltip content={<ChartTooltipContent />} />
              <Area dataKey="avg" type="monotone" fill="var(--color-avg)" stroke="var(--color-avg)" />
            </AreaChart>
          </ChartContainer>
        </ChartCard>

        <ChartCard title="代理使用排行" description="最近24小时代理请求数">
          <ChartContainer config={barChartConfig} className="h-72 w-full">
            <BarChart data={usageData} margin={rankingChartMargin}>
              <CartesianGrid vertical={false} />
              <XAxis
                dataKey="name"
                tickLine={false}
                axisLine={false}
                interval={0}
                height={60}
                tickMargin={10}
                angle={-28}
                textAnchor="end"
                tick={{ fontSize: 11 }}
                tickFormatter={(value) => truncateLabel(String(value), 6)}
              />
              <YAxis tickLine={false} axisLine={false} width={36} />
              <ChartTooltip content={<ChartTooltipContent />} />
              <Bar dataKey="requests" fill="var(--color-requests)" radius={4} />
            </BarChart>
          </ChartContainer>
        </ChartCard>

        <ChartCard title="目标资源排行" description="最近24小时目标主机请求数">
          <ChartContainer config={barChartConfig} className="h-72 w-full">
            <BarChart data={targetData} margin={rankingChartMargin}>
              <CartesianGrid vertical={false} />
              <XAxis
                dataKey="name"
                tickLine={false}
                axisLine={false}
                interval={0}
                height={60}
                tickMargin={10}
                angle={-28}
                textAnchor="end"
                tick={{ fontSize: 11 }}
                tickFormatter={(value) => truncateLabel(String(value), 10)}
              />
              <YAxis tickLine={false} axisLine={false} width={36} />
              <ChartTooltip content={<ChartTooltipContent />} />
              <Bar dataKey="requests" fill="var(--color-requests)" radius={4} />
            </BarChart>
          </ChartContainer>
        </ChartCard>
      </div>

      <Card>
        <CardHeader className="flex flex-col gap-4 md:flex-row md:items-start md:justify-between">
          <div>
            <CardTitle>流量日志</CardTitle>
            <CardDescription>
              共{" "}
              <span className="font-mono tabular-nums">
                {logs.total.toLocaleString()}
              </span>{" "}
              条，当前第{" "}
              <span className="font-mono tabular-nums">{logs.page}</span> /{" "}
              <span className="font-mono tabular-nums">{logs.totalPages}</span> 页
            </CardDescription>
          </div>
          <div className="flex w-full flex-col gap-2 sm:flex-row md:w-auto md:items-center">
            <Input
              aria-label="搜索代理名称"
              className="sm:w-64"
              value={searchValue}
              onChange={(event) => onSearchChange(event.target.value)}
              placeholder="搜索代理名称"
            />
            <Button
              variant="destructive"
              onClick={async () => {
                await api("/api/traffic-logs", { method: "DELETE" })
                toast.success("流量日志已清除")
                await onChanged()
              }}
            >
              <Trash2 data-icon="inline-start" />
              清除日志
            </Button>
          </div>
        </CardHeader>
        <CardContent className="flex flex-col gap-4">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>时间</TableHead>
                <TableHead>目标</TableHead>
                <TableHead>代理</TableHead>
                <TableHead>状态</TableHead>
                <TableHead>类型</TableHead>
                <TableHead className="text-right">响应</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {logs.items.map((log) => (
                <TableRow key={log.id}>
                  <TableCell className="font-mono tabular-nums text-muted-foreground">
                    {formatDate(log.created_at)}
                  </TableCell>
                  <TableCell className="font-mono tabular-nums">
                    {formatTarget(log.target_host, log.target_port)}
                  </TableCell>
                  <TableCell>{log.proxy_name || "-"}</TableCell>
                  <TableCell>
                    {log.success ? (
                      <Badge
                        variant="outline"
                        className="border-success/30 bg-success/10 text-success"
                      >
                        成功
                      </Badge>
                    ) : (
                      <Badge variant="destructive">失败</Badge>
                    )}
                  </TableCell>
                  <TableCell>
                    <span className="text-muted-foreground">
                      {log.result_type || "-"}
                    </span>
                  </TableCell>
                  <TableCell className="text-right font-mono tabular-nums">
                    {log.response_time ?? "-"} ms
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
          {logs.items.length === 0 && (
            <EmptyState icon={ListChecks} text="暂无流量日志" />
          )}
          <div className="flex flex-col gap-3 border-t pt-4 md:flex-row md:items-center md:justify-between">
            <div className="flex items-center gap-2 text-sm text-muted-foreground">
              <span>每页</span>
              <Select
                value={String(logs.pageSize)}
                onValueChange={(value) => onPageSizeChange(Number(value))}
              >
                <SelectTrigger className="w-24 font-mono tabular-nums">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectGroup>
                    <SelectItem value="25" className="font-mono tabular-nums">
                      25
                    </SelectItem>
                    <SelectItem value="50" className="font-mono tabular-nums">
                      50
                    </SelectItem>
                    <SelectItem value="100" className="font-mono tabular-nums">
                      100
                    </SelectItem>
                  </SelectGroup>
                </SelectContent>
              </Select>
              <span>条</span>
            </div>
            <Pagination className="mx-0 w-auto justify-end">
              <PaginationContent>
                <PaginationItem>
                  <PaginationPrevious
                    href="#"
                    onClick={(event) => {
                      event.preventDefault()
                      if (logs.page > 1) onPageChange(logs.page - 1)
                    }}
                    aria-disabled={logs.page <= 1}
                    className={cn(logs.page <= 1 && "pointer-events-none opacity-50")}
                  />
                </PaginationItem>
                {paginationPages(logs.page, logs.totalPages).map((page) => (
                  <PaginationItem key={page}>
                    <PaginationLink
                      href="#"
                      isActive={page === logs.page}
                      className="font-mono tabular-nums"
                      onClick={(event) => {
                        event.preventDefault()
                        onPageChange(page)
                      }}
                    >
                      {page}
                    </PaginationLink>
                  </PaginationItem>
                ))}
                <PaginationItem>
                  <PaginationNext
                    href="#"
                    onClick={(event) => {
                      event.preventDefault()
                      if (logs.page < logs.totalPages) onPageChange(logs.page + 1)
                    }}
                    aria-disabled={logs.page >= logs.totalPages}
                    className={cn(logs.page >= logs.totalPages && "pointer-events-none opacity-50")}
                  />
                </PaginationItem>
              </PaginationContent>
            </Pagination>
          </div>
        </CardContent>
      </Card>
    </div>
  )
}

function ChartCard({
  title,
  description,
  children,
}: {
  title: string
  description: string
  children: ReactNode
}) {
  return (
    <Card>
      <CardHeader>
        <CardTitle>{title}</CardTitle>
        <CardDescription>{description}</CardDescription>
      </CardHeader>
      <CardContent>{children}</CardContent>
    </Card>
  )
}

function MetricTile({
  label,
  value,
}: {
  label: string
  value: string | number
}) {
  return (
    <div className="rounded-lg border bg-card p-4">
      <div className="text-[0.7rem] uppercase tracking-wider text-muted-foreground">
        {label}
      </div>
      <div className="mt-2 font-mono text-xl tabular-nums">{value}</div>
    </div>
  )
}

function formatTarget(host?: string | null, port?: number | null) {
  const trimmedHost = host?.trim()
  if (!trimmedHost) return "-"
  if (port === 80) return `http://${trimmedHost}`
  if (port === 443) return `https://${trimmedHost}`
  if (port && port > 0) return `${trimmedHost}:${port}`
  return trimmedHost
}
