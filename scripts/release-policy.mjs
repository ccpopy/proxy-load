import { readFile, writeFile } from "node:fs/promises"
import { resolve } from "node:path"
import { pathToFileURL } from "node:url"

export function releaseMode(env = process.env) {
  const mode = env.RELEASE_MODE || "signed"
  if (!["signed", "manual"].includes(mode)) throw new Error("RELEASE_MODE must explicitly be signed or manual")
  return mode
}

export function releaseNotes(notes, mode) {
  releaseMode({ RELEASE_MODE: mode })
  const notice = mode === "manual"
    ? "此版本为手动分发版本，仅供从官方 Releases 下载后手动安装；不提供未验签的应用内安装。"
    : "此版本的应用内更新包使用本项目 Ed25519 密钥签名。"
  return `${notes.trim()}\n\n## 安装方式\n\n${notice}\n\nmacOS 产物采用 ad-hoc 签名，不是 Apple Developer ID 签名或公证；系统仍可能要求在隐私与安全性中手动允许。\n\n<!-- proxy-load-release-mode: ${mode} -->\n`
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  const [command, input, output] = process.argv.slice(2)
  if (command !== "notes" || !input || !output) throw new Error("Usage: release-policy.mjs notes <input> <output>")
  await writeFile(output, releaseNotes(await readFile(input,"utf8"), releaseMode()))
}
