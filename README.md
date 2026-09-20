# proxy-load

![Tauri](https://img.shields.io/badge/Tauri-v2-24C8DB?logo=tauri&logoColor=white)
![Rust](https://img.shields.io/badge/Rust-backend-000000?logo=rust&logoColor=white)
![React](https://img.shields.io/badge/React-19-61DAFB?logo=react&logoColor=111111)
![TypeScript](https://img.shields.io/badge/TypeScript-5-3178C6?logo=typescript&logoColor=white)
![Vite](https://img.shields.io/badge/Vite-7-646CFF?logo=vite&logoColor=white)
![shadcn/ui](https://img.shields.io/badge/shadcn%2Fui-Teal-111827)
![Recharts](https://img.shields.io/badge/Recharts-dashboard-009689)

代理负载均衡管理系统。当前版本是基于 Tauri v2 的桌面应用：后端核心由 Rust 实现，前端由 React、Vite、TypeScript、shadcn/ui、lucide-react 和 Recharts 构建。

## 项目现状

- 桌面壳：Tauri v2
- 后端核心：Rust、Tokio、rusqlite、reqwest
- 前端界面：React、Vite、TypeScript、shadcn/ui、lucide-react、Recharts
- 数据存储：SQLite，本地持久化代理、分组、DNS 映射、设置和请求日志
- 应用通信：前端通过 Tauri IPC 命令和事件访问 Rust 后端，不再启动独立管理 API 服务
- 代理监听：默认 `127.0.0.1:5678`，只有显式开启“允许局域网连接”后才监听 `0.0.0.0`

正式应用只对外启动代理服务端口。开发时 Tauri 会拉起 Vite 页面服务 `1420` 供应用窗口加载前端资源，但页面数据仍通过 Tauri 应用内通信获取，不支持单独用普通浏览器直连管理 API 调试。

## 功能

- 代理配置管理：新增、编辑、删除、启用、停用和连通性测试。
- DNS 映射：按域名覆盖解析结果。
- 代理分组：优先使用最具体的域名规则，未命中时使用默认分组；没有默认分组时使用全局已启用代理。
- 负载设置：支持 `adaptive`、`round_robin`、`least_connections`、`sticky_host`。
- 混合入站：同一端口支持 SOCKS5、HTTP 和用于 HTTPS 目标的 HTTP CONNECT。
- 可选入站认证：默认关闭；开启后 SOCKS5 和 HTTP/HTTPS CONNECT 共用一组持久化用户名密码。
- 自适应测活：默认启动时探测已启用代理，运行期间每三分钟探测；通用代理可用真实流量替代心跳，专用业务节点必须独立通过业务测活才能承接新连接。
- 高级配置：监听地址、入站认证、测活、日志保留、熔断器和快速失败等真实运行参数。
- 系统状态：请求趋势、代理建连耗时、代理使用排行、目标资源排行。
- 流量日志：分页查看请求日志，支持清空日志。
- 实时刷新：通过 Tauri 事件推送代理、DNS、分组和请求日志变化。
- 检查更新：开发环境禁止检查更新；生产环境从 GitHub Releases 获取更新包，并按应用所在目录更新。
- 主题：shadcn Teal 色系，支持亮色和暗色切换。

## 负载选路与故障切换

- `adaptive` 使用近期建连成功率和延迟作为质量评分，再按 `质量评分 / (当前连接数 + 1)` 排序；正在建连的请求也计入连接数。五分钟前的运行时样本会过期。
- 代理 TCP 连接、认证或明确的协议故障累计到全局熔断；代理已连接后的目标拒绝、不可达或等待目标响应超时，仅累计到该代理到该目标的熔断。目标按原始域名、实际连接地址和端口区分，不会因为 GitHub 不通而降低同一代理访问政务网的全局评分。
- 两类熔断分别计数，共用高级配置中的失败阈值和冷却时长；冷却结束后只允许一个半开尝试，成功才恢复。测试地址的测活成功不会清除其他目标的熔断。
- 每次重试重新比较当前负载，只尝试匹配分组内的已启用代理，同一请求不会重复尝试同一个代理。未匹配域名规则时使用默认分组；没有默认分组才使用全部已启用代理。分组并不是域名访问白名单。
- 新配置的快速失败默认为最多 `3` 次、单次 `5` 秒、总计 `15` 秒，失败后不再额外等待 `300ms`。已有数据库中的超时值保持不变，可在高级配置中按需调整单次和总超时。
- 自动切换只发生在建连阶段、业务数据转发前。已建立的 TCP/TLS 隧道不能迁移，转发过的 POST 等业务请求不会自动重放；仍可能感受到超时等待，不能保证所有网页访问完全无感。
- 流量日志中的建连耗时保留整个请求的等待时间；自适应评分只使用最终选中代理自身的建连耗时，不把之前候选的失败等待算到成功代理头上。

## 目录结构

```text
proxy-load/
├── release/                   # 本地打包产物收集目录，仓库仅保留 .gitkeep
├── scripts/
│   └── collect-release-artifacts.mjs
├── src-tauri/                 # Tauri 和 Rust 后端
│   ├── src/
│   │   ├── commands.rs        # Tauri IPC 命令、事件和更新检查
│   │   ├── database.rs        # SQLite schema、迁移和数据访问
│   │   ├── proxy.rs           # 代理服务、入站认证、负载均衡和熔断器
│   │   ├── proxy_tester.rs    # 代理连通性测试
│   │   ├── state.rs           # 应用状态和代理服务启动参数
│   │   └── version.rs         # 版本信息
│   ├── Cargo.toml
│   └── tauri.conf.json
├── web/                       # React 前端
│   ├── src/
│   │   ├── App.tsx            # 主界面和业务交互
│   │   ├── components/ui/     # shadcn/ui 组件
│   │   ├── lib/api.ts         # Tauri 命令和事件桥接
│   │   ├── styles.css         # Tailwind v4 主题变量
│   │   └── types.ts           # 前端类型定义
│   └── vite.config.ts
├── .github/workflows/         # GitHub Actions 发布工作流
├── package.json
└── README.md
```

## 环境要求

- Node.js `>= 20`
- Rust stable
- Windows：需要 WebView2 Runtime，通常 Windows 10/11 已内置或可由 Tauri 安装流程处理。
- macOS：需要 Xcode Command Line Tools。
- Linux：构建 Tauri 需要 WebKitGTK 等系统依赖。

Ubuntu 22.04 示例：

```bash
sudo apt-get update
sudo apt-get install -y libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev patchelf
```

## 安装依赖

```bash
npm install
```

CI 环境建议使用：

```bash
npm ci
```

## 开发

启动 Tauri 桌面应用：

```bash
npm start
```

等价于：

```bash
npm run tauri:dev
```

`npm run dev` 只启动 Vite 前端服务，不能作为当前版本的独立调试入口。需要调试功能页面时，请直接启动 Tauri 应用窗口。

## 构建

只构建前端：

```bash
npm run build:web
```

检查 Rust 后端：

```bash
cargo check --manifest-path src-tauri/Cargo.toml
```

构建桌面安装包并收集到根目录 `release`：

```bash
npm run tauri:build
```

Tauri 原始产物仍会保留在：

```text
src-tauri/target/release/bundle/
```

`scripts/collect-release-artifacts.mjs` 会把 `.exe`、`.msi`、`.dmg`、`.deb`、`.rpm` 和 `.AppImage` 复制到：

```text
release/
```

Windows 本地构建还会额外复制一个可直接运行的便携 exe：

```text
release/proxy-load_26.9.2003_x64-portable.exe
```

这个文件主要用于本机验证，可以直接双击运行；正式更新安装仍建议使用 setup 或 msi 安装包。

## 三平台安装和使用

从 GitHub Releases 下载当前版本对应平台的产物。

Windows：

- 便携运行：下载 `proxy-load_26.9.2003_x64-portable.exe`，放到目标目录后直接双击运行。
- 安装运行：下载 Windows x64 的 `setup.exe` 或 `.msi` 安装包，按安装向导完成安装。GitHub Release 文件名会使用 `proxy-load_26.9.2003_windows_*` 前缀。
- 启动后应用会监听默认代理端口 `5678`，浏览器或系统代理可配置为 `SOCKS5 127.0.0.1:5678` 或 `HTTP 127.0.0.1:5678`。

macOS：

- Intel 芯片下载 `proxy-load_26.9.2003_macos_x64.dmg`。
- Apple Silicon 芯片下载 `proxy-load_26.9.2003_macos_aarch64.dmg`。
- `.app.tar.gz` 是同架构的应用包压缩产物，通常优先使用 `.dmg` 安装。
- 打开 `.dmg` 后把应用拖入 `Applications`。未签名构建首次打开时可能需要在系统设置的“隐私与安全性”中允许打开。
- 启动后代理端口同样默认为 `5678`，可在系统网络代理或浏览器代理中配置 `127.0.0.1:5678`。

Linux：

- 优先下载 Linux x64 的 `.AppImage`，赋予执行权限后运行：

```bash
chmod +x proxy-load_*_*.AppImage
./proxy-load_*_*.AppImage
```

- Debian/Ubuntu 可下载 `.deb` 后安装：

```bash
sudo apt install ./proxy-load_*_amd64.deb
```

- Fedora/RHEL 系发行版可下载 `.rpm` 后安装：

```bash
sudo dnf install ./proxy-load_*_x86_64.rpm
```

- 启动后代理端口默认为 `5678`，可将应用或系统代理指向 `127.0.0.1:5678`。

## 数据目录和随包配置

应用会把 SQLite 数据库写入平台默认数据目录。Windows 发布版默认使用应用目录下的 `data`，方便安装版和便携版随包读取配置；macOS 和 Linux 默认使用系统约定的用户数据目录。`DATA_DIR` 环境变量仍可强制指定数据目录。

默认数据目录：

| 平台 | 默认位置 |
| --- | --- |
| 开发环境 | 项目根目录 `data` |
| Windows 发布版（安装版和便携版） | exe 所在目录的 `data` |
| macOS | `~/Library/Application Support/proxy-load` |
| Linux | `$XDG_DATA_HOME/proxy-load`，未设置时使用 `~/.local/share/proxy-load` |

如果给 macOS 用户的 zip 里同时包含 `proxy-load.app` 和 `data/`，不要只把 `proxy-load.app` 拖入 `Applications` 后再启动。应用只能看到被复制后的 `.app`，看不到 zip 解压目录里的同级 `data`。

带随包配置的 macOS zip 推荐流程：

1. 解压后保持 `proxy-load.app` 和 `data/` 在同一个目录。
2. 先从这个解压目录启动一次 `proxy-load.app`。
3. 应用会在用户数据目录还没有 `proxy.db` 时，自动导入同级 `data/proxy.db`。
4. 确认配置列表显示正常后，再把 `proxy-load.app` 拖入 `Applications`。

如果已经只把 `.app` 拖入 `Applications`，可以手动复制数据库。复制前先退出 `proxy-load`，否则 SQLite 的 WAL 文件可能还在写入。

大多数用户会把 zip 解压到“下载”目录。假设解压后的目录是 `~/Downloads/proxy-load`，可以执行：

```bash
SOURCE_DATA="$HOME/Downloads/proxy-load/data"
TARGET_DATA="$HOME/Library/Application Support/proxy-load"

mkdir -p "$TARGET_DATA"
cp "$SOURCE_DATA"/proxy.db* "$TARGET_DATA"/
```

如果解压目录不是 `~/Downloads/proxy-load`，把 `SOURCE_DATA` 改成实际的 `data` 目录路径。需要复制的是同一批 SQLite 数据文件，包括 `proxy.db`、`proxy.db-shm` 和 `proxy.db-wal`。

自动导入只会在目标目录还没有 `proxy.db` 时执行，避免覆盖用户已有配置。

## 检查更新

“关于与更新”中的国内加速地址支持自行修改，默认值为 `https://ghproxy.net/`。点击“保存”时仅对基础地址进行最长 10 秒的 HTTP 连通性测试（不读取正文、不下载更新包），收到成功响应后才持久化；失败时输入框标红并保留原有配置。点击“恢复默认”会填入默认地址，仍需点击“保存”并通过校验后生效。

开启“国内加速”后，手动/自动检查更新和下载更新包均使用已保存地址，关闭时仍直连 GitHub。加速服务需兼容 `加速地址/原始 GitHub URL` 的前缀形式，并能代理 GitHub 发布页面和资产下载；版本与更新包列表从发布页面解析，不通过镜像访问 GitHub API。基础地址可访问不代表一定具备这些加速能力，请使用可信服务；镜像请求不会携带 GitHub Token。

应用内“检查更新”遵循运行环境策略：

- 开发环境：直接返回错误，避免把本地调试产物误当成线上更新。
- 生产环境：请求 GitHub Releases 最新版本，选择当前平台可用的安装包。
- 安装包运行：Windows 下会静默启动下载的 setup 或 msi，并把安装目录指向当前应用所在目录，例如 `F:\proxy-load`，随后退出当前应用以便安装器覆盖旧版本。如果目标目录需要管理员权限，系统仍可能弹出 UAC 权限确认。
- 便携版运行：Windows 下会把已验签的 portable exe 暂存到独立更新目录，退出当前应用后由辅助进程替换并重启原 EXE 入口，保持快捷方式和数据目录不变；启动失败时回滚旧程序。
- macOS 运行：下载对应架构的 `.dmg` 到 `~/Downloads` 并自动打开，用户需要退出当前应用后在 DMG 中把应用拖入 `Applications` 覆盖旧版。

Windows 更新检查不会默认安装到系统盘其他位置；应用放在 `F:\proxy-load` 时，更新也会以该目录作为安装位置。

Windows 下“更新目标目录”保持为当前应用所在目录，下载文件先进入该目录下的独立更新暂存目录。比如便携 exe 放在 `F:\project\proxy-load\release` 中运行时，最终仍替换该目录内的原 EXE，不会把暂存目录当作新的应用目录。macOS 下载以 `~/Downloads` 为基础目录。

便携 exe 在运行时不能直接覆盖自身，因此由辅助进程等待旧进程退出后安装新程序。GitHub Release 中的下载文件名包含版本号，但安装后仍使用原 EXE 文件名，不直接运行暂存文件。

如果发布仓库是私有仓库，GitHub 未认证访问会返回 `404`。生产环境需要在启动应用前设置环境变量：

```powershell
$env:PROXY_LOAD_GITHUB_TOKEN = "github_pat_xxx"
```

Token 需要具备读取私有仓库 Release 的权限。不要把 Token 打包进应用或提交到仓库。

## 发布工作流

仓库包含 tag 触发的 GitHub Actions workflow：

```text
.github/workflows/release.yml
```

触发规则：

```text
v*
```

推送匹配规则的 tag 后，会在 GitHub Actions 中构建：

- Linux x64
- Windows x64
- macOS Apple Silicon
- macOS Intel

workflow 先运行三平台回归检查，随后按明确选择的模式构建、上传本地安装包。所有选定平台成功后才将草稿发布为正式 GitHub Release；普通提交只运行检查，不触发发布。Release 内容来自 `RELEASE_NOTES.md`，只保留本次变更，工作流附加安装方式说明。

- `signed`：需要项目公私钥齐全且匹配，构建后为最终产物生成 `.manifest.json`；缺钥、错钥直接失败，不悄悄改为 manual。
- `manual`：无需项目私钥或 Apple Secrets，正常构建、上传供用户手动安装；不生成自动安装清单，客户端仍可检查更新，但只能打开固定官方 Releases 页面。

手动运行 Release 时明确选择 `release_mode`（默认 manual）。tag 触发默认 signed；若希望 tag 使用 manual，需要事先明确设置仓库 Variable `PROXY_LOAD_RELEASE_MODE=manual`。同一 tag 不混用模式，避免旧清单与新产物错配。正式发布前同步 package/Cargo/tauri/version 定义与日期标签。

macOS bundle 使用 `signingIdentity: "-"` 的 ad-hoc 签名，无需 Apple Developer ID 或公证身份；系统仍可能要求手动允许，不承诺没有 Gatekeeper 提示，也不要求全局关闭系统安全检查。配置依据 [Tauri ad-hoc 文档](https://v2.tauri.app/distribute/sign/macos/#ad-hoc-signing)，系统提示见 [Apple 说明](https://support.apple.com/en-us/102445)。HTTPS 证书校验保持开启。

### 更新验签配置（不需要应用商店证书）

这是应用内更新包的 Ed25519 发布者验签，不是 Windows/macOS 应用签名，也不替代 Apple 公证。首次配置由仓库维护者在可信设备上执行一次：

```powershell
node scripts/update-signing.mjs generate "$HOME/proxy-load-keys/update.private.pem"
```

密钥必须存储在仓库外，妥善备份私钥并限制本机文件访问权限（Windows 请检查 ACL）。不要在聊天、日志或提交中粘贴私钥。配置 GitHub Actions：

- Secret `PROXY_LOAD_UPDATE_PRIVATE_KEY`：私钥 PEM 文件完整内容。
- Variable `PROXY_LOAD_UPDATE_PUBLIC_KEY`：相邻 `.pub` 文件的 Base64 公钥内容；构建时嵌入应用。
- 可选 Secret `PROXY_LOAD_UPDATE_KEY_PASSWORD`：使用自行加密的 PEM 私钥时填写密码。

signed 模式缺少配置或公私钥不匹配会阻止 Release。私钥只传入签名步骤，不传给应用编译步骤。每份清单绑定版本、系统、架构、安装形式、文件名、长度及 SHA-256 摘要；镜像仅负责传输，不能提供或替换应用信任的公钥。没有可信公钥或旧包缺签时明确提供官方发布页手动路径；签名/摘要/平台校验失败时拒绝当前产物，不自动降级执行。

首次启用前，应通过可信渠道手动安装包含正确公钥的版本；旧的未验签客户端不会因服务器增加清单而自动获得验签能力。后续发布必须沿用同一私钥；直接替换公钥会使旧客户端拒绝新包，密钥轮换需要单独设计过渡。未配置公钥的开发构建仍可运行，但不能自动安装更新。本仓库不包含生产私钥，也不会由脚本自动配置仓库 Secrets。

## 运行端口和配置

| 配置项 | 默认值 | 说明 |
| --- | --- | --- |
| `PROXY_PORT` | `5678` | 首次初始化代理服务端口 |
| `DATA_DIR` | 未设置 | 强制指定 SQLite 数据目录；未设置时使用上方平台默认数据目录 |
| `proxy_port` | `5678` | 高级配置中的代理端口，保存于 SQLite |
| `allow_lan` | `false` | 是否监听所有网卡；修改后需要重启应用 |
| `inbound_auth_enabled` | `false` | 是否要求 SOCKS5/HTTP 入站用户名密码认证 |
| `periodic_test_interval` | `180000` | 活跃节点心跳测活间隔，单位毫秒 |
| `probe_recovery_interval` | `180000` | 失败或未知节点重测间隔，单位毫秒 |
| `probe_failure_threshold` | `2` | 定时测活连续失败多少次后标记离线 |
| `startup_probe_enabled` | `true` | 应用启动时是否立即探测全部已启用代理；代理连接/认证失败可标记离线，单个测试站失败不会全局降级 |
| `max_connections` | `1024` | 业务连接上限，1–16384，重启生效 |
| `max_handshakes` | `128` | 入站握手并发，1–4096，重启生效 |
| `max_global_dials` | `64` | 全局业务拨号并发，1–2048，重启生效，不含独立测活 |
| `max_proxy_dials` | `32` | 单节点业务拨号并发，1–512，重启生效 |
| `target_quality_mode` | `off` | 目标链路质量：关闭 / 仅观测 `observe` / 参与自适应评分 `adaptive` |
| `log_retention_days` | `7` | 流量日志及其派生统计保留天数 |
| `circuit_failure_threshold` | `5` | 熔断失败阈值 |
| `failfast_enabled` | `true` | 是否启用快速失败 |

代理端口、“允许局域网连接”和四项业务并发限制修改后需要重启应用；入站认证、测活、熔断和快速失败参数会立即用于新连接。业务限制需满足：单节点拨号 ≤ 全局业务拨号 ≤ 业务连接，入站握手 ≤ 业务连接。高级设置分别显示已保存值和当前生效值；保存不替换正在使用的许可，也不会中断既有连接。入站认证密码保存在本地 SQLite 设置表中，以便用户日后在高级设置中重新查看；能够读取本机数据目录的用户也能读取该凭据。SOCKS5 用户名密码和 HTTP Basic 认证本身不加密，开放局域网监听时应只在可信网络中使用。

“HTTPS 代理”在这里指客户端通过 HTTP CONNECT 访问 HTTPS 目标，并不是带服务器证书的 TLS 加密代理监听端口。浏览器或应用可把 HTTP 和 HTTPS 代理地址都设置为同一个 `127.0.0.1:5678`，SOCKS5 也使用该端口。

测活始终绑定所选节点：无重定向、单地址成功场景只建立一条上游 TCP 连接，在同一条连接上完成 SOCKS/CONNECT、目标 TLS 和 HTTP 响应头检查。HTTP 代理访问 HTTP 测试地址直接使用 absolute-form GET，不要求 CONNECT 80。测活与业务使用相同的 DNS 映射：命中时连接映射 IP，仍保留原域名 Host/SNI；未命中时 SOCKS 域名由上游解析。通用代理最多跟随 5 次重定向，专用业务探针不接受重定向；响应头最多 64 KiB，网络阶段共用总超时，入队另有最多 5 秒预算。同节点重叠探测合并为一次有效观测，不重复累计。

测活分别记录代理入口、业务就绪与完整探测结果。TCP/认证成功而 CONNECT 超时会保留入口证据，但完整测活失败，不能增加“测活成功”。目标证书错误、5xx、隧道后的目标 407 不按代理认证失败处理；不确定、本地资源或配置错误不混入远端失败计数。节点列表“入口握手”只表示 TCP/可观察认证耗时，测试提示显示整轮总耗时。跳过证书验证仅适用于该节点的测活，不影响更新下载及发布者验签。

每个代理可显式选择健康策略，旧配置默认保留“通用代理”，不会根据名称自动切换：

- `transport_only`（通用代理）：目标失败显示“最近测活失败”及入口状态，不据此隔离整个节点；兼容既有连通性规则，可接受 404。
- `required_probe`（专用节点）：探测是此节点全部新连接的必要条件。默认连续失败 2 次隔离、连续成功 2 次恢复，阈值各可设为 1–10；首次失败立即提示，不再显示纯绿色在线。四种算法、粘性路由和最后候选均受同一门禁约束，分组内全部不就绪时快速报错，不跨组兜底。已有连接不迁移、不重放。
- 专用节点默认只接受 HTTP 200、204，可配置预期状态码，不跟随登录页重定向。请使用只有对应 VPN / 业务链路就绪时才能访问的稳定只读接口；公共网站、VPN 登录门户、代理监听和 VNC 端口均不能证明业务就绪。界面显示测试地址及独立／继承来源，不声称直接获知 VPN 登录状态。
- 专用节点始终接受后台固定节点恢复探测，不会因普通公网访问成功而跳过或解除隔离。探测沿用并发上限、总超时、退避和抖动。业务就绪证明默认 600 秒有效，可设 30–86400 秒，应大于探测间隔；过期、重启、连接配置／探针配置／DNS 映射变化后先等待新验证。关闭普通启动测活也不会跳过专用节点验证；禁用节点不参加自动探测。
- 新测活成功／失败计数与旧混合历史分开，显示统计起始时间和最近观测；不清空历史。排队超时、取消和过期结果通过独立 `proxy_probe_discarded` 诊断返回，不写成远端失败，也不覆盖新配置。就绪判定先在内存生效，再异步落库，以节点配置代号、设置版本和观测版本防止旧写入覆盖。

## 数据库

当前版本使用 SQLite。主要表包括：

- `proxies`：代理配置、状态、测试结果和优先级。
- `settings`：负载算法、运行配置和高级配置。
- `dns_mappings`：域名到 IP 的映射规则。
- `proxy_groups`：代理分组。
- `proxy_group_domains`：分组域名规则。
- `proxy_group_members`：分组成员代理。
- `request_logs`：请求流量日志。

Rust 后端启动时会自动创建表，并补齐缺失字段。

代理选路使用内存配置快照，配置保存成功后用于新连接。请求日志由有界后台队列批写：最多 2048 项（为失败和状态事件预留 256 项），每批最多 128 项、合并等待最多 100ms。运行状态显示队列长度、丢弃数和持久化错误；队列满时优先保留失败事件，不无限增长。正常退出最多等待 5 秒刷盘，异常断电仍可能丢失未落盘记录。

仪表盘聚合缓存最多 5 秒，清空日志和删除代理会使缓存失效，重启后从已有日志重建。删除代理沿用原有日志关联删除语义；清空后正在结束的新连接仍可产生新日志。历史日志使用固定 ID 快照，连续向后翻页使用游标；每页上限 200 条。

四种负载策略保留原配置值。轮询与最小连接数平局按池独立轮转；按目标主机粘滞使用稳定成员 ID 的 HRW 排名，优先级排序不改变哈希身份。自适应保留质量/当前负载选优，指标最多保留五分钟内最近 2048 个样本；新节点及恢复节点获得有界学习机会，但不会绕过熔断。既有超时配置不会被新默认值覆盖。

分组成员和精确/后缀域名规则使用快照索引，保留原先的规则长度排名与同分先出现者优先语义（并非无条件精确域名优先）；健康发布尽量复用静态索引。候选排序使用引用，仅选定节点后复制连接配置。

目标质量默认关闭。启用观测时，只收集同节点代号、原目标、实际地址、端口和协议测量类型的建连样本；开启评分后，仅以 0.2–1.0 的有界系数修正自适应算法，不影响轮询、最小连接或粘滞出口。全运行时最多 4096 条样本，5 分钟过期，未知目标保持中性；保留已有每 16 次选择的学习机会，已观测慢链路至少间隔 30 秒才进入恢复试探候选。HTTP 普通转发响应、HTTPS 加密业务 TTFB 不参与该评分。关闭立即清空目标质量缓存并恢复原策略。

每个分组可继承全局算法或覆盖为四种算法之一。主机粘滞支持 0–86400 秒故障切换保持期：主节点故障后成功使用的备用节点，在保持期内优先用于同一目标的新连接；不会因每次成功访问而续期。备用节点失败、被禁用、分组规则或成员配置变更时失效；仅因拨号容量排队而换节点不会建立保持绑定。默认 0（关闭），已有分组默认继承全局算法。绑定缓存最多 4096 项，重启后清空。禁用节点可保留在成员列表中，但不参加自动测活和业务选路。

代理服务限制为最多 1024 个已接收连接、128 个并发入站握手、全局 64 个及每节点 32 个同时拨号；入站握手预算为 5 秒，后续排队/选路/拨号/交付共享配置的建连总预算。选路临界区只做短时内存操作，数据库写入与网络等待在外部执行。全局拨号槽位使用异步公平队列；节点槽位满时释放选路锁及全局许可等待通知，不把容量不足记为节点故障。已经建立的隧道不受建连总超时限制；半关闭处理使用双向独立复制，并由原生 TCP 验收检查另一方向能否继续传输。失败切换只用于尚未发出业务数据的新连接，不重放 POST，也不迁移既有 TCP/TLS 会话。

普通 HTTP 日志区分“已转发待响应”“上游已响应”和传输结束/错误，目标 HTTP 5xx 不直接判定代理全局故障；407 属于代理认证失败。传输记录包含双向字节数、持续时间和关闭原因。系统明确报告本地网络不可用时单独归类，不根据一批超时猜测本地断网。

允许应用内安装时，更新下载先验证不超过 16 KiB 的发布者签名清单，再写入独立会话目录中的流式 `.part` 文件，限制为 1 GiB，完成签名所绑定的摘要、大小及格式检查后才发布；取消与失败清理未完成文件。资产按系统、安装形式及 CPU 架构筛选，Windows 便携包验证 PE 架构，NSIS 允许为 64 位载荷使用 32 位安装器外壳。后端禁止重复安装，并保留 Windows 辅助进程等待旧进程退出的交接。macOS 使用 ad-hoc 签名，未接入 Apple Developer ID 或公证。

Windows 便携更新由辅助进程在旧 PID 退出后替换原 EXE 入口，不直接启动 staging 中的文件，因此原快捷方式和默认 `data` 根目录保持不变；显式 `DATA_DIR` 也保留（相对路径在交接前固定为绝对路径）。旧 EXE 保存在原目录的唯一 `.rollback-*` 文件中，新版本 30 秒内未确认启动会恢复并重启旧入口。不复制运行中的 SQLite 数据目录；仅清理本次成功更新后空的 staging，保留回滚文件。升级/退出会停止接入、取消现有连接，并关闭后台日志入队后最多刷盘 5 秒，超时会记录失败而不是无限等待。

当前入口是 Tauri：

```bash
npm start
```

## 质量检查

常用检查命令：

```bash
npm run build:web
npm run test:frontend
npm run test:signing
cargo fmt --manifest-path src-tauri/Cargo.toml --check
cargo test --manifest-path src-tauri/Cargo.toml --locked
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --locked -- -D warnings
```

PR 与 main 分支推送会执行 Windows / Linux / macOS 回归检查；Release 构建依赖同一提交的检查通过。可手动运行确定性本地模拟基准：

```bash
cargo test --release --manifest-path src-tauri/Cargo.toml --locked benchmark_loopback_matrix -- --ignored --nocapture
cargo test --manifest-path src-tauri/Cargo.toml --locked million_metric_updates -- --nocapture
```

设置 `PROXY_LOAD_BENCH_OUTPUT` 输出 JSON；`PROXY_LOAD_BENCH_DB=disk` 使用实际临时 SQLite WAL 数据库，默认预置 100000 条历史日志（可用 `PROXY_LOAD_BENCH_HISTORY` 调整）。默认内存模式用于快速回归，不能代表磁盘性能。PowerShell 示例：

```powershell
$env:PROXY_LOAD_BENCH_DB = 'disk'
$env:PROXY_LOAD_BENCH_OUTPUT = "$PWD/docs/performance-release-disk.json"
cargo test --release --manifest-path src-tauri/Cargo.toml --locked benchmark_loopback_matrix -- --ignored --nocapture
```

矩阵共 90 个场景，覆盖 3/10/100 节点、1/32/256 并发、短连接/保留 100ms 的隧道/混合、认证失败、上游握手超时、单目标拒绝、SOCKS5 慢认证、日志队列满、全局/目标半开，同时运行图表和日志页使用的后台查询与优先级更新（不等同于真实浏览器绘制基准）。可以用 `PROXY_LOAD_BENCH_NODES`、`PROXY_LOAD_BENCH_CONCURRENCY`、`PROXY_LOAD_BENCH_SCENARIOS`（逗号分隔）筛选。基准单次拨号预算 500ms、总建连预算 5 秒、失败阈值 1；只作用于测试实例。输出构建模式、历史量、冷暖阶段延迟分位数、各阶段耗时、可达率、选路锁等待/持有、故障后切换耗时、事件循环延迟、后台队列、分布和进程资源。冷暖是单次运行的前后半段，不是独立稳定态测试；不代表公网性能或小时级转发吞吐，不承诺无感会话迁移。比较性能时单独运行基准，避免同时编译或执行完整测试。

日志清空通过后台队列有序屏障执行：清除点击清空前已经接收的日志，保留之后的新日志；持续流量不会让旧日志在清空后重新写回。退出与升级会先停止接收写入并限时排空队列，失败会报告而非当作已完成。运行时健康立即生效，不依赖日志队列；状态按节点合并持久化，诊断可查看状态待写、合并、重试与丢弃计数。

常规测试还覆盖取消释放及建连截止时间之后的原生 TCP 大文件双向转发。原生半关闭验收和不经过代理的裸 TCP 对照默认忽略，由 CI 单独执行：

```bash
cargo test --manifest-path src-tauri/Cargo.toml --locked native_half_close -- --ignored
cargo test --manifest-path src-tauri/Cargo.toml --locked native_tcp_half_close_baseline -- --ignored
cargo test --manifest-path src-tauri/Cargo.toml --locked native_probe_ -- --ignored
```

如果两者均失败，需要先排查运行环境；不能以 Tokio 内存流测试替代原生网络验收。三平台 CI 配置不等同于实际已通过，真实浏览器/WebSocket、长时间运行、系统休眠恢复及安装器仍需要在目标设备验证。

单链路 SOCKS4a 原生测活与裸 TCP 域名尾部对照也由 CI 单独验收，不用内存流协议测试替代。本机若失败，保留原始结果并在未受影响的目标设备复验。索引和测活专项基准分别运行 `benchmark_routing_index_matrix`、`benchmark_probe_single_chain`（release + `--ignored`），使用 `PROXY_LOAD_INDEX_BENCH_OUTPUT` / `PROXY_LOAD_PROBE_BENCH_OUTPUT` 保存 JSON。索引每场景独立预热 100 次，测活每协议/每轮预热 2 次；两项均运行 5 轮。旧 90 场景矩阵的前后半段字段不代表真正冷启动/预热，不可平均各场景 p99 冒充总体 p99。

完整打包检查：

```bash
npm run tauri:build
```

## 许可证

以仓库根目录 `LICENSE` 文件为准。

