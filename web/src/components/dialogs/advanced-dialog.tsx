import type { ReactNode } from "react"
import {
  Database,
  RotateCcw,
  Save,
  Settings,
  Shield,
  Zap,
  type LucideIcon,
} from "lucide-react"
import { toast } from "sonner"

import { api, jsonBody } from "@/lib/api"
import type { AdvancedConfig } from "@/types"
import { Button } from "@/components/ui/button"
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
import { ScrollArea } from "@/components/ui/scroll-area"
import { Switch } from "@/components/ui/switch"

export function AdvancedDialog({
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

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[calc(100vh-4rem)] w-[calc(100vw-3rem)] max-w-none overflow-hidden sm:max-w-6xl">
        <DialogHeader>
          <DialogTitle>高级设置</DialogTitle>
          <DialogDescription>连接池、熔断器、健康检查和快速失败参数</DialogDescription>
        </DialogHeader>
        <ScrollArea className="h-[calc(100vh-14rem)] max-h-[620px] pr-4">
          <div className="grid gap-6 pb-1 lg:grid-cols-2">
            <ConfigGroup title="基础配置" icon={Settings}>
              <NumberField label="代理服务端口" value={config.proxy_port} onChange={(value) => update("proxy_port", value)} />
              <NumberField label="活跃节点测活间隔（分钟）" value={Math.round(config.periodic_test_interval / 60000)} onChange={(value) => update("periodic_test_interval", value * 60000)} />
              <FieldGroup className="grid gap-4 sm:grid-cols-2">
                <NumberField label="失败节点重测间隔（秒）" value={Math.round(config.probe_recovery_interval / 1000)} onChange={(value) => update("probe_recovery_interval", value * 1000)} />
                <NumberField label="并发测活数" value={config.probe_concurrency} onChange={(value) => update("probe_concurrency", value)} />
              </FieldGroup>
              <FieldGroup className="grid gap-4 sm:grid-cols-2">
                <NumberField label="请求日志保留天数" value={config.log_retention_days} onChange={(value) => update("log_retention_days", value)} />
                <NumberField label="统计保留天数" value={config.stats_retention_days} onChange={(value) => update("stats_retention_days", value)} />
              </FieldGroup>
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
          <Button variant="outline" onClick={reset}>
            <RotateCcw />
            恢复默认
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

function ConfigGroup({
  title,
  icon: Icon,
  children,
}: {
  title: string
  icon: LucideIcon
  children: ReactNode
}) {
  return (
    <div className="rounded-md border bg-card/40 p-4">
      <div className="mb-4 flex items-center gap-2.5">
        <span className="flex size-7 items-center justify-center rounded-sm border border-border bg-muted/50 text-muted-foreground">
          <Icon className="size-3.5" />
        </span>
        <span className="text-sm font-medium tracking-tight">{title}</span>
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
      <FieldLabel className="text-[0.7rem] uppercase tracking-wider text-muted-foreground">
        {label}
      </FieldLabel>
      <Input
        type="number"
        className="font-mono tabular-nums"
        value={value}
        onChange={(event) => onChange(Number(event.target.value))}
      />
    </Field>
  )
}
