const express = require('express');
const path = require('path');
const compression = require('compression');
const WebSocket = require('ws');
const ProxyLoadBalancer = require('./proxyServer');
const { printVersion } = require('./version');

// 模块引入
const { DEFAULT_ADVANCED_CONFIG, VALID_ALGORITHMS } = require('./src/constants');
const { initDatabase } = require('./src/database');
const { testProxy, measureBandwidth } = require('./src/proxyTester');
const { createLogger } = require('./src/logger');
const { createConfigManager } = require('./src/configManager');
const { createScheduler } = require('./src/scheduler');

// 路由模块
const createProxiesRouter = require('./src/routes/proxies');
const createProxyGroupsRouter = require('./src/routes/proxyGroups');
const createSettingsRouter = require('./src/routes/settings');
const createAdvancedConfigRouter = require('./src/routes/advancedConfig');
const createStatsRouter = require('./src/routes/stats');
const createDnsRouter = require('./src/routes/dns');
const createVersionRouter = require('./src/routes/version');

// 常量
const app = express();
const PORT = Number(process.env.PORT) || 3333;

// 中间件
app.use(compression());
app.use(express.json());
app.use(express.static(path.join(__dirname, 'public')));

// 全局变量
let db;
let proxyServer;
let wss;
let scheduler;
const wsClients = new Set();

// 进程内统计
const stats = {
  totalRequests: 0,
  successRequests: 0,
  failedRequests: 0,
  startTime: Date.now()
};

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

// 启动服务器
async function startServer () {
  db = await initDatabase();

  // 打印版本信息
  printVersion();

  // 创建请求日志函数
  const logRequest = createLogger({ db, broadcast, stats });

  // 惰性访问器（proxyServer 在后面才创建）
  const getProxyServer = () => proxyServer;

  // 创建配置管理器（scheduler 在后面才创建，通过 getter 解决循环）
  const configManager = createConfigManager({
    db,
    getProxyServer,
    getSchedulerTimers: () => scheduler?.timers,
    getPeriodicProxyTest: () => scheduler?.periodicProxyTest
  });

  // 创建定时任务调度器
  scheduler = createScheduler({
    db,
    broadcast,
    getProxyServer,
    testProxy,
    loadAdvancedConfig: configManager.loadAdvancedConfig
  });

  // 加载高级配置
  const advancedConfig = await configManager.loadAdvancedConfig();

  // 共享依赖对象
  const deps = {
    db,
    broadcast,
    getProxyServer,
    stats,
    testProxy,
    measureBandwidth,
    logRequest,
    applyRuntimeConfig: configManager.applyRuntimeConfig
  };

  // 挂载路由
  app.use('/api/proxies', createProxiesRouter(deps));
  app.use('/api/proxy-groups', createProxyGroupsRouter(deps));
  app.use('/api/settings', createSettingsRouter(deps));
  app.use('/api/advanced-config', createAdvancedConfigRouter(deps));
  app.use('/api/stats', createStatsRouter(deps));
  app.use('/api/dns-mappings', createDnsRouter(deps));
  app.use('/api/version', createVersionRouter());

  // test-urls 独立路由（不在 /api/proxies 前缀下）
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

  // traffic-logs 挂载在原路径（前端直接调用 /api/traffic-logs）
  const { TRAFFIC_LOG_LIMIT } = require('./src/constants');
  app.get('/api/traffic-logs', async (req, res) => {
    try {
      const logs = await db.all(`
        SELECT rl.id, rl.proxy_id, p.name AS proxy_name, p.type AS proxy_type,
          p.host AS proxy_host, p.port AS proxy_port, rl.target_host, rl.target_port,
          rl.success, rl.response_time, rl.error_message, rl.result_type, rl.created_at
        FROM request_logs rl LEFT JOIN proxies p ON p.id = rl.proxy_id
        ORDER BY rl.id DESC LIMIT ?
      `, [TRAFFIC_LOG_LIMIT]);
      res.json(logs);
    } catch (error) {
      res.status(500).json({ error: error.message });
    }
  });

  // 启动 HTTP 服务器
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

  // 启动代理服务器
  const proxyPort = advancedConfig.proxy_port;
  proxyServer = new ProxyLoadBalancer(db, logRequest);

  // 应用高级配置到代理服务器
  configManager.applyProxyServerConfig(proxyServer, advancedConfig);

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

  // 启动定时任务
  const testInterval = advancedConfig.periodic_test_interval || DEFAULT_ADVANCED_CONFIG.periodic_test_interval;
  scheduler.start(testInterval);
}

// 优雅退出
process.on('SIGINT', gracefulExit);
process.on('SIGTERM', gracefulExit);

function gracefulExit () {
  // 停止定时任务
  if (scheduler) {
    scheduler.stop();
  }

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
