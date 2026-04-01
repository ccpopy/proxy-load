const express = require('express');

module.exports = function ({ db, broadcast, getProxyServer, testProxy, logRequest }) {
  const router = express.Router();

  // 获取所有代理
  router.get('/', async (req, res) => {
    try {
      const proxies = await db.all(`
        SELECT p.*,
          (SELECT weight FROM load_stats WHERE proxy_id = p.id ORDER BY timestamp DESC LIMIT 1) as current_weight
        FROM proxies p
        ORDER BY priority ASC, id ASC
      `);

      // 如果是自动模式，添加智能评分信息
      const modeSetting = await db.get("SELECT value FROM settings WHERE key = 'load_mode'");
      const proxyServer = getProxyServer();
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

  // 获取单个代理
  router.get('/:id', async (req, res) => {
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

  // 创建代理
  router.post('/', async (req, res) => {
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

  // 更新代理
  router.put('/:id', async (req, res) => {
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

  // 删除代理
  router.delete('/:id', async (req, res) => {
    try {
      await db.run('DELETE FROM proxies WHERE id = ?', req.params.id);
      broadcast('proxy_deleted', { id: req.params.id });
      res.json({ message: '代理已删除' });
    } catch (error) {
      res.status(500).json({ error: error.message });
    }
  });

  // 更新单个优先级
  router.put('/:id/priority', async (req, res) => {
    try {
      const { priority } = req.body;
      await db.run('UPDATE proxies SET priority = ? WHERE id = ?', [priority, req.params.id]);
      res.json({ message: '优先级已更新' });
    } catch (error) {
      res.status(500).json({ error: error.message });
    }
  });

  // 批量更新优先级
  router.post('/priorities', async (req, res) => {
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

  // 测试代理
  router.post('/:id/test', async (req, res) => {
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

  return router;
};
