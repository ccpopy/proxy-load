import { useCallback, useEffect, useState } from "react"
import { toast } from "sonner"

import {
  api,
  initServiceInfo,
  onServerEvent,
  type ServerEvent,
  type ServiceInfo,
} from "@/lib/api"
import {
  INITIAL_TRAFFIC_PAGE_SIZE,
  defaultAdvanced,
  emptyOverview,
  navItems,
  type SectionKey,
  type ThemeMode,
} from "@/lib/constants"
import type {
  AdvancedConfig,
  DnsMapping,
  HourlyStat,
  Overview,
  ProxyGroup,
  ProxyRecord,
  ProxyServiceStatus,
  ProxyUsageStat,
  TargetStat,
  TrafficLogPage,
  VersionInfo,
} from "@/types"
import { ScrollArea } from "@/components/ui/scroll-area"
import { SidebarInset, SidebarProvider } from "@/components/ui/sidebar"
import { Toaster } from "@/components/ui/sonner"
import { AppHeader } from "@/components/layout/app-header"
import { AppSidebar } from "@/components/layout/app-sidebar"
import { MetricBar } from "@/components/layout/metric-bar"
import { ProxiesSection } from "@/components/sections/proxies-section"
import { DnsSection } from "@/components/sections/dns-section"
import { LoadSettingsSection } from "@/components/sections/load-settings-section"
import { GroupSection } from "@/components/sections/groups-section"
import { StatusSection } from "@/components/sections/status-section"
import { TransferSection } from "@/components/sections/transfer-section"
import { ProxyDialog } from "@/components/dialogs/proxy-dialog"
import { DnsDialog } from "@/components/dialogs/dns-dialog"
import { GroupDialog } from "@/components/dialogs/group-dialog"
import { AdvancedDialog } from "@/components/dialogs/advanced-dialog"
import { AboutDialog } from "@/components/dialogs/about-dialog"

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
    return "dark"
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
  const [trafficLogProxySearch, setTrafficLogProxySearch] = useState("")
  const [version, setVersion] = useState<VersionInfo | null>(null)
  const [proxyDialog, setProxyDialog] = useState<ProxyRecord | "new" | null>(null)
  const [dnsDialog, setDnsDialog] = useState<DnsMapping | "new" | null>(null)
  const [groupDialog, setGroupDialog] = useState<ProxyGroup | "new" | null>(null)
  const [advancedOpen, setAdvancedOpen] = useState(false)
  const [aboutOpen, setAboutOpen] = useState(false)

  useEffect(() => {
    document.documentElement.classList.toggle("dark", theme === "dark")
    localStorage.setItem("zwfw-theme", theme)
  }, [theme])

  const loadTrafficLogs = useCallback(
    async (page: number, pageSize: number, proxySearch = "") => {
      const params = new URLSearchParams({
        page: String(page),
        page_size: String(pageSize),
      })
      const trimmedSearch = proxySearch.trim()
      if (trimmedSearch) params.set("proxy", trimmedSearch)
      const nextLogs = await api<TrafficLogPage>(
        `/api/traffic-logs?${params.toString()}`
      )
      setTrafficLogs(nextLogs)
    },
    []
  )

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
      nextProxyServiceStatus,
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
      api<ProxyServiceStatus>("/api/proxy-service-status"),
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
    setConnectionState(proxyConnectionState(nextProxyServiceStatus))
  }, [])

  useEffect(() => {
    let closed = false
    initServiceInfo()
      .then(async (info) => {
        if (closed) return
        setServiceInfo(info)
        await Promise.all([refresh(), loadTrafficLogs(1, INITIAL_TRAFFIC_PAGE_SIZE)])
      })
      .catch((error) => {
        if (!closed) {
          setConnectionState("offline")
          toast.error(describeError(error))
        }
      })
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
    onServerEvent((message: ServerEvent) => {
      if (
        [
          "proxy_created",
          "proxy_updated",
          "proxy_deleted",
          "proxy_testing",
          "proxy_tested",
          "dns_mapping_added",
          "dns_mapping_updated",
          "dns_mapping_deleted",
          "dns_mapping_toggled",
          "proxy_group_created",
          "proxy_group_updated",
          "proxy_group_deleted",
          "config_imported",
          "request_logged",
          "proxy_service_status_changed",
        ].includes(message.type)
      ) {
        Promise.all([
          refresh(),
          loadTrafficLogs(
            trafficLogs.page,
            trafficLogs.pageSize,
            trafficLogProxySearch
          ),
        ]).catch((error) => toast.error(describeError(error)))
      }
    })
      .then((dispose) => {
        if (closed) {
          dispose()
          return
        }
        unlisten = dispose
      })
      .catch((error) => {
        if (!closed) {
          toast.error(`应用事件监听失败: ${describeError(error)}`)
        }
      })

    return () => {
      closed = true
      unlisten?.()
    }
  }, [
    apiReady,
    loadTrafficLogs,
    refresh,
    trafficLogProxySearch,
    trafficLogs.page,
    trafficLogs.pageSize,
  ])

  const currentTitle =
    navItems.find((item) => item.key === section)?.label ?? "代理配置"
  const activeCount = proxies.filter(
    (proxy) => proxy.enabled === 1 && proxy.status === "active"
  ).length
  const failedCount = proxies.filter(
    (proxy) => proxy.enabled === 1 && proxy.status === "inactive"
  ).length

  async function handleRefresh() {
    setLoading(true)
    try {
      await Promise.all([
        refresh(),
        loadTrafficLogs(
          trafficLogs.page,
          trafficLogs.pageSize,
          trafficLogProxySearch
        ),
      ])
      toast.success("数据已刷新")
    } catch (error) {
      toast.error(describeError(error))
    } finally {
      setLoading(false)
    }
  }

  return (
    <SidebarProvider className="h-svh overflow-hidden">
      <AppSidebar
        section={section}
        onSectionChange={setSection}
        theme={theme}
        onToggleTheme={() => setTheme(theme === "dark" ? "light" : "dark")}
        onOpenAdvanced={() => setAdvancedOpen(true)}
        onOpenAbout={() => setAboutOpen(true)}
      />

      <SidebarInset className="min-h-0">
        <AppHeader
          title={currentTitle}
          connectionState={connectionState}
          loading={loading}
          onRefresh={handleRefresh}
        />

        <ScrollArea className="console-surface min-h-0 flex-1">
          <div className="p-6">
            <div className="mx-auto flex max-w-7xl flex-col gap-6">
              {section === "proxies" && (
                <>
                  <MetricBar
                    activeCount={activeCount}
                    failedCount={failedCount}
                    totalRequests={overview.totalRequests}
                    avgResponseMs={overview.avgResponseTime}
                  />
                  <ProxiesSection
                    proxies={proxies}
                    onCreate={() => setProxyDialog("new")}
                    onEdit={setProxyDialog}
                    onChanged={refresh}
                  />
                </>
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
              {section === "transfer" && (
                <TransferSection
                  proxies={proxies}
                  dnsMappings={dnsMappings}
                  groups={groups}
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
                  searchValue={trafficLogProxySearch}
                  onSearchChange={(value) => {
                    setTrafficLogProxySearch(value)
                    loadTrafficLogs(1, trafficLogs.pageSize, value).catch((error) =>
                      toast.error(describeError(error))
                    )
                  }}
                  onPageChange={(page) =>
                    loadTrafficLogs(
                      page,
                      trafficLogs.pageSize,
                      trafficLogProxySearch
                    )
                  }
                  onPageSizeChange={(pageSize) =>
                    loadTrafficLogs(1, pageSize, trafficLogProxySearch)
                  }
                  onChanged={async () => {
                    await loadTrafficLogs(
                      1,
                      trafficLogs.pageSize,
                      trafficLogProxySearch
                    )
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
      <AdvancedDialog
        open={advancedOpen}
        config={advanced}
        onOpenChange={setAdvancedOpen}
        onConfigChange={setAdvanced}
        onChanged={refresh}
      />
      <AboutDialog
        open={aboutOpen}
        onOpenChange={setAboutOpen}
        version={version}
        serviceInfo={serviceInfo}
      />
      <Toaster />
    </SidebarProvider>
  )
}

function proxyConnectionState(
  status: ProxyServiceStatus
): "online" | "offline" | "connecting" {
  if (status.state === "starting") return "connecting"
  return status.running ? "online" : "offline"
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
