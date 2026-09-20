import { Edit, Layers3, Plus, Trash2 } from "lucide-react"

import { api } from "@/lib/api"
import { groupAlgorithms } from "@/lib/group-policy"
import type { ProxyGroup } from "@/types"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card"
import { EmptyState } from "@/components/common/empty-state"

export function GroupSection({
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
          <CardDescription>
            命中域名规则时使用分组代理；未命中时使用全局已启用代理
          </CardDescription>
        </div>
        <Button onClick={onCreate}>
          <Plus />
          新增分组
        </Button>
      </CardHeader>
      <CardContent className="grid gap-2.5">
        {groups.map((group) => (
          <div
            key={group.id}
            className="group relative grid gap-3 rounded-md border bg-card/40 p-4 transition-colors hover:border-primary/40 hover:bg-card lg:grid-cols-[1fr_auto] lg:items-start"
          >
            <div className="min-w-0 space-y-3">
              <div className="flex flex-wrap items-center gap-2">
                <span className="truncate font-medium">{group.name}</span>
                <Badge variant="outline">{groupAlgorithms[group.algorithm_override || "inherit"] || "继承全局设置"}</Badge>
                {(group.sticky_failover_seconds ?? 0) > 0 && <Badge variant="secondary">粘滞保持 {group.sticky_failover_seconds} 秒</Badge>}
                {group.enabled !== 1 && (
                  <Badge variant="secondary">已禁用</Badge>
                )}
              </div>
              <div className="flex flex-wrap items-center gap-1.5">
                <span className="text-[0.7rem] uppercase tracking-wider text-muted-foreground/70">
                  域名
                </span>
                {group.domains.length > 0 ? (
                  group.domains.map((domain) => (
                    <span
                      key={domain.id}
                      className="rounded-sm border border-border bg-muted/50 px-1.5 py-0.5 font-mono text-[0.7rem] text-muted-foreground"
                    >
                      {domain.domain}
                    </span>
                  ))
                ) : (
                  <span className="text-xs text-muted-foreground">无域名</span>
                )}
              </div>
              <div className="flex flex-wrap items-center gap-1.5">
                <span className="text-[0.7rem] uppercase tracking-wider text-muted-foreground/70">
                  代理
                </span>
                {group.members.length > 0 ? (
                  group.members.map((member) => (
                    <Badge key={member.proxy_id} variant="secondary">
                      {member.name}
                    </Badge>
                  ))
                ) : (
                  <span className="text-xs text-muted-foreground">无代理</span>
                )}
              </div>
            </div>
            <div className="flex items-center gap-2">
              <Button variant="outline" size="sm" onClick={() => onEdit(group)}>
                <Edit />
                编辑
              </Button>
              <Button
                variant="ghost"
                size="sm"
                className="text-muted-foreground hover:bg-destructive/10 hover:text-destructive"
                onClick={async () => {
                  await api(`/api/proxy-groups/${group.id}`, {
                    method: "DELETE",
                  })
                  await onChanged()
                }}
              >
                <Trash2 />
                删除
              </Button>
            </div>
          </div>
        ))}
        {groups.length === 0 && (
          <EmptyState icon={Layers3} text="暂无代理分组" />
        )}
      </CardContent>
    </Card>
  )
}
