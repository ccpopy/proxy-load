import { useEffect, useState } from "react"
import { Save } from "lucide-react"
import { toast } from "sonner"

import { api } from "@/lib/api"
import type { DnsMapping } from "@/types"
import { Button } from "@/components/ui/button"
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import { Field, FieldContent, FieldDescription, FieldGroup, FieldLabel, FieldTitle } from "@/components/ui/field"
import { Input } from "@/components/ui/input"
import { Switch } from "@/components/ui/switch"

const consoleLabel = "text-[0.7rem] uppercase tracking-wider text-muted-foreground"

export function DnsDialog({
  value,
  onOpenChange,
  onSaved,
}: {
  value: DnsMapping | "new" | null
  onOpenChange: (open: boolean) => void
  onSaved: () => Promise<void>
}) {
  const mapping = value && value !== "new" ? value : null
  const [form, setForm] = useState({
    domain: "",
    ip: "",
    description: "",
    enabled: true,
    dynamic: false,
  })

  useEffect(() => {
    setForm({
      domain: mapping?.domain ?? "",
      ip: mapping?.ip ?? "",
      description: mapping?.description ?? "",
      enabled: mapping ? mapping.enabled === 1 : true,
      dynamic: mapping ? mapping.dynamic === 1 : false,
    })
  }, [mapping, value])

  async function save() {
    await api(mapping ? `/api/dns-mappings/${mapping.id}` : "/api/dns-mappings", {
      method: mapping ? "PUT" : "POST",
      body: JSON.stringify({
        ...form,
        enabled: form.enabled ? 1 : 0,
        dynamic: form.dynamic ? 1 : 0,
      }),
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
            <FieldLabel className={consoleLabel}>域名</FieldLabel>
            <Input
              value={form.domain}
              onChange={(event) => setForm({ ...form, domain: event.target.value })}
            />
          </Field>
          <Field>
            <FieldLabel className={consoleLabel}>
              {form.dynamic ? "IP地址（初始/备用，自动更新）" : "IP地址"}
            </FieldLabel>
            <Input
              className="font-mono tabular-nums"
              value={form.ip}
              onChange={(event) => setForm({ ...form, ip: event.target.value })}
            />
            {form.dynamic && (
              <FieldDescription>
                已启用动态解析：保存时会按域名解析一次，之后后台定期解析并在 IP
                变化时自动更新此处。若当前解析不到，可先填一个备用 IP。
              </FieldDescription>
            )}
          </Field>
          <Field>
            <FieldLabel className={consoleLabel}>说明</FieldLabel>
            <Input
              value={form.description}
              onChange={(event) => setForm({ ...form, description: event.target.value })}
            />
          </Field>
          <Field orientation="horizontal">
            <FieldContent>
              <FieldTitle className={consoleLabel}>动态解析</FieldTitle>
              <FieldDescription>
                按域名自动解析并定期更新 IP，适合 IP 会变化的内网/政务域名
              </FieldDescription>
            </FieldContent>
            <Switch
              checked={form.dynamic}
              onCheckedChange={(dynamic) => setForm({ ...form, dynamic })}
            />
          </Field>
          <Field orientation="horizontal">
            <FieldTitle className={consoleLabel}>启用</FieldTitle>
            <Switch
              checked={form.enabled}
              onCheckedChange={(enabled) => setForm({ ...form, enabled })}
            />
          </Field>
        </FieldGroup>
        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>
            取消
          </Button>
          <Button onClick={save}>
            <Save />
            保存
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
