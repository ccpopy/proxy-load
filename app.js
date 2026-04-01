const express = require('express');
const path = require('path');
const compression = require('compression');
const { open } = require('sqlite');
const sqlite3 = require('sqlite3').verbose();
const ProxyLoadBalancer = require('./proxyServer');
const axios = require('axios');
const { SocksProxyAgent } = require('socks-proxy-agent');
const { HttpProxyAgent, HttpsProxyAgent } = require('hpagent');
const fs = require('fs');
const WebSocket = require('ws');
const { getVersion, printVersion } = require('./version');

// 常量
const app = express();
const PORT = Number(process.env.PORT) || 3333;

// 启用压缩
app.use(compression());

// 中间件
app.use(express.json());
app.use(express.static(path.join(__dirname, 'public')));

// 全局变量
let db;
let proxyServer;
let wss;
const wsClients = new Set();

// 进程内统计
const stats = {
  totalRequests: 0,
  successRequests: 0,
  failedRequests: 0,
  startTime: Date.now()
};

// 定时器管理
const timers = {
  periodicTest: null,
  cleanLogs: null,
  statsUpdate: null
};

// 任务执行状态跟踪
const taskRunning = {
  periodicTest: false,
  cleanLogs: false
};

// 默认高级配置
const DEFAULT_ADVANCED_CONFIG = {
  // 基础配置
  proxy_port: Number(process.env.PROXY_PORT) || 5678,
  periodic_test_interval: 5 * 60 * 1000, // 5分钟
  log_retention_days: 7,
  stats_retention_days: 30,

  // 连接池配置
  pool_max_size: 50,
  pool_idle_timeout: 30000, // 30秒
  pool_wait_timeout: 10000, // 10秒

  // 熔断器配置
  circuit_failure_threshold: 5,
  circuit_timeout: 60000, // 60秒
  circuit_half_open_attempts: 2,

  // 健康检查配置
  health_check_interval: 30000, // 30秒
  health_degrade_threshold: 0.5,
  health_recover_threshold: 0.8,

  // 快速失败配置
  failfast_enabled: true,
  failfast_max_attempts: 3,
  failfast_attempt_timeout: 10000,
  failfast_total_timeout: 30000,

  // 算法权重配置
  algorithm_weights: {
    responseTime: 0.25,
    successRate: 0.20,
    bandwidth: 0.15,
    connections: 0.15,
    stability: 0.15,
    recentPerf: 0.10
  }
};

const VALID_ALGORITHMS = new Set([
  'weighted_round_robin',
  'least_connections',
  'adaptive',
  'sticky_host'
]);
const TRAFFIC_LOG_LIMIT = 100;

// 初始化数据库
async function initDatabase () {
  const DATA_DIR = process.env.DATA_DIR || path.join(process.cwd(), 'data');
  await fs.promises.mkdir(DATA_DIR, { recursive: true });
  const DB_FILE = path.join(DATA_DIR, 'proxy.db');

  db = await open({ filename: DB_FILE, driver: sqlite3.Database });

  await db.exec(`PRAGMA journal_mode = WAL;`);
  await db.exec(`PRAGMA foreign_keys = ON;`);
  await db.exec(`PRAGMA synchronous = NORMAL;`);

  // 创建表结构
  await db.exec(`
    CREATE TABLE IF NOT EXISTS proxies (
      id INTEGER PRIMARY KEY AUTOINCREMENT,
      name TEXT NOT NULL,
      type TEXT NOT NULL,
      host TEXT NOT NULL,
      port INTEGER NOT NULL,
      username TEXT,
      password TEXT,
      status TEXT DEFAULT 'unknown',
      last_test DATETIME,
      response_time INTEGER,
      success_count INTEGER DEFAULT 0,
      fail_count INTEGER DEFAULT 0,
      priority INTEGER DEFAULT 999,
      enabled INTEGER DEFAULT 1,
      bandwidth_bps INTEGER DEFAULT NULL,
      bandwidth_test_time DATETIME DEFAULT NULL,
      created_at DATETIME DEFAULT CURRENT_TIMESTAMP
    )
  `);

  await db.exec(`
    CREATE TABLE IF NOT EXISTS settings (
      key TEXT PRIMARY KEY,
      value TEXT NOT NULL
    )
  `);

  await db.exec(`
    CREATE TABLE IF NOT EXISTS request_logs (
      id INTEGER PRIMARY KEY AUTOINCREMENT,
      proxy_id INTEGER,
      target_host TEXT,
      target_port INTEGER,
      success BOOLEAN,
      response_time INTEGER,
      error_message TEXT,
      result_type TEXT,
      created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
      FOREIGN KEY (proxy_id) REFERENCES proxies(id) ON DELETE CASCADE
    )
  `);

  await db.exec(`
    CREATE TABLE IF NOT EXISTS load_stats (
      id INTEGER PRIMARY KEY AUTOINCREMENT,
      proxy_id INTEGER,
      weight REAL,
      success_rate REAL,
      avg_response_time INTEGER,
      requests_count INTEGER,
      timestamp DATETIME DEFAULT CURRENT_TIMESTAMP,
      FOREIGN KEY (proxy_id) REFERENCES proxies(id) ON DELETE CASCADE
    )
  `);

  // 创建DNS映射表
  await db.exec(`
    CREATE TABLE IF NOT EXISTS dns_mappings (
      id INTEGER PRIMARY KEY AUTOINCREMENT,
      domain TEXT NOT NULL UNIQUE,
      ip TEXT NOT NULL,
      description TEXT,
      enabled INTEGER DEFAULT 1,
      created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
      updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
    )
  `);

  // 创建索引
  await db.exec(`CREATE INDEX IF NOT EXISTS idx_logs_created_at ON request_logs (created_at);`);
  await db.exec(`CREATE INDEX IF NOT EXISTS idx_logs_proxy_id ON request_logs (proxy_id);`);
  await db.exec(`CREATE INDEX IF NOT EXISTS idx_logs_target ON request_logs (target_host, created_at);`);
  await db.exec(`CREATE INDEX IF NOT EXISTS idx_load_stats_proxy ON load_stats (proxy_id, timestamp);`);
  await db.exec(`CREATE INDEX IF NOT EXISTS idx_dns_domain ON dns_mappings (domain);`);
  await db.exec(`CREATE INDEX IF NOT EXISTS idx_dns_ip ON dns_mappings (ip);`);

  // 创建代理分组表
  await db.exec(`
    CREATE TABLE IF NOT EXISTS proxy_groups (
      id INTEGER PRIMARY KEY AUTOINCREMENT,
      name TEXT NOT NULL,
      is_default INTEGER DEFAULT 0,
      enabled INTEGER DEFAULT 1,
      created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
      updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
    )
  `);

  await db.exec(`
    CREATE TABLE IF NOT EXISTS proxy_group_domains (
      id INTEGER PRIMARY KEY AUTOINCREMENT,
      group_id INTEGER NOT NULL,
      domain TEXT NOT NULL,
      FOREIGN KEY (group_id) REFERENCES proxy_groups(id) ON DELETE CASCADE
    )
  `);

  await db.exec(`
    CREATE TABLE IF NOT EXISTS proxy_group_members (
      id INTEGER PRIMARY KEY AUTOINCREMENT,
      group_id INTEGER NOT NULL,
      proxy_id INTEGER NOT NULL,
      FOREIGN KEY (group_id) REFERENCES proxy_groups(id) ON DELETE CASCADE,
      FOREIGN KEY (proxy_id) REFERENCES proxies(id) ON DELETE CASCADE,
      UNIQUE(group_id, proxy_id)
    )
  `);

  await db.exec(`CREATE INDEX IF NOT EXISTS idx_group_domains_group ON proxy_group_domains (group_id);`);
  await db.exec(`CREATE INDEX IF NOT EXISTS idx_group_domains_domain ON proxy_group_domains (domain);`);
  await db.exec(`CREATE INDEX IF NOT EXISTS idx_group_members_group ON proxy_group_members (group_id);`);
  await db.exec(`CREATE INDEX IF NOT EXISTS idx_group_members_proxy ON proxy_group_members (proxy_id);`);

  // 添加带宽列（如果不存在）
  const columns = await db.all("PRAGMA table_info(proxies)");
  const hasBandwidth = columns.some(col => col.name === 'bandwidth_bps');
  if (!hasBandwidth) {
    await db.exec(`ALTER TABLE proxies ADD COLUMN bandwidth_bps INTEGER DEFAULT NULL`);
    await db.exec(`ALTER TABLE proxies ADD COLUMN bandwidth_test_time DATETIME DEFAULT NULL`);
  }

  // 添加代理专属测试配置列（如果不存在）
  const hasTestUrl = columns.some(col => col.name === 'test_url');
  if (!hasTestUrl) {
    await db.exec(`ALTER TABLE proxies ADD COLUMN test_url TEXT DEFAULT NULL`);
    await db.exec(`ALTER TABLE proxies ADD COLUMN test_timeout INTEGER DEFAULT NULL`);
  }

  const logColumns = await db.all("PRAGMA table_info(request_logs)");
  const hasResultType = logColumns.some(col => col.name === 'result_type');
  if (!hasResultType) {
    await db.exec(`ALTER TABLE request_logs ADD COLUMN result_type TEXT`);
  }

  // 初始化默认设置
  const testUrl = await db.get("SELECT value FROM settings WHERE key = 'test_url'");
  if (!testUrl) {
    await db.run("INSERT INTO settings (key, value) VALUES ('test_url', 'https://cms.zjzwfw.gov.cn/favicon.ico')");
    await db.run("INSERT INTO settings (key, value) VALUES ('timeout', '10')");
    await db.run("INSERT INTO settings (key, value) VALUES ('load_mode', 'auto')");
    await db.run("INSERT INTO settings (key, value) VALUES ('algorithm', 'adaptive')");
  }
}

