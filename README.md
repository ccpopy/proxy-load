# zwfw-load

代理负载均衡管理系统。当前版本已从单体 Node.js 服务重构为 Tauri v2 桌面应用：后端核心由 Rust 实现，前端由 React、Vite、TypeScript、shadcn/ui、lucide-react 和 Recharts 构建。旧 Node.js 实现仅作为 legacy 入口保留，用于对照验证。

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
- 代理分组：按域名规则选择代理组，支持默认分组。
- 负载设置：支持 `adaptive`、`weighted_round_robin`、`least_connections`、`sticky_host`。
- 高级配置：代理端口、日志保留、连接池、熔断器、快速失败等运行参数。
- 系统状态：请求趋势、响应耗时、代理使用排行、目标资源排行。
- 流量日志：分页查看请求日志，支持清空日志。
- 实时刷新：通过 Tauri 事件推送代理、DNS、分组和请求日志变化。
- 检查更新：扫描本地 `release` 目录中的安装包，并在生产环境按应用所在目录启动更新安装。
- 主题：shadcn Teal 色系，支持亮色和暗色切换。

## 目录结构

```text
zwfw-load/
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
├── public/                    # 旧 Node.js 前端静态文件，legacy 保留
├── src/                       # 旧 Node.js 后端模块，legacy 保留
├── app.js                     # 旧 Node.js 服务入口，legacy 保留
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
release/zwfw-load_1.2.2_x64-portable.exe
```

这个文件主要用于本机验证，可以直接双击运行；正式更新安装仍建议使用 setup 或 msi 安装包。

## 检查更新

应用内“检查更新”遵循本地目录策略：

- 开发环境：扫描项目根目录 `release`，即 `F:\project\zwfw-load\release`。
- 生产环境：扫描可执行文件所在目录下的 `release`。如果应用放在 `F:\zwfw-load`，则扫描 `F:\zwfw-load\release`。
- 安装更新：Windows 下会启动本地安装包，并把安装目录指向当前应用所在目录，例如 `F:\zwfw-load`。

更新检查只接受 `release` 目录内的安装包，不会默认安装到系统盘其他位置。

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

workflow 会创建 draft release，并上传 Tauri bundle 和 workflow artifacts。普通提交不会触发发布。

## 运行端口和配置

| 配置项 | 默认值 | 说明 |
| --- | --- | --- |
| `PROXY_PORT` | `5678` | 首次初始化代理服务端口 |
| `DATA_DIR` | 开发为项目根目录 `data`，生产为应用目录 `data` | SQLite 数据目录 |
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

## 旧 Node.js 入口

旧实现仍保留在 `app.js`、`src/` 和 `public/` 中，用于对照和兼容验证：

```bash
npm run legacy:start
npm run legacy:dev
```

当前推荐入口是 Tauri：

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
