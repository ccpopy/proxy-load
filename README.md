# 智能代理负载均衡系统

## 项目概述

这是一个高性能的代理负载均衡系统，支持多种代理协议（SOCKS4/5、HTTP/HTTPS）和智能负载均衡算法。系统具备连接池管理、熔断器保护、健康检查和实时监控等企业级功能。

## 核心特性

### 🚀 负载均衡算法

- **自适应算法** (`adaptive`)：综合多种指标的智能选择算法
- **加权轮询** (`weighted_round_robin`)：基于代理性能评分的加权轮询
- **最少连接** (`least_connections`)：选择当前连接数最少的代理
- **会话粘滞（按域名）** (`sticky_host`)：同一域名尽量使用同一代理

### 🔧 核心组件

- **连接池管理**：高效的连接复用和管理
- **熔断器保护**：Circuit Breaker模式防止雪崩
- **健康检查**：定期检测代理可用性
- **性能监控**：实时统计和性能分析
- **WebSocket通信**：实时状态推送

## 文件结构

```
zwfw-load/
├── app.js                  # Express应用主文件
├── proxyServer.js         # 代理负载均衡核心逻辑
├── version.js             # 版本信息管理模块
├── public/                # 前端静态文件
├── data/                  # 数据存储目录
│   └── proxy.db          # SQLite数据库
├── package.json          # 项目依赖配置
└── CLAUDE.md             # 本文档
```

## 核心代码解析

### ProxyLoadBalancer 类

位于 `proxyServer.js`，是系统的核心组件：

#### 主要属性

```javascript
class ProxyLoadBalancer {
  constructor(db, logRequest) {
    this.db = db; // 数据库连接
    this.loadMode = "auto"; // 负载模式
    this.circuitBreakers = new Map(); // 熔断器集合
    this.connectionPool = new ConnectionPool(); // 连接池
    this.algorithms = {
      // 算法映射
      adaptive: this.adaptiveSelection.bind(this),
      weighted_round_robin: this.weightedRoundRobin.bind(this),
      least_connections: this.leastConnections.bind(this),
      sticky_host: this.stickyHostSelection.bind(this),
    };
  }
}
```

### 连接池管理

```javascript
class ConnectionPool {
  async getConnection(proxyId, createFn) {
    // 复用现有连接或创建新连接
    // 支持连接数限制和空闲超时
  }

  releaseConnection(conn) {
    // 释放连接回池中
  }
}
```

### 熔断器保护

```javascript
class CircuitBreaker {
  constructor(threshold = 5, timeout = 60000, halfOpenAttempts = 2) {
    this.state = "CLOSED"; // CLOSED, OPEN, HALF_OPEN
  }

  canAttempt() {
    // 判断是否可以尝试连接
  }

  recordSuccess() {
    // 记录成功，可能关闭熔断器
  }

  recordFailure() {
    // 记录失败，可能打开熔断器
  }
}
```

## API 接口

### 代理管理

```http
GET    /api/proxies           # 获取所有代理
POST   /api/proxies           # 创建新代理
PUT    /api/proxies/:id       # 更新代理
DELETE /api/proxies/:id       # 删除代理
POST   /api/proxies/:id/test  # 测试代理
```

### 设置管理

```http
GET    /api/settings          # 获取系统设置
POST   /api/settings          # 更新系统设置
```

### 统计监控

```http
GET    /api/stats/overview           # 系统概览
GET    /api/stats/hourly             # 小时统计
GET    /api/stats/proxy-usage        # 代理使用统计
GET    /api/stats/circuit-breakers   # 熔断器状态
GET    /api/stats/connection-pools   # 连接池状态
```

### 高级配置

```http
GET    /api/advanced-config          # 获取高级配置
POST   /api/advanced-config          # 保存高级配置
POST   /api/advanced-config/reset    # 重置为默认配置
GET    /api/advanced-config/export   # 导出配置
```

### 版本信息

```http
GET    /api/version                  # 获取版本信息
```

## 数据库结构

### proxies 表

```sql
CREATE TABLE proxies (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  name TEXT NOT NULL,
  type TEXT NOT NULL,           -- socks4, socks5, http, https
  host TEXT NOT NULL,
  port INTEGER NOT NULL,
  username TEXT,
  password TEXT,
  status TEXT DEFAULT 'unknown', -- active, inactive, testing
  last_test DATETIME,
  response_time INTEGER,
  success_count INTEGER DEFAULT 0,
  fail_count INTEGER DEFAULT 0,
  priority INTEGER DEFAULT 999,
  enabled INTEGER DEFAULT 1,
  bandwidth_bps INTEGER DEFAULT NULL,
  bandwidth_test_time DATETIME DEFAULT NULL,
  created_at DATETIME DEFAULT CURRENT_TIMESTAMP
);
```

### request_logs 表

```sql
CREATE TABLE request_logs (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  proxy_id INTEGER,
  target_host TEXT,
  target_port INTEGER,
  success BOOLEAN,
  response_time INTEGER,
  error_message TEXT,
  created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
  FOREIGN KEY (proxy_id) REFERENCES proxies(id) ON DELETE CASCADE
);
```

### settings 表