// WebSocket广播
function broadcast (type, data) {
  const message = JSON.stringify({ type, data, timestamp: Date.now() });
  wsClients.forEach(client => {
    if (client.readyState === WebSocket.OPEN) {
      try {
        client.send(message);
      } catch (e) {
        wsClients.delete(client);
      }
    }
  });
}

// 获取高级配置
app.get('/api/advanced-config', async (req, res) => {
  try {
    const configKeys = Object.keys(DEFAULT_ADVANCED_CONFIG);
    const settings = await db.all('SELECT key, value FROM settings WHERE key IN (' + configKeys.map(() => '?').join(',') + ')', configKeys);

    const config = { ...DEFAULT_ADVANCED_CONFIG };

    settings.forEach(setting => {
      try {
        // 尝试解析JSON（对于复杂对象如algorithm_weights）
        const value = JSON.parse(setting.value);
        config[setting.key] = value;
      } catch {
        // 如果不是JSON，直接使用值
        const numValue = Number(setting.value);
        config[setting.key] = isNaN(numValue) ? setting.value : numValue;
      }
    });

    res.json(config);
  } catch (error) {
    res.status(500).json({ error: error.message });
  }
});

// 保存高级配置
app.post('/api/advanced-config', async (req, res) => {
  try {
    const config = req.body;
    let requiresRestart = false;

    // 检查是否修改了需要重启的配置
    const currentProxyPort = await db.get("SELECT value FROM settings WHERE key = 'proxy_port'");
    if (currentProxyPort && Number(currentProxyPort.value) !== config.proxy_port) {
      requiresRestart = true;
    }

    // 保存配置到数据库
    for (const [key, value] of Object.entries(config)) {
      let saveValue = value;

      // 对于对象类型，转换为JSON字符串
      if (typeof value === 'object' && value !== null) {
        saveValue = JSON.stringify(value);
      }

      await db.run(
        'INSERT OR REPLACE INTO settings (key, value) VALUES (?, ?)',
        [key, String(saveValue)]
      );
    }

    // 应用配置到运行时
    applyRuntimeConfig(config);

    res.json({
      success: true,
      requiresRestart,
      message: requiresRestart ? '部分配置需要重启服务才能生效' : '配置已应用'
    });
  } catch (error) {
    res.status(500).json({ error: error.message });
  }
});

// 重置为默认配置
app.post('/api/advanced-config/reset', async (req, res) => {
  try {
    // 删除所有高级配置
    const configKeys = Object.keys(DEFAULT_ADVANCED_CONFIG);
    await db.run('DELETE FROM settings WHERE key IN (' + configKeys.map(() => '?').join(',') + ')', configKeys);

    // 应用默认配置到运行时
    applyRuntimeConfig(DEFAULT_ADVANCED_CONFIG);

    res.json({ success: true, message: '已恢复默认配置' });
  } catch (error) {
    res.status(500).json({ error: error.message });
  }
});

// 导出配置
app.get('/api/advanced-config/export', async (req, res) => {
  try {
    const allSettings = await db.all('SELECT key, value FROM settings');
    const config = {};

    allSettings.forEach(setting => {
      try {
        config[setting.key] = JSON.parse(setting.value);
      } catch {
        const numValue = Number(setting.value);
        config[setting.key] = isNaN(numValue) ? setting.value : numValue;
      }
    });

    // 添加代理列表
    const proxies = await db.all('SELECT * FROM proxies ORDER BY priority ASC, id ASC');
    config.proxies = proxies;

    res.json({
      version: '1.0.0',
      exportTime: new Date().toISOString(),
      config
    });
  } catch (error) {
    res.status(500).json({ error: error.message });
  }
});

