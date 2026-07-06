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
- 代理服务端口：默认 `5678`

正式应用只对外启动代理服务端口。开发时 Tauri 会拉起 Vite 页面服务 `1420` 供应用窗口加载前端资源，但页面数据仍通过 Tauri 应用内通信获取，不支持单独用普通浏览器直连管理 API 调试。

## 功能

- 代理配置管理：新增、编辑、删除、启用、停用和连通性测试。
- DNS 映射：按域名覆盖解析结果。
- 代理分组：按域名规则选择代理组；未命中分组时使用全局已启用代理逐个尝试。
- 负载设置：支持 `adaptive`、`weighted_round_robin`、`least_connections`、`sticky_host`。
- 高级配置：代理端口、日志保留、连接池、熔断器、快速失败等运行参数。
- 系统状态：请求趋势、响应耗时、代理使用排行、目标资源排行。
- 流量日志：分页查看请求日志，支持清空日志。
- 实时刷新：通过 Tauri 事件推送代理、DNS、分组和请求日志变化。
- 检查更新：开发环境禁止检查更新；生产环境从 GitHub Releases 获取更新包，并按应用所在目录更新。
- 主题：shadcn Teal 色系，支持亮色和暗色切换。

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
│   │   ├── proxy.rs           # 代理服务、负载均衡、连接池和熔断器
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
release/proxy-load_0.3.3_x64-portable.exe
```

这个文件主要用于本机验证，可以直接双击运行；正式更新安装仍建议使用 setup 或 msi 安装包。

## 三平台安装和使用

从 GitHub Releases 下载当前版本对应平台的产物。

Windows：

- 便携运行：下载 `proxy-load_0.3.3_x64-portable.exe`，放到目标目录后直接双击运行。
- 安装运行：下载 Windows x64 的 `setup.exe` 或 `.msi` 安装包，按安装向导完成安装。GitHub Release 文件名会使用 `proxy-load_0.3.3_windows_*` 前缀。
- 启动后应用会监听默认代理端口 `5678`，浏览器或系统代理可配置为 `SOCKS5 127.0.0.1:5678` 或 `HTTP 127.0.0.1:5678`。

macOS：

- Intel 芯片下载 `proxy-load_0.3.3_darwin_x64.dmg`。
- Apple Silicon 芯片下载 `proxy-load_0.3.3_darwin_aarch64.dmg`。
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

应用内“检查更新”遵循运行环境策略：

- 开发环境：直接返回错误，避免把本地调试产物误当成线上更新。
- 生产环境：请求 GitHub Releases 最新版本，选择当前平台可用的安装包。
- 安装包运行：Windows 下会启动下载的 setup 或 msi，并把安装目录指向当前应用所在目录，例如 `F:\proxy-load`。
- 便携版运行：Windows 下会下载 portable exe 到当前应用目录，退出当前应用后启动新版本文件。

更新检查不会默认安装到系统盘其他位置；应用放在 `F:\proxy-load` 时，更新也会以该目录作为安装位置。

应用内显示的“更新目标目录”和“下载保存目录”都会指向当前应用所在目录。比如便携 exe 放在 `F:\project\proxy-load\release` 中运行时，更新也会下载到 `F:\project\proxy-load\release`，不会再放到 `F:\project\proxy-load\release\release`。

便携 exe 在运行时不能直接覆盖自身，所以更新时会直接下载 GitHub Release 中的新版本文件名，例如 `proxy-load_0.3.3_x64-portable.exe`，然后退出当前应用并启动这个新版本文件。

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

workflow 会创建正式 GitHub Release，并上传 Tauri bundle 和 Windows 便携 exe。普通提交不会触发发布。

## 运行端口和配置

| 配置项 | 默认值 | 说明 |
| --- | --- | --- |
| `PROXY_PORT` | `5678` | 首次初始化代理服务端口 |
| `DATA_DIR` | 未设置 | 强制指定 SQLite 数据目录；未设置时使用上方平台默认数据目录 |
| `proxy_port` | `5678` | 高级配置中的代理端口，保存于 SQLite |
| `periodic_test_interval` | `300000` | 定期测试间隔，单位毫秒 |
| `log_retention_days` | `7` | 请求日志保留天数 |
| `stats_retention_days` | `30` | 统计数据保留天数 |
| `pool_max_size` | `50` | 连接池最大连接数 |
| `circuit_failure_threshold` | `5` | 熔断失败阈值 |
| `failfast_enabled` | `true` | 是否启用快速失败 |

部分高级配置保存后需要重启应用才能完全生效，尤其是代理服务端口。

## 数据库

当前版本使用 SQLite。主要表包括：

- `proxies`：代理配置、状态、测试结果和优先级。
- `settings`：负载算法、运行配置和高级配置。
- `dns_mappings`：域名到 IP 的映射规则。
- `proxy_groups`：代理分组。
- `proxy_group_domains`：分组域名规则。
- `proxy_group_members`：分组成员代理。
- `request_logs`：请求流量日志。
- `load_stats`：负载统计数据。

Rust 后端启动时会自动创建表，并补齐缺失字段。

当前入口是 Tauri：

```bash
npm start
```

## 质量检查

常用检查命令：

```bash
npm run build:web
cargo check --manifest-path src-tauri/Cargo.toml
```

完整打包检查：

```bash
npm run tauri:build
```

## 许可证

以仓库根目录 `LICENSE` 文件为准。
