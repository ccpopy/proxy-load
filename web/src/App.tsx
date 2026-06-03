import { useCallback, useEffect, useMemo, useState } from "react"
import {
  Activity,
  BadgeCheck,
  Cable,
  CheckCircle2,
  Circle,
  Database,
  Download,
  Edit,
  Gauge,
  Globe2,
  Layers3,
  ListChecks,
  Loader2,
  Moon,
  Network,
  Plus,
  RefreshCw,
  RotateCcw,
  Save,
  Server,
  Settings,
  Shield,
  SlidersHorizontal,
  Sun,
  Trash2,
  XCircle,
  Zap,
} from "lucide-react"
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

import {
  api,
  command,
  initServiceInfo,
  jsonBody,
  onServerEvent,
  type ServerEvent,
  type ServiceInfo,
} from "@/lib/api"
import { cn } from "@/lib/utils"
import type {
  AdvancedConfig,
  DnsMapping,
  HourlyStat,
  Overview,
  ProxyGroup,
  ProxyRecord,
  ProxyUsageStat,
  TargetStat,
  TrafficLogPage,
  UpdateInfo,
  VersionInfo,
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
  type ChartConfig,
} from "@/components/ui/chart"
import { Checkbox } from "@/components/ui/checkbox"
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import {
  Field,
  FieldContent,
  FieldDescription,
  FieldGroup,
  FieldLabel,
  FieldTitle,
} from "@/components/ui/field"
import { Input } from "@/components/ui/input"
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
import { ScrollArea } from "@/components/ui/scroll-area"
import { Separator } from "@/components/ui/separator"
import {
  Sidebar,
  SidebarContent,
  SidebarFooter,
  SidebarGroup,
  SidebarGroupContent,
  SidebarGroupLabel,
  SidebarHeader,
  SidebarInset,
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
  SidebarProvider,
  SidebarRail,
  SidebarSeparator,
  SidebarTrigger,
} from "@/components/ui/sidebar"
import { Switch } from "@/components/ui/switch"
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table"
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs"
import { Textarea } from "@/components/ui/textarea"
import { Toaster } from "@/components/ui/sonner"

type SectionKey = "proxies" | "dns" | "settings" | "groups" | "status"
type ThemeMode = "light" | "dark"

const navItems = [
  { key: "proxies", label: "代理配置", icon: Server },
  { key: "dns", label: "DNS映射", icon: Globe2 },
  { key: "settings", label: "负载设置", icon: SlidersHorizontal },
  { key: "groups", label: "代理分组", icon: Layers3 },
  { key: "status", label: "系统状态", icon: Activity },
] satisfies Array<{ key: SectionKey; label: string; icon: typeof Server }>

const emptyOverview: Overview = {
  activeProxies: 0,
  totalRequests: 0,
  successRequests: 0,
  failedRequests: 0,
  avgResponseTime: 0,
  uptime: 0,
}