// 应用运行时配置的函数
function applyRuntimeConfig (config) {
  // 更新定时器
  if (config.periodic_test_interval !== undefined) {
    if (timers.periodicTest) {
      clearTimeout(timers.periodicTest);
      timers.periodicTest = setTimeout(periodicProxyTest, config.periodic_test_interval);
    }
  }

  // 更新代理服务器配置
  if (proxyServer) {
    // 更新连接池配置
    if (proxyServer.connectionPool) {
      if (config.pool_max_size !== undefined) {
        proxyServer.connectionPool.maxSize = config.pool_max_size;
      }
      if (config.pool_idle_timeout !== undefined) {
        proxyServer.connectionPool.maxIdleTime = config.pool_idle_timeout;
      }
    }

    // 更新熔断器默认配置
    if (config.circuit_failure_threshold !== undefined ||
      config.circuit_timeout !== undefined ||
      config.circuit_half_open_attempts !== undefined) {

      if (!proxyServer.circuitBreakerConfig) {
        proxyServer.circuitBreakerConfig = {};
      }

      if (config.circuit_failure_threshold !== undefined) {
        proxyServer.circuitBreakerConfig.threshold = config.circuit_failure_threshold;
      }
      if (config.circuit_timeout !== undefined) {
        proxyServer.circuitBreakerConfig.timeout = config.circuit_timeout;
      }
      if (config.circuit_half_open_attempts !== undefined) {
        proxyServer.circuitBreakerConfig.halfOpenAttempts = config.circuit_half_open_attempts;
      }
      // 清空现有断路器实例，使用新配置重新创建
      proxyServer.circuitBreakers.clear();
    }

    // 更新健康检查配置
    if (config.health_check_interval !== undefined ||
      config.health_degrade_threshold !== undefined ||
      config.health_recover_threshold !== undefined) {

      if (!proxyServer.healthCheck) {
        proxyServer.healthCheck = {
          interval: 30000,
          timeout: 5000,
          retries: 3,
          degradeThreshold: 0.5,
          recoverThreshold: 0.8
        };
      }

      if (config.health_check_interval !== undefined) {
        proxyServer.healthCheck.interval = config.health_check_interval;
      }
      if (config.health_degrade_threshold !== undefined) {
        proxyServer.healthCheck.degradeThreshold = config.health_degrade_threshold;
      }
      if (config.health_recover_threshold !== undefined) {
        proxyServer.healthCheck.recoverThreshold = config.health_recover_threshold;
      }

      // 重启健康检查定时器
      if (proxyServer.healthCheckTimer) {
        clearInterval(proxyServer.healthCheckTimer);
        proxyServer.healthCheckTimer = setInterval(
          () => proxyServer.performHealthCheck(),
          proxyServer.healthCheck.interval
        );
      }
    }

    // 更新快速失败配置
    if (config.failfast_enabled !== undefined ||
      config.failfast_max_attempts !== undefined ||
      config.failfast_attempt_timeout !== undefined ||
      config.failfast_total_timeout !== undefined) {

      if (!proxyServer.failFast) {
        proxyServer.failFast = {
          enabled: true,
          maxAttempts: 3,
          attemptTimeout: 10000,
          totalTimeout: 30000,
          betweenAttempts: 500
        };
      }

      if (config.failfast_enabled !== undefined) {
        proxyServer.failFast.enabled = config.failfast_enabled;
      }
      if (config.failfast_max_attempts !== undefined) {
        proxyServer.failFast.maxAttempts = config.failfast_max_attempts;
      }
      if (config.failfast_attempt_timeout !== undefined) {
        proxyServer.failFast.attemptTimeout = config.failfast_attempt_timeout;
      }
      if (config.failfast_total_timeout !== undefined) {
        proxyServer.failFast.totalTimeout = config.failfast_total_timeout;
      }
    }

    // 更新算法权重
    if (config.algorithm_weights && proxyServer) {
      if (typeof proxyServer.setAlgorithmWeights === 'function') {
        proxyServer.setAlgorithmWeights(config.algorithm_weights);
      } else {
        const weights = { ...config.algorithm_weights };
        const sum = Object.values(weights).reduce((a, b) => a + b, 0);

        if (Math.abs(sum - 1.0) > 0.01) {
          for (let key in weights) {
            weights[key] = weights[key] / sum;
          }
        }

        proxyServer.algorithmWeights = weights;
        // 保存原始权重用于动态调整
        proxyServer.originalWeights = { ...weights };
      }
    }
  }
}

// 代理CRUD操作
app.get('/api/proxies', async (req, res) => {
  try {
    const proxies = await db.all(`
      SELECT p.*,
        (SELECT weight FROM load_stats WHERE proxy_id = p.id ORDER BY timestamp DESC LIMIT 1) as current_weight
      FROM proxies p
      ORDER BY priority ASC, id ASC
    `);

    // 如果是自动模式，添加智能评分信息
    const modeSetting = await db.get("SELECT value FROM settings WHERE key = 'load_mode'");
    if (modeSetting?.value === 'auto' && proxyServer) {
      const stats = proxyServer.getStats();
      const now = Date.now();

      for (const proxy of proxies) {
        const stat = stats.find(s => s.proxyId === proxy.id);
        if (stat) {
          proxy._score = stat.weight;
          proxy._activeConnections = stat.activeConnections;
        }

        // 获取近期请求统计（15分钟内）
        const recentStats = await db.get(`
          SELECT 
            COUNT(*) as total,
            SUM(CASE WHEN success = 1 THEN 1 ELSE 0 END) as success,
            SUM(CASE WHEN success = 0 THEN 1 ELSE 0 END) as failed,
            AVG(CASE WHEN success = 1 THEN response_time END) as avg_rt
          FROM request_logs
          WHERE proxy_id = ? AND created_at >= datetime('now', '-15 minutes')
        `, proxy.id);

        if (recentStats) {
          proxy._recentTotal = recentStats.total || 0;
          proxy._recentSuccess = recentStats.success || 0;
          proxy._recentFails = recentStats.failed || 0;
          proxy._avgSuccRt = Math.round(recentStats.avg_rt || 0);
        }
      }
    }

    res.json(proxies);
  } catch (error) {
    res.status(500).json({ error: error.message });
  }
});

app.get('/api/proxies/:id', async (req, res) => {
  try {
    const proxy = await db.get('SELECT * FROM proxies WHERE id = ?', req.params.id);
    if (!proxy) {
      return res.status(404).json({ error: '代理不存在' });
    }
    res.json(proxy);
  } catch (error) {
    res.status(500).json({ error: error.message });
  }
});

app.post('/api/proxies', async (req, res) => {
  try {
    const { name, type, host, port, username, password, enabled = 1, test_url, test_timeout } = req.body;

    // 验证必填字段
    if (!name || !type || !host || !port) {
      return res.status(400).json({ code: 400, error: '缺少必填字段' });
    }

    // 检查重复
    const existing = await db.get(
      'SELECT id FROM proxies WHERE host = ? AND port = ? AND type = ?',
      [host, port, type]
    );
    if (existing) {
      return res.status(409).json({ code: 409, error: `该代理已存在（${type}://${host}:${port} 已被使用）` });
    }

    const result = await db.run(
      'INSERT INTO proxies (name, type, host, port, username, password, enabled, test_url, test_timeout) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)',
      [name, type, host, port, username, password, enabled, test_url || null, test_timeout || null]
    );

    const newProxy = await db.get('SELECT * FROM proxies WHERE id = ?', result.lastID);
    broadcast('proxy_created', newProxy);
    res.json(newProxy);
  } catch (error) {
    res.status(500).json({ code: 500, error: error.message });
  }
});

app.put('/api/proxies/:id', async (req, res) => {
  try {
    const { name, type, host, port, username, password, enabled, test_url, test_timeout } = req.body;

    await db.run(
      'UPDATE proxies SET name = ?, type = ?, host = ?, port = ?, username = ?, password = ?, enabled = ?, test_url = ?, test_timeout = ? WHERE id = ?',
      [name, type, host, port, username, password, enabled, test_url || null, test_timeout || null, req.params.id]
    );

    const updatedProxy = await db.get('SELECT * FROM proxies WHERE id = ?', req.params.id);
    broadcast('proxy_updated', updatedProxy);
    res.json(updatedProxy);
  } catch (error) {
    res.status(500).json({ error: error.message });
  }
});

app.delete('/api/proxies/:id', async (req, res) => {
  try {
    await db.run('DELETE FROM proxies WHERE id = ?', req.params.id);
    broadcast('proxy_deleted', { id: req.params.id });
    res.json({ message: '代理已删除' });
  } catch (error) {
    res.status(500).json({ error: error.message });
  }
});

// 优先级管理
app.put('/api/proxies/:id/priority', async (req, res) => {
  try {
    const { priority } = req.body;
    await db.run('UPDATE proxies SET priority = ? WHERE id = ?', [priority, req.params.id]);
    res.json({ message: '优先级已更新' });
  } catch (error) {
    res.status(500).json({ error: error.message });
  }
});

app.post('/api/proxies/priorities', async (req, res) => {
  try {
    const { priorities } = req.body;
    for (const [proxyId, priority] of Object.entries(priorities)) {
      await db.run('UPDATE proxies SET priority = ? WHERE id = ?', [priority, proxyId]);
    }
    res.json({ message: '优先级批量更新成功' });
  } catch (error) {
    res.status(500).json({ error: error.message });
  }
});

