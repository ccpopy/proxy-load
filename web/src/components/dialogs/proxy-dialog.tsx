import { useEffect, useMemo, useState } from "react"
import { Check, ChevronsUpDown, Save } from "lucide-react"
import { toast } from "sonner"

import { api } from "@/lib/api"
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

  useEffect(() => {
    if (value === null) return undefined

    let closed = false
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
                onValueChange={(type) => setForm({ ...form, type })}
              >
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
                  value={form.password}
                  onChange={(event) =>
                    setForm({ ...form, password: event.target.value })
                  }
                />
              </Field>
            </div>
            <Field>
              <FieldLabel>测试地址</FieldLabel>
              <TestUrlCombobox
                value={form.test_url}
                options={testUrls}
                onChange={(test_url) => setForm({ ...form, test_url })}
              />
            </Field>
            <Field>
              <FieldLabel>超时时间（秒）</FieldLabel>
              <Input
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