const defaultAdvanced: AdvancedConfig = {
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

const trafficChartConfig = {
  total: { label: "总请求", color: "var(--chart-1)" },
  success: { label: "成功", color: "var(--chart-2)" },
  failed: { label: "失败", color: "var(--chart-3)" },
} satisfies ChartConfig

const latencyChartConfig = {
  avg: { label: "平均响应", color: "var(--chart-1)" },
} satisfies ChartConfig

const barChartConfig = {
  requests: { label: "请求数", color: "var(--chart-1)" },
} satisfies ChartConfig

const INITIAL_TRAFFIC_PAGE_SIZE = 25

export function App() {
  const [section, setSection] = useState<SectionKey>("proxies")
  const [loading, setLoading] = useState(true)
  const [apiReady, setApiReady] = useState(false)
  const [connectionState, setConnectionState] = useState<
    "online" | "offline" | "connecting"
  >("connecting")
  const [serviceInfo, setServiceInfo] = useState<ServiceInfo | null>(null)
  const [theme, setTheme] = useState<ThemeMode>(() => {
    const stored = localStorage.getItem("zwfw-theme")
    if (stored === "dark" || stored === "light") return stored
    return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light"
  })
  const [proxies, setProxies] = useState<ProxyRecord[]>([])
  const [dnsMappings, setDnsMappings] = useState<DnsMapping[]>([])
  const [groups, setGroups] = useState<ProxyGroup[]>([])
  const [settings, setSettings] = useState<Record<string, string>>({})
  const [advanced, setAdvanced] = useState<AdvancedConfig>(defaultAdvanced)
  const [overview, setOverview] = useState<Overview>(emptyOverview)
  const [hourlyStats, setHourlyStats] = useState<HourlyStat[]>([])
  const [proxyUsage, setProxyUsage] = useState<ProxyUsageStat[]>([])
  const [targetStats, setTargetStats] = useState<TargetStat[]>([])
  const [trafficLogs, setTrafficLogs] = useState<TrafficLogPage>({
    items: [],
    page: 1,
    pageSize: INITIAL_TRAFFIC_PAGE_SIZE,
    total: 0,
    totalPages: 1,
  })
  const [version, setVersion] = useState<VersionInfo | null>(null)
  const [updateInfo, setUpdateInfo] = useState<UpdateInfo | null>(null)
  const [updateOpen, setUpdateOpen] = useState(false)
  const [checkingUpdate, setCheckingUpdate] = useState(false)
  const [proxyDialog, setProxyDialog] = useState<ProxyRecord | "new" | null>(null)
  const [dnsDialog, setDnsDialog] = useState<DnsMapping | "new" | null>(null)
  const [groupDialog, setGroupDialog] = useState<ProxyGroup | "new" | null>(null)
  const [advancedOpen, setAdvancedOpen] = useState(false)

  useEffect(() => {
    document.documentElement.classList.toggle("dark", theme === "dark")
    localStorage.setItem("zwfw-theme", theme)
  }, [theme])

  const loadTrafficLogs = useCallback(async (page: number, pageSize: number) => {
    const nextLogs = await api<TrafficLogPage>(
      `/api/traffic-logs?page=${page}&page_size=${pageSize}`
    )
    setTrafficLogs(nextLogs)
  }, [])

  const refresh = useCallback(async () => {
    const [
      nextProxies,
      nextDnsMappings,
      nextGroups,
      nextSettings,
      nextAdvanced,
      nextOverview,
      nextHourly,
      nextProxyUsage,
      nextTargets,
      nextVersion,
    ] = await Promise.all([
      api<ProxyRecord[]>("/api/proxies"),
      api<DnsMapping[]>("/api/dns-mappings"),
      api<ProxyGroup[]>("/api/proxy-groups"),
      api<Record<string, string>>("/api/settings"),
      api<AdvancedConfig>("/api/advanced-config"),
      api<Overview>("/api/stats/overview"),
      api<HourlyStat[]>("/api/stats/hourly"),
      api<ProxyUsageStat[]>("/api/stats/proxy-usage"),
      api<TargetStat[]>("/api/stats/targets"),
      api<VersionInfo>("/api/version"),
    ])
    setProxies(nextProxies)
    setDnsMappings(nextDnsMappings)
    setGroups(nextGroups)
    setSettings(nextSettings)
    setAdvanced(nextAdvanced)
    setOverview(nextOverview)
    setHourlyStats(nextHourly)
    setProxyUsage(nextProxyUsage)
    setTargetStats(nextTargets)
    setVersion(nextVersion)
  }, [])

  useEffect(() => {
    let closed = false
    initServiceInfo()
      .then(async (info) => {
        if (closed) return
        setServiceInfo(info)
        await Promise.all([refresh(), loadTrafficLogs(1, INITIAL_TRAFFIC_PAGE_SIZE)])
      })
      .catch((error) => toast.error(error.message))
      .finally(() => {
        if (!closed) {
          setApiReady(true)
          setLoading(false)
        }
      })
    return () => {
      closed = true
    }
  }, [loadTrafficLogs, refresh])

  useEffect(() => {
    if (!apiReady) return undefined

    let closed = false
    let unlisten: (() => void) | undefined
    setConnectionState("connecting")
    onServerEvent((message: ServerEvent) => {
      if (
        [
          "proxy_created",
          "proxy_updated",
          "proxy_deleted",
          "proxy_tested",
          "dns_mapping_added",
          "dns_mapping_updated",
          "dns_mapping_deleted",
          "dns_mapping_toggled",
          "proxy_group_created",
          "proxy_group_updated",
          "proxy_group_deleted",
          "traffic_logs_cleared",
          "request_logged",
        ].includes(message.type)
      ) {
        Promise.all([
          refresh(),
          loadTrafficLogs(trafficLogs.page, trafficLogs.pageSize),
        ]).catch((error) => toast.error(error.message))
      }
    })
      .then((dispose) => {
        if (closed) {
          dispose()
          return
        }
        unlisten = dispose
        setConnectionState("online")
      })
      .catch((error) => {
        if (!closed) {
          setConnectionState("offline")
          toast.error(error.message)
        }
      })

    return () => {
      closed = true
      unlisten?.()
    }
  }, [apiReady, loadTrafficLogs, refresh, trafficLogs.page, trafficLogs.pageSize])

  const currentTitle = navItems.find((item) => item.key === section)?.label ?? "代理配置"
  const activeCount = proxies.filter((proxy) => proxy.enabled === 1 && proxy.status === "active").length
  const failedCount = proxies.filter((proxy) => proxy.enabled === 1 && proxy.status === "inactive").length

  async function handleRefresh() {
    setLoading(true)
    try {
      await Promise.all([refresh(), loadTrafficLogs(trafficLogs.page, trafficLogs.pageSize)])
      toast.success("数据已刷新")
    } catch (error) {
      toast.error(error instanceof Error ? error.message : "刷新失败")
    } finally {
      setLoading(false)
    }
  }

  async function handleCheckUpdate() {
    setCheckingUpdate(true)
    try {
      const info = await command<UpdateInfo>("check_for_updates")
      setUpdateInfo(info)
      setUpdateOpen(true)
      if (info.hasUpdate) {
        toast.success(`发现新版本 ${info.latest?.version}`)
      } else {
        toast.info("当前已是最新版本")
      }
    } catch (error) {
      toast.error(error instanceof Error ? error.message : "检查更新失败")
    } finally {
      setCheckingUpdate(false)
    }
  }

  return (
    <SidebarProvider className="h-svh overflow-hidden">
      <Sidebar collapsible="icon">
        <SidebarHeader>
          <SidebarMenu>
            <SidebarMenuItem>
              <SidebarMenuButton size="lg" tooltip="代理管理系统">
                <div className="flex size-8 items-center justify-center rounded-md bg-sidebar-primary text-sidebar-primary-foreground">
                  <Network />
                </div>
                <span className="flex min-w-0 flex-col">
                  <span className="truncate font-semibold">代理管理系统</span>
                  <span className="truncate text-xs text-muted-foreground">
                    Rust / Tauri / shadcn
                  </span>
                </span>
              </SidebarMenuButton>
            </SidebarMenuItem>
          </SidebarMenu>
        </SidebarHeader>
        <SidebarContent>
          <SidebarGroup>
            <SidebarGroupLabel>导航</SidebarGroupLabel>
            <SidebarGroupContent>
              <SidebarMenu>
                {navItems.map((item) => {
                  const Icon = item.icon
                  return (
                    <SidebarMenuItem key={item.key}>
                      <SidebarMenuButton
                        type="button"
                        tooltip={item.label}
                        isActive={section === item.key}
                        onClick={() => setSection(item.key)}
                      >
                        <Icon />
                        <span>{item.label}</span>
                      </SidebarMenuButton>
                    </SidebarMenuItem>
                  )
                })}
              </SidebarMenu>
            </SidebarGroupContent>
          </SidebarGroup>
        </SidebarContent>
        <SidebarFooter>
          <SidebarSeparator />
          <SidebarMenu>
            <SidebarMenuItem>
              <SidebarMenuButton
                type="button"
                tooltip="高级配置"
                onClick={() => setAdvancedOpen(true)}
              >
                <Settings />
                <span>高级配置</span>
              </SidebarMenuButton>
            </SidebarMenuItem>
            <SidebarMenuItem>
              <SidebarMenuButton
                type="button"
                tooltip={theme === "dark" ? "切换浅色" : "切换深色"}
                onClick={() => setTheme(theme === "dark" ? "light" : "dark")}
              >
                {theme === "dark" ? <Sun /> : <Moon />}
                <span>{theme === "dark" ? "浅色模式" : "深色模式"}</span>
              </SidebarMenuButton>
            </SidebarMenuItem>
          </SidebarMenu>
        </SidebarFooter>
        <SidebarRail />
      </Sidebar>

      <SidebarInset className="min-h-0">
        <header className="flex h-16 shrink-0 items-center justify-between gap-3 border-b bg-card px-4">
          <div className="flex min-w-0 items-center gap-3">
            <SidebarTrigger />
            <Separator orientation="vertical" className="h-5" />
            <div className="min-w-0">
              <h1 className="truncate text-lg font-semibold">{currentTitle}</h1>
              <div className="flex flex-wrap items-center gap-2 text-xs text-muted-foreground">
                <ConnectionBadge state={connectionState} />
                <span className="truncate">Tauri 本地应用通信</span>
              </div>
            </div>
          </div>
          <div className="flex shrink-0 items-center gap-2">
            <Badge variant="secondary">
              代理端口 {serviceInfo?.proxy_port ?? advanced.proxy_port}
            </Badge>
            <Badge variant="outline">v{version?.version ?? "..."}</Badge>
            <Button
              variant="outline"
              onClick={handleCheckUpdate}
              disabled={checkingUpdate}
            >
              {checkingUpdate ? (
                <Loader2 data-icon="inline-start" className="animate-spin" />
              ) : (
                <Download data-icon="inline-start" />
              )}
              检查更新
            </Button>
            <Button variant="outline" onClick={handleRefresh} disabled={loading}>
              {loading ? (
                <Loader2 data-icon="inline-start" className="animate-spin" />
              ) : (
                <RefreshCw data-icon="inline-start" />
              )}
              刷新
            </Button>
          </div>
        </header>

        <ScrollArea className="min-h-0 flex-1">
          <div className="p-6">
            <div className="mx-auto flex max-w-7xl flex-col gap-6">
              <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-4">
                <MetricCard title="在线代理" value={activeCount} icon={CheckCircle2} />
                <MetricCard title="离线代理" value={failedCount} icon={XCircle} />
                <MetricCard title="24小时请求" value={overview.totalRequests} icon={Cable} />
                <MetricCard title="平均响应" value={`${overview.avgResponseTime} ms`} icon={Gauge} />
              </div>

              {section === "proxies" && (
                <ProxySection
                  proxies={proxies}
                  onCreate={() => setProxyDialog("new")}
                  onEdit={setProxyDialog}
                  onChanged={refresh}
                />
              )}
              {section === "dns" && (
                <DnsSection
                  mappings={dnsMappings}
                  onCreate={() => setDnsDialog("new")}
                  onEdit={setDnsDialog}
                  onChanged={refresh}
                />
              )}
              {section === "settings" && (
                <LoadSettingsSection
                  settings={settings}
                  proxies={proxies}
                  onChanged={refresh}
                />
              )}
              {section === "groups" && (
                <GroupSection
                  groups={groups}
                  onCreate={() => setGroupDialog("new")}
                  onEdit={setGroupDialog}
                  onChanged={refresh}
                />
              )}
              {section === "status" && (
                <StatusSection
                  overview={overview}
                  hourlyStats={hourlyStats}
                  proxyUsage={proxyUsage}
                  targetStats={targetStats}
                  logs={trafficLogs}
                  onPageChange={(page) => loadTrafficLogs(page, trafficLogs.pageSize)}
                  onPageSizeChange={(pageSize) => loadTrafficLogs(1, pageSize)}
                  onChanged={async () => {
                    await Promise.all([refresh(), loadTrafficLogs(1, trafficLogs.pageSize)])
                  }}
                />
              )}
            </div>
          </div>
        </ScrollArea>
      </SidebarInset>

      <ProxyDialog
        value={proxyDialog}
        onOpenChange={(open) => !open && setProxyDialog(null)}
        onSaved={async () => {
          setProxyDialog(null)
          await refresh()
        }}
      />
      <DnsDialog
        value={dnsDialog}
        onOpenChange={(open) => !open && setDnsDialog(null)}
        onSaved={async () => {
          setDnsDialog(null)
          await refresh()
        }}
      />
      <GroupDialog
        value={groupDialog}
        proxies={proxies}
        onOpenChange={(open) => !open && setGroupDialog(null)}
        onSaved={async () => {
          setGroupDialog(null)
          await refresh()
        }}
      />
      <UpdateDialog
        open={updateOpen}
        info={updateInfo}
        onOpenChange={setUpdateOpen}
      />
      <AdvancedDialog
        open={advancedOpen}
        config={advanced}
        onOpenChange={setAdvancedOpen}
        onConfigChange={setAdvanced}
        onChanged={refresh}
      />
      <Toaster />
    </SidebarProvider>
  )
}

function UpdateDialog({
  open,
  info,
  onOpenChange,
}: {
  open: boolean
  info: UpdateInfo | null
  onOpenChange: (open: boolean) => void
}) {
  const [installing, setInstalling] = useState(false)

  async function handleInstall() {
    if (!info?.latest) return

    setInstalling(true)
    try {
      const result = await command<{ message?: string }>("install_update", {
        artifactPath: info.latest.path,
      })
      toast.success(result.message ?? "已启动更新安装程序")
      onOpenChange(false)
    } catch (error) {
      toast.error(error instanceof Error ? error.message : "安装更新失败")
    } finally {
      setInstalling(false)
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>检查更新</DialogTitle>
          <DialogDescription>
            开发环境扫描项目根目录 release，生产环境扫描应用所在目录 release。
          </DialogDescription>
        </DialogHeader>

        {info ? (
          <FieldGroup>
            <Field>
              <FieldLabel>当前版本</FieldLabel>
              <FieldDescription>{info.currentVersion}</FieldDescription>
            </Field>
            <Field>
              <FieldLabel>安装目录</FieldLabel>
              <FieldDescription className="break-all">{info.appDir}</FieldDescription>
            </Field>
            <Field>
              <FieldLabel>更新包目录</FieldLabel>
              <FieldDescription className="break-all">{info.releaseDir}</FieldDescription>
            </Field>
            <Field>
              <FieldLabel>更新状态</FieldLabel>
              <FieldDescription>
                {info.hasUpdate && info.latest
                  ? `发现新版本 ${info.latest.version}`
                  : "未发现高于当前版本的更新包"}
              </FieldDescription>
            </Field>
            {info.latest && (
              <Field>
                <FieldLabel>候选更新包</FieldLabel>
                <FieldDescription className="break-all">
                  {info.latest.fileName}
                </FieldDescription>
              </Field>
            )}
          </FieldGroup>
        ) : (
          <p className="text-sm text-muted-foreground">尚未执行更新检查。</p>
        )}

        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>
            关闭
          </Button>
          <Button onClick={handleInstall} disabled={!info?.latest || installing}>
            {installing ? (
              <Loader2 data-icon="inline-start" className="animate-spin" />
            ) : (
              <Download data-icon="inline-start" />
            )}
            安装更新
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

function ConnectionBadge({ state }: { state: "online" | "offline" | "connecting" }) {
  const label = state === "online" ? "系统运行中" : state === "offline" ? "连接断开" : "连接中"
  const tone = state === "online" ? "text-primary" : state === "offline" ? "text-destructive" : "text-muted-foreground"
  return (
    <span className={cn("inline-flex items-center gap-1", tone)}>
      <Circle className="size-2 fill-current" />
      {label}
    </span>
  )
}

function MetricCard({
  title,
  value,
  icon: Icon,
}: {
  title: string
  value: string | number
  icon: typeof Activity
}) {
  return (
    <Card>
      <CardHeader className="flex flex-row items-center justify-between gap-3 pb-2">
        <CardDescription>{title}</CardDescription>
        <Icon className="text-muted-foreground" />
      </CardHeader>
      <CardContent>
        <div className="text-2xl font-semibold">{value}</div>
      </CardContent>
    </Card>
  )
}

function ProxySection({
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
  const filtered = useMemo(() => {
    return proxies.filter((proxy) => {
      if (filter === "all") return true
      if (filter === "enabled") return proxy.enabled === 1
      if (filter === "disabled") return proxy.enabled !== 1
      return proxy.status === filter
    })
  }, [filter, proxies])

  async function testProxy(proxy: ProxyRecord) {
    try {
      await api(`/api/proxies/${proxy.id}/test`, { method: "POST" })
      toast.success(`${proxy.name} 测试完成`)
      await onChanged()
    } catch (error) {
      toast.error(error instanceof Error ? error.message : "测试失败")
    }
  }

  return (
    <Card>
      <CardHeader className="flex flex-row items-start justify-between gap-4">
        <div>
          <CardTitle>代理配置</CardTitle>
          <CardDescription>上游代理、状态、优先级和连通性测试</CardDescription>
        </div>
        <Button onClick={onCreate}>
          <Plus data-icon="inline-start" />
          新增代理
        </Button>
      </CardHeader>
      <CardContent className="flex flex-col gap-4">
        <Tabs value={filter} onValueChange={setFilter}>
          <TabsList className="flex flex-wrap">
            {[
              ["all", "全部"],
              ["enabled", "已启用"],
              ["active", "在线"],
              ["inactive", "离线"],
              ["testing", "测试中"],
              ["disabled", "未启用"],
            ].map(([value, label]) => (
              <TabsTrigger key={value} value={value}>
                {label}
              </TabsTrigger>
            ))}
          </TabsList>
          <TabsContent value={filter} className="mt-4">
            <div className="grid gap-3">
              {filtered.map((proxy) => (
                <div
                  key={proxy.id}
                  className="grid gap-3 rounded-md border bg-card p-4 lg:grid-cols-[1fr_auto]"
                >
                  <div className="min-w-0">
                    <div className="flex flex-wrap items-center gap-2">
                      <div className="truncate font-medium">{proxy.name}</div>
                      <StatusBadge proxy={proxy} />
                      <Badge variant="outline">{proxy.type.toUpperCase()}</Badge>
                      {proxy.enabled !== 1 && <Badge variant="secondary">未启用</Badge>}
                    </div>
                    <div className="mt-2 grid gap-2 text-sm text-muted-foreground md:grid-cols-4">
                      <span className="truncate">{proxy.host}:{proxy.port}</span>
                      <span>响应 {proxy.response_time ?? "-"} ms</span>
                      <span>成功 {proxy.success_count}</span>
                      <span>失败 {proxy.fail_count}</span>
                    </div>
                  </div>
                  <div className="flex items-center gap-2">
                    <Button variant="outline" size="sm" onClick={() => testProxy(proxy)}>
                      <Zap data-icon="inline-start" />
                      测试
                    </Button>
                    <Button variant="outline" size="sm" onClick={() => onEdit(proxy)}>
                      <Edit data-icon="inline-start" />
                      编辑
                    </Button>
                    <Button
                      variant="destructive"
                      size="sm"
                      onClick={async () => {
                        await api(`/api/proxies/${proxy.id}`, { method: "DELETE" })
                        toast.success("代理已删除")
                        await onChanged()
                      }}
                    >
                      <Trash2 data-icon="inline-start" />
                      删除
                    </Button>
                  </div>
                </div>
              ))}
              {filtered.length === 0 && <EmptyState icon={Server} text="暂无代理配置" />}
            </div>
          </TabsContent>
        </Tabs>
      </CardContent>
    </Card>
  )
}

function StatusBadge({ proxy }: { proxy: ProxyRecord }) {
  if (proxy.status === "active") return <Badge>在线</Badge>
  if (proxy.status === "inactive") return <Badge variant="destructive">离线</Badge>
  if (proxy.status === "testing") return <Badge variant="secondary">测试中</Badge>
  return <Badge variant="outline">未知</Badge>
}

function DnsSection({
  mappings,
  onCreate,
  onEdit,
  onChanged,
}: {
  mappings: DnsMapping[]
  onCreate: () => void
  onEdit: (mapping: DnsMapping) => void
  onChanged: () => Promise<void>
}) {
  return (
    <Card>
      <CardHeader className="flex flex-row items-start justify-between gap-4">
        <div>
          <CardTitle>DNS映射</CardTitle>
          <CardDescription>域名到固定 IP 的映射规则</CardDescription>
        </div>
        <Button onClick={onCreate}>
          <Plus data-icon="inline-start" />
          新增映射
        </Button>
      </CardHeader>
      <CardContent>
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>域名</TableHead>
              <TableHead>IP地址</TableHead>
              <TableHead>说明</TableHead>
              <TableHead>状态</TableHead>
              <TableHead className="text-right">操作</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {mappings.map((mapping) => (
              <TableRow key={mapping.id}>
                <TableCell className="font-medium">{mapping.domain}</TableCell>
                <TableCell>{mapping.ip}</TableCell>
                <TableCell>{mapping.description || "-"}</TableCell>
                <TableCell>
                  <Badge variant={mapping.enabled === 1 ? "default" : "secondary"}>
                    {mapping.enabled === 1 ? "已启用" : "已禁用"}
                  </Badge>
                </TableCell>
                <TableCell>
                  <div className="flex justify-end gap-2">
                    <Button variant="outline" size="sm" onClick={() => onEdit(mapping)}>
                      <Edit data-icon="inline-start" />
                      编辑
                    </Button>
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={async () => {
                        await api(`/api/dns-mappings/${mapping.id}/toggle`, { method: "PUT" })
                        await onChanged()
                      }}
                    >
                      <BadgeCheck data-icon="inline-start" />
                      切换
                    </Button>
                    <Button
                      variant="destructive"
                      size="sm"
                      onClick={async () => {
                        await api(`/api/dns-mappings/${mapping.id}`, { method: "DELETE" })
                        toast.success("DNS映射已删除")
                        await onChanged()
                      }}
                    >
                      <Trash2 data-icon="inline-start" />
                      删除
                    </Button>
                  </div>
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
        {mappings.length === 0 && <EmptyState icon={Globe2} text="暂无DNS映射" />}
      </CardContent>
    </Card>
  )
}

function LoadSettingsSection({
  settings,
  proxies,
  onChanged,
}: {
  settings: Record<string, string>
  proxies: ProxyRecord[]
  onChanged: () => Promise<void>
}) {
  const [form, setForm] = useState({
    load_mode: settings.load_mode || "auto",
    algorithm: settings.algorithm || "adaptive",
    test_url: settings.test_url || "https://cms.zjzwfw.gov.cn/favicon.ico",
    timeout: settings.timeout || "10",
  })

  useEffect(() => {
    setForm({
      load_mode: settings.load_mode || "auto",
      algorithm: settings.algorithm || "adaptive",
      test_url: settings.test_url || "https://cms.zjzwfw.gov.cn/favicon.ico",
      timeout: settings.timeout || "10",
    })
  }, [settings])

  async function save() {
    await api("/api/settings", jsonBody(form))
    toast.success("负载设置已保存")
    await onChanged()
  }

  return (
    <div className="grid gap-6 xl:grid-cols-[1fr_420px]">
      <Card>
        <CardHeader>
          <CardTitle>负载均衡</CardTitle>
          <CardDescription>代理选择策略和全局测试参数</CardDescription>
        </CardHeader>
        <CardContent>
          <FieldGroup>
            <Field>
              <FieldLabel>负载模式</FieldLabel>
              <Select value={form.load_mode} onValueChange={(value) => setForm({ ...form, load_mode: value })}>
                <SelectTrigger className="w-full">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectGroup>
                    <SelectItem value="auto">自动模式</SelectItem>
                    <SelectItem value="manual">手动模式</SelectItem>
                  </SelectGroup>
                </SelectContent>
              </Select>
            </Field>
            <Field>
              <FieldLabel>自动模式算法</FieldLabel>
              <Select value={form.algorithm} onValueChange={(value) => setForm({ ...form, algorithm: value })}>
                <SelectTrigger className="w-full">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectGroup>
                    <SelectItem value="adaptive">自适应算法</SelectItem>
                    <SelectItem value="weighted_round_robin">加权轮询</SelectItem>
                    <SelectItem value="least_connections">最小连接数</SelectItem>
                    <SelectItem value="sticky_host">会话粘滞</SelectItem>
                  </SelectGroup>
                </SelectContent>
              </Select>
            </Field>
            <Field>
              <FieldLabel>默认测试地址</FieldLabel>
              <Input value={form.test_url} onChange={(event) => setForm({ ...form, test_url: event.target.value })} />
            </Field>
            <Field>
              <FieldLabel>默认超时（秒）</FieldLabel>
              <Input type="number" min={1} value={form.timeout} onChange={(event) => setForm({ ...form, timeout: event.target.value })} />
            </Field>
            <Button onClick={save}>
              <Save data-icon="inline-start" />
              保存设置
            </Button>
          </FieldGroup>
        </CardContent>
      </Card>
      <Card>
        <CardHeader>
          <CardTitle>优先级</CardTitle>
          <CardDescription>手动模式按优先级从小到大选择</CardDescription>
        </CardHeader>
        <CardContent className="flex flex-col gap-3">
          {proxies.map((proxy) => (
            <div key={proxy.id} className="grid grid-cols-[1fr_96px] items-center gap-3">
              <div className="min-w-0">
                <div className="truncate text-sm font-medium">{proxy.name}</div>
                <div className="truncate text-xs text-muted-foreground">{proxy.host}:{proxy.port}</div>
              </div>
              <Input
                type="number"
                value={proxy.priority}
                onChange={async (event) => {
                  await api(`/api/proxies/${proxy.id}/priority`, {
                    method: "PUT",
                    body: JSON.stringify({ priority: Number(event.target.value) }),
                  })
                  await onChanged()
                }}
              />
            </div>
          ))}
        </CardContent>
      </Card>
    </div>
  )
}

function GroupSection({
  groups,
  onCreate,
  onEdit,
  onChanged,
}: {
  groups: ProxyGroup[]
  onCreate: () => void
  onEdit: (group: ProxyGroup) => void
  onChanged: () => Promise<void>
}) {
  return (
    <Card>
      <CardHeader className="flex flex-row items-start justify-between gap-4">
        <div>
          <CardTitle>代理分组</CardTitle>
          <CardDescription>按域名把请求约束到指定代理集合</CardDescription>
        </div>
        <Button onClick={onCreate}>
          <Plus data-icon="inline-start" />
          新增分组
        </Button>
      </CardHeader>
      <CardContent className="grid gap-3">
        {groups.map((group) => (
          <div key={group.id} className="rounded-md border p-4">
            <div className="flex flex-wrap items-start justify-between gap-3">
              <div className="min-w-0">
                <div className="flex flex-wrap items-center gap-2">
                  <div className="font-medium">{group.name}</div>
                  {group.is_default === 1 && <Badge>默认</Badge>}
                  {group.enabled !== 1 && <Badge variant="secondary">已禁用</Badge>}
                </div>
                <div className="mt-3 flex flex-wrap gap-2">
                  {group.domains.map((domain) => (
                    <Badge key={domain.id} variant="outline">
                      {domain.domain}
                    </Badge>
                  ))}
                  {group.domains.length === 0 && <Badge variant="secondary">无域名</Badge>}
                </div>
                <div className="mt-2 flex flex-wrap gap-2">
                  {group.members.map((member) => (
                    <Badge key={member.proxy_id} variant="secondary">
                      {member.name}
                    </Badge>
                  ))}
                  {group.members.length === 0 && <Badge variant="secondary">无代理</Badge>}
                </div>
              </div>
              <div className="flex gap-2">
                <Button variant="outline" size="sm" onClick={() => onEdit(group)}>
                  <Edit data-icon="inline-start" />
                  编辑
                </Button>
                <Button
                  variant="destructive"
                  size="sm"
                  onClick={async () => {
                    await api(`/api/proxy-groups/${group.id}`, { method: "DELETE" })
                    await onChanged()
                  }}
                >
                  <Trash2 data-icon="inline-start" />
                  删除
                </Button>
              </div>
            </div>
          </div>
        ))}
        {groups.length === 0 && <EmptyState icon={Layers3} text="暂无代理分组" />}
      </CardContent>
    </Card>
  )
}

function StatusSection({
  overview,
  hourlyStats,
  proxyUsage,
  targetStats,
  logs,
  onPageChange,
  onPageSizeChange,
  onChanged,
}: {
  overview: Overview
  hourlyStats: HourlyStat[]
  proxyUsage: ProxyUsageStat[]
  targetStats: TargetStat[]
  logs: TrafficLogPage
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
          <ChartContainer config={barChartConfig} className="h-64 w-full">
            <BarChart data={usageData}>
              <CartesianGrid vertical={false} />
              <XAxis dataKey="name" tickLine={false} axisLine={false} interval={0} tickFormatter={(value) => truncateLabel(value)} />
              <YAxis tickLine={false} axisLine={false} width={36} />
              <ChartTooltip content={<ChartTooltipContent />} />
              <Bar dataKey="requests" fill="var(--color-requests)" radius={4} />
            </BarChart>
          </ChartContainer>
        </ChartCard>

        <ChartCard title="目标资源排行" description="最近24小时目标主机请求数">
          <ChartContainer config={barChartConfig} className="h-64 w-full">
            <BarChart data={targetData}>
              <CartesianGrid vertical={false} />
              <XAxis dataKey="name" tickLine={false} axisLine={false} interval={0} tickFormatter={(value) => truncateLabel(value)} />
              <YAxis tickLine={false} axisLine={false} width={36} />
              <ChartTooltip content={<ChartTooltipContent />} />
              <Bar dataKey="requests" fill="var(--color-requests)" radius={4} />
            </BarChart>
          </ChartContainer>
        </ChartCard>
      </div>

      <Card>
        <CardHeader className="flex flex-row items-start justify-between gap-4">
          <div>
            <CardTitle>流量日志</CardTitle>
            <CardDescription>
              共 {logs.total.toLocaleString()} 条，当前第 {logs.page} / {logs.totalPages} 页
            </CardDescription>
          </div>
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
                  <TableCell>{formatDate(log.created_at)}</TableCell>
                  <TableCell>{log.target_host}:{log.target_port}</TableCell>
                  <TableCell>{log.proxy_name || "-"}</TableCell>
                  <TableCell>
                    <Badge variant={log.success ? "default" : "destructive"}>
                      {log.success ? "成功" : "失败"}
                    </Badge>
                  </TableCell>
                  <TableCell>{log.result_type || "-"}</TableCell>
                  <TableCell className="text-right">{log.response_time ?? "-"} ms</TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
          {logs.items.length === 0 && <EmptyState icon={ListChecks} text="暂无流量日志" />}
          <div className="flex flex-col gap-3 border-t pt-4 md:flex-row md:items-center md:justify-between">
            <div className="flex items-center gap-2 text-sm text-muted-foreground">
              <span>每页</span>
              <Select value={String(logs.pageSize)} onValueChange={(value) => onPageSizeChange(Number(value))}>
                <SelectTrigger className="w-24">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectGroup>
                    <SelectItem value="25">25</SelectItem>
                    <SelectItem value="50">50</SelectItem>
                    <SelectItem value="100">100</SelectItem>
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
  children: React.ReactNode
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

function AdvancedDialog({
  open,
  config,
  onOpenChange,
  onConfigChange,
  onChanged,
}: {
  open: boolean
  config: AdvancedConfig
  onOpenChange: (open: boolean) => void
  onConfigChange: (config: AdvancedConfig) => void
  onChanged: () => Promise<void>
}) {
  function update<K extends keyof AdvancedConfig>(key: K, value: AdvancedConfig[K]) {
    onConfigChange({ ...config, [key]: value })
  }

  async function save() {
    await api("/api/advanced-config", jsonBody(config))
    toast.success("高级配置已保存")
    await onChanged()
  }

  async function reset() {
    await api("/api/advanced-config/reset", { method: "POST" })
    toast.success("已恢复默认配置")
    await onChanged()
  }

  async function exportConfig() {
    const payload = await api<unknown>("/api/advanced-config/export")
    const blob = new Blob([JSON.stringify(payload, null, 2)], { type: "application/json" })
    const url = URL.createObjectURL(blob)
    const link = document.createElement("a")
    link.href = url
    link.download = `proxy-config-${new Date().toISOString().slice(0, 10)}.json`
    link.click()
    URL.revokeObjectURL(url)
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[calc(100vh-4rem)] max-w-5xl overflow-hidden">
        <DialogHeader>
          <DialogTitle>高级配置</DialogTitle>
          <DialogDescription>连接池、熔断器、健康检查和快速失败参数</DialogDescription>
        </DialogHeader>
        <ScrollArea className="h-[calc(100vh-14rem)] max-h-[620px] pr-4">
          <div className="grid gap-6 pb-1 lg:grid-cols-2">
            <ConfigGroup title="基础配置" icon={Settings}>
              <NumberField label="代理服务端口" value={config.proxy_port} onChange={(value) => update("proxy_port", value)} />
              <NumberField label="定期测试间隔（分钟）" value={Math.round(config.periodic_test_interval / 60000)} onChange={(value) => update("periodic_test_interval", value * 60000)} />
              <NumberField label="请求日志保留天数" value={config.log_retention_days} onChange={(value) => update("log_retention_days", value)} />
              <NumberField label="统计保留天数" value={config.stats_retention_days} onChange={(value) => update("stats_retention_days", value)} />
            </ConfigGroup>
            <ConfigGroup title="连接池" icon={Database}>
              <NumberField label="最大连接数" value={config.pool_max_size} onChange={(value) => update("pool_max_size", value)} />
              <NumberField label="空闲超时（秒）" value={Math.round(config.pool_idle_timeout / 1000)} onChange={(value) => update("pool_idle_timeout", value * 1000)} />
              <NumberField label="等待超时（秒）" value={Math.round(config.pool_wait_timeout / 1000)} onChange={(value) => update("pool_wait_timeout", value * 1000)} />
            </ConfigGroup>
            <ConfigGroup title="熔断器" icon={Shield}>
              <NumberField label="失败阈值" value={config.circuit_failure_threshold} onChange={(value) => update("circuit_failure_threshold", value)} />
              <NumberField label="熔断时长（秒）" value={Math.round(config.circuit_timeout / 1000)} onChange={(value) => update("circuit_timeout", value * 1000)} />
              <NumberField label="半开尝试次数" value={config.circuit_half_open_attempts} onChange={(value) => update("circuit_half_open_attempts", value)} />
            </ConfigGroup>
            <ConfigGroup title="快速失败" icon={Zap}>
              <Field orientation="horizontal">
                <FieldContent>
                  <FieldTitle>启用快速失败</FieldTitle>
                  <FieldDescription>失败后按策略尝试下一个代理</FieldDescription>
                </FieldContent>
                <Switch checked={config.failfast_enabled} onCheckedChange={(value) => update("failfast_enabled", value)} />
              </Field>
              <NumberField label="最大尝试次数" value={config.failfast_max_attempts} onChange={(value) => update("failfast_max_attempts", value)} />
              <NumberField label="单次超时（秒）" value={Math.round(config.failfast_attempt_timeout / 1000)} onChange={(value) => update("failfast_attempt_timeout", value * 1000)} />
              <NumberField label="总超时（秒）" value={Math.round(config.failfast_total_timeout / 1000)} onChange={(value) => update("failfast_total_timeout", value * 1000)} />
            </ConfigGroup>
          </div>
        </ScrollArea>
        <DialogFooter>
          <Button variant="outline" onClick={exportConfig}>
            <Download data-icon="inline-start" />
            导出
          </Button>
          <Button variant="outline" onClick={reset}>
            <RotateCcw data-icon="inline-start" />
            恢复默认
          </Button>
          <Button onClick={save}>
            <Save data-icon="inline-start" />
            保存
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

function ConfigGroup({
  title,
  icon: Icon,
  children,
}: {
  title: string
  icon: typeof Settings
  children: React.ReactNode
}) {
  return (
    <div className="rounded-md border p-4">
      <div className="mb-4 flex items-center gap-2 font-medium">
        <Icon className="text-muted-foreground" />
        {title}
      </div>
      <FieldGroup>{children}</FieldGroup>
    </div>
  )
}

function NumberField({
  label,
  value,
  onChange,
}: {
  label: string
  value: number
  onChange: (value: number) => void
}) {
  return (
    <Field>
      <FieldLabel>{label}</FieldLabel>
      <Input type="number" value={value} onChange={(event) => onChange(Number(event.target.value))} />
    </Field>
  )
}

function MetricTile({ label, value }: { label: string; value: string | number }) {
  return (
    <div className="rounded-md border p-4">
      <div className="text-sm text-muted-foreground">{label}</div>
      <div className="mt-2 text-xl font-semibold">{value}</div>
    </div>
  )
}

function ProxyDialog({
  value,
  onOpenChange,
  onSaved,
}: {
  value: ProxyRecord | "new" | null
  onOpenChange: (open: boolean) => void
  onSaved: () => Promise<void>
}) {
  const proxy = value && value !== "new" ? value : null
  const [form, setForm] = useState({
    name: "",
    type: "http",
    host: "",
    port: 1080,
    username: "",
    password: "",
    enabled: true,
    test_url: "",
    test_timeout: "",
    skip_cert_verify: false,
  })

  useEffect(() => {
    setForm({
      name: proxy?.name ?? "",
      type: proxy?.type ?? "http",
      host: proxy?.host ?? "",
      port: proxy?.port ?? 1080,
      username: proxy?.username ?? "",
      password: proxy?.password ?? "",
      enabled: proxy ? proxy.enabled === 1 : true,
      test_url: proxy?.test_url ?? "",
      test_timeout: proxy?.test_timeout ? String(proxy.test_timeout) : "",
      skip_cert_verify: proxy ? proxy.skip_cert_verify === 1 : false,
    })
  }, [proxy, value])

  async function save() {
    const body = {
      ...form,
      port: Number(form.port),
      enabled: form.enabled ? 1 : 0,
      test_timeout: form.test_timeout ? Number(form.test_timeout) : null,
      skip_cert_verify: form.skip_cert_verify ? 1 : 0,
    }
    await api(proxy ? `/api/proxies/${proxy.id}` : "/api/proxies", {
      method: proxy ? "PUT" : "POST",
      body: JSON.stringify(body),
    })
    toast.success(proxy ? "代理已更新" : "代理已创建")
    await onSaved()
  }

  return (
    <Dialog open={value !== null} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[calc(100vh-4rem)] overflow-hidden">
        <DialogHeader>
          <DialogTitle>{proxy ? "编辑代理" : "新增代理"}</DialogTitle>
          <DialogDescription>配置上游代理连接参数</DialogDescription>
        </DialogHeader>
        <ScrollArea className="h-[calc(100vh-14rem)] max-h-[560px] pr-4">
          <FieldGroup className="pb-1">
            <Field>
              <FieldLabel>名称</FieldLabel>
              <Input value={form.name} onChange={(event) => setForm({ ...form, name: event.target.value })} />
            </Field>
            <Field orientation="horizontal">
              <FieldContent>
                <FieldTitle>启用</FieldTitle>
                <FieldDescription>启用后参与负载均衡</FieldDescription>
              </FieldContent>
              <Switch checked={form.enabled} onCheckedChange={(enabled) => setForm({ ...form, enabled })} />
            </Field>
            <Field>
              <FieldLabel>类型</FieldLabel>
              <Select value={form.type} onValueChange={(type) => setForm({ ...form, type })}>
                <SelectTrigger className="w-full">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectGroup>
                    <SelectItem value="http">HTTP</SelectItem>
                    <SelectItem value="https">HTTPS</SelectItem>
                    <SelectItem value="socks4">SOCKS4</SelectItem>
                    <SelectItem value="socks5">SOCKS5</SelectItem>
                  </SelectGroup>
                </SelectContent>
              </Select>
            </Field>
            <div className="grid gap-4 sm:grid-cols-2">
              <Field>
                <FieldLabel>主机</FieldLabel>
                <Input value={form.host} onChange={(event) => setForm({ ...form, host: event.target.value })} />
              </Field>
              <Field>
                <FieldLabel>端口</FieldLabel>
                <Input type="number" value={form.port} onChange={(event) => setForm({ ...form, port: Number(event.target.value) })} />
              </Field>
            </div>
            <div className="grid gap-4 sm:grid-cols-2">
              <Field>
                <FieldLabel>用户名</FieldLabel>
                <Input value={form.username} onChange={(event) => setForm({ ...form, username: event.target.value })} />
              </Field>
              <Field>
                <FieldLabel>密码</FieldLabel>
                <Input type="password" value={form.password} onChange={(event) => setForm({ ...form, password: event.target.value })} />
              </Field>
            </div>
            <Field>
              <FieldLabel>测试地址</FieldLabel>
              <Input value={form.test_url} onChange={(event) => setForm({ ...form, test_url: event.target.value })} />
            </Field>
            <Field>
              <FieldLabel>超时时间（秒）</FieldLabel>
              <Input value={form.test_timeout} onChange={(event) => setForm({ ...form, test_timeout: event.target.value })} />
            </Field>
            <Field orientation="horizontal">
              <FieldContent>
                <FieldTitle>跳过证书验证</FieldTitle>
                <FieldDescription>仅影响连通性测试</FieldDescription>
              </FieldContent>
              <Switch checked={form.skip_cert_verify} onCheckedChange={(skip_cert_verify) => setForm({ ...form, skip_cert_verify })} />
            </Field>
          </FieldGroup>
        </ScrollArea>
        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>取消</Button>
          <Button onClick={save}>
            <Save data-icon="inline-start" />
            保存
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

function DnsDialog({
  value,
  onOpenChange,
  onSaved,
}: {
  value: DnsMapping | "new" | null
  onOpenChange: (open: boolean) => void
  onSaved: () => Promise<void>
}) {
  const mapping = value && value !== "new" ? value : null
  const [form, setForm] = useState({ domain: "", ip: "", description: "", enabled: true })

  useEffect(() => {
    setForm({
      domain: mapping?.domain ?? "",
      ip: mapping?.ip ?? "",
      description: mapping?.description ?? "",
      enabled: mapping ? mapping.enabled === 1 : true,
    })
  }, [mapping, value])

  async function save() {
    await api(mapping ? `/api/dns-mappings/${mapping.id}` : "/api/dns-mappings", {
      method: mapping ? "PUT" : "POST",
      body: JSON.stringify({ ...form, enabled: form.enabled ? 1 : 0 }),
    })
    toast.success(mapping ? "DNS映射已更新" : "DNS映射已创建")
    await onSaved()
  }

  return (
    <Dialog open={value !== null} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{mapping ? "编辑DNS映射" : "新增DNS映射"}</DialogTitle>
          <DialogDescription>配置域名解析覆盖规则</DialogDescription>
        </DialogHeader>
        <FieldGroup>
          <Field>
            <FieldLabel>域名</FieldLabel>
            <Input value={form.domain} onChange={(event) => setForm({ ...form, domain: event.target.value })} />
          </Field>
          <Field>
            <FieldLabel>IP地址</FieldLabel>
            <Input value={form.ip} onChange={(event) => setForm({ ...form, ip: event.target.value })} />
          </Field>
          <Field>
            <FieldLabel>说明</FieldLabel>
            <Input value={form.description} onChange={(event) => setForm({ ...form, description: event.target.value })} />
          </Field>
          <Field orientation="horizontal">
            <FieldTitle>启用</FieldTitle>
            <Switch checked={form.enabled} onCheckedChange={(enabled) => setForm({ ...form, enabled })} />
          </Field>
        </FieldGroup>
        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>取消</Button>
          <Button onClick={save}>
            <Save data-icon="inline-start" />
            保存
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

function GroupDialog({
  value,
  proxies,
  onOpenChange,
  onSaved,
}: {
  value: ProxyGroup | "new" | null
  proxies: ProxyRecord[]
  onOpenChange: (open: boolean) => void
  onSaved: () => Promise<void>
}) {
  const group = value && value !== "new" ? value : null
  const [form, setForm] = useState({
    name: "",
    domains: "",
    proxy_ids: [] as number[],
    is_default: false,
    enabled: true,
  })

  useEffect(() => {
    setForm({
      name: group?.name ?? "",
      domains: group?.domains.map((item) => item.domain).join("\n") ?? "",
      proxy_ids: group?.members.map((item) => item.proxy_id) ?? [],
      is_default: group ? group.is_default === 1 : false,
      enabled: group ? group.enabled === 1 : true,
    })
  }, [group, value])

  function toggleProxy(id: number, checked: boolean) {
    setForm((current) => ({
      ...current,
      proxy_ids: checked
        ? [...current.proxy_ids, id]
        : current.proxy_ids.filter((proxyId) => proxyId !== id),
    }))
  }

  async function save() {
    const domains = form.domains
      .split(/[\n,]/)
      .map((item) => item.trim())
      .filter(Boolean)
    await api(group ? `/api/proxy-groups/${group.id}` : "/api/proxy-groups", {
      method: group ? "PUT" : "POST",
      body: JSON.stringify({
        name: form.name,
        domains,
        proxy_ids: form.proxy_ids,
        is_default: form.is_default ? 1 : 0,
        enabled: form.enabled ? 1 : 0,
      }),
    })
    toast.success(group ? "代理分组已更新" : "代理分组已创建")
    await onSaved()
  }

  return (
    <Dialog open={value !== null} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[calc(100vh-4rem)] overflow-hidden">
        <DialogHeader>
          <DialogTitle>{group ? "编辑代理分组" : "新增代理分组"}</DialogTitle>
          <DialogDescription>设置域名规则和代理成员</DialogDescription>
        </DialogHeader>
        <ScrollArea className="h-[calc(100vh-14rem)] max-h-[560px] pr-4">
          <FieldGroup className="pb-1">
            <Field>
              <FieldLabel>分组名称</FieldLabel>
              <Input value={form.name} onChange={(event) => setForm({ ...form, name: event.target.value })} />
            </Field>
            <Field>
              <FieldLabel>域名列表</FieldLabel>
              <Textarea value={form.domains} rows={4} onChange={(event) => setForm({ ...form, domains: event.target.value })} />
            </Field>
            <Field orientation="horizontal">
              <FieldTitle>默认分组</FieldTitle>
              <Switch checked={form.is_default} onCheckedChange={(is_default) => setForm({ ...form, is_default })} />
            </Field>
            <Field orientation="horizontal">
              <FieldTitle>启用</FieldTitle>
              <Switch checked={form.enabled} onCheckedChange={(enabled) => setForm({ ...form, enabled })} />
            </Field>
            <Separator />
            <div className="grid gap-2">
              {proxies.map((proxy) => (
                <label key={proxy.id} className="flex items-center gap-3 rounded-md border p-3">
                  <Checkbox
                    checked={form.proxy_ids.includes(proxy.id)}
                    onCheckedChange={(checked) => toggleProxy(proxy.id, checked === true)}
                  />
                  <span className="min-w-0 flex-1">
                    <span className="block truncate text-sm font-medium">{proxy.name}</span>
                    <span className="block truncate text-xs text-muted-foreground">
                      {proxy.type}://{proxy.host}:{proxy.port}
                    </span>
                  </span>
                </label>
              ))}
            </div>
          </FieldGroup>
        </ScrollArea>
        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>取消</Button>
          <Button onClick={save}>
            <Save data-icon="inline-start" />
            保存
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

function EmptyState({ icon: Icon, text }: { icon: typeof Server; text: string }) {
  return (
    <div className="flex min-h-40 flex-col items-center justify-center gap-2 rounded-md border border-dashed text-muted-foreground">
      <Icon />
      <div className="text-sm">{text}</div>
    </div>
  )
}

function formatDate(input?: string | null) {
  if (!input) return "-"
  const date = new Date(input)
  if (Number.isNaN(date.getTime())) return input
  return new Intl.DateTimeFormat("zh-CN", {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  }).format(date)
}

function formatHour(input: string) {
  const date = new Date(input.replace(" ", "T") + "Z")
  if (Number.isNaN(date.getTime())) return input.slice(-5)
  return new Intl.DateTimeFormat("zh-CN", {
    hour: "2-digit",
    minute: "2-digit",
  }).format(date)
}

function formatDuration(seconds: number) {
  const hours = Math.floor(seconds / 3600)
  const minutes = Math.floor((seconds % 3600) / 60)
  if (hours > 0) return `${hours}小时${minutes}分`
  return `${minutes}分`
}

function truncateLabel(value: string) {
  return value.length > 8 ? `${value.slice(0, 8)}…` : value
}

function paginationPages(page: number, totalPages: number) {
  const start = Math.max(1, Math.min(page - 1, totalPages - 2))
  return Array.from({ length: Math.min(3, totalPages) }, (_, index) => start + index)
}
