const express = require('express');

module.exports = function ({ db, getProxyServer, stats }) {
  const router = express.Router();

  // 获取断路器状态
  router.get('/circuit-breakers', async (req, res) => {
    try {
      const proxyServer = getProxyServer();
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
  router.get('/connection-pools', async (req, res) => {
    try {
      const proxyServer = getProxyServer();
      if (!proxyServer || !proxyServer.connectionPool) {
        return res.json({});
      }

      const poolStats = proxyServer.connectionPool.getStats();
      const result = [];

      for (const [proxyId, stat] of Object.entries(poolStats)) {
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

  // 系统概览
  router.get('/overview', async (req, res) => {
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

  // 小时统计
  router.get('/hourly', async (req, res) => {
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

  // 代理使用统计
  router.get('/proxy-usage', async (req, res) => {
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

  // 目标统计
  router.get('/targets', async (req, res) => {
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

  // 失败目标排行
  router.get('/failed-targets', async (req, res) => {
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

  return router;
};
