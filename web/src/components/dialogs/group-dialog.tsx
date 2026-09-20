import { useEffect, useState } from "react"
import { Save } from "lucide-react"
import { toast } from "sonner"

import { api, commandErrorMessage } from "@/lib/api"
import { groupAlgorithms, groupPolicyPayload } from "@/lib/group-policy"
import type { ProxyGroup, ProxyRecord } from "@/types"
import { Button } from "@/components/ui/button"
import { Checkbox } from "@/components/ui/checkbox"
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import { Field, FieldDescription, FieldGroup, FieldLabel, FieldTitle } from "@/components/ui/field"
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select"
import { Input } from "@/components/ui/input"
import { ScrollArea } from "@/components/ui/scroll-area"
import { Separator } from "@/components/ui/separator"
import { Switch } from "@/components/ui/switch"
import { Textarea } from "@/components/ui/textarea"

export function GroupDialog({
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
    enabled: true,
    algorithm: "inherit",
    hold: "0",
  })
  const [saving, setSaving] = useState(false)
  const [policyError, setPolicyError] = useState<string | null>(null)

  useEffect(() => {
    setForm({
      name: group?.name ?? "",
      domains: group?.domains.map((item) => item.domain).join("\n") ?? "",
      proxy_ids: group?.members.map((item) => item.proxy_id) ?? [],
      enabled: group ? group.enabled === 1 : true,
      algorithm: group?.algorithm_override || "inherit",
      hold: String(group?.sticky_failover_seconds ?? 0),
    })
    setPolicyError(null)
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
    if (saving) return
    setSaving(true)
    try {
      let policy
      try {
        policy = groupPolicyPayload(form.algorithm, form.hold)
        setPolicyError(null)
      } catch (error) {
        setPolicyError(commandErrorMessage(error, "分组策略配置无效"))
        return
      }
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
          is_default: group?.is_default ?? 0,
          enabled: form.enabled ? 1 : 0,
          ...policy,
        }),
      })
      toast.success(group ? "代理分组已更新" : "代理分组已创建")
      await onSaved()
    } catch (error) {
      toast.error(commandErrorMessage(error, "代理分组保存失败"))
    } finally {
      setSaving(false)
    }
  }

  return (
    <Dialog open={value !== null} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[calc(100vh-4rem)] overflow-hidden">
        <DialogHeader>
          <DialogTitle>{group ? "编辑代理分组" : "新增代理分组"}</DialogTitle>
          <DialogDescription>设置域名规则、负载策略和代理成员</DialogDescription>
        </DialogHeader>
        <ScrollArea className="h-[calc(100vh-14rem)] max-h-[560px] pr-4">
          <FieldGroup className="pb-1">
            <Field>
              <FieldLabel>分组名称</FieldLabel>
              <Input
                value={form.name}
                onChange={(event) =>
                  setForm({ ...form, name: event.target.value })
                }
              />
            </Field>
            <Field>
              <FieldLabel>域名列表</FieldLabel>
              <Textarea
                value={form.domains}
                rows={4}
                onChange={(event) =>
                  setForm({ ...form, domains: event.target.value })
                }
              />
            </Field>
            <Field orientation="horizontal">
              <FieldTitle>启用</FieldTitle>
              <Switch
                checked={form.enabled}
                onCheckedChange={(enabled) => setForm({ ...form, enabled })}
              />
            </Field>
            <Field>
              <FieldLabel htmlFor="group-algorithm">分组负载算法</FieldLabel>
              <Select value={form.algorithm} onValueChange={(algorithm) => setForm({ ...form, algorithm })}>
                <SelectTrigger id="group-algorithm" className="w-full"><SelectValue /></SelectTrigger>
                <SelectContent>
                  {Object.entries(groupAlgorithms).map(([key, label]) => <SelectItem key={key} value={key}>{label}</SelectItem>)}
                </SelectContent>
              </Select>
            </Field>
            <Field>
              <FieldLabel htmlFor="group-failover-hold">故障切换保持期（秒）</FieldLabel>
              <Input id="group-failover-hold" type="number" min={0} max={86400} step={1}
                className="font-mono tabular-nums" value={form.hold} aria-invalid={Boolean(policyError)}
                aria-describedby="group-failover-help"
                onChange={(event) => { setForm({ ...form, hold: event.target.value }); setPolicyError(null) }} />
              <FieldDescription id="group-failover-help">
                仅在使用主机粘滞时生效。切换后优先保持可用的备用代理，避免恢复后立即切回；0 表示关闭。
              </FieldDescription>
              {policyError && <p role="alert" className="text-sm text-destructive">{policyError}</p>}
            </Field>
            <Separator />
            <div className="grid gap-2">
              {proxies.map((proxy) => (
                <label
                  key={proxy.id}
                  className="flex items-center gap-3 rounded-md border bg-card/40 p-3 transition-colors hover:bg-card"
                >
                  <Checkbox
                    checked={form.proxy_ids.includes(proxy.id)}
                    onCheckedChange={(checked) =>
                      toggleProxy(proxy.id, checked === true)
                    }
                  />
                  <span className="min-w-0 flex-1">
                    <span className="block truncate text-sm font-medium">
                      {proxy.name}
                      {proxy.enabled !== 1 && <span className="ml-2 text-xs font-normal text-muted-foreground">已禁用</span>}
                    </span>
                    <span className="block truncate font-mono text-xs tabular-nums text-muted-foreground">
                      {proxy.type}://{proxy.host}:{proxy.port}
                    </span>
                  </span>
                </label>
              ))}
            </div>
          </FieldGroup>
        </ScrollArea>
        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>
            取消
          </Button>
          <Button onClick={save} disabled={saving}>
            <Save />
            {saving ? "保存中…" : "保存"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