// 带宽测速功能
async function measureBandwidth (proxy, testUrls) {
  const urls = [
    "https://cms.zjzwfw.gov.cn/ucenter_files/tempfile/logo/logo.svg",
    "https://cms.zjzwfw.gov.cn/ucenter_files/tempfile/logo/logo.svg",
    "https://cms.zjzwfw.gov.cn/ucenter_files/tempfile/logo/logo.svg",
    "https://cms.zjzwfw.gov.cn/ucenter_files/tempfile/logo/logo.svg",
  ];

  try {
    let agent;

    if (proxy.type === 'socks4' || proxy.type === 'socks5') {
      const auth = proxy.username ?
        `${encodeURIComponent(proxy.username)}:${encodeURIComponent(proxy.password)}@` : '';
      const proxyUrl = `${proxy.type}://${auth}${proxy.host}:${proxy.port}`;
      agent = new SocksProxyAgent(proxyUrl);
    } else if (proxy.type === 'http' || proxy.type === 'https') {
      const auth = proxy.username ?
        `${encodeURIComponent(proxy.username)}:${encodeURIComponent(proxy.password)}@` : '';
      const proxyUrl = `${proxy.type}://${auth}${proxy.host}:${proxy.port}`;
      const isHttps = urls[0].startsWith('https');
      agent = isHttps ?
        new HttpsProxyAgent({ proxy: proxyUrl }) :
        new HttpProxyAgent({ proxy: proxyUrl });
    }

    const results = [];
    for (const url of urls) {
      const startTime = Date.now();

      try {
        const response = await axios.get(url, {
          httpAgent: agent,
          httpsAgent: agent,
          timeout: 30000,
          proxy: false,
          responseType: 'arraybuffer'
        });

        const endTime = Date.now();
        const duration = (endTime - startTime) / 1000; // 秒
        const bytes = response.data.length;
        const bits = bytes * 8;
        const bps = bits / duration;

        results.push({
          url,
          bytes,
          duration,
          bps
        });
      } catch (error) {
        console.error(`带宽测试失败 ${url}:`, error.message);
      }
    }

    if (results.length === 0) {
      return { success: false, error: '所有测试都失败' };
    }

    // 计算平均带宽
    const avgBps = results.reduce((sum, r) => sum + r.bps, 0) / results.length;

    return {
      success: true,
      throughputBps: avgBps,
      throughputMbps: avgBps / 1048576,
      results
    };
  } catch (error) {
    return { success: false, error: error.message };
  }
}

// 测试代理
async function testProxy (proxy, testUrl, timeout, measureBandwidthFlag = false) {
  const startTime = Date.now();

  try {
    let agent;

    if (proxy.type === 'socks4' || proxy.type === 'socks5') {
      const auth = proxy.username ?
        `${encodeURIComponent(proxy.username)}:${encodeURIComponent(proxy.password)}@` : '';
      const proxyUrl = `${proxy.type}://${auth}${proxy.host}:${proxy.port}`;
      agent = new SocksProxyAgent(proxyUrl);
    } else if (proxy.type === 'http' || proxy.type === 'https') {
      const auth = proxy.username ?
        `${encodeURIComponent(proxy.username)}:${encodeURIComponent(proxy.password)}@` : '';
      const proxyUrl = `${proxy.type}://${auth}${proxy.host}:${proxy.port}`;
      const isHttps = testUrl.startsWith('https');
      agent = isHttps ?
        new HttpsProxyAgent({ proxy: proxyUrl }) :
        new HttpProxyAgent({ proxy: proxyUrl });
    }

    const response = await axios.get(testUrl, {
      httpAgent: agent,
      httpsAgent: agent,
      timeout,
      proxy: false,
      maxRedirects: 5,
      validateStatus: (status) => status >= 200 && status < 500
    });

    const responseTime = Date.now() - startTime;

    const result = {
      success: true,
      responseTime,
      statusCode: response.status
    };

    // 如果需要测量带宽
    if (measureBandwidthFlag) {
      const bandwidthResult = await measureBandwidth(proxy);
      result.bandwidth = bandwidthResult;
    }

    return result;
  } catch (error) {
    return {
      success: false,
      responseTime: Date.now() - startTime,
      error: error.message
    };
  }
}

app.post('/api/proxies/:id/test', async (req, res) => {
  try {
    const proxy = await db.get('SELECT * FROM proxies WHERE id = ?', req.params.id);
    if (!proxy) {
      return res.status(404).json({ error: '代理不存在' });
    }

    const settings = await db.all('SELECT * FROM settings');
    const settingsMap = {};
    settings.forEach(s => { settingsMap[s.key] = s.value; });

    const globalTestUrl = settingsMap.test_url || 'https://cms.zjzwfw.gov.cn/favicon.ico';
    const globalTimeout = parseInt(settingsMap.timeout || '10') * 1000;

    // 优先使用代理自身配置，否则用全局设置
    const testUrl = proxy.test_url || globalTestUrl;
    const timeout = proxy.test_timeout ? proxy.test_timeout * 1000 : globalTimeout;

    // 更新状态为测试中
    await db.run('UPDATE proxies SET status = ? WHERE id = ?', ['testing', proxy.id]);
    broadcast('proxy_testing', { id: proxy.id });

    // 只进行连通性测试，不测带宽
    const result = await testProxy(proxy, testUrl, timeout, false);

    if (result.success) {
      await db.run(
        'UPDATE proxies SET status = ?, last_test = CURRENT_TIMESTAMP, response_time = ?, success_count = success_count + 1 WHERE id = ?',
        ['active', result.responseTime, proxy.id]
      );

      await logRequest(proxy.id, new URL(testUrl).hostname, 80, true, result.responseTime, null, {
        resultType: 'health_success',
        proxyName: proxy.name,
        proxyType: proxy.type,
        proxyHost: proxy.host,
        proxyPort: proxy.port
      });
    } else {
      await db.run(
        'UPDATE proxies SET status = ?, last_test = CURRENT_TIMESTAMP, response_time = NULL, fail_count = fail_count + 1 WHERE id = ?',
        ['inactive', proxy.id]
      );

      await logRequest(proxy.id, new URL(testUrl).hostname, 80, false, null, result.error, {
        resultType: 'health_failure',
        proxyName: proxy.name,
        proxyType: proxy.type,
        proxyHost: proxy.host,
        proxyPort: proxy.port
      });
    }

    const updatedProxy = await db.get('SELECT * FROM proxies WHERE id = ?', proxy.id);
    broadcast('proxy_tested', { proxy: updatedProxy, result });

    res.json(result);
  } catch (error) {
    res.status(500).json({ error: error.message });
  }
});

// 测试地址历史列表
app.get('/api/test-urls', async (req, res) => {
  try {
    const globalUrl = await db.get("SELECT value FROM settings WHERE key = 'test_url'");
    const proxyUrls = await db.all("SELECT DISTINCT test_url FROM proxies WHERE test_url IS NOT NULL AND test_url != ''");
    const urls = new Set();
    if (globalUrl?.value) urls.add(globalUrl.value);
    proxyUrls.forEach(r => urls.add(r.test_url));
    res.json([...urls]);
  } catch (error) {
    res.status(500).json({ error: error.message });
  }
});

// 代理分组管理
app.get('/api/proxy-groups', async (req, res) => {
  try {
    const groups = await db.all('SELECT * FROM proxy_groups ORDER BY is_default DESC, id ASC');
    for (const group of groups) {
      group.domains = await db.all('SELECT * FROM proxy_group_domains WHERE group_id = ?', group.id);
      group.members = await db.all(`
        SELECT pgm.proxy_id, p.name, p.type, p.host, p.port, p.status, p.enabled
        FROM proxy_group_members pgm
        JOIN proxies p ON p.id = pgm.proxy_id
        WHERE pgm.group_id = ?
      `, group.id);
    }
    res.json(groups);
  } catch (error) {
    res.status(500).json({ error: error.message });
  }
});

