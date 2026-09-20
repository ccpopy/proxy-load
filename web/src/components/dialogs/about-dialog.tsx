import { type ReactNode } from "react"
import { Download, Loader2, Network, RefreshCw } from "lucide-react"

import { type ServiceInfo } from "@/lib/api"
import { updateAction } from "@/lib/update-action"
import { useUpdateMirrorSettings } from "@/lib/use-update-mirror-settings"
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
import { UpdateMirrorAddressField } from "@/components/dialogs/update-mirror-address-field"

export function AboutDialog({
  open,
  onOpenChange,
  version,
  serviceInfo,
  updateInfo,
  checking,
  installing,
  useMirror,
  autoCheckUpdates,
  onUseMirrorChange,
  onMirrorUrlSaved,
  onAutoCheckUpdatesChange,
  onCheckUpdates,
  onInstallUpdate,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  version: VersionInfo | null
  serviceInfo: ServiceInfo | null
  updateInfo: UpdateInfo | null
  checking: boolean
  installing: boolean
  useMirror: boolean
  autoCheckUpdates: boolean
  onUseMirrorChange: (value: boolean) => void
  onMirrorUrlSaved: () => void
  onAutoCheckUpdatesChange: (value: boolean) => void
  onCheckUpdates: () => void
  onInstallUpdate: () => void
}) {
  const mirror = useUpdateMirrorSettings(open, onMirrorUrlSaved)
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[calc(100dvh-2rem)] overflow-y-auto sm:max-w-xl">
        <DialogHeader>
          <DialogTitle>关于与更新</DialogTitle>
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
              <Meta
                label="代理监听"
                value={`${serviceInfo.proxy_host}:${serviceInfo.proxy_port}`}
              />
            )}
          </div>

          <div className="grid gap-4 rounded-md border bg-card/40 p-4">
            <Field orientation="horizontal">
              <FieldContent>
                <FieldTitle>国内加速</FieldTitle>
                <FieldDescription>
                  开启后通过下方已保存的地址检查更新与下载，关闭时直连 GitHub
                </FieldDescription>
              </FieldContent>
              <Switch
                aria-label="启用国内加速"
                checked={useMirror}
                onCheckedChange={onUseMirrorChange}
                disabled={mirror.saving || installing}
              />
            </Field>
            <UpdateMirrorAddressField state={mirror} busy={checking || installing} />
          </div>

          <Field orientation="horizontal" className="rounded-md border bg-card/40 p-4">
            <FieldContent>
              <FieldTitle>自动检查更新</FieldTitle>
              <FieldDescription>
                默认关闭；开启后应用启动和运行期间会自动检查，发现新版本时弹出通知
              </FieldDescription>
            </FieldContent>
            <Switch
              aria-label="自动检查更新"
              checked={autoCheckUpdates}
              onCheckedChange={onAutoCheckUpdatesChange}
            />
          </Field>
        </div>

        {updateInfo?.hasUpdate && updateInfo.manualReason && (
          <p role="status" className="rounded-md border bg-muted/40 p-3 text-sm text-muted-foreground">
            {updateInfo.manualReason}
          </p>
        )}
        <DialogFooter>
          <Button variant="outline" onClick={onCheckUpdates} disabled={checking || installing || mirror.saving}>
            {checking ? (
              <Loader2 data-icon="inline-start" className="animate-spin" />
            ) : (
              <RefreshCw data-icon="inline-start" />
            )}
            检查更新
          </Button>
          <Button
            onClick={onInstallUpdate}
            disabled={!updateInfo?.latest || installing || checking || mirror.saving}
          >
            {installing ? (
              <Loader2 data-icon="inline-start" className="animate-spin" />
            ) : (
              <Download data-icon="inline-start" />
            )}
            {updateAction(updateInfo) === "manual" ? "前往官方发布页" : updateInfo?.installMode === "portable" ? "下载并重启" : "安装更新"}
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