```sql
CREATE TABLE settings (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
```

### load_stats 表

```sql
CREATE TABLE load_stats (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  proxy_id INTEGER,
  weight REAL,
  success_rate REAL,
  avg_response_time INTEGER,
  requests_count INTEGER,
  timestamp DATETIME DEFAULT CURRENT_TIMESTAMP,
  FOREIGN KEY (proxy_id) REFERENCES proxies(id) ON DELETE CASCADE
);
```

## 配置参数

### 默认高级配置

```javascript
const DEFAULT_ADVANCED_CONFIG = {
  // 基础配置
  proxy_port: 5678,
  periodic_test_interval: 5 * 60 * 1000, // 5分钟
  log_retention_days: 7,
  stats_retention_days: 30,

  // 连接池配置
  pool_max_size: 50,
  pool_idle_timeout: 30000,
  pool_wait_timeout: 10000,

  // 熔断器配置
  circuit_failure_threshold: 5,
  circuit_timeout: 60000,
  circuit_half_open_attempts: 2,

  // 健康检查配置
  health_check_interval: 30000,
  health_degrade_threshold: 0.5,
  health_recover_threshold: 0.8,

  // 快速失败配置
  failfast_enabled: true,
  failfast_max_attempts: 3,
  failfast_attempt_timeout: 10000,
  failfast_total_timeout: 30000,

  // 算法权重配置
  algorithm_weights: {
    responseTime: 0.3,
    successRate: 0.25,
    connections: 0.2,
    stability: 0.15,
    recentPerf: 0.1,
  },
};
```

## 部署指南

### 环境要求

- Node.js 14+
- SQLite3
- 内存: 512MB+
- 磁盘: 100MB+

### 安装步骤

```bash
# 1. 安装依赖
npm install

# 2. 设置环境变量（可选）
export PORT=3333
export PROXY_PORT=5678
export DATA_DIR=./data

# 3. 启动服务
npm start
```

### 环境变量

- `PORT`: Web管理界面端口（默认: 3333）
- `PROXY_PORT`: 代理服务端口（默认: 5678）
- `DATA_DIR`: 数据存储目录（默认: ./data）

## 监控和运维

### 关键指标

- **代理可用性**: 活跃代理数量和健康状态
- **响应时间**: 平均响应时间和响应时间分布
- **成功率**: 请求成功率和错误率
- **连接数**: 活跃连接数和连接池状态
- **熔断器状态**: 各代理的熔断器状态

### 日志管理

- 请求日志自动清理（默认保留7天）
- 负载统计自动清理（默认保留30天）
- 支持通过配置调整保留时间

### 性能优化

- 连接池复用减少连接开销
- 熔断器防止级联故障
- 智能算法权重动态调整
- WebSocket实时推送减少轮询

## 故障排除

### 常见问题

1. **代理连接失败**
   - 检查代理服务器状态
   - 验证认证信息
   - 查看熔断器状态

2. **性能问题**
   - 调整连接池大小
   - 优化算法权重
   - 检查网络延迟

3. **内存使用过高**
   - 减少连接池大小
   - 调整日志保留时间
   - 检查连接泄漏

### 调试技巧

- 使用 `/api/stats/circuit-breakers` 查看熔断器状态
- 通过 `/api/stats/connection-pools` 监控连接池

## 版本管理

### 版本信息模块 (version.js)

系统提供了完整的版本管理功能，支持开发环境和pkg打包环境：

```javascript
const { getVersion, printVersion } = require("./version");

// 获取版本信息
const versionInfo = getVersion();
// 返回对象包含:
// - version: 版本号（来自package.json）
// - name: 项目名称
// - description: 项目描述
// - author: 作者
// - buildTime: 构建时间（ISO格式）
// - environment: 运行环境（development/production）
// - nodeVersion: Node.js版本
// - platform: 操作系统平台
// - arch: CPU架构

// 打印版本信息到控制台
printVersion();
```

### 版本信息显示

1. **启动时显示**：服务启动时会在控制台打印完整的版本信息
2. **前端显示**：Web界面左侧边栏底部显示版本号，鼠标悬停可查看详细信息
3. **API接口**：通过 `/api/version` 获取JSON格式的版本信息

### pkg打包支持

版本信息模块完全支持pkg打包：

- 自动检测运行环境（`process.pkg`）
- 开发环境从文件系统读取package.json
- 打包环境从嵌入的快照读取package.json
- 构建时间自动从可执行文件或package.json获取

### 更新版本号

修改 `package.json` 中的 `version` 字段即可：

```json
{
  "version": "1.0.1"
}
```

重新启动或打包后，新版本号会自动生效。

## 扩展开发

### 添加新算法

1. 在 `ProxyLoadBalancer` 类中添加新算法方法
2. 在 `algorithms` 对象中注册算法
3. 在应用层的 `validAlgorithms` 数组中添加算法名称

### 添加新监控指标

1. 在相应的组件中记录数据
2. 创建新的API端点暴露数据
3. 在前端添加展示逻辑

### 自定义连接处理

1. 继承或修改相应的连接方法
2. 实现自定义的协议处理逻辑
3. 集成到负载均衡流程中
