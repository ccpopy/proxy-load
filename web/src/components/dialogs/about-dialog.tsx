import { useState, type ReactNode } from "react"
import { Download, Loader2, Network, RefreshCw } from "lucide-react"
import { toast } from "sonner"

import { command, commandErrorMessage, type ServiceInfo } from "@/lib/api"
import type { UpdateInfo, VersionInfo } from "@/types"
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
  FieldTitle,
} from "@/components/ui/field"
import { Switch } from "@/components/ui/switch"

const MIRROR_STORAGE_KEY = "proxy-load-update-mirror"

export function AboutDialog({
  open,
  onOpenChange,
  version,
  serviceInfo,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  version: VersionInfo | null
  serviceInfo: ServiceInfo | null
}) {
  const [updateInfo, setUpdateInfo] = useState<UpdateInfo | null>(null)
  const [checking, setChecking] = useState(false)
  const [installing, setInstalling] = useState(false)
  const [useMirror, setUseMirror] = useState(
    () => localStorage.getItem(MIRROR_STORAGE_KEY) === "1"
  )

  function toggleMirror(value: boolean) {
    setUseMirror(value)
    localStorage.setItem(MIRROR_STORAGE_KEY, value ? "1" : "0")
    setUpdateInfo(null)
  }

  async function handleCheck() {
    setChecking(true)
    try {
      const info = await command<UpdateInfo>("check_for_updates", { useMirror })
      setUpdateInfo(info)
      if (info.hasUpdate) {
        toast.success(`发现新版本 ${info.latest?.version}`)
      } else {
        toast.info("当前已是最新版本")
      }
    } catch (error) {
      toast.error(commandErrorMessage(error, "检查更新失败"))
    } finally {
      setChecking(false)
    }
  }

  async function handleInstall() {
    if (!updateInfo?.latest) return

    setInstalling(true)
    try {
      const result = await command<{ message?: string }>("install_update", {
        artifactPath: updateInfo.latest.path,
        useMirror,
      })
      toast.success(result.message ?? "已启动更新安装程序")
      onOpenChange(false)
    } catch (error) {
      toast.error(commandErrorMessage(error, "安装更新失败"))
    } finally {
      setInstalling(false)
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-xl">
        <DialogHeader>
          <DialogTitle>关于</DialogTitle>
          <DialogDescription>应用版本与运行信息</DialogDescription>
        </DialogHeader>

        <div className="flex flex-col gap-4">
          <div className="flex items-center gap-4 rounded-md border bg-card/40 p-4">
            <div className="flex size-11 shrink-0 items-center justify-center rounded-md bg-primary text-primary-foreground">
              <Network className="size-5" />
            </div>
            <div className="min-w-0 flex-1">
              <div className="font-semibold leading-tight">代理管理系统</div>
              <div className="truncate text-xs text-muted-foreground">
                Proxy Manager · Rust · Tauri · shadcn/ui
              </div>
            </div>
            <div className="flex shrink-0 flex-col items-end leading-none">
              <span className="text-[0.65rem] uppercase tracking-wider text-muted-foreground/70">
                版本
              </span>
              <span className="mt-1 font-mono text-2xl font-semibold tabular-nums tracking-tight text-primary">
                v{version?.version ?? "—"}
              </span>
            </div>
          </div>

          <div className="grid gap-x-6 gap-y-2 sm:grid-cols-2">
            {version?.runtime && <Meta label="技术栈" value={version.runtime} />}
            {version?.platform && (
              <Meta label="系统" value={version.platform} />
            )}
            {version?.arch && <Meta label="架构" value={version.arch} />}
            {serviceInfo && (
              <Meta label="代理端口" value={serviceInfo.proxy_port} />
            )}
          </div>

          <Field orientation="horizontal" className="rounded-md border bg-card/40 p-4">
            <FieldContent>
              <FieldTitle>国内加速</FieldTitle>
              <FieldDescription>
                通过 gh.lessdo.top 镜像检查更新与下载，适用于无法直连 GitHub 的网络
              </FieldDescription>
            </FieldContent>
            <Switch checked={useMirror} onCheckedChange={toggleMirror} />
          </Field>
        </div>

        <DialogFooter>
          <Button variant="outline" onClick={handleCheck} disabled={checking}>
            {checking ? <Loader2 className="animate-spin" /> : <RefreshCw />}
            检查更新
          </Button>
          <Button
            onClick={handleInstall}
            disabled={!updateInfo?.latest || installing}
          >
            {installing ? <Loader2 className="animate-spin" /> : <Download />}
            {updateInfo?.installMode === "portable" ? "下载并重启" : "安装更新"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

function Meta({ label, value }: { label: string; value: ReactNode }) {
  return (
    <div className="flex items-baseline justify-between gap-3 border-b border-border/40 pb-1.5">
      <span className="text-[0.7rem] uppercase tracking-wider text-muted-foreground/70">
        {label}
      </span>
      <span className="truncate font-mono text-sm tabular-nums text-foreground/90">
        {value}
      </span>
    </div>
  )
}
