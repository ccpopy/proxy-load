const { DEFAULT_ADVANCED_CONFIG } = require('./constants');

function restartHealthCheckTimer (proxyServer) {
  if (!proxyServer.healthCheckTimer) return;

  clearInterval(proxyServer.healthCheckTimer);
  proxyServer.healthCheckTimer = setInterval(
    () => proxyServer.performHealthCheck(),
    proxyServer.healthCheck.interval
  );
}

function createConfigManager ({ db, getProxyServer, getSchedulerTimers, getPeriodicProxyTest }) {
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

  function applyRuntimeConfig (config) {
    // 更新定时器
    if (config.periodic_test_interval !== undefined) {
      const timers = getSchedulerTimers();
      if (timers && timers.periodicTest) {
        clearTimeout(timers.periodicTest);
        timers.periodicTest = setTimeout(getPeriodicProxyTest(), config.periodic_test_interval);
      }
    }

    const proxyServer = getProxyServer();

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
        if (config.pool_wait_timeout !== undefined) {
          proxyServer.connectionPool.maxWaitTime = config.pool_wait_timeout;
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
        restartHealthCheckTimer(proxyServer);
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
          throw new Error('代理服务器缺少算法权重配置方法');
        }
      }
    }
  }

  // 应用配置到代理服务器（初始化时使用）
  function applyProxyServerConfig (proxyServerInstance, config) {
    // 设置连接池配置
    proxyServerInstance.connectionPool.maxSize = config.pool_max_size;
    proxyServerInstance.connectionPool.maxIdleTime = config.pool_idle_timeout;
    proxyServerInstance.connectionPool.maxWaitTime = config.pool_wait_timeout;

    // 设置熔断器默认配置
    proxyServerInstance.circuitBreakerConfig = {
      threshold: config.circuit_failure_threshold,
      timeout: config.circuit_timeout,
      halfOpenAttempts: config.circuit_half_open_attempts
    };

    // 设置健康检查配置
    proxyServerInstance.healthCheck.interval = config.health_check_interval;
    proxyServerInstance.healthCheck.degradeThreshold = config.health_degrade_threshold;
    proxyServerInstance.healthCheck.recoverThreshold = config.health_recover_threshold;
    restartHealthCheckTimer(proxyServerInstance);

    // 设置快速失败配置
    proxyServerInstance.failFast = {
      enabled: config.failfast_enabled,
      maxAttempts: config.failfast_max_attempts,
      attemptTimeout: config.failfast_attempt_timeout,
      totalTimeout: config.failfast_total_timeout,
      betweenAttempts: 500
    };

    // 设置算法权重
    if (config.algorithm_weights) {
      if (typeof proxyServerInstance.setAlgorithmWeights === 'function') {
        proxyServerInstance.setAlgorithmWeights(config.algorithm_weights);
      } else {
        throw new Error('代理服务器缺少算法权重配置方法');
      }
    }
  }

  return { loadAdvancedConfig, applyRuntimeConfig, applyProxyServerConfig };
}

module.exports = { createConfigManager };