app.post('/api/proxy-groups', async (req, res) => {
  try {
    const { name, domains = [], proxy_ids = [], is_default = 0, enabled = 1 } = req.body;
    if (!name) return res.status(400).json({ error: '分组名称不能为空' });

    // 如果设为默认组，先取消其他默认组
    if (is_default) {
      await db.run('UPDATE proxy_groups SET is_default = 0');
    }

    const result = await db.run(
      'INSERT INTO proxy_groups (name, is_default, enabled) VALUES (?, ?, ?)',
      [name, is_default ? 1 : 0, enabled ? 1 : 0]
    );
    const groupId = result.lastID;

    for (const domain of domains) {
      if (domain.trim()) {
        await db.run('INSERT INTO proxy_group_domains (group_id, domain) VALUES (?, ?)', [groupId, domain.trim().toLowerCase()]);
      }
    }
    for (const proxyId of proxy_ids) {
      await db.run('INSERT OR IGNORE INTO proxy_group_members (group_id, proxy_id) VALUES (?, ?)', [groupId, proxyId]);
    }

    const group = await db.get('SELECT * FROM proxy_groups WHERE id = ?', groupId);
    group.domains = await db.all('SELECT * FROM proxy_group_domains WHERE group_id = ?', groupId);
    group.members = await db.all(`
      SELECT pgm.proxy_id, p.name, p.type, p.host, p.port, p.status, p.enabled
      FROM proxy_group_members pgm JOIN proxies p ON p.id = pgm.proxy_id
      WHERE pgm.group_id = ?
    `, groupId);

    broadcast('proxy_group_created', group);
    res.json(group);
  } catch (error) {
    res.status(500).json({ error: error.message });
  }
});

app.put('/api/proxy-groups/:id', async (req, res) => {
  try {
    const { name, domains, proxy_ids, is_default, enabled } = req.body;
    const groupId = req.params.id;

    const existing = await db.get('SELECT * FROM proxy_groups WHERE id = ?', groupId);
    if (!existing) return res.status(404).json({ error: '分组不存在' });

    // 如果设为默认组，先取消其他默认组
    if (is_default) {
      await db.run('UPDATE proxy_groups SET is_default = 0');
    }

    await db.run(
      'UPDATE proxy_groups SET name = ?, is_default = ?, enabled = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ?',
      [name ?? existing.name, is_default !== undefined ? (is_default ? 1 : 0) : existing.is_default, enabled !== undefined ? (enabled ? 1 : 0) : existing.enabled, groupId]
    );

    if (domains !== undefined) {
      await db.run('DELETE FROM proxy_group_domains WHERE group_id = ?', groupId);
      for (const domain of domains) {
        if (domain.trim()) {
          await db.run('INSERT INTO proxy_group_domains (group_id, domain) VALUES (?, ?)', [groupId, domain.trim().toLowerCase()]);
        }
      }
    }

    if (proxy_ids !== undefined) {
      await db.run('DELETE FROM proxy_group_members WHERE group_id = ?', groupId);
      for (const proxyId of proxy_ids) {
        await db.run('INSERT OR IGNORE INTO proxy_group_members (group_id, proxy_id) VALUES (?, ?)', [groupId, proxyId]);
      }
    }

    const group = await db.get('SELECT * FROM proxy_groups WHERE id = ?', groupId);
    group.domains = await db.all('SELECT * FROM proxy_group_domains WHERE group_id = ?', groupId);
    group.members = await db.all(`
      SELECT pgm.proxy_id, p.name, p.type, p.host, p.port, p.status, p.enabled
      FROM proxy_group_members pgm JOIN proxies p ON p.id = pgm.proxy_id
      WHERE pgm.group_id = ?
    `, groupId);

    broadcast('proxy_group_updated', group);
    res.json(group);
  } catch (error) {
    res.status(500).json({ error: error.message });
  }
});

app.delete('/api/proxy-groups/:id', async (req, res) => {
  try {
    await db.run('DELETE FROM proxy_groups WHERE id = ?', req.params.id);
    broadcast('proxy_group_deleted', { id: req.params.id });
    res.json({ message: '分组已删除' });
  } catch (error) {
    res.status(500).json({ error: error.message });
  }
});

// 设置管理
app.get('/api/settings', async (req, res) => {
  try {
    const settings = await db.all('SELECT * FROM settings');
    const result = {};
    settings.forEach(s => { result[s.key] = s.value; });
    res.json(result);
  } catch (error) {
    res.status(500).json({ error: error.message });
  }
});

app.post('/api/settings', async (req, res) => {
  try {
    for (const [key, value] of Object.entries(req.body)) {
      if (key === 'algorithm') {
        const normalized = VALID_ALGORITHMS.has(value) ? value : 'adaptive';
        await db.run(
          'INSERT OR REPLACE INTO settings (key, value) VALUES (?, ?)',
          [key, normalized]
        );

        if (proxyServer) {
          proxyServer.currentAlgorithm = normalized;
          console.log(`切换到${normalized}算法`);
        }
        continue;
      }

      await db.run(
        'INSERT OR REPLACE INTO settings (key, value) VALUES (?, ?)',
        [key, value]
      );
    }
    res.json({ message: '设置已保存' });
  } catch (error) {
    res.status(500).json({ error: error.message });
  }
});

// 获取断路器状态
app.get('/api/stats/circuit-breakers', async (req, res) => {
  try {
    if (!proxyServer) {
      return res.json([]);
    }

    const breakers = [];
    for (const [proxyId, breaker] of proxyServer.circuitBreakers) {
      const proxy = await db.get('SELECT name FROM proxies WHERE id = ?', proxyId);
      breakers.push({
        proxyId,
        proxyName: proxy?.name || 'Unknown',
        ...breaker.getState()
      });
    }

    res.json(breakers);
  } catch (error) {
    res.status(500).json({ error: error.message });
  }
});

// 获取连接池状态
app.get('/api/stats/connection-pools', async (req, res) => {
  try {
    if (!proxyServer || !proxyServer.connectionPool) {
      return res.json({});
    }

    const stats = proxyServer.connectionPool.getStats();
    const result = [];

    for (const [proxyId, stat] of Object.entries(stats)) {
      const proxy = await db.get('SELECT name FROM proxies WHERE id = ?', proxyId);
      result.push({
        proxyId,
        proxyName: proxy?.name || 'Unknown',
        ...stat
      });
    }

    res.json(result);
  } catch (error) {
    res.status(500).json({ error: error.message });
  }
});

// 统计接口
app.get('/api/traffic-logs', async (req, res) => {
  try {
    const logs = await db.all(`
      SELECT
        rl.id,
        rl.proxy_id,
        p.name AS proxy_name,
        p.type AS proxy_type,
        p.host AS proxy_host,
        p.port AS proxy_port,
        rl.target_host,
        rl.target_port,
        rl.success,
        rl.response_time,
        rl.error_message,
        rl.result_type,
        rl.created_at
      FROM request_logs rl
      LEFT JOIN proxies p ON p.id = rl.proxy_id
      ORDER BY rl.id DESC
      LIMIT ?
    `, [TRAFFIC_LOG_LIMIT]);

    res.json(logs);
  } catch (error) {
    res.status(500).json({ error: error.message });
  }
});

app.get('/api/stats/overview', async (req, res) => {
  try {
    const activeProxies = await db.get(
      "SELECT COUNT(*) as count FROM proxies WHERE status = 'active' AND enabled = 1"
    );

    const requestStats = await db.get(`
      SELECT 
        COUNT(*) as total,
        SUM(CASE WHEN success = 1 THEN 1 ELSE 0 END) as success,
        SUM(CASE WHEN success = 0 THEN 1 ELSE 0 END) as failed
      FROM request_logs
      WHERE created_at >= datetime('now', '-24 hours')
    `);

    const avgResponseTime = await db.get(`
      SELECT AVG(response_time) as avg_time
      FROM request_logs
      WHERE success = 1 AND created_at >= datetime('now', '-24 hours')
    `);

    res.json({
      activeProxies: activeProxies.count || 0,
      totalRequests: requestStats?.total || 0,
      successRequests: requestStats?.success || 0,
      failedRequests: requestStats?.failed || 0,
      avgResponseTime: Math.round(avgResponseTime?.avg_time || 0),
      uptime: Math.floor((Date.now() - stats.startTime) / 1000)
    });
  } catch (error) {
    res.status(500).json({ error: error.message });
  }
});

