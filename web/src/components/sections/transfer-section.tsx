import { useState, type ReactNode } from "react"
import {
  ArrowLeftRight,
  Download,
  FileUp,
  Globe2,
  Layers3,
  Loader2,
  Server,
  type LucideIcon,
} from "lucide-react"
import { toast } from "sonner"

import { command, commandErrorMessage } from "@/lib/api"
import type {
  DnsMapping,
  ExportResult,
  ImportResult,
  ImportSummary,
  ProxyGroup,
  ProxyRecord,
} from "@/types"
import {
  Accordion,
  AccordionContent,
  AccordionItem,
  AccordionTrigger,
} from "@/components/ui/accordion"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card"
import { Checkbox } from "@/components/ui/checkbox"
import { ScrollArea } from "@/components/ui/scroll-area"

export function TransferSection({
  proxies,
  dnsMappings,
  groups,
  onChanged,
}: {
  proxies: ProxyRecord[]
  dnsMappings: DnsMapping[]
  groups: ProxyGroup[]
  onChanged: () => Promise<void>
}) {
  const [selectedProxies, setSelectedProxies] = useState<ReadonlySet<number>>(new Set())
  const [selectedDns, setSelectedDns] = useState<ReadonlySet<number>>(new Set())
  const [selectedGroups, setSelectedGroups] = useState<ReadonlySet<number>>(new Set())
  const [exporting, setExporting] = useState(false)
  const [importing, setImporting] = useState(false)
  const [lastImport, setLastImport] = useState<ImportSummary | null>(null)

  const totalSelected = selectedProxies.size + selectedDns.size + selectedGroups.size

  async function handleExport() {
    setExporting(true)
    try {
      const result = await command<ExportResult>("export_selected_config", {
        selection: {
          proxyIds: [...selectedProxies],
          dnsIds: [...selectedDns],
          groupIds: [...selectedGroups],
        },
      })
      if (result.canceled) return
      const counts = result.counts
      const detail = counts
        ? `（代理 ${counts.proxies} · DNS ${counts.dnsMappings} · 分组 ${counts.proxyGroups}）`
        : ""
      toast.success(`配置已导出${detail}`, { description: result.path })
    } catch (error) {
      toast.error(commandErrorMessage(error, "导出配置失败"))
    } finally {
      setExporting(false)
    }
  }

  async function handleImport() {
    setImporting(true)
    try {
      const result = await command<ImportResult>("import_config_file")
      if (result.canceled || !result.summary) return
      setLastImport(result.summary)
      const { proxies: p, dnsMappings: d, proxyGroups: g } = result.summary
      toast.success(
        `导入完成：新增代理 ${p.added}、DNS ${d.added}、分组 ${g.added}，跳过已存在 ${
          p.skipped + d.skipped + g.skipped
        } 项`
      )
      await onChanged()
    } catch (error) {
      toast.error(commandErrorMessage(error, "导入配置失败"))
    } finally {
      setImporting(false)
    }
  }

  return (
    <>
      <Card>
        <CardHeader className="flex flex-row items-start justify-between gap-4">
          <div>
            <CardTitle>导出配置</CardTitle>
            <CardDescription>
              勾选要分享的代理、DNS 映射和代理分组，导出为 JSON 文件供其他设备导入
            </CardDescription>
          </div>
          <Button onClick={handleExport} disabled={totalSelected === 0 || exporting}>
            {exporting ? <Loader2 className="animate-spin" /> : <Download />}
            导出所选{totalSelected > 0 ? `（${totalSelected}）` : ""}
          </Button>
        </CardHeader>
        <CardContent className="grid gap-4">
          <Accordion
            type="multiple"
            defaultValue={["proxies", "dns", "groups"]}
            className="overflow-hidden rounded-md border"
          >
            <SelectGroup
              value="proxies"
              icon={Server}
              title="代理列表"
              emptyText="暂无代理"
              selected={selectedProxies}
              onSelectedChange={setSelectedProxies}
              rows={proxies.map((proxy) => ({
                id: proxy.id,
                primary: proxy.name,
                secondary: `${proxy.type}://${proxy.host}:${proxy.port}`,
              }))}
            />
            <SelectGroup
              value="dns"
              icon={Globe2}
              title="DNS映射"
              emptyText="暂无DNS映射"
              selected={selectedDns}
              onSelectedChange={setSelectedDns}
              rows={dnsMappings.map((mapping) => ({
                id: mapping.id,
                primary: mapping.domain,
                secondary: mapping.ip,
              }))}
            />
            <SelectGroup
              value="groups"
              icon={Layers3}
              title="代理分组"
              emptyText="暂无代理分组"
              selected={selectedGroups}
              onSelectedChange={setSelectedGroups}
              rows={groups.map((group) => ({
                id: group.id,
                primary: group.name,
                secondary: `${group.domains.length} 个域名 · ${group.members.length} 个代理`,
              }))}
            />
          </Accordion>
          <p className="text-xs text-muted-foreground">
            分组成员按「类型 + 主机 + 端口」关联代理：导出分组时建议同时勾选其成员代理，
            导入端缺失的成员会被自动跳过。
          </p>
        </CardContent>
      </Card>

      <Card>
        <CardHeader className="flex flex-row items-start justify-between gap-4">
          <div>
            <CardTitle>导入配置</CardTitle>
            <CardDescription>
              选择本系统导出的 JSON 配置文件，合并到当前配置；已存在的同地址代理、同域名映射和同名分组会自动跳过
            </CardDescription>
          </div>
          <Button variant="outline" onClick={handleImport} disabled={importing}>
            {importing ? <Loader2 className="animate-spin" /> : <FileUp />}
            选择文件导入
          </Button>
        </CardHeader>
        {lastImport && (
          <CardContent>
            <div className="rounded-md border bg-card/40 p-4">
              <div className="mb-3 flex items-center gap-2 text-[0.7rem] uppercase tracking-wider text-muted-foreground">
                <ArrowLeftRight className="size-3.5" />
                最近一次导入结果
              </div>
              <div className="flex flex-wrap gap-2">
                <SummaryBadge label="代理" added={lastImport.proxies.added} skipped={lastImport.proxies.skipped} />
                <SummaryBadge label="DNS映射" added={lastImport.dnsMappings.added} skipped={lastImport.dnsMappings.skipped} />
                <SummaryBadge label="代理分组" added={lastImport.proxyGroups.added} skipped={lastImport.proxyGroups.skipped} />
                {lastImport.unresolvedMembers > 0 && (
                  <Badge variant="outline" className="rounded-sm text-muted-foreground">
                    {lastImport.unresolvedMembers} 个分组成员未匹配到本地代理
                  </Badge>
                )}
              </div>
            </div>
          </CardContent>
        )}
      </Card>
    </>
  )
}

