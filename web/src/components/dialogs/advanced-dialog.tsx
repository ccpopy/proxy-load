import { useEffect, useRef, useState, type ReactNode } from "react"
import {
  Activity,
  AppWindow,
  Eye,
  EyeOff,
  Network,
  RefreshCw,
  RotateCcw,
  Save,
  Shield,
  Zap,
  type LucideIcon,
} from "lucide-react"
import { toast } from "sonner"

import { api, commandErrorMessage, jsonBody } from "@/lib/api"
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

interface SaveResult {
  message?: string
  requiresRestart?: boolean
}

export function AdvancedDialog({
  open,
  config: persistedConfig,
  onOpenChange,
  onChanged,
}: {
  open: boolean
  config: AdvancedConfig
  onOpenChange: (open: boolean) => void
  onChanged: () => Promise<void>
}) {
  const [config, setConfig] = useState(persistedConfig)
  const [showInboundPassword, setShowInboundPassword] = useState(false)
  const wasOpenRef = useRef(false)

  useEffect(() => {
    if (open && !wasOpenRef.current) setConfig(persistedConfig)
    wasOpenRef.current = open
  }, [open, persistedConfig])

  function update<K extends keyof AdvancedConfig>(key: K, value: AdvancedConfig[K]) {
    setConfig({ ...config, [key]: value })
  }

  function generateCredentials() {
    try {
      const credentials = createInboundCredentials()
      setConfig({
        ...config,
        inbound_auth_username: credentials.username,
        inbound_auth_password: credentials.password,
      })
      setShowInboundPassword(true)
    } catch (error) {
      toast.error(commandErrorMessage(error, "随机凭据生成失败"))
    }
  }

  async function save() {
    try {
      const result = await api<SaveResult>(
        "/api/advanced-config",
        jsonBody(config)
      )
      toast.success(result.message ?? "高级配置已保存")
      await onChanged()
      setConfig(await api<AdvancedConfig>("/api/advanced-config"))
    } catch (error) {
      toast.error(commandErrorMessage(error, "高级配置保存失败"))
    }
  }

  async function reset() {
    try {
      const result = await api<SaveResult>("/api/advanced-config/reset", {
        method: "POST",
      })
      toast.success(result.message ?? "已恢复默认配置")
      await onChanged()
      setConfig(await api<AdvancedConfig>("/api/advanced-config"))
    } catch (error) {
      toast.error(commandErrorMessage(error, "恢复默认配置失败"))
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[calc(100vh-4rem)] w-[calc(100vw-3rem)] max-w-none overflow-hidden sm:max-w-6xl">
        <DialogHeader>
          <DialogTitle>高级设置</DialogTitle>
          <DialogDescription>监听、认证、测活、熔断和应用行为参数</DialogDescription>
        </DialogHeader>
        <ScrollArea className="h-[calc(100vh-14rem)] max-h-[650px] pr-4">
          <div className="grid gap-6 pb-1 lg:grid-cols-2 lg:items-start">
            <div className="grid gap-6">
              <ConfigGroup title="监听与认证" icon={Network}>
                <NumberField
                  label="代理服务端口"
                  value={config.proxy_port}
                  onChange={(value) => update("proxy_port", value)}
                />
                <Field orientation="horizontal">
                  <FieldContent>
                    <FieldTitle>允许局域网连接</FieldTitle>
                    <FieldDescription>
                      关闭时仅监听 127.0.0.1；开启后建议同时启用认证，修改需重启应用
                    </FieldDescription>
                  </FieldContent>
                  <Switch
                    checked={config.allow_lan}
                    onCheckedChange={(value) => update("allow_lan", value)}
                  />
                </Field>
                <Field orientation="horizontal">
                  <FieldContent>
                    <FieldTitle>启用入站认证</FieldTitle>
                    <FieldDescription>
                      SOCKS5、HTTP 和 HTTPS CONNECT 共用下方凭据；局域网传输不加密
                    </FieldDescription>
                  </FieldContent>
                  <Switch
                    checked={config.inbound_auth_enabled}
                    onCheckedChange={(value) =>
                      update("inbound_auth_enabled", value)
                    }
                  />
                </Field>
                <Field>
                  <FieldLabel>认证用户名</FieldLabel>
                  <div className="flex gap-2">
                    <Input
                      className="font-mono text-xs"
                      value={config.inbound_auth_username}
                      autoComplete="off"
                      onChange={(event) =>
                        update("inbound_auth_username", event.target.value)
                      }
                    />
                    <Button
                      type="button"
                      variant="outline"
                      onClick={generateCredentials}
                    >
                      <RefreshCw />
                      随机生成
                    </Button>
                  </div>
                </Field>
                <Field>
                  <FieldLabel>认证密码</FieldLabel>
                  <div className="relative">
                    <Input
                      type={showInboundPassword ? "text" : "password"}
                      className="pr-10 font-mono text-xs"
                      value={config.inbound_auth_password}
                      autoComplete="new-password"
                      onChange={(event) =>
                        update("inbound_auth_password", event.target.value)
                      }
                    />
                    <Button
                      type="button"
                      variant="ghost"
                      size="icon"
                      className="absolute top-1/2 right-1 size-7 -translate-y-1/2"
                      aria-label={showInboundPassword ? "隐藏认证密码" : "显示认证密码"}
                      title={showInboundPassword ? "隐藏认证密码" : "显示认证密码"}
                      onClick={() => setShowInboundPassword((value) => !value)}
                    >
                      {showInboundPassword ? <EyeOff /> : <Eye />}
                    </Button>
                  </div>
                </Field>
              </ConfigGroup>

              <ConfigGroup title="测活与数据" icon={Activity}>
                <FieldGroup className="grid gap-4 sm:grid-cols-2">
                  <NumberField
                    label="活跃节点心跳（分钟）"
                    value={Math.round(config.periodic_test_interval / 60000)}
                    onChange={(value) =>
                      update("periodic_test_interval", value * 60000)
                    }
                  />
                  <NumberField
                    label="失败节点重测（分钟）"
                    value={Math.round(config.probe_recovery_interval / 60000)}
                    onChange={(value) =>
                      update("probe_recovery_interval", value * 60000)
                    }
                  />
                </FieldGroup>
                <FieldGroup className="grid gap-4 sm:grid-cols-2">
                  <NumberField
                    label="并发测活数"
                    value={config.probe_concurrency}
                    onChange={(value) => update("probe_concurrency", value)}
                  />
                  <NumberField
                    label="连续失败阈值"
                    value={config.probe_failure_threshold}
                    onChange={(value) =>
                      update("probe_failure_threshold", value)
                    }
                  />
                </FieldGroup>
                <FieldGroup className="grid gap-4 sm:grid-cols-2">
                  <NumberField
                    label="动态 DNS 刷新（分钟）"
                    value={Math.round(config.dns_refresh_interval / 60000)}
                    onChange={(value) =>
                      update("dns_refresh_interval", value * 60000)
                    }
                  />
                  <NumberField
                    label="流量与统计保留天数"
                    value={config.log_retention_days}
                    onChange={(value) => update("log_retention_days", value)}
                  />
                </FieldGroup>
              </ConfigGroup>
            </div>

            <div className="grid gap-6">
              <ConfigGroup title="应用行为" icon={AppWindow}>
                <Field orientation="horizontal">
                  <FieldContent>
                    <FieldTitle>后台运行</FieldTitle>
                    <FieldDescription>
                      点击关闭按钮时最小化到系统托盘
                    </FieldDescription>
                  </FieldContent>
                  <Switch
                    checked={config.background_run}
                    onCheckedChange={(value) => update("background_run", value)}
                  />
                </Field>
                <Field orientation="horizontal">
                  <FieldContent>
                    <FieldTitle>启动时最小化到托盘</FieldTitle>
                    <FieldDescription>下次启动时不显示主窗口</FieldDescription>
                  </FieldContent>
                  <Switch
                    checked={config.start_minimized}
                    onCheckedChange={(value) => update("start_minimized", value)}
                  />
                </Field>
              </ConfigGroup>

              <ConfigGroup title="熔断器" icon={Shield}>
                <NumberField
                  label="连续连接失败阈值"
                  value={config.circuit_failure_threshold}
                  onChange={(value) =>
                    update("circuit_failure_threshold", value)
                  }
                />
                <NumberField
                  label="熔断时长（秒）"
                  value={Math.round(config.circuit_timeout / 1000)}
                  onChange={(value) => update("circuit_timeout", value * 1000)}
                />
              </ConfigGroup>

              <ConfigGroup title="快速失败" icon={Zap}>
                <Field orientation="horizontal">
                  <FieldContent>
                    <FieldTitle>限制代理尝试次数</FieldTitle>
                    <FieldDescription>
                      关闭后会在总超时内尝试全部可用代理
                    </FieldDescription>
                  </FieldContent>
                  <Switch
                    checked={config.failfast_enabled}
                    onCheckedChange={(value) =>
                      update("failfast_enabled", value)
                    }
                  />
                </Field>
                <NumberField
                  label="最大尝试次数"
                  value={config.failfast_max_attempts}
                  onChange={(value) => update("failfast_max_attempts", value)}
                />
                <FieldGroup className="grid gap-4 sm:grid-cols-2">
                  <NumberField
                    label="单次超时（秒）"
                    value={Math.round(config.failfast_attempt_timeout / 1000)}
                    onChange={(value) =>
                      update("failfast_attempt_timeout", value * 1000)
                    }
                  />
                  <NumberField
                    label="总超时（秒）"
                    value={Math.round(config.failfast_total_timeout / 1000)}
                    onChange={(value) =>
                      update("failfast_total_timeout", value * 1000)
                    }
                  />
                </FieldGroup>
              </ConfigGroup>
            </div>
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
    <div className="h-fit rounded-md border bg-card/40 p-4">
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
        min={1}
        className="font-mono tabular-nums"
        value={value}
        onChange={(event) => onChange(Number(event.target.value))}
      />
    </Field>
  )
}

function createInboundCredentials() {
  if (typeof crypto.randomUUID !== "function") {
    throw new Error("当前运行环境不支持安全 UUID 生成")
  }
  const alphabet =
    "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_"
  const random = new Uint8Array(32)
  crypto.getRandomValues(random)
  return {
    username: crypto.randomUUID(),
    password: Array.from(random, (value) => alphabet[value & 63]).join(""),
  }
}
