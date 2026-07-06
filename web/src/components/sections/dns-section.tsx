import { Ban, BadgeCheck, Edit, Globe2, Plus, RefreshCw, Trash2 } from "lucide-react"
import { toast } from "sonner"

import { api } from "@/lib/api"
import { cn } from "@/lib/utils"
import type { DnsMapping } from "@/types"
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
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table"
import { EmptyState } from "@/components/common/empty-state"

export function DnsSection({
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
  async function toggleMapping(mapping: DnsMapping) {
    const result = await api<{ enabled: number }>(
      `/api/dns-mappings/${mapping.id}/toggle`,
      { method: "PUT" }
    )
    toast.success(result.enabled === 1 ? "DNS映射已启用" : "DNS映射已禁用")
    await onChanged()
  }

  async function deleteMapping(mapping: DnsMapping) {
    await api(`/api/dns-mappings/${mapping.id}`, { method: "DELETE" })
    toast.success("DNS映射已删除")
    await onChanged()
  }

  async function refreshMapping(mapping: DnsMapping) {
    try {
      const result = await api<{
        changed: boolean
        ip: string
        previousIp: string
      }>(`/api/dns-mappings/${mapping.id}/refresh`, { method: "POST" })
      if (result.changed) {
        toast.success(
          `${mapping.domain} 已更新：${result.previousIp} → ${result.ip}`
        )
      } else {
        toast.info(`${mapping.domain} 解析未变化（${result.ip}）`)
      }
      await onChanged()
    } catch (error) {
      toast.error(error instanceof Error ? error.message : "域名解析失败")
    }
  }

  return (
    <Card>
      <CardHeader className="flex flex-row items-start justify-between gap-4">
        <div>
          <CardTitle>DNS映射</CardTitle>
          <CardDescription>域名到固定 IP 的映射规则</CardDescription>
        </div>
        <Button onClick={onCreate}>
          <Plus />
          新增映射
        </Button>
      </CardHeader>
      <CardContent>
        {mappings.length === 0 ? (
          <EmptyState icon={Globe2} text="暂无DNS映射" />
        ) : (
          <div className="overflow-hidden rounded-md border">
            <Table>
              <TableHeader>
                <TableRow className="bg-muted/30 hover:bg-muted/30">
                  <TableHead>域名</TableHead>
                  <TableHead>IP地址</TableHead>
                  <TableHead>说明</TableHead>
                  <TableHead>状态</TableHead>
                  <TableHead className="text-right">操作</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {mappings.map((mapping) => {
                  const isEnabled = mapping.enabled === 1
                  const isDynamic = mapping.dynamic === 1
                  return (
                    <TableRow key={mapping.id}>
                    <TableCell className="font-mono font-medium tabular-nums">
                      {mapping.domain}
                    </TableCell>
                    <TableCell className="font-mono tabular-nums text-foreground/90">
                      <div className="flex items-center gap-2">
                        <span>{mapping.ip}</span>
                        {isDynamic && (
                          <Badge
                            variant="outline"
                            className="gap-1 rounded-sm border-primary/30 bg-primary/10 px-1.5 py-0 text-[0.65rem] font-normal text-primary"
                          >
                            <RefreshCw className="size-2.5" />
                            动态
                          </Badge>
                        )}
                      </div>
                    </TableCell>
                    <TableCell className="text-muted-foreground">
                      {mapping.description || "—"}
                    </TableCell>
                    <TableCell>
                      <DnsStatusBadge enabled={mapping.enabled} />
                    </TableCell>
                    <TableCell>
                      <div className="flex justify-end gap-2">
                        <Button
                          variant="outline"
                          size="sm"
                          onClick={() => onEdit(mapping)}
                        >
                          <Edit />
                          编辑
                        </Button>
                        {isDynamic && (
                          <Button
                            variant="outline"
                            size="sm"
                            onClick={() => refreshMapping(mapping)}
                          >
                            <RefreshCw />
                            刷新
                          </Button>
                        )}
                        <Button
                          variant="outline"
                          size="sm"
                          className={cn(
                            isEnabled
                              ? "border-destructive/30 text-destructive hover:bg-destructive/10 hover:text-destructive"
                              : "border-success/40 text-success hover:bg-success/10 hover:text-success"
                          )}
                          onClick={() => toggleMapping(mapping)}
                        >
                          {isEnabled ? <Ban /> : <BadgeCheck />}
                          {isEnabled ? "禁用" : "启用"}
                        </Button>
                        <Button
                          variant="ghost"
                          size="sm"
                          className="text-muted-foreground hover:bg-destructive/10 hover:text-destructive"
                          onClick={() => deleteMapping(mapping)}
                        >
                          <Trash2 />
                          删除
                        </Button>
                      </div>
                    </TableCell>
                  </TableRow>
                  )
                })}
              </TableBody>
            </Table>
          </div>
        )}
      </CardContent>
    </Card>
  )
}

function DnsStatusBadge({ enabled }: { enabled: number }) {
  const on = enabled === 1
  return (
    <Badge
      variant="outline"
      className={cn(
        "gap-1.5 rounded-sm",
        on
          ? "border-success/30 bg-success/10 text-success"
          : "border-border bg-muted/50 text-muted-foreground"
      )}
    >
      <span
        className="size-1.5 rounded-full"
        style={{ background: on ? "var(--success)" : "var(--muted-foreground)" }}
      />
      {on ? "已启用" : "已禁用"}
    </Badge>
  )
}
