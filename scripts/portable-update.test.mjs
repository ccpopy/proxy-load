// Explicit Windows acceptance: PORTABLE_UPDATE_HELPER points to a release application binary.
import { test } from "node:test"
import assert from "node:assert/strict"
import { execFileSync, spawn } from "node:child_process"
import { createHash } from "node:crypto"
import { mkdtemp, mkdir, copyFile, readFile, writeFile, readdir, unlink, rmdir, realpath } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join, resolve } from "node:path"
import { fileURLToPath } from "node:url"

async function assertSameDirectory(actual, expected) {
  // Windows TEMP may use an 8.3 alias, while current_exe returns the long path.
  assert.equal(await realpath(actual), await realpath(expected))
}

test("release helper: two portable upgrades preserve stable entry/data; invalid executable rolls back", { skip: process.platform !== "win32" || !process.env.PORTABLE_UPDATE_HELPER }, async () => {
  const root = await mkdtemp(join(tmpdir(), "proxy portable 中文 "))
  const fixture = join(root, "fixture.exe")
  const entry = join(root, "原快捷方式 portable.exe")
  // Only our mkdtemp tree; no recursive removal of arbitrary caller paths.
  async function cleanup(dir) { for (const item of await readdir(dir, { withFileTypes: true })) { const p = join(dir,item.name); if (item.isDirectory()) { await cleanup(p); await rmdir(p) } else await unlink(p) } }
  try {
    execFileSync("rustc", ["--edition=2021", "-O", fileURLToPath(new URL("../src-tauri/tests/fixtures/portable_app.rs", import.meta.url)), "-o", fixture])
    await copyFile(fixture, entry)
    await mkdir(join(root,"data")); await writeFile(join(root,"data/proxy.db"), "existing proxy/group/DNS/auth/log data")
    const originalData = await readFile(join(root,"data/proxy.db"))
    const run = async (version, bytes, dataDir) => {
      const session = join(root,".proxy-load-updates",version); await mkdir(session,{recursive:true})
      const staged = join(session,"next.exe"); await writeFile(staged,bytes)
      // Wait for an actual parent process, not an assumed unused PID.
      const parent = spawn(process.execPath,["-e","setTimeout(()=>{},500)"],{windowsHide:true,stdio:"ignore"})
      const env = { ...process.env }; delete env.DATA_DIR
      if (dataDir) env.DATA_DIR = dataDir
      let succeeded = true
      try { execFileSync(resolve(process.env.PORTABLE_UPDATE_HELPER),["--proxy-load-update-helper","--parent-pid",String(parent.pid),"--installer-path",staged,"--installer-kind","windows-portable","--install-dir",root,"--launch-path",entry,"--verified-sha256",createHash("sha256").update(bytes).digest("base64")],{env,windowsHide:true,timeout:40000,stdio:"pipe"}) } catch { succeeded=false }
      await new Promise(resolve=>setTimeout(resolve,2200))
      return succeeded
    }
    const bytes = await readFile(fixture)
    for (const version of ["v2","v3"]) {
      assert(await run(version,bytes))
      const paths = (await readFile(join(root,"fixture-paths.txt"),"utf8")).split("\n")
      await assertSameDirectory(paths[0],root); await assertSameDirectory(paths[1],join(root,"data"))
      assert.deepEqual(await readFile(join(root,"data/proxy.db")),originalData)
      assert.deepEqual(await readFile(entry),bytes)
    }
    const external = join(root,"explicit DATA_DIR"); await mkdir(external); await writeFile(join(external,"proxy.db"),"external configuration")
    assert(await run("v4",bytes,external))
    await assertSameDirectory((await readFile(join(root,"fixture-paths.txt"),"utf8")).split("\n")[1],external)
    assert.equal(await run("broken",Buffer.from("not an executable")),false)
    assert.deepEqual(await readFile(entry),bytes)
    assert.deepEqual(await readFile(join(root,"data/proxy.db")),originalData)
  } finally { await cleanup(root); await rmdir(root) }
})
