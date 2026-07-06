import { useEffect, useState } from "react"
import { Save } from "lucide-react"
import { toast } from "sonner"

import { api, jsonBody } from "@/lib/api"
import { Button } from "@/components/ui/button"
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card"
import { Field, FieldGroup, FieldLabel } from "@/components/ui/field"
import { Input } from "@/components/ui/input"
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"
import { Separator } from "@/components/ui/separator"

const microLabel =
  "text-[0.7rem] uppercase tracking-wider text-muted-foreground"

export function LoadSettingsSection({
  settings,
  onChanged,
}: {
  settings: Record<string, string>
  onChanged: () => Promise<void>
}) {
  const [form, setForm] = useState({
    load_mode: settings.load_mode || "auto",
    algorithm: settings.algorithm || "adaptive",
    test_url: settings.test_url || "https://cms.zjzwfw.gov.cn/favicon.ico",
    timeout: settings.timeout || "10",
  })

  useEffect(() => {
    setForm({
      load_mode: settings.load_mode || "auto",
      algorithm: settings.algorithm || "adaptive",
      test_url: settings.test_url || "https://cms.zjzwfw.gov.cn/favicon.ico",
      timeout: settings.timeout || "10",
    })
  }, [settings])

  async function save() {
    const body = {
      load_mode: form.load_mode,
      algorithm: form.algorithm,
      test_url: form.test_url,
      timeout: form.timeout,
    }
    await api("/api/settings", jsonBody(body))
    toast.success("负载设置已保存")
    await onChanged()
  }

  return (
    <div className="grid gap-6">
      <Card>
        <CardHeader>
          <CardTitle>负载均衡</CardTitle>
          <CardDescription>代理选择策略和全局测试参数</CardDescription>
        </CardHeader>
        <CardContent>
          <FieldGroup>
            <div className={microLabel}>选择策略</div>
            <Field>
              <FieldLabel>负载模式</FieldLabel>
              <Select
                value={form.load_mode}
                onValueChange={(value) => setForm({ ...form, load_mode: value })}
              >
                <SelectTrigger className="w-full">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectGroup>
                    <SelectItem value="auto">自动模式</SelectItem>
                    <SelectItem value="manual">手动模式</SelectItem>
                  </SelectGroup>
                </SelectContent>
              </Select>
            </Field>
            <Field>
              <FieldLabel>自动模式算法</FieldLabel>
              <Select
                value={form.algorithm}
                onValueChange={(value) => setForm({ ...form, algorithm: value })}
              >
                <SelectTrigger className="w-full">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectGroup>
                    <SelectItem value="adaptive">自适应算法</SelectItem>
                    <SelectItem value="weighted_round_robin">加权轮询</SelectItem>
                    <SelectItem value="least_connections">最小连接数</SelectItem>
                    <SelectItem value="sticky_host">会话粘滞</SelectItem>
                  </SelectGroup>
                </SelectContent>
              </Select>
            </Field>

            <div className={microLabel}>测试参数</div>
            <Field>
              <FieldLabel>默认测试地址</FieldLabel>
              <Input
                className="font-mono"
                value={form.test_url}
                onChange={(event) =>
                  setForm({ ...form, test_url: event.target.value })
                }
              />
            </Field>
            <Field>
              <FieldLabel>默认超时（秒）</FieldLabel>
              <Input
                type="number"
                min={1}
                className="font-mono tabular-nums"
                value={form.timeout}
                onChange={(event) =>
                  setForm({ ...form, timeout: event.target.value })
                }
              />
            </Field>

            <Separator />
            <Button className="w-full" onClick={save}>
              <Save />
              保存设置
            </Button>
          </FieldGroup>
        </CardContent>
      </Card>
    </div>
  )
}
