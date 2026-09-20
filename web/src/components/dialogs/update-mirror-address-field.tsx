import { Loader2, RotateCcw, Save } from "lucide-react"

import { type useUpdateMirrorSettings } from "@/lib/use-update-mirror-settings"
import { Button } from "@/components/ui/button"
import { Field, FieldDescription, FieldError, FieldLabel } from "@/components/ui/field"
import { Input } from "@/components/ui/input"

export function UpdateMirrorAddressField({ state, busy }: {
  state: ReturnType<typeof useUpdateMirrorSettings>
  busy: boolean
}) {
  const disabled = busy || state.loading || state.saving || !state.settings || !!state.loadError

  return (
    <form
      noValidate
      aria-busy={state.loading || state.saving}
      className="grid gap-3 border-t border-border/60 pt-4"
      onSubmit={(event) => {
        event.preventDefault()
        if (!disabled) void state.save()
      }}
    >
      <Field>
        <FieldLabel htmlFor="update-mirror-url">加速地址</FieldLabel>
        <Input
          id="update-mirror-url"
          type="url"
          value={state.draft}
          onChange={(event) => state.edit(event.target.value)}
          placeholder={state.loading ? "正在加载…" : "https://加速服务地址"}
          className="font-mono text-sm"
          autoComplete="off"
          spellCheck={false}
          disabled={disabled}
          aria-invalid={!!state.error}
          aria-describedby={state.error ? "update-mirror-help update-mirror-error" : "update-mirror-help"}
        />
        <FieldDescription id="update-mirror-help" className="text-xs leading-relaxed">
          保存时自动测试连通性，测试通过后生效。
        </FieldDescription>
        {state.error && <FieldError id="update-mirror-error" role="alert">{state.error}</FieldError>}
        {state.loadError && (
          <FieldError role="alert">
            {state.loadError}
            <Button type="button" variant="link" size="sm" onClick={state.retryLoad}>重新加载</Button>
          </FieldError>
        )}
        {state.notice && <p role="status" className="text-xs leading-relaxed text-muted-foreground">{state.notice}</p>}
      </Field>
      <div className="flex flex-wrap items-center justify-end gap-2">
        <Button type="button" variant="outline" size="sm" onClick={state.restoreDefault} disabled={disabled}>
          <RotateCcw />恢复默认
        </Button>
        <Button type="submit" size="sm" disabled={disabled}>
          {state.saving ? <Loader2 className="animate-spin" /> : <Save />}
          {state.saving ? "测试中…" : "保存"}
        </Button>
      </div>
    </form>
  )
}
