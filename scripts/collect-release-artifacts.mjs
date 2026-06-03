import { copyFile, mkdir, readFile, readdir, stat } from "node:fs/promises"
import { basename, extname, join, resolve } from "node:path"

const root = resolve(process.cwd())
const bundleDir = join(root, "src-tauri", "target", "release", "bundle")
const portableExe = join(root, "src-tauri", "target", "release", "zwfw-load-tauri.exe")
const outputDir = join(root, "release")
const allowedExtensions = new Set([".exe", ".msi", ".dmg", ".deb", ".rpm", ".appimage"])

await mkdir(outputDir, { recursive: true })

const artifacts = await collectArtifacts(bundleDir)
for (const artifact of artifacts) {
  await copyFile(artifact, join(outputDir, basename(artifact)))
}

if (await exists(portableExe)) {
  const { version } = JSON.parse(await readFile(join(root, "package.json"), "utf8"))
  const arch = process.arch === "x64" ? "x64" : process.arch
  artifacts.push(portableExe)
  await copyFile(portableExe, join(outputDir, `zwfw-load_${version}_${arch}-portable.exe`))
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

async function exists(path) {
  try {
    await stat(path)
    return true
  } catch (error) {
    if (error?.code === "ENOENT") return false
    throw error
  }
}
