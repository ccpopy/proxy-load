const { DEFAULT_ADVANCED_CONFIG } = require('./constants');

function createScheduler ({ db, broadcast, getProxyServer, testProxy, loadAdvancedConfig }) {
  const timers = { periodicTest: null, cleanLogs: null, statsUpdate: null };
  const taskRunning = { periodicTest: false, cleanLogs: false };

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
      const proxyServer = getProxyServer();
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

  function start (testInterval) {
    timers.periodicTest = setTimeout(periodicProxyTest, testInterval);
    timers.cleanLogs = setTimeout(cleanOldLogs, 60 * 60 * 1000);
  }

  function stop () {
    Object.entries(timers).forEach(([name, timer]) => {
      if (timer) {
        clearTimeout(timer);
        clearInterval(timer);
        timers[name] = null;
      }
    });
    Object.keys(taskRunning).forEach(key => {
      taskRunning[key] = false;
    });
  }

  return { start, stop, timers, periodicProxyTest };
}

module.exports = { createScheduler };