app.get('/api/stats/hourly', async (req, res) => {
  try {
    const hourlyStats = await db.all(`
      SELECT 
        strftime('%Y-%m-%d %H:00', created_at) as hour,
        COUNT(*) as total_requests,
        SUM(CASE WHEN success = 1 THEN 1 ELSE 0 END) as success_requests,
        SUM(CASE WHEN success = 0 THEN 1 ELSE 0 END) as failed_requests,
        AVG(CASE WHEN success = 1 THEN response_time END) as avg_response_time
      FROM request_logs
      WHERE created_at >= datetime('now', '-24 hours')
      GROUP BY hour
      ORDER BY hour DESC
    `);

    res.json(hourlyStats);
  } catch (error) {
    res.status(500).json({ error: error.message });
  }
});

app.get('/api/stats/proxy-usage', async (req, res) => {
  try {
    const usage = await db.all(`
      SELECT 
        p.id,
        p.name,
        p.type,
        COUNT(rl.id) as total_requests,
        SUM(CASE WHEN rl.success = 1 THEN 1 ELSE 0 END) as success_requests
      FROM proxies p
      LEFT JOIN request_logs rl ON p.id = rl.proxy_id 
        AND rl.created_at >= datetime('now', '-24 hours')
      GROUP BY p.id
      ORDER BY total_requests DESC
    `);

    res.json(usage);
  } catch (error) {
    res.status(500).json({ error: error.message });
  }
});

app.get('/api/stats/targets', async (req, res) => {
  try {
    const targets = await db.all(`
      SELECT 
        target_host,
        COUNT(*) as request_count,
        SUM(CASE WHEN success = 1 THEN 1 ELSE 0 END) as success_count,
        AVG(CASE WHEN success = 1 THEN response_time END) as avg_response_time
      FROM request_logs
      WHERE created_at >= datetime('now', '-24 hours')
      GROUP BY target_host
      ORDER BY request_count DESC
      LIMIT 20
    `);

    res.json(targets);
  } catch (error) {
    res.status(500).json({ error: error.message });
  }
});

app.get('/api/stats/failed-targets', async (req, res) => {
  try {
    const failedTargets = await db.all(`
      SELECT 
        target_host || ':' || target_port as target,
        COUNT(*) as fail_count,
        MAX(created_at) as last_fail_time
      FROM request_logs
      WHERE success = 0 AND created_at >= datetime('now', '-24 hours')
      GROUP BY target_host, target_port
      ORDER BY fail_count DESC
      LIMIT 10
    `);

    res.json(failedTargets);
  } catch (error) {
    res.status(500).json({ error: error.message });
  }
});

// 记录请求
async function logRequest (proxyId, targetHost, targetPort, success, responseTime, errorMessage, options = {}) {
  try {
    const normalizedSuccess = success === true ? 1 : success === false ? 0 : null;
    const normalizedResponseTime = typeof responseTime === 'number' ? responseTime : null;
    const resultType = options.resultType || (success === true ? 'success' : success === false ? 'failure' : 'neutral');
    const normalizedError = errorMessage != null ? errorMessage : null;
    const normalizedTargetHost = targetHost != null ? String(targetHost) : null;
    const hasTargetPort = targetPort !== null && targetPort !== undefined && targetPort !== '';
    const parsedTargetPort = Number(targetPort);
    const normalizedTargetPort = hasTargetPort && Number.isFinite(parsedTargetPort) ? parsedTargetPort : null;

    const insertResult = await db.run(
      `INSERT INTO request_logs (proxy_id, target_host, target_port, success, response_time, error_message, result_type)
       VALUES (?, ?, ?, ?, ?, ?, ?)`,
      [proxyId, normalizedTargetHost, normalizedTargetPort, normalizedSuccess, normalizedResponseTime, normalizedError, resultType]
    );

    stats.totalRequests++;
    if (success === true) {
      stats.successRequests++;
    } else if (success === false) {
      stats.failedRequests++;
    }

    if (proxyId) {
      if (success === true) {
        await db.run('UPDATE proxies SET success_count = success_count + 1 WHERE id = ?', [proxyId]);
      } else if (success === false) {
        await db.run('UPDATE proxies SET fail_count = fail_count + 1 WHERE id = ?', [proxyId]);
      }
    }

    broadcast('request_logged', {
      id: insertResult?.lastID || null,
      proxyId,
      proxyName: options.proxyName || null,
      proxyType: options.proxyType || null,
      proxyHost: options.proxyHost || null,
      proxyPort: options.proxyPort || null,
      targetHost: normalizedTargetHost,
      targetPort: normalizedTargetPort,
      success,
      responseTime: normalizedResponseTime,
      errorMessage: normalizedError,
      resultType,
      createdAt: new Date().toISOString()
    });
  } catch (error) {
    console.error('记录请求失败:', error);
  }
}

