import { useEffect, useMemo, useState } from "react"
import { Check, ChevronsUpDown, Save } from "lucide-react"
import { toast } from "sonner"

import { api, commandErrorMessage } from "@/lib/api"
import { cn } from "@/lib/utils"
import type { ProxyRecord } from "@/types"
import { Button } from "@/components/ui/button"
import {
  Command,
  CommandEmpty,
  CommandGroup,
  CommandItem,
  CommandList,
} from "@/components/ui/command"
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
  Popover,
  PopoverAnchor,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover"
import { ScrollArea } from "@/components/ui/scroll-area"
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"
import { Switch } from "@/components/ui/switch"

export function ProxyDialog({
  value,
  onOpenChange,
  onSaved,
}: {
  value: ProxyRecord | "new" | null
  onOpenChange: (open: boolean) => void
  onSaved: () => Promise<void>
}) {
  const proxy = value && value !== "new" ? value : null
  const [testUrls, setTestUrls] = useState<string[]>([])
  const [globalTestUrl, setGlobalTestUrl] = useState("")
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
    health_mode: "transport_only",
    failure_threshold: 2,
    recovery_threshold: 2,
    expected_statuses: "200, 204",
    max_age_seconds: 600,
  })

  useEffect(() => {
    setForm({
      name: proxy?.name ?? "",
      type: proxy?.type ?? "http",
      host: proxy?.host ?? "",
      port: proxy?.port ?? 1080,
      username: proxy?.username ?? "",
      password: proxy?.type === "socks4" ? "" : (proxy?.password ?? ""),
      enabled: proxy ? proxy.enabled === 1 : true,
      test_url: proxy?.test_url ?? "",
      test_timeout: proxy?.test_timeout ? String(proxy.test_timeout) : "",
      skip_cert_verify: proxy ? proxy.skip_cert_verify === 1 : false,
      health_mode: proxy?.health_policy?.mode ?? "transport_only",
      failure_threshold: proxy?.health_policy?.failure_threshold ?? 2,
      recovery_threshold: proxy?.health_policy?.recovery_threshold ?? 2,
      expected_statuses: (proxy?.health_policy?.expected_statuses ?? [200, 204]).join(", "),
      max_age_seconds: proxy?.health_policy?.max_age_seconds ?? 600,
    })
  }, [proxy, value])

  useEffect(() => {
    if (value === null) return undefined

    let closed = false
    api<Record<string, string>>("/api/settings").then((settings) => {
      if (!closed) setGlobalTestUrl(settings.test_url ?? "")
    }).catch(() => { if (!closed) setGlobalTestUrl("") })
    api<string[]>("/api/test-urls")
      .then((urls) => {
        if (!closed) {
          setTestUrls(urls)
        }
      })
      .catch((error) => {
        if (!closed) {
          toast.error(error instanceof Error ? error.message : "读取测试地址失败")
        }
      })

    return () => {
      closed = true
    }
  }, [value])

  async function save() {
    const body = {
      ...form,
      port: Number(form.port),
      enabled: form.enabled ? 1 : 0,
      test_timeout: form.test_timeout ? Number(form.test_timeout) : null,
      skip_cert_verify: form.skip_cert_verify ? 1 : 0,
      health_policy: {
        mode: form.health_mode,
        failure_threshold: form.failure_threshold,
        recovery_threshold: form.recovery_threshold,
        expected_statuses: form.expected_statuses.split(/[,，\s]+/).filter(Boolean).map(Number),
        max_age_seconds: form.max_age_seconds,
      },
    }
    try {
      await api(proxy ? `/api/proxies/${proxy.id}` : "/api/proxies", {
        method: proxy ? "PUT" : "POST",
        body: JSON.stringify(body),
      })
      toast.success(proxy ? "代理已更新" : "代理已创建")
      await onSaved()
    } catch (error) {
      toast.error(commandErrorMessage(error, proxy ? "代理更新失败" : "代理创建失败"))
    }
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
              <Input
                value={form.name}
                onChange={(event) =>
                  setForm({ ...form, name: event.target.value })
                }
              />
            </Field>
            <Field orientation="horizontal">
              <FieldContent>
                <FieldTitle>启用</FieldTitle>
                <FieldDescription>启用后参与负载均衡</FieldDescription>
              </FieldContent>
              <Switch
                checked={form.enabled}
                onCheckedChange={(enabled) => setForm({ ...form, enabled })}
              />
            </Field>
            <Field>
              <FieldLabel>类型</FieldLabel>
              <Select
                value={form.type}
                onValueChange={(type) =>
                  setForm({
                    ...form,
                    type,
                    password: type === "socks4" ? "" : form.password,
                  })
                }
              >
                <SelectTrigger className="w-full">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectGroup>
                    <SelectItem value="http">HTTP（支持 HTTPS CONNECT）</SelectItem>
                    <SelectItem value="socks4">SOCKS4</SelectItem>
                    <SelectItem value="socks5">SOCKS5</SelectItem>
                  </SelectGroup>
                </SelectContent>
              </Select>
              {form.type === "socks4" && (
                <FieldDescription className="text-amber-600 dark:text-amber-500">
                  SOCKS4 仅支持 USERID，不支持密码认证。若上游代理需要账号密码，请改用 SOCKS5。
                </FieldDescription>
              )}
            </Field>
            <div className="grid gap-4 sm:grid-cols-2">
              <Field>
                <FieldLabel>主机</FieldLabel>
                <Input
                  value={form.host}
                  onChange={(event) =>
                    setForm({ ...form, host: event.target.value })
                  }
                />
              </Field>
              <Field>
                <FieldLabel>端口</FieldLabel>
                <Input
                  type="number"
                  className="font-mono tabular-nums"
                  value={form.port}
                  onChange={(event) =>
                    setForm({ ...form, port: Number(event.target.value) })
                  }
                />
              </Field>
            </div>
            <div className="grid gap-4 sm:grid-cols-2">
              <Field>
                <FieldLabel>用户名</FieldLabel>
                <Input
                  value={form.username}
                  onChange={(event) =>
                    setForm({ ...form, username: event.target.value })
                  }
                />
              </Field>
              <Field>
                <FieldLabel>密码</FieldLabel>
                <Input
                  type="password"
                  disabled={form.type === "socks4"}
                  value={form.password}
                  onChange={(event) =>
                    setForm({ ...form, password: event.target.value })
                  }
                />
              </Field>
            </div>
            <Field>
              <FieldLabel>健康策略</FieldLabel>
              <Select value={form.health_mode} onValueChange={(health_mode) => setForm({ ...form, health_mode })}>
                <SelectTrigger className="w-full" aria-label="健康策略"><SelectValue /></SelectTrigger>
                <SelectContent>
                  <SelectItem value="transport_only">通用代理</SelectItem>
                  <SelectItem value="required_probe">专用节点 · 必须通过业务测活</SelectItem>
                </SelectContent>
              </Select>
              <FieldDescription>
                {form.health_mode === "required_probe"
                  ? "失败达到阈值后，隔离此节点的全部新连接；后台连续探测成功后恢复。已有连接不受影响。"
                  : "目标网站测试失败只提示异常，不隔离节点。VPN / 专用业务节点请切换为业务测活。"}
              </FieldDescription>
            </Field>
            {form.health_mode === "required_probe" && <>
              <div className="grid grid-cols-2 gap-4">
                <Field><FieldLabel htmlFor="probe-failures">连续失败阈值</FieldLabel>
                  <Input id="probe-failures" type="number" min={1} max={10} value={form.failure_threshold}
                    onChange={(e) => setForm({ ...form, failure_threshold: Number(e.target.value) })} />
                </Field>
                <Field><FieldLabel htmlFor="probe-recovery">连续恢复阈值</FieldLabel>
                  <Input id="probe-recovery" type="number" min={1} max={10} value={form.recovery_threshold}
                    onChange={(e) => setForm({ ...form, recovery_threshold: Number(e.target.value) })} />
                </Field>
              </div>
              <Field><FieldLabel htmlFor="probe-statuses">预期 HTTP 状态码</FieldLabel>
                <Input id="probe-statuses" value={form.expected_statuses}
                  onChange={(e) => setForm({ ...form, expected_statuses: e.target.value })} />
                <FieldDescription>逗号分隔，默认 200、204。不跟随重定向，登录页跳转不计成功。</FieldDescription>
              </Field>
              <Field><FieldLabel htmlFor="probe-max-age">业务就绪有效期（秒）</FieldLabel>
                <Input id="probe-max-age" type="number" min={30} max={86400} value={form.max_age_seconds}
                  onChange={(e) => setForm({ ...form, max_age_seconds: Number(e.target.value) })} />
                <FieldDescription>超过此时间未完整测活成功，将等待重新验证。应大于后台测活间隔。</FieldDescription>
              </Field>
            </>}
            <Field>
              <FieldLabel htmlFor="proxy-test-url">测试地址</FieldLabel>
              <TestUrlCombobox
                value={form.test_url}
                options={testUrls}
                onChange={(test_url) => setForm({ ...form, test_url })}
              />
              <FieldDescription className="break-all">
                生效地址（{form.test_url.trim() ? "节点独立" : "继承全局"}）：{form.test_url.trim() || globalTestUrl || "未读取"}
              </FieldDescription>
              {form.health_mode === "required_probe" && <FieldDescription className="text-amber-600 dark:text-amber-500">
                请填写仅在 VPN / 业务链路就绪时可访问的只读健康接口。公共网站、代理监听端口、VPN 登录页和 VNC 端口不代表业务就绪；不要在 URL 中填写凭据。
              </FieldDescription>}
            </Field>
            <Field>
              <FieldLabel>超时时间（秒）</FieldLabel>
              <Input
                type="number"
                min={1}
                max={300}
                className="font-mono tabular-nums"
                value={form.test_timeout}
                onChange={(event) =>
                  setForm({ ...form, test_timeout: event.target.value })
                }
              />
            </Field>
            <Field orientation="horizontal">
              <FieldContent>
                <FieldTitle>跳过证书验证</FieldTitle>
                <FieldDescription>仅影响连通性测试</FieldDescription>
              </FieldContent>
              <Switch
                checked={form.skip_cert_verify}
                onCheckedChange={(skip_cert_verify) =>
                  setForm({ ...form, skip_cert_verify })
                }
              />
            </Field>
          </FieldGroup>
        </ScrollArea>
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

function TestUrlCombobox({
  value,
  options,
  onChange,
}: {
  value: string
  options: string[]
  onChange: (value: string) => void
}) {
  const [open, setOpen] = useState(false)
  const normalizedValue = value.trim().toLowerCase()
  const uniqueOptions = useMemo(
    () =>
      Array.from(
        new Set(options.map((option) => option.trim()).filter(Boolean))
      ).sort((left, right) => left.localeCompare(right)),
    [options]
  )
  const filteredOptions = useMemo(() => {
    if (!normalizedValue) return uniqueOptions
    return uniqueOptions.filter((option) =>
      option.toLowerCase().includes(normalizedValue)
    )
  }, [normalizedValue, uniqueOptions])

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverAnchor asChild>
        <div className="relative w-full">
          <Input
            id="proxy-test-url"
            role="combobox"
            aria-expanded={open}
            value={value}
            onFocus={() => setOpen(true)}
            onChange={(event) => {
              onChange(event.target.value)
              setOpen(true)
            }}
            className="pr-10 font-mono text-xs"
            placeholder="https://example.com"
          />
          <PopoverTrigger asChild>
            <Button
              type="button"
              variant="ghost"
              size="icon"
              className="absolute top-1/2 right-1 size-7 -translate-y-1/2 rounded-sm text-muted-foreground hover:bg-accent hover:text-foreground"
              aria-label="选择已保存测试地址"
            >
              <ChevronsUpDown />
            </Button>
          </PopoverTrigger>
        </div>
      </PopoverAnchor>
      <PopoverContent
        onOpenAutoFocus={(event) => event.preventDefault()}
        align="start"
        side="bottom"
        className="w-[var(--radix-popover-trigger-width)] max-w-[calc(100vw-3rem)] p-0"
      >
        <Command shouldFilter={false}>
          <CommandList>
            {filteredOptions.length === 0 ? (
              <CommandEmpty>
                {uniqueOptions.length === 0 ? "暂无已保存测试地址" : "无匹配地址"}
              </CommandEmpty>
            ) : (
              <CommandGroup heading="已保存测试地址">
                {filteredOptions.map((option) => (
                  <CommandItem
                    key={option}
                    value={option}
                    onSelect={() => {
                      onChange(option)
                      setOpen(false)
                    }}
                  >
                    <Check
                      className={cn(
                        value === option ? "opacity-100" : "opacity-0"
                      )}
                    />
                    <span className="truncate font-mono text-xs">{option}</span>
                  </CommandItem>
                ))}
              </CommandGroup>
            )}
          </CommandList>
        </Command>
      </PopoverContent>
    </Popover>
  )
}
