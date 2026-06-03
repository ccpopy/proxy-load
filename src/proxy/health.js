// 健康检查、性能评估、指标管理
// 所有方法作为 ProxyLoadBalancer.prototype mixin

const ALGORITHM_WEIGHT_KEYS = ['responseTime', 'successRate', 'connections', 'stability', 'recentPerf'];

module.exports = {
  // ── 监控 ──

  startMonitoring () {
    this.healthCheckTimer = setInterval(() => this.performHealthCheck(), this.healthCheck.interval);
    this.performanceTimer = setInterval(() => this.evaluatePerformance(), 5000);
    this.cleanupTimer = setInterval(() => this.cleanupMetrics(), 60000);
    this.loadDNSMappings();
  },

  async performHealthCheck () {
    const proxies = await this.getEnabledProxies();

    for (const proxy of proxies) {
      const metrics = this.getProxyMetrics(proxy.id);
      if (!metrics) continue;

      if (metrics.successRate < this.healthCheck.degradeThreshold) {
        await this.degradeProxy(proxy.id);
      } else if (metrics.successRate > this.healthCheck.recoverThreshold) {
        await this.recoverProxy(proxy.id);
      }
    }
  },

  async degradeProxy (proxyId) {
    const poolInfo = this.proxyPool.get(proxyId);
    if (poolInfo) {
      poolInfo.degraded = true;
      poolInfo.degradedAt = Date.now();
    }
  },

  async recoverProxy (proxyId) {
    const poolInfo = this.proxyPool.get(proxyId);
    if (poolInfo && poolInfo.degraded) {
      delete poolInfo.degraded;
      delete poolInfo.degradedAt;
    }
  },

  // ── 性能评估 ──

  evaluatePerformance () {
    const totalRequests = this.getTotalRequests();
    const avgSuccess = this.getAverageSuccessRate();

    if (!this.originalWeights) {
      this.originalWeights = { ...this.algorithmWeights };
    }

    let weights = { ...this.originalWeights };

    if (avgSuccess < 0.7) {
      const adjustment = 0.15;
      weights.successRate = Math.min(0.40, weights.successRate + adjustment);

      const toReduce = adjustment;
      const otherWeights = ['responseTime', 'connections', 'stability', 'recentPerf'];
      const reductionEach = toReduce / otherWeights.length;

      otherWeights.forEach(key => {
        weights[key] = Math.max(0.05, weights[key] - reductionEach);
      });

    } else if (avgSuccess > 0.95) {
      const adjustment = 0.10;
      weights.responseTime = Math.min(0.40, weights.responseTime + adjustment);
      weights.successRate = Math.max(0.10, weights.successRate - adjustment);
    }

    if (totalRequests > 1000) {
      const adjustment = 0.10;
      weights.connections = Math.min(0.35, weights.connections + adjustment);

      const otherWeights = ['responseTime', 'stability', 'recentPerf'];
      const reductionEach = adjustment / otherWeights.length;

      otherWeights.forEach(key => {
        weights[key] = Math.max(0.05, weights[key] - reductionEach);
      });
    }

    const sum = Object.values(weights).reduce((a, b) => a + b, 0);
    if (Math.abs(sum - 1.0) > 0.001) {
      for (let key in weights) {
        weights[key] = weights[key] / sum;
      }
    }

    this.algorithmWeights = weights;
  },

  setAlgorithmWeights (weights) {
    const nextWeights = {};
    for (const key of ALGORITHM_WEIGHT_KEYS) {
      const value = Number(weights?.[key]);
      nextWeights[key] = Number.isFinite(value) && value >= 0
        ? value
        : this.algorithmWeights[key];
    }

    const sum = Object.values(nextWeights).reduce((a, b) => a + b, 0);
    if (sum <= 0) {
      throw new Error('算法权重总和必须大于 0');
    }
    if (Math.abs(sum - 1.0) > 0.01) {
      for (const key of ALGORITHM_WEIGHT_KEYS) {
        nextWeights[key] = nextWeights[key] / sum;
      }
    }

    this.algorithmWeights = { ...nextWeights };
    this.originalWeights = { ...nextWeights };
  },

  getTotalRequests () {
    let total = 0;
    for (const [_, poolInfo] of this.proxyPool) {
      total += poolInfo.metrics?.totalRequests || 0;
    }
    return total;
  },

  getAverageSuccessRate () {
    const rates = [];
    for (const [_, poolInfo] of this.proxyPool) {
      if (poolInfo.metrics?.successRate !== undefined) {
        rates.push(poolInfo.metrics.successRate);
      }
    }
    return rates.length > 0 ? rates.reduce((a, b) => a + b, 0) / rates.length : 0;
  },

  // ── 指标管理 ──

  getProxyMetrics (proxyId) {
    const poolInfo = this.proxyPool.get(proxyId);
    if (!poolInfo) {
      this.proxyPool.set(proxyId, {
        requests: [],
        metrics: {},
        lastUsed: 0,
        score: 50
      });
    }

    return this.proxyPool.get(proxyId).metrics;
  },

  updateProxyPool (proxyId, updates) {
    const current = this.proxyPool.get(proxyId) || {};
    this.proxyPool.set(proxyId, { ...current, ...updates });
  },

  recordRequest (proxyId, success, responseTime = null, metadata = {}) {
    const poolInfo = this.proxyPool.get(proxyId) || {
      requests: [],
      metrics: {},
      lastUsed: 0
    };

    const now = Date.now();

    poolInfo.requests.push({
      timestamp: now,
      success,
      responseTime,
      metadata
    });

    poolInfo.requests = poolInfo.requests.filter(
      r => now - r.timestamp < this.windows.long
    );

    poolInfo.lastUsed = now;

    this.updateMetrics(proxyId, poolInfo);

    this.proxyPool.set(proxyId, poolInfo);
  },

  updateMetrics (proxyId, poolInfo) {
    const now = Date.now();
    const requests = poolInfo.requests;

    const windows = {};
    for (const [name, duration] of Object.entries(this.windows)) {
      const windowReqs = requests.filter(r => now - r.timestamp < duration);
      windows[name] = this.calculateWindowMetrics(windowReqs);
    }

    const allSuccessReqs = requests.filter(r => r.success === true);
    const responseTimeValues = allSuccessReqs
      .map(r => r.responseTime)
      .filter(rt => rt != null);

    poolInfo.metrics = {
      totalRequests: requests.length,
      successRate: requests.length > 0 ? allSuccessReqs.length / requests.length : 0,
      avgResponseTime: responseTimeValues.length > 0
        ? responseTimeValues.reduce((a, b) => a + b, 0) / responseTimeValues.length
        : 1000,
      minResponseTime: responseTimeValues.length > 0
        ? Math.min(...responseTimeValues)
        : 100,
      maxResponseTime: responseTimeValues.length > 0
        ? Math.max(...responseTimeValues)
        : 5000,
      responseTimeVariance: this.calculateVariance(responseTimeValues),
      windows,
      failStreak: this.calculateFailStreak(requests),
      recentFails: windows.short.failed,
      recentWindow: windows.short,
      historyWindow: windows.long
    };
  },

  calculateWindowMetrics (requests) {
    const success = requests.filter(r => r.success).length;
    const failed = requests.length - success;
    const successReqs = requests.filter(r => r.success === true && r.responseTime);

    return {
      total: requests.length,
      success,
      failed,
      successRate: requests.length > 0 ? success / requests.length : 0,
      avgResponseTime: successReqs.length > 0
        ? successReqs.reduce((sum, r) => sum + r.responseTime, 0) / successReqs.length
        : null
    };
  },

  calculateVariance (values) {
    if (values.length === 0) return 0;
    const mean = values.reduce((a, b) => a + b, 0) / values.length;
    const squaredDiffs = values.map(v => Math.pow(v - mean, 2));
    return squaredDiffs.reduce((a, b) => a + b, 0) / values.length;
  },

  calculateFailStreak (requests) {
    let streak = 0;
    for (let i = requests.length - 1; i >= 0; i--) {
      const request = requests[i];
      if (request.success === false) {
        streak++;
      } else if (request.success === true) {
        break;
      }
    }
    return streak;
  },

  cleanupMetrics () {
    const now = Date.now();
    for (const [proxyId, poolInfo] of this.proxyPool) {
      poolInfo.requests = poolInfo.requests.filter(
        r => now - r.timestamp < this.windows.long * 2
      );

      if (now - poolInfo.lastUsed > 3600000) {
        poolInfo.metrics = {};
        poolInfo.requests = [];
      }
    }
  }
};
