import type { UpdateInfo } from "../types"

export function updateAction(info: Pick<UpdateInfo, "latest" | "automaticInstallAvailable"> | null) {
  if (!info?.latest) return "none"
  return info.automaticInstallAvailable === true ? "install" : "manual"
}