// 定期测试代理
async function periodicProxyTest () {
  // 防止重复执行
  if (taskRunning.periodicTest) {
    return;
  }

  taskRunning.periodicTest = true;
  const startTime = Date.now();

  console.log('开始定期代理测试...');
  try {
    const proxies = await db.all('SELECT * FROM proxies WHERE enabled = 1');
    const settings = await db.all('SELECT * FROM settings');
    const settingsMap = {};
    settings.forEach(s => { settingsMap[s.key] = s.value; });

    const globalTestUrl = settingsMap.test_url || 'https://cms.zjzwfw.gov.cn/favicon.ico';
    const globalTimeout = parseInt(settingsMap.timeout || '10') * 1000;

    const batchSize = 5;
    const testResults = { total: proxies.length, success: 0, failed: 0 };

    // 获取小时级统计
    const hourlyStats = await db.all(`
      SELECT 
        strftime('%Y-%m-%d %H:00', created_at) as hour,
        COUNT(*) as total_requests,
        SUM(CASE WHEN success = 1 THEN 1 ELSE 0 END) as success_requests,
        SUM(CASE WHEN success = 0 THEN 1 ELSE 0 END) as failed_requests,
        AVG(CASE WHEN success = 1 THEN response_time END) as avg_response_time
      FROM request_logs
      WHERE created_at >= datetime('now', '-24 hours')
      GROUP BY hour
      ORDER BY hour ASC
    `);

    // 获取代理使用统计
    const proxyUsage = await db.all(`
      SELECT 
        p.id,
        p.name,
        p.type,
        COUNT(rl.id) as total_requests,
        SUM(CASE WHEN rl.success = 1 THEN 1 ELSE 0 END) as success_requests
      FROM proxies p
      LEFT JOIN request_logs rl ON p.id = rl.proxy_id 
        AND rl.created_at >= datetime('now', '-24 hours')
      GROUP BY p.id
      ORDER BY total_requests DESC
    `);

    const overview = await db.get(`
      SELECT 
        (SELECT COUNT(*) FROM proxies WHERE status = 'active' AND enabled = 1) as activeProxies,
        (SELECT COUNT(*) FROM request_logs WHERE created_at >= datetime('now', '-24 hours')) as totalRequests,
        (SELECT SUM(CASE WHEN success = 0 THEN 1 ELSE 0 END) FROM request_logs WHERE created_at >= datetime('now', '-24 hours')) as failedRequests,
        (SELECT AVG(response_time) FROM request_logs WHERE success = 1 AND created_at >= datetime('now', '-24 hours')) as avgResponseTime
    `);

    const changes = [];

    for (let i = 0; i < proxies.length; i += batchSize) {
      const batch = proxies.slice(i, i + batchSize);
      const batchPromises = batch.map(async (proxy) => {
        const oldStatus = proxy.status;
        const proxyTestUrl = proxy.test_url || globalTestUrl;
        const proxyTimeout = proxy.test_timeout ? proxy.test_timeout * 1000 : globalTimeout;
        const result = await testProxy(proxy, proxyTestUrl, proxyTimeout);

        if (result.success) {
          await db.run(
            'UPDATE proxies SET status = ?, last_test = CURRENT_TIMESTAMP, response_time = ? WHERE id = ?',
            ['active', result.responseTime, proxy.id]
          );
          testResults.success++;

          if (oldStatus !== 'active') {
            changes.push({ proxyId: proxy.id, oldStatus, newStatus: 'active' });
          }
        } else {
          await db.run(
            'UPDATE proxies SET status = ?, last_test = CURRENT_TIMESTAMP, response_time = NULL WHERE id = ?',
            ['inactive', proxy.id]
          );
          testResults.failed++;

          if (oldStatus !== 'inactive') {
            changes.push({ proxyId: proxy.id, oldStatus, newStatus: 'inactive' });
          }
        }

        return { proxy, result };
      });

      await Promise.all(batchPromises);

      // 广播测试进度
      broadcast('batch_test_completed', {
        batch: Math.min(i + batchSize, proxies.length),
        totalBatches: proxies.length
      });
    }

    // 记录负载统计
    if (proxyServer) {
      const stats = proxyServer.getStats();
      const weights = proxyServer.getWeights();

      for (const stat of stats) {
        const weight = weights.find(w => w.proxyId === stat.proxyId);
        const total = stat.success + stat.failed;
        const successRate = total > 0 ? stat.success / total : 0;

        await db.run(
          `INSERT INTO load_stats (proxy_id, weight, success_rate, avg_response_time, requests_count) VALUES (?, ?, ?, ?, ?)`,
          [stat.proxyId, weight?.weight || 0, successRate, stat.avgResponseTime, total]
        );
      }
    }

    // 更新概览数据
    const updatedOverview = {
      activeProxies: testResults.success,
      totalRequests: overview?.totalRequests || 0,
      failedRequests: overview?.failedRequests || 0,
      avgResponseTime: Math.round(overview?.avgResponseTime || 0)
    };

    const executionTime = Date.now() - startTime;
    console.log(`定期测试完成: ${testResults.success}/${proxies.length} 个代理可用，耗时 ${executionTime}ms`);

    // 广播完成消息，包含统计数据
    broadcast('periodic_test_completed', {
      testResults,
      overview: updatedOverview,
      hourly: hourlyStats,
      proxyUsage: proxyUsage,
      changes: changes.length > 0 ? changes : undefined,
      executionTime
    });

    if (changes.length > 0) {
      broadcast('proxies_status_changed', { changes });
    }
  } catch (error) {
    console.error('定期代理测试失败:', error);
    broadcast('periodic_test_error', { error: error.message });
  } finally {
    // 释放执行锁
    taskRunning.periodicTest = false;

    // 从配置中获取间隔时间
    const config = await loadAdvancedConfig();
    const interval = config.periodic_test_interval || DEFAULT_ADVANCED_CONFIG.periodic_test_interval;

    // 使用setTimeout调度下一次执行，避免堆积
    timers.periodicTest = setTimeout(periodicProxyTest, interval);
  }
}

// 清理旧日志
async function cleanOldLogs () {
  if (taskRunning.cleanLogs) {
    return;
  }

  taskRunning.cleanLogs = true;

  try {
    // 获取配置的保留天数
    const config = await loadAdvancedConfig();
    const logDays = config.log_retention_days || DEFAULT_ADVANCED_CONFIG.log_retention_days;
    const statsDays = config.stats_retention_days || DEFAULT_ADVANCED_CONFIG.stats_retention_days;

    const result = await db.run(
      `DELETE FROM request_logs WHERE created_at < datetime('now', '-${logDays} days')`
    );
    const statsResult = await db.run(
      `DELETE FROM load_stats WHERE timestamp < datetime('now', '-${statsDays} days')`
    );

    console.log(`清理完成: 删除了 ${result.changes || 0} 条请求日志，${statsResult.changes || 0} 条负载统计`);
  } catch (error) {
    console.error('清理日志失败:', error);
  } finally {
    taskRunning.cleanLogs = false;
    // 24小时后再次执行
    timers.cleanLogs = setTimeout(cleanOldLogs, 24 * 60 * 60 * 1000);
  }
}

// 辅助函数：加载高级配置
async function loadAdvancedConfig () {
  const configKeys = Object.keys(DEFAULT_ADVANCED_CONFIG);
  const settings = await db.all('SELECT key, value FROM settings WHERE key IN (' + configKeys.map(() => '?').join(',') + ')', configKeys);

  const config = { ...DEFAULT_ADVANCED_CONFIG };

  settings.forEach(setting => {
    try {
      config[setting.key] = JSON.parse(setting.value);
    } catch {
      const numValue = Number(setting.value);
      config[setting.key] = isNaN(numValue) ? setting.value : numValue;
    }
  });

  return config;
}

// 应用配置到代理服务器
function applyProxyServerConfig (proxyServer, config) {
  // 设置连接池配置
  proxyServer.connectionPool.maxSize = config.pool_max_size;
  proxyServer.connectionPool.maxIdleTime = config.pool_idle_timeout;

  // 设置熔断器默认配置
  proxyServer.circuitBreakerConfig = {
    threshold: config.circuit_failure_threshold,
    timeout: config.circuit_timeout,
    halfOpenAttempts: config.circuit_half_open_attempts
  };

  // 设置健康检查配置
  proxyServer.healthCheck.interval = config.health_check_interval;
  proxyServer.healthCheck.degradeThreshold = config.health_degrade_threshold;
  proxyServer.healthCheck.recoverThreshold = config.health_recover_threshold;

  // 设置快速失败配置
  proxyServer.failFast = {
    enabled: config.failfast_enabled,
    maxAttempts: config.failfast_max_attempts,
    attemptTimeout: config.failfast_attempt_timeout,
    totalTimeout: config.failfast_total_timeout,
    betweenAttempts: 500
  };

  // 设置算法权重
  if (config.algorithm_weights) {
    if (typeof proxyServer.setAlgorithmWeights === 'function') {
      proxyServer.setAlgorithmWeights(config.algorithm_weights);
    } else {
      const weights = { ...config.algorithm_weights };
      const sum = Object.values(weights).reduce((a, b) => a + b, 0);

      if (Math.abs(sum - 1.0) > 0.01) {
        for (let key in weights) {
          weights[key] = weights[key] / sum;
        }
      }

      proxyServer.algorithmWeights = weights;
      proxyServer.originalWeights = { ...weights };
    }
  }
}

// DNS映射管理API
// 获取所有DNS映射
app.get('/api/dns-mappings', async (req, res) => {
  try {
    const mappings = await db.all('SELECT * FROM dns_mappings ORDER BY domain ASC');
    res.json(mappings);
  } catch (error) {
    res.status(500).json({ error: error.message });
  }
});

// 创建DNS映射
app.post('/api/dns-mappings', async (req, res) => {
  try {
    const { domain, ip, description } = req.body;

    if (!domain || !ip) {
      return res.status(400).json({ error: '域名和IP地址不能为空' });
    }

    // 验证IP格式
    const ipRegex = /^(\d{1,3}\.){3}\d{1,3}$/;
    if (!ipRegex.test(ip)) {
      return res.status(400).json({ error: 'IP地址格式不正确' });
    }

    const result = await db.run(
      'INSERT INTO dns_mappings (domain, ip, description) VALUES (?, ?, ?)',
      [domain.toLowerCase(), ip, description || null]
    );

    // 重新加载DNS映射缓存
    if (proxyServer) {
      await proxyServer.loadDNSMappings();
      // 清除连接池，确保新的DNS映射立即生效
      proxyServer.connectionPool.cleanup();
      // 关闭该域名的现存隧道，确保“秒级”生效
      if (typeof proxyServer.flushConnectionsForDomain === 'function') {
        proxyServer.flushConnectionsForDomain(domain.toLowerCase());
      }
    }

    broadcast('dns_mapping_added', { id: result.lastID, domain, ip, description });
    res.json({ id: result.lastID, domain, ip, description, enabled: 1 });
  } catch (error) {
    if (error.message.includes('UNIQUE')) {
      res.status(400).json({ error: '该域名已存在' });
    } else {
      res.status(500).json({ error: error.message });
    }
  }
});

