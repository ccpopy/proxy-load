import { test } from "node:test"
import assert from "node:assert/strict"
import { createHash, generateKeyPairSync, verify } from "node:crypto"
import { mkdtemp, readFile, rm, rmdir, writeFile } from "node:fs/promises"
import { join } from "node:path"
import { tmpdir } from "node:os"
import { artifactKind, loadSigningKey, signArtifact } from "./update-signing.mjs"
import { releaseMode, releaseNotes } from "./release-policy.mjs"
import { spawnSync } from "node:child_process"

test("release configuration CLI accepts keyless manual mode but rejects keyless signed mode", async () => {
  const {version} = JSON.parse(await readFile("package.json","utf8"))
  const env = {...process.env,RELEASE_TAG:`v${version}`,PROXY_LOAD_UPDATE_PRIVATE_KEY:"",PROXY_LOAD_UPDATE_PUBLIC_KEY:""}
  const run = mode => spawnSync(process.execPath,["scripts/update-signing.mjs","check"],{env:{...env,RELEASE_MODE:mode},encoding:"utf8",windowsHide:true})
  assert.equal(run("manual").status,0)
  assert.notEqual(run("signed").status,0)
  assert.notEqual(run("auto").status,0)
})

test("manual publishing is explicit and cannot silently replace signed mode", () => {
  assert.equal(releaseMode({}),"signed")
  assert.throws(()=>loadSigningKey({RELEASE_MODE:"signed"}))
  assert.equal(releaseMode({RELEASE_MODE:"manual"}),"manual")
  assert.throws(()=>releaseMode({RELEASE_MODE:"auto"}))
  assert.match(releaseNotes("changes","manual"),/仅供.*手动安装/)
  assert.match(releaseNotes("changes","signed"),/Ed25519/)
})

test("release signing binds the exact streamed bytes and rejects mismatched trust roots", async () => {
  const { privateKey, publicKey } = generateKeyPairSync("ed25519")
  const env = { PROXY_LOAD_UPDATE_PRIVATE_KEY: privateKey.export({type: "pkcs8", format:"pem"}), PROXY_LOAD_UPDATE_PUBLIC_KEY: Buffer.from(publicKey.export({format:"jwk"}).x,"base64url").toString("base64") }
  const key = loadSigningKey(env)
  assert.throws(() => loadSigningKey({ ...env, PROXY_LOAD_UPDATE_PUBLIC_KEY: Buffer.alloc(32).toString("base64") }))
  assert.throws(() => loadSigningKey({}))
  const dir = await mkdtemp(join(tmpdir(), "proxy-load-sign-test-"))
  try {
    const file = join(dir, "proxy-load_26.9.20_x64-portable.exe")
    const bytes = Buffer.alloc(100_000, 7)
    await writeFile(file, bytes)
    const options = {version:"26.9.20",platform:"windows",arch:"x86_64",key}
    const signed = JSON.parse(await readFile(await signArtifact(file, options), "utf8"))
    const payload = Buffer.from(signed.payload, "base64")
    assert(verify(null, payload, publicKey, Buffer.from(signed.signature, "base64")))
    assert.equal(JSON.parse(payload).sha256, createHash("sha256").update(bytes).digest("base64"))
    payload[0] ^= 1
    assert(!verify(null, payload, publicKey, Buffer.from(signed.signature, "base64")))
    await assert.rejects(signArtifact(file, {...options,arch:"aarch64"}))
    await assert.rejects(signArtifact(file, {...options,platform:"linux"}))
    await assert.rejects(signArtifact(file, {...options,version:"99.1.1"}))
  } finally {
    for (const name of ["proxy-load_26.9.20_x64-portable.exe", "proxy-load_26.9.20_x64-portable.exe.manifest.json"]) await rm(join(dir,name),{force:true})
    await rmdir(dir)
  }
})

test("artifact kinds agree with the custom updater", () => {
  for (const [name, kind] of [["x-setup.exe","windows-nsis"],["x-portable.exe","windows-portable"],["x.msi","windows-msi"],["x.dmg","macos-dmg"],["x.AppImage","linux-appimage"]]) assert.equal(artifactKind(name),kind)
  assert.equal(artifactKind("x.exe.manifest.json"),undefined)
})
