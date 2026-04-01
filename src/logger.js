function createLogger ({ db, broadcast, stats }) {
  return async function logRequest (proxyId, targetHost, targetPort, success, responseTime, errorMessage, options = {}) {
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
  };
}

module.exports = { createLogger };