// 更新DNS映射
app.put('/api/dns-mappings/:id', async (req, res) => {
  try {
    const { id } = req.params;
    const { domain, ip, description, enabled } = req.body;
    // 取旧域名，便于更新后刷新旧/新两侧连接
    const before = await db.get('SELECT domain FROM dns_mappings WHERE id = ?', [id]);

    if (!domain || !ip) {
      return res.status(400).json({ error: '域名和IP地址不能为空' });
    }

    // 验证IP格式
    const ipRegex = /^(\d{1,3}\.){3}\d{1,3}$/;
    if (!ipRegex.test(ip)) {
      return res.status(400).json({ error: 'IP地址格式不正确' });
    }

    await db.run(
      'UPDATE dns_mappings SET domain = ?, ip = ?, description = ?, enabled = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ?',
      [domain.toLowerCase(), ip, description || null, enabled !== undefined ? enabled : 1, id]
    );

    // 重新加载DNS映射缓存
    if (proxyServer) {
      await proxyServer.loadDNSMappings();
      // 清除连接池，确保DNS映射修改立即生效
      proxyServer.connectionPool.cleanup();
      // 关闭旧/新域名相关的现存隧道
      if (typeof proxyServer.flushConnectionsForDomain === 'function') {
        if (before?.domain) proxyServer.flushConnectionsForDomain(String(before.domain).toLowerCase());
        proxyServer.flushConnectionsForDomain(domain.toLowerCase());
      }
    }

    broadcast('dns_mapping_updated', { id, domain, ip, description, enabled });
    res.json({ success: true });
  } catch (error) {
    if (error.message.includes('UNIQUE')) {
      res.status(400).json({ error: '该域名已存在' });
    } else {
      res.status(500).json({ error: error.message });
    }
  }
});

// 删除DNS映射
app.delete('/api/dns-mappings/:id', async (req, res) => {
  try {
    const { id } = req.params;
    const before = await db.get('SELECT domain FROM dns_mappings WHERE id = ?', [id]);
    await db.run('DELETE FROM dns_mappings WHERE id = ?', [id]);

    // 重新加载DNS映射缓存
    if (proxyServer) {
      await proxyServer.loadDNSMappings();
      // 清除连接池，确保DNS映射删除立即生效
      proxyServer.connectionPool.cleanup();
      // 关闭该域名的现存隧道
      if (typeof proxyServer.flushConnectionsForDomain === 'function' && before?.domain) {
        proxyServer.flushConnectionsForDomain(String(before.domain).toLowerCase());
      }
    }

    broadcast('dns_mapping_deleted', { id });
    res.json({ success: true });
  } catch (error) {
    res.status(500).json({ error: error.message });
  }
});

// 切换DNS映射启用状态
app.put('/api/dns-mappings/:id/toggle', async (req, res) => {
  try {
    const { id } = req.params;
    const mapping = await db.get('SELECT domain, enabled FROM dns_mappings WHERE id = ?', [id]);

    if (!mapping) {
      return res.status(404).json({ error: 'DNS映射不存在' });
    }

    const newEnabled = mapping.enabled === 1 ? 0 : 1;
    await db.run('UPDATE dns_mappings SET enabled = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ?', [newEnabled, id]);

    // 重新加载DNS映射缓存
    if (proxyServer) {
      await proxyServer.loadDNSMappings();
      // 清除连接池，确保DNS映射启用/禁用立即生效
      proxyServer.connectionPool.cleanup();
      // 关闭该域名的现存隧道
      if (typeof proxyServer.flushConnectionsForDomain === 'function' && mapping?.domain) {
        proxyServer.flushConnectionsForDomain(String(mapping.domain).toLowerCase());
      }
    }

    broadcast('dns_mapping_toggled', { id, enabled: newEnabled });
    res.json({ success: true, enabled: newEnabled });
  } catch (error) {
    res.status(500).json({ error: error.message });
  }
});

// 版本信息API
app.get('/api/version', (req, res) => {
  try {
    const versionInfo = getVersion();
    res.json(versionInfo);
  } catch (error) {
    res.status(500).json({ error: error.message });
  }
});

// 启动服务器
async function startServer () {
  await initDatabase();

  // 打印版本信息
  printVersion();

  // 加载高级配置
  const advancedConfig = await loadAdvancedConfig();

  const server = app.listen(PORT, () => {
    console.log(`管理界面运行在 http://localhost:${PORT}`);
  });

  // 初始化WebSocket
  wss = new WebSocket.Server({ server });
  wss.on('connection', (ws) => {
    wsClients.add(ws);
    ws.on('close', () => wsClients.delete(ws));
    ws.on('error', () => wsClients.delete(ws));

    ws.send(JSON.stringify({
      type: 'connected',
      timestamp: Date.now()
    }));
  });

  // 启动代理服务器，使用配置的端口
  const proxyPort = advancedConfig.proxy_port;
  proxyServer = new ProxyLoadBalancer(db, logRequest);

  // 应用高级配置到代理服务器
  applyProxyServerConfig(proxyServer, advancedConfig);

  // 从设置中加载算法
  const algorithmSetting = await db.get("SELECT value FROM settings WHERE key = 'algorithm'");
  if (algorithmSetting) {
    const normalized = VALID_ALGORITHMS.has(algorithmSetting.value) ? algorithmSetting.value : 'adaptive';
    proxyServer.currentAlgorithm = normalized;
    if (normalized !== algorithmSetting.value) {
      await db.run(
        'INSERT OR REPLACE INTO settings (key, value) VALUES (?, ?)',
        ['algorithm', normalized]
      );
    }
  }

  await proxyServer.start(proxyPort);

  // 使用配置的间隔启动定时任务
  const testInterval = advancedConfig.periodic_test_interval || DEFAULT_ADVANCED_CONFIG.periodic_test_interval;
  timers.periodicTest = setTimeout(periodicProxyTest, testInterval);
  timers.cleanLogs = setTimeout(cleanOldLogs, 60 * 60 * 1000);
}

// 优雅退出
process.on('SIGINT', gracefulExit);
process.on('SIGTERM', gracefulExit);

function gracefulExit () {
  // 清理所有定时器
  Object.entries(timers).forEach(([name, timer]) => {
    if (timer) {
      clearTimeout(timer);
      clearInterval(timer);
      timers[name] = null;
    }
  });

  // 标记任务为非运行状态
  Object.keys(taskRunning).forEach(key => {
    taskRunning[key] = false;
  });

  // 关闭代理服务器
  if (proxyServer) {
    proxyServer.stop();
  }

  // 关闭WebSocket服务器
  if (wss) {
    wsClients.forEach(client => {
      try {
        client.close();
      } catch (e) {
        // 忽略关闭错误
      }
    });
    wsClients.clear();
    wss.close(() => {
      console.log('WebSocket服务器已关闭');
    });
  }

  // 关闭数据库连接
  if (db) {
    db.close()
      .then(() => {
        console.log('数据库连接已关闭');
        process.exit(0);
      })
      .catch(err => {
        console.error('关闭数据库时出错:', err);
        process.exit(1);
      });
  } else {
    process.exit(0);
  }
}

// 启动应用
startServer().catch(console.error);