function SummaryBadge({
  label,
  added,
  skipped,
}: {
  label: string
  added: number
  skipped: number
}) {
  return (
    <Badge variant="secondary" className="rounded-sm font-normal">
      {label}
      <span className="font-mono tabular-nums">
        新增 {added} / 跳过 {skipped}
      </span>
    </Badge>
  )
}

function SelectGroup({
  value,
  icon: Icon,
  title,
  rows,
  selected,
  onSelectedChange,
  emptyText,
}: {
  value: string
  icon: LucideIcon
  title: string
  rows: Array<{ id: number; primary: ReactNode; secondary: ReactNode }>
  selected: ReadonlySet<number>
  onSelectedChange: (selected: ReadonlySet<number>) => void
  emptyText: string
}) {
  const allChecked = rows.length > 0 && selected.size === rows.length
  const headerState = allChecked ? true : selected.size > 0 ? "indeterminate" : false

  function toggleAll() {
    onSelectedChange(allChecked ? new Set() : new Set(rows.map((row) => row.id)))
  }

  function toggleOne(id: number) {
    const next = new Set(selected)
    if (next.has(id)) {
      next.delete(id)
    } else {
      next.add(id)
    }
    onSelectedChange(next)
  }

  return (
    <AccordionItem value={value}>
      {/* 全选框是手风琴触发器的兄弟节点：按钮不能嵌套按钮，且勾选不应折叠面板 */}
      <div className="flex items-center gap-2.5 px-3 transition-colors hover:bg-muted/30">
        <Checkbox
          checked={headerState}
          onCheckedChange={toggleAll}
          disabled={rows.length === 0}
          aria-label={`全选${title}`}
        />
        <AccordionTrigger className="py-3 hover:no-underline">
          <span className="flex items-center gap-1.5 font-medium tracking-tight">
            <Icon className="size-3.5 text-muted-foreground" />
            {title}
          </span>
          <span
            className={`ml-auto font-mono text-xs tabular-nums ${
              selected.size > 0 ? "text-primary" : "text-muted-foreground"
            }`}
          >
            {selected.size}/{rows.length}
          </span>
        </AccordionTrigger>
      </div>
      <AccordionContent className="p-0">
        <div className="border-t">
          {rows.length === 0 ? (
            <div className="px-3 py-6 text-center text-sm text-muted-foreground">
              {emptyText}
            </div>
          ) : (
            <ScrollArea className="[&>[data-slot=scroll-area-viewport]]:max-h-72">
              {rows.map((row) => (
                <label
                  key={row.id}
                  className="flex cursor-pointer items-center gap-2.5 border-b border-border/40 px-3 py-2 transition-colors last:border-0 hover:bg-muted/30 has-[[data-state=checked]]:bg-muted/20"
                >
                  <Checkbox
                    checked={selected.has(row.id)}
                    onCheckedChange={() => toggleOne(row.id)}
                  />
                  <span className="min-w-0 flex-1 truncate text-sm">{row.primary}</span>
                  <span className="max-w-[45%] truncate font-mono text-xs tabular-nums text-muted-foreground">
                    {row.secondary}
                  </span>
                </label>
              ))}
            </ScrollArea>
          )}
        </div>
      </AccordionContent>
    </AccordionItem>
  )
}
