import { copyFile, mkdir, readFile, readdir, rm, stat } from "node:fs/promises"
import { basename, extname, join, resolve } from "node:path"

const root = resolve(process.cwd())
const bundleDir = join(root, "src-tauri", "target", "release", "bundle")
const portableExe = join(root, "src-tauri", "target", "release", "proxy-load-tauri.exe")
const outputDir = join(root, "release")
const allowedExtensions = new Set([".exe", ".msi", ".dmg", ".deb", ".rpm", ".appimage"])
const { version } = JSON.parse(await readFile(join(root, "package.json"), "utf8"))
const tauriConfig = JSON.parse(await readFile(join(root, "src-tauri", "tauri.conf.json"), "utf8"))
const bundleVersion = tauriConfig.version

await mkdir(outputDir, { recursive: true })
await removeExistingReleaseArtifacts(outputDir)

const artifacts = (await collectArtifacts(bundleDir)).filter((artifact) =>
  basename(artifact).includes(`_${bundleVersion}_`)
)
for (const artifact of artifacts) {
  await copyFile(artifact, join(outputDir, releaseFileName(basename(artifact))))
}

if (await exists(portableExe)) {
  const arch = process.arch === "x64" ? "x64" : process.arch
  artifacts.push(portableExe)
  await copyFile(portableExe, join(outputDir, `proxy-load_${version}_${arch}-portable.exe`))
}

console.log(`Copied ${artifacts.length} release artifact(s) to ${outputDir}`)

async function collectArtifacts(dir) {
  if (!(await exists(dir))) {
    throw new Error(`Tauri bundle directory does not exist: ${dir}`)
  }

  const results = []
  for (const entry of await readdir(dir)) {
    const path = join(dir, entry)
    const metadata = await stat(path)
    if (metadata.isDirectory()) {
      results.push(...(await collectArtifacts(path)))
      continue
    }

    const extension = extname(path).toLowerCase()
    if (allowedExtensions.has(extension)) {
      results.push(path)
    }
  }
  return results
}

async function removeExistingReleaseArtifacts(dir) {
  for (const entry of await readdir(dir)) {
    const path = join(dir, entry)
    const metadata = await stat(path)
    if (!metadata.isFile()) {
      continue
    }
    if (entry.startsWith("proxy-load_") && allowedExtensions.has(extname(entry).toLowerCase())) {
      await rm(path)
    }
  }
}

function releaseFileName(fileName) {
  return fileName.replace(`_${bundleVersion}_`, `_${version}_`)
}

async function exists(path) {
  try {
    await stat(path)
    return true
  } catch (error) {
    if (error?.code === "ENOENT") return false
    throw error
  }
}
