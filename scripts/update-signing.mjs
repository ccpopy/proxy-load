import { createHash, createPrivateKey, createPublicKey, generateKeyPairSync, sign, verify } from "node:crypto"
import { createReadStream } from "node:fs"
import { mkdir, readFile, stat, writeFile } from "node:fs/promises"
import { basename, dirname, isAbsolute, relative, resolve, sep } from "node:path"
import { pathToFileURL } from "node:url"
import { releaseMode } from "./release-policy.mjs"

export function artifactKind(name) {
  const lower = name.toLowerCase()
  if (lower.endsWith(".exe")) return lower.includes("portable") ? "windows-portable" : lower.includes("setup") ? "windows-nsis" : "windows-exe"
  return Object.entries({ ".msi": "windows-msi", ".dmg": "macos-dmg", ".deb": "linux-deb", ".rpm": "linux-rpm", ".appimage": "linux-appimage" }).find(([extension]) => lower.endsWith(extension))?.[1]
}

export function loadSigningKey(env = process.env) {
  if (!env.PROXY_LOAD_UPDATE_PRIVATE_KEY || !env.PROXY_LOAD_UPDATE_PUBLIC_KEY) throw new Error("Configure the update signing private key secret and public key variable before releasing")
  const key = createPrivateKey({ key: env.PROXY_LOAD_UPDATE_PRIVATE_KEY, passphrase: env.PROXY_LOAD_UPDATE_KEY_PASSWORD })
  if (key.asymmetricKeyType !== "ed25519") throw new Error("The update signing key must be Ed25519")
  const publicKey = createPublicKey(key)
  const raw = Buffer.from(publicKey.export({ format: "jwk" }).x, "base64url")
  const pinned = Buffer.from(env.PROXY_LOAD_UPDATE_PUBLIC_KEY.trim(), "base64")
  if (pinned.length !== 32 || !raw.equals(pinned)) throw new Error("The private key does not match the public key embedded in the application")
  return key
}

export async function signArtifact(file, { version, platform, arch, key }) {
  const name = basename(file)
  const kind = artifactKind(name)
  if (!/^[\w][\w.-]{0,199}$/.test(name) || !kind || !name.includes(`_${version}_`)) throw new Error(`Unsupported or incorrectly versioned artifact: ${name}`)
  if (!/^\d+\.\d+\.\d+(?:[+-][\w.-]+)?$/.test(version)) throw new Error("Invalid release version")
  if (!kind.startsWith(`${platform}-`) || !["x86_64", "aarch64"].includes(arch)) throw new Error("Artifact platform/architecture mismatch")
  const tokens = name.toLowerCase().replaceAll("x86_64", "x64").replaceAll("amd64", "x64").replaceAll("arm64", "aarch64").split(/[^a-z0-9]+/)
  const expected = arch === "x86_64" ? "x64" : arch
  if (!tokens.includes(expected) || tokens.some(token => ["x64", "aarch64", "i386", "i686", "x86"].includes(token) && token !== expected)) throw new Error("Artifact filename architecture mismatch")
  const metadata = await stat(file)
  if (!metadata.isFile() || metadata.size <= 0 || metadata.size > 1024 ** 3) throw new Error("Invalid artifact size")
  const hash = createHash("sha256")
  let size = 0
  for await (const chunk of createReadStream(file)) { hash.update(chunk); size += chunk.length }
  if (size !== metadata.size) throw new Error("Artifact changed while signing")
  const payload = Buffer.from(JSON.stringify({ schema: 1, app: "ccpopy/proxy-load", version, platform, arch, kind, file_name: name, size, sha256: hash.digest("base64") }))
  const signature = sign(null, payload, key)
  if (!verify(null, payload, createPublicKey(key), signature)) throw new Error("Signing self-check failed")
  const envelope = JSON.stringify({ payload: payload.toString("base64"), signature: signature.toString("base64") })
  await writeFile(`${file}.manifest.json`, `${envelope}\n`)
  return `${file}.manifest.json`
}

async function main() {
  const [command, ...args] = process.argv.slice(2)
  if (command === "generate") {
    if (args.length !== 1) throw new Error("Usage: node scripts/update-signing.mjs generate <private-key-path-outside-repository>")
    const target = resolve(args[0])
    const inside = relative(process.cwd(), target)
    if (!isAbsolute(inside) && inside !== ".." && !inside.startsWith(`..${sep}`)) throw new Error("Store signing private keys outside the repository")
    const { privateKey, publicKey } = generateKeyPairSync("ed25519")
    await mkdir(dirname(target), { recursive: true, mode: 0o700 })
    await writeFile(target, privateKey.export({ type: "pkcs8", format: "pem" }), { flag: "wx", mode: 0o600 })
    await writeFile(`${target}.pub`, Buffer.from(publicKey.export({ format: "jwk" }).x, "base64url").toString("base64") + "\n", { flag: "wx" })
    console.log("Signing key files created. Back up the private key securely; never commit it.")
    return
  }
  const mode = releaseMode()
  const key = mode === "signed" ? loadSigningKey() : null
  const { version } = JSON.parse(await readFile("package.json", "utf8"))
  if (process.env.RELEASE_TAG !== `v${version}`) throw new Error("Release tag must match the checked-out package version")
  if (command === "check") { console.log(mode === "signed" ? "Update signing configuration verified" : "Explicit manual distribution: no automatic installation manifest will be produced"); return }
  if (!key) throw new Error("Manual distribution cannot sign update manifests")
  if (command !== "sign" || args.length < 3) throw new Error("Usage: update-signing.mjs sign <platform> <arch> <artifact>...")
  const [platform, arch, ...files] = args
  for (const file of files) await signArtifact(file, { version, platform, arch, key })
  console.log(`Signed ${files.length} update artifact(s)`)
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  main().catch(error => { console.error(error.message); process.exitCode = 1 })
}
