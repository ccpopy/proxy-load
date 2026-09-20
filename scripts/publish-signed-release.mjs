// CI-only: sign local build outputs before uploading them; never sign mirror downloads.
import { execFileSync } from "node:child_process"
import { copyFile, mkdtemp, readFile, stat, writeFile } from "node:fs/promises"
import { basename, extname, join } from "node:path"
import { artifactKind, loadSigningKey, signArtifact } from "./update-signing.mjs"
import { releaseMode, releaseNotes } from "./release-policy.mjs"

const { version } = JSON.parse(await readFile("package.json", "utf8"))
const tag = process.env.RELEASE_TAG
if (tag !== `v${version}`) throw new Error("Release tag/version mismatch")
const mode = releaseMode()
const key = mode === "signed" ? loadSigningKey() : null
const label = process.env.BUILD_LABEL
const platform = label?.startsWith("windows-") ? "windows" : label?.startsWith("linux-") ? "linux" : label?.startsWith("macos-") ? "macos" : null
if (!platform) throw new Error("Unknown build platform")
const arch = label.endsWith("aarch64") ? "aarch64" : "x86_64"
const paths = JSON.parse(process.env.TAURI_ARTIFACT_PATHS || "[]")
if (!Array.isArray(paths)) throw new Error("Missing build artifact paths")
const prepared = []
const directory = await mkdtemp(join(process.env.RUNNER_TEMP, "proxy-load-signed-"))
for (const path of paths) {
  const kind = artifactKind(basename(path))
  if (!kind || !(await stat(path)).isFile()) continue
  const suffix = kind === "windows-nsis" ? "-setup" : ""
  const name = `proxy-load_${version}_${platform}_${arch === "x86_64" ? "x64" : arch}${suffix}${extname(path)}`
  const target = join(directory, name)
  await copyFile(path, target)
  prepared.push(target)
}
if (platform === "windows") {
  const target = join(directory, `proxy-load_${version}_x64-portable.exe`)
  await copyFile("src-tauri/target/release/proxy-load-tauri.exe", target)
  prepared.push(target)
}
if (prepared.length === 0) throw new Error("No installable release artifacts produced")
const uploads = []
for (const file of prepared) {
  uploads.push(file)
  if (key) uploads.push(await signArtifact(file, { version, platform, arch, key }))
}
const notes = join(directory,"release-notes.md")
await writeFile(notes,releaseNotes(await readFile("RELEASE_NOTES.md","utf8"),mode))
const gh = (...args) => execFileSync("gh", args, { stdio: ["ignore", "pipe", "pipe"] })
try { gh("release", "view", tag) } catch {
  try { gh("release", "create", tag, "--verify-tag", "--draft", "--title", `proxy-load ${tag}`, "--notes-file", notes) }
  catch (error) { try { gh("release", "view", tag) } catch { throw error } }
}
const existing = JSON.parse(gh("release","view",tag,"--json","body").toString())
if (!existing.body.includes(`<!-- proxy-load-release-mode: ${mode} -->`)) throw new Error("Release mode differs or is unknown; use a new version/tag instead of mixing signed and manual artifacts")
gh("release", "upload", tag, ...uploads, "--clobber")
console.log(`Uploaded ${prepared.length} ${mode} update artifact(s) for ${label}`)
