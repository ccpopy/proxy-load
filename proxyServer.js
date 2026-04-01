const net = require('net');
const dns = require('dns').promises;

function getArithmeticName (str) {
  const mapName = {
    "adaptive": "自适应算法",
    "weighted_round_robin": "加权轮询",
    "least_connections": "最小连接数",
    "sticky_host": "会话粘滞（按域名）"
  }
  return mapName[str] || str
}

class CircuitBreaker {
  constructor(threshold = 5, timeout = 60000, halfOpenAttempts = 2) {
    this.failureCount = 0;
    this.successCount = 0;
    this.threshold = threshold;
    this.timeout = timeout;
    this.halfOpenAttempts = halfOpenAttempts;
    this.halfOpenSuccesses = 0;
    this.state = 'CLOSED';
    this.nextAttempt = 0;
    this.lastFailTime = 0;
  }

  canAttempt () {
    if (this.state === 'CLOSED') return true;

    if (this.state === 'OPEN') {
      if (Date.now() >= this.nextAttempt) {
        this.state = 'HALF_OPEN';
        this.halfOpenSuccesses = 0;
        return true;
      }
      return false;
    }

    return this.state === 'HALF_OPEN';
  }

  recordSuccess () {
    this.failureCount = 0;

    if (this.state === 'HALF_OPEN') {
      this.halfOpenSuccesses++;
      if (this.halfOpenSuccesses >= this.halfOpenAttempts) {
        this.state = 'CLOSED';
      }
    } else {
      this.successCount++;
    }
  }

  recordFailure () {
    this.failureCount++;
    this.lastFailTime = Date.now();

    if (this.state === 'HALF_OPEN') {
      this.state = 'OPEN';
      this.nextAttempt = Date.now() + this.timeout;
    } else if (this.failureCount >= this.threshold) {
      this.state = 'OPEN';
      this.nextAttempt = Date.now() + this.timeout;
    }
  }

  getState () {
    return {
      state: this.state,
      failures: this.failureCount,
      canAttempt: this.canAttempt()
    };
  }
}

class ConnectionPool {
  constructor(maxSize = 50, maxIdleTime = 30000) {
    this.pools = new Map();
    this.maxSize = maxSize;
    this.maxIdleTime = maxIdleTime;
    this.waitQueues = new Map();
    this.stats = new Map();
  }

  async getConnection (proxyId, createFn) {
    if (!this.pools.has(proxyId)) {
      this.pools.set(proxyId, []);
      this.waitQueues.set(proxyId, []);
      this.stats.set(proxyId, {
        created: 0,
        reused: 0,
        destroyed: 0,
        current: 0,
        waiting: 0
      });
    }

    const pool = this.pools.get(proxyId);
    const stat = this.stats.get(proxyId);

    for (let i = pool.length - 1; i >= 0; i--) {
      const conn = pool[i];
      if (!conn.inUse && !conn.destroyed && conn.socket && !conn.socket.destroyed) {
        const idleTime = Date.now() - conn.lastUsed;
        if (idleTime < this.maxIdleTime) {
          conn.inUse = true;
          conn.lastUsed = Date.now();
          clearTimeout(conn.idleTimer);
          stat.reused++;
          return conn;
        } else {
          this.destroyConnection(proxyId, i);
        }
      }
    }

    const activeCount = pool.filter(c => c.inUse && !c.destroyed).length;
    if (activeCount >= this.maxSize) {
      return this.waitForConnection(proxyId);
    }

    try {
      const socket = await createFn();
      const conn = {
        socket,
        inUse: true,
        destroyed: false,
        createdAt: Date.now(),
        lastUsed: Date.now(),
        proxyId,
        idleTimer: null
      };

      pool.push(conn);
      stat.created++;
      stat.current++;

      socket.once('close', () => {
        conn.destroyed = true;
        this.removeConnection(proxyId, conn);
      });

      socket.once('error', () => {
        conn.destroyed = true;
        this.removeConnection(proxyId, conn);
      });

      return conn;
    } catch (error) {
      throw error;
    }
  }

  releaseConnection (conn) {
    if (!conn || conn.destroyed) return;

    conn.inUse = false;
    conn.lastUsed = Date.now();

    const stat = this.stats.get(conn.proxyId);
    if (stat) {
      stat.current = Math.max(0, stat.current - 1);
    }

    conn.idleTimer = setTimeout(() => {
      this.removeConnection(conn.proxyId, conn);
    }, this.maxIdleTime);

    this.notifyWaiters(conn.proxyId);
  }

  waitForConnection (proxyId) {
    return new Promise((resolve, reject) => {
      const queue = this.waitQueues.get(proxyId) || [];
      const stat = this.stats.get(proxyId);

      if (stat) stat.waiting++;

      const timer = setTimeout(() => {
        const idx = queue.indexOf(callback);
        if (idx !== -1) {
          queue.splice(idx, 1);
          if (stat) stat.waiting--;
        }
        reject(new Error('Connection wait timeout'));
      }, 10000);

      const callback = (conn) => {
        clearTimeout(timer);
        if (stat) stat.waiting--;
        resolve(conn);
      };

      queue.push(callback);
    });
  }

  notifyWaiters (proxyId) {
    const queue = this.waitQueues.get(proxyId);
    if (!queue || queue.length === 0) return;

    const pool = this.pools.get(proxyId);
    const available = pool.find(c => !c.inUse && !c.destroyed);

    if (available && queue.length > 0) {
      const callback = queue.shift();
      available.inUse = true;
      available.lastUsed = Date.now();
      callback(available);
    }
  }

  removeConnection (proxyId, conn) {
    const pool = this.pools.get(proxyId);
    if (!pool) return;

    const idx = pool.indexOf(conn);
    if (idx !== -1) {
      pool.splice(idx, 1);
      if (conn.idleTimer) {
        clearTimeout(conn.idleTimer);
      }
      if (conn.socket && !conn.socket.destroyed) {
        conn.socket.destroy();
      }

      const stat = this.stats.get(proxyId);
      if (stat) {
        stat.destroyed++;
        stat.current = Math.max(0, stat.current - 1);
      }
    }
  }

  destroyConnection (proxyId, index) {
    const pool = this.pools.get(proxyId);
    if (!pool || !pool[index]) return;

    const conn = pool[index];
    pool.splice(index, 1);

    if (conn.idleTimer) clearTimeout(conn.idleTimer);
    if (conn.socket && !conn.socket.destroyed) {
      conn.socket.destroy();
    }

    const stat = this.stats.get(proxyId);
    if (stat) {
      stat.destroyed++;
      stat.current = Math.max(0, stat.current - 1);
    }
  }

  getStats () {
    const result = {};
    for (const [proxyId, stat] of this.stats) {
      const pool = this.pools.get(proxyId) || [];
      result[proxyId] = {
        ...stat,
        active: pool.filter(c => c.inUse && !c.destroyed).length,
        idle: pool.filter(c => !c.inUse && !c.destroyed).length,
        total: pool.length
      };
    }
    return result;
  }

  cleanup () {
    let totalClosed = 0;
    for (const [proxyId, pool] of this.pools) {
      for (const conn of pool) {
        if (conn.idleTimer) clearTimeout(conn.idleTimer);
        if (conn.socket && !conn.socket.destroyed) {
          conn.socket.destroy();
          totalClosed++;
        }
      }
    }
    this.pools.clear();
    this.waitQueues.clear();
    this.stats.clear();

    if (totalClosed > 0) {
      console.log(`连接池已清理，关闭了 ${totalClosed} 个连接`);
    }
  }
}

class ProxyLoadBalancer {
  constructor(db, logRequest) {
    this.db = db;
    this.logRequest = logRequest;
    this.server = null;
    this.connections = new Set();
    // 跟踪每个客户端连接对应的目标
    this.clientTargets = new Map();
    this.loadMode = 'auto';
    this.circuitBreakers = new Map();
    this.dnsCache = new Map(); // DNS映射缓存

    // 添加配置属性用于动态更新
    this.circuitBreakerConfig = {
      threshold: 5,
      timeout: 60000,
      halfOpenAttempts: 2
    };

    // 修改连接池初始化
    this.connectionPool = new ConnectionPool(50, 30000);

    this.healthCheckTimer = null;
    this.performanceTimer = null;
    this.cleanupTimer = null;

    this.failFast = {
      enabled: true,
      maxAttempts: 3,
      attemptTimeout: 10000,
      totalTimeout: 30000,
      betweenAttempts: 500
    };

    this.algorithmWeights = {
      responseTime: 0.25,
      successRate: 0.20,
      bandwidth: 0.15,
      connections: 0.15,
      stability: 0.15,
      recentPerf: 0.10
    };

    this.proxyPool = new Map();
    this.activeConnections = new Map();

    this.windows = {
      instant: 1000,
      short: 10000,
      medium: 60000,
      long: 300000
    };

    this.algorithms = {
      adaptive: this.adaptiveSelection.bind(this),
      least_connections: this.leastConnections.bind(this),
      weighted_round_robin: this.weightedRoundRobin.bind(this),
      sticky_host: this.stickyHostSelection.bind(this)
    };
    this.allowedAlgorithms = new Set(Object.keys(this.algorithms));

    this.currentAlgorithm = 'adaptive';
    this.roundRobinIndex = 0;

    this.healthCheck = {
      interval: 30000,
      timeout: 5000,
      retries: 3,
      degradeThreshold: 0.5,
      recoverThreshold: 0.8
    };

    this.startMonitoring();
  }

  // 更新配置
  updateConfig (config) {
    if (config.circuitBreakerConfig) {
      this.circuitBreakerConfig = { ...this.circuitBreakerConfig, ...config.circuitBreakerConfig };
      // 清除现有熔断器以应用新配置
      this.circuitBreakers.clear();
    }

    if (config.failFast) {
      this.failFast = { ...this.failFast, ...config.failFast };
    }

    if (config.algorithmWeights) {
      this.algorithmWeights = { ...this.algorithmWeights, ...config.algorithmWeights };
    }

    if (config.healthCheck) {
      this.healthCheck = { ...this.healthCheck, ...config.healthCheck };
      // 重启健康检查定时器
      if (this.healthCheckTimer) {
        clearInterval(this.healthCheckTimer);
        this.healthCheckTimer = setInterval(() => this.performHealthCheck(), this.healthCheck.interval);
      }
    }

    if (config.connectionPool) {
      // 更新连接池配置
      this.connectionPool.maxSize = config.connectionPool.maxSize || this.connectionPool.maxSize;
      this.connectionPool.maxIdleTime = config.connectionPool.maxIdleTime || this.connectionPool.maxIdleTime;
    }
  }

  getCircuitBreaker (proxyId) {
    if (!this.circuitBreakers.has(proxyId)) {
      // 使用配置创建熔断器
      const config = this.circuitBreakerConfig || {
        threshold: 5,
        timeout: 60000,
        halfOpenAttempts: 2
      };

      this.circuitBreakers.set(proxyId, new CircuitBreaker(
        config.threshold,
        config.timeout,
        config.halfOpenAttempts
      ));
    }
    return this.circuitBreakers.get(proxyId);
  }

  startMonitoring () {
    this.healthCheckTimer = setInterval(() => this.performHealthCheck(), this.healthCheck.interval);
    this.performanceTimer = setInterval(() => this.evaluatePerformance(), 5000);
    this.cleanupTimer = setInterval(() => this.cleanupMetrics(), 60000);
    this.loadDNSMappings(); // 初始加载DNS映射
  }

  // 关闭与某个域名相关的现存隧道，使DNS映射变更立即生效
  flushConnectionsForDomain (domain) {
    if (!domain) return 0;
    const key = String(domain).toLowerCase();
    let closed = 0;
    for (const [client, meta] of this.clientTargets.entries()) {
      // 如果需要联动子域名，则改为if (meta.originalHost.toLowerCase() === key || meta.originalHost.toLowerCase().endsWith('.' + key)) { ... }
      if (meta && typeof meta.originalHost === 'string' && meta.originalHost.toLowerCase() === key) {
        try { client.destroy(); } catch (_) { }
        this.clientTargets.delete(client);
        closed++;
      }
    }
    if (closed > 0) {
      // console.log(`[DNS] flushConnectionsForDomain(${key}) closed ${closed} client tunnels`);
    }
    return closed;
  }

  // 加载DNS映射到缓存
  async loadDNSMappings () {
    try {
      const mappings = await this.db.all('SELECT domain, ip FROM dns_mappings WHERE enabled = 1');
      this.dnsCache.clear();
      for (const mapping of mappings) {
        this.dnsCache.set(mapping.domain.toLowerCase(), mapping.ip);
      }
    } catch (error) {
      console.error('加载DNS映射失败:', error);
    }
  }

  // 解析目标地址（支持DNS重写）
  async resolveTarget (request) {
    // 如果是域名类型，检查DNS映射
    if (request.addressType === 0x03) {
      const domain = request.host.toLowerCase();
      const mappedIP = this.dnsCache.get(domain);
      if (mappedIP) {
        return {
          ...request,
          originalHost: request.host,
          host: mappedIP,
          addressType: 0x01,
          dnsRewritten: true
        };
      }
    }
    return request;
  }

  calculateProxyScore (proxyId) {
    const metrics = this.getProxyMetrics(proxyId);
    if (!metrics) return 0;

    let score = 100;

    const rtScore = this.calculateResponseTimeScore(metrics);
    const srScore = this.calculateSuccessRateScore(metrics);
    const bwScore = this.calculateBandwidthScore(metrics);
    const connScore = this.calculateConnectionScore(proxyId);
    const stabScore = this.calculateStabilityScore(metrics);
    const trendScore = this.calculateTrendScore(metrics);

    score =
      rtScore * this.algorithmWeights.responseTime +
      srScore * this.algorithmWeights.successRate +
      bwScore * this.algorithmWeights.bandwidth +
      connScore * this.algorithmWeights.connections +
      stabScore * this.algorithmWeights.stability +
      trendScore * this.algorithmWeights.recentPerf;

    score = this.applyPenalties(proxyId, score, metrics);

    return Math.max(0.01, Math.min(100, score));
  }

  calculateResponseTimeScore (metrics) {
    const avgRt = metrics.avgResponseTime || 1000;
    const minRt = metrics.minResponseTime || 100;

    if (avgRt <= 200) return 100;
    if (avgRt <= 500) return 90 - (avgRt - 200) * 0.1;
    if (avgRt <= 1000) return 70 - (avgRt - 500) * 0.08;
    if (avgRt <= 2000) return 50 - (avgRt - 1000) * 0.03;
    if (avgRt <= 5000) return 30 - (avgRt - 2000) * 0.005;
    return Math.max(10, 30 - Math.log10(avgRt) * 5);
  }

  calculateSuccessRateScore (metrics) {
    const rate = metrics.successRate || 0;

    if (rate < 0.5) return rate * 40;
    if (rate < 0.8) return 20 + (rate - 0.5) * 100;
    if (rate < 0.95) return 50 + (rate - 0.8) * 200;
    return 80 + (rate - 0.95) * 400;
  }

  calculateBandwidthScore (metrics) {
    const bw = metrics.bandwidth || 0;
    if (bw === 0) return 50;

    if (bw >= 100) return 100;
    if (bw >= 50) return 85 + (bw - 50) * 0.3;
    if (bw >= 10) return 60 + (bw - 10) * 0.625;
    if (bw >= 1) return 30 + (bw - 1) * 3.33;
    return bw * 30;
  }

  calculateConnectionScore (proxyId) {
    const active = this.activeConnections.get(proxyId) || 0;
    const capacity = 100;

    const usage = active / capacity;
    if (usage <= 0.3) return 100;
    if (usage <= 0.5) return 90;
    if (usage <= 0.7) return 70;
    if (usage <= 0.9) return 40;
    return 10;
  }

  calculateStabilityScore (metrics) {
    const variance = metrics.responseTimeVariance || 0;
    const avgRt = metrics.avgResponseTime || 1000;

    const cv = avgRt > 0 ? Math.sqrt(variance) / avgRt : 1;

    if (cv <= 0.1) return 100;
    if (cv <= 0.3) return 80;
    if (cv <= 0.5) return 60;
    if (cv <= 1.0) return 30;
    return 10;
  }

  calculateTrendScore (metrics) {
    const recent = metrics.recentWindow || {};
    const history = metrics.historyWindow || {};

    if (!recent.successRate || !history.successRate) return 50;

    const trend = recent.successRate - history.successRate;

    if (trend > 0.2) return 100;
    if (trend > 0.1) return 80;
    if (trend > 0) return 60;
    if (trend > -0.1) return 40;
    if (trend > -0.2) return 20;
    return 0;
  }

  applyPenalties (proxyId, baseScore, metrics) {
    let score = baseScore;

    const failStreak = metrics.failStreak || 0;
    if (failStreak > 0) {
      score *= Math.max(0.1, 1 - failStreak * 0.2);
    }

    const recentFails = metrics.recentFails || 0;
    if (recentFails > 5) {
      score *= 0.5;
    } else if (recentFails > 2) {
      score *= 0.8;
    }

    const lastUsed = this.proxyPool.get(proxyId)?.lastUsed || 0;
    const idleTime = Date.now() - lastUsed;
    if (idleTime > 60000) {
      score *= 1.1;
    }

    return score;
  }

  /**
   * 根据 host 对代理进行稳定哈希排序，用于 sticky_host 算法
   * 确保同一个 host 总是优先使用同一个代理
   */
  orderProxiesByStickyHost (proxies, hostKey) {
    if (!hostKey || proxies.length <= 1) return proxies;

    // 转换为小写确保一致性
    const key = hostKey.toLowerCase();

    // 使用简单的哈希算法计算 host 的哈希值
    let hash = 0;
    for (let i = 0; i < key.length; i++) {
      hash = ((hash << 5) - hash) + key.charCodeAt(i);
      hash |= 0; // 转成 32 位整数
    }

    // 根据哈希值选择首选代理的索引
    const idx = Math.abs(hash) % proxies.length;
    const primary = proxies[idx];
    const rest = proxies.filter((_, i) => i !== idx);

    // 返回"首选 + 备用"的顺序，后续 for...of 会按这个顺序尝试
    return [primary, ...rest];
  }

  async adaptiveSelection (proxies) {
    if (!proxies || proxies.length === 0) return null;

    const scoredProxies = proxies.map(proxy => {
      const score = this.calculateProxyScore(proxy.id);
      this.updateProxyPool(proxy.id, { score, lastEvaluated: Date.now() });
      return { proxy, score };
    });

    scoredProxies.sort((a, b) => b.score - a.score);

    const preferredPool = scoredProxies.filter(p => p.score > 30);
    const selectionPool = preferredPool.length > 0 ? preferredPool : scoredProxies;
    const selected = this.probabilisticSelection(selectionPool);
    const primary = selected?.proxy || scoredProxies[0]?.proxy;

    if (!primary) return null;
    const ordered = scoredProxies.map(item => item.proxy);
    return [primary, ...ordered.filter(p => p.id !== primary.id)];
  }

  probabilisticSelection (scoredProxies) {
    const totalScore = scoredProxies.reduce((sum, p) => sum + p.score, 0);
    const random = Math.random() * totalScore;

    let accumulator = 0;
    for (const item of scoredProxies) {
      accumulator += item.score;
      if (random <= accumulator) {
        return item;
      }
    }

    return scoredProxies[0];
  }

  weightedRoundRobin (proxies) {
    if (!proxies || proxies.length === 0) return null;

    const weightedItems = proxies.map(proxy => {
      const score = this.calculateProxyScore(proxy.id);
      const weight = Math.max(1, Math.round(score / 10));
      return { proxy, score, weight };
    });
    const totalWeight = weightedItems.reduce((sum, item) => sum + item.weight, 0);
    if (totalWeight <= 0) return proxies.slice();

    this.roundRobinIndex = (this.roundRobinIndex + 1) % totalWeight;
    let idx = this.roundRobinIndex;
    let selectedIndex = 0;
    for (let i = 0; i < weightedItems.length; i++) {
      idx -= weightedItems[i].weight;
      if (idx < 0) {
        selectedIndex = i;
        break;
      }
    }

    const selected = weightedItems[selectedIndex];
    const rest = weightedItems
      .filter((_, idx) => idx !== selectedIndex)
      .sort((a, b) => {
        if (b.weight !== a.weight) return b.weight - a.weight;
        return b.score - a.score;
      })
      .map(item => item.proxy);

    return [selected.proxy, ...rest];
  }

  leastConnections (proxies) {
    if (!proxies || proxies.length === 0) return null;

    const ranked = proxies.map(proxy => {
      const connections = this.activeConnections.get(proxy.id) || 0;
      const score = this.calculateProxyScore(proxy.id);
      return { proxy, connections, score };
    });

    ranked.sort((a, b) => {
      if (a.connections !== b.connections) return a.connections - b.connections;
      return b.score - a.score;
    });

    return ranked.map(item => item.proxy);
  }

  stickyHostSelection (proxies, hostKey) {
    const list = Array.isArray(proxies) ? proxies.slice() : [];
    return this.orderProxiesByStickyHost(list, hostKey);
  }
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
  }

  updateProxyPool (proxyId, updates) {
    const current = this.proxyPool.get(proxyId) || {};
    this.proxyPool.set(proxyId, { ...current, ...updates });
  }

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
  }

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
  }

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
  }

  calculateVariance (values) {
    if (values.length === 0) return 0;
    const mean = values.reduce((a, b) => a + b, 0) / values.length;
    const squaredDiffs = values.map(v => Math.pow(v - mean, 2));
    return squaredDiffs.reduce((a, b) => a + b, 0) / values.length;
  }

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
  }

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
  }

  async degradeProxy (proxyId) {
    const poolInfo = this.proxyPool.get(proxyId);
    if (poolInfo) {
      poolInfo.degraded = true;
      poolInfo.degradedAt = Date.now();
    }
  }

  async recoverProxy (proxyId) {
    const poolInfo = this.proxyPool.get(proxyId);
    if (poolInfo && poolInfo.degraded) {
      delete poolInfo.degraded;
      delete poolInfo.degradedAt;
    }
  }

  evaluatePerformance () {
    const totalRequests = this.getTotalRequests();
    const avgSuccess = this.getAverageSuccessRate();

    // 保存原始权重
    if (!this.originalWeights) {
      this.originalWeights = { ...this.algorithmWeights };
    }

    let weights = { ...this.originalWeights };

    // 根据情况调整权重，保持总和为1.0
    if (avgSuccess < 0.7) {
      // 低成功率：增加成功率权重，减少其他权重
      const adjustment = 0.15;
      weights.successRate = Math.min(0.40, weights.successRate + adjustment);

      // 按比例减少其他权重
      const toReduce = adjustment;
      const otherWeights = ['responseTime', 'bandwidth', 'connections', 'stability', 'recentPerf'];
      const reductionEach = toReduce / otherWeights.length;

      otherWeights.forEach(key => {
        weights[key] = Math.max(0.05, weights[key] - reductionEach);
      });

    } else if (avgSuccess > 0.95) {
      // 高成功率：增加响应时间权重，减少成功率权重
      const adjustment = 0.10;
      weights.responseTime = Math.min(0.40, weights.responseTime + adjustment);
      weights.successRate = Math.max(0.10, weights.successRate - adjustment);
    }

    if (totalRequests > 1000) {
      // 高流量：增加连接数权重
      const adjustment = 0.10;
      weights.connections = Math.min(0.35, weights.connections + adjustment);

      // 按比例减少其他权重
      const otherWeights = ['responseTime', 'bandwidth', 'stability', 'recentPerf'];
      const reductionEach = adjustment / otherWeights.length;

      otherWeights.forEach(key => {
        weights[key] = Math.max(0.05, weights[key] - reductionEach);
      });
    }

    // 归一化确保总和为1.0
    const sum = Object.values(weights).reduce((a, b) => a + b, 0);
    if (Math.abs(sum - 1.0) > 0.001) {
      // 归一化
      for (let key in weights) {
        weights[key] = weights[key] / sum;
      }
    }

    // 应用调整后的权重
    this.algorithmWeights = weights;
  }

  // 重置权重
  resetAlgorithmWeights () {
    if (this.originalWeights) {
      this.algorithmWeights = { ...this.originalWeights };
    }
  }

  // 手动设置权重
  setAlgorithmWeights (weights) {
    // 验证总和
    const sum = Object.values(weights).reduce((a, b) => a + b, 0);
    if (Math.abs(sum - 1.0) > 0.01) {
      for (let key in weights) {
        weights[key] = weights[key] / sum;
      }
    }

    this.algorithmWeights = { ...weights };
    this.originalWeights = { ...weights };
  }

  getTotalRequests () {
    let total = 0;
    for (const [_, poolInfo] of this.proxyPool) {
      total += poolInfo.metrics?.totalRequests || 0;
    }
    return total;
  }

  getAverageSuccessRate () {
    const rates = [];
    for (const [_, poolInfo] of this.proxyPool) {
      if (poolInfo.metrics?.successRate !== undefined) {
        rates.push(poolInfo.metrics.successRate);
      }
    }
    return rates.length > 0 ? rates.reduce((a, b) => a + b, 0) / rates.length : 0;
  }

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

  async getEnabledProxies () {
    return await this.db.all(`
      SELECT * FROM proxies 
      WHERE enabled = 1 
      ORDER BY priority ASC, id ASC
    `);
  }

  async resolveTargetDomain (targetHost) {
    // 如果targetHost看起来像IP地址，尝试反查DNS映射表
    const ipPattern = /^(\d{1,3}\.){3}\d{1,3}$/;
    if (ipPattern.test(targetHost)) {
      const mapping = await this.db.get(
        'SELECT domain FROM dns_mappings WHERE ip = ? AND enabled = 1',
        [targetHost]
      );
      if (mapping) return mapping.domain;
    }
    return targetHost;
  }

  async getGroupProxyIds (targetHost) {
    // 解析域名（可能从IP反查）
    const domain = await this.resolveTargetDomain(targetHost);

    // 查找命中的分组
    const matchedGroup = await this.db.get(`
      SELECT pg.id FROM proxy_groups pg
      JOIN proxy_group_domains pgd ON pgd.group_id = pg.id
      WHERE pgd.domain = ? AND pg.enabled = 1
    `, [domain.toLowerCase()]);

    if (matchedGroup) {
      const members = await this.db.all(
        'SELECT proxy_id FROM proxy_group_members WHERE group_id = ?',
        [matchedGroup.id]
      );
      if (members.length > 0) {
        return new Set(members.map(m => m.proxy_id));
      }
    }

    // 未命中 → 查找默认分组
    const defaultGroup = await this.db.get(
      'SELECT id FROM proxy_groups WHERE is_default = 1 AND enabled = 1'
    );
    if (defaultGroup) {
      const members = await this.db.all(
        'SELECT proxy_id FROM proxy_group_members WHERE group_id = ?',
        [defaultGroup.id]
      );
      if (members.length > 0) {
        return new Set(members.map(m => m.proxy_id));
      }
    }

    // 无分组匹配 → 返回null，使用全部代理
    return null;
  }

  async selectProxy (targetHost) {
    const modeSetting = await this.db.get("SELECT value FROM settings WHERE key = 'load_mode'");
    this.loadMode = modeSetting?.value || 'auto';

    const proxies = await this.getEnabledProxies();

    // 根据分组过滤代理
    const groupProxyIds = await this.getGroupProxyIds(targetHost);

    const activeProxies = proxies.filter(p => {
      // 如果有分组限制，只使用分组内的代理
      if (groupProxyIds && !groupProxyIds.has(p.id)) {
        return false;
      }

      const circuitBreaker = this.getCircuitBreaker(p.id);
      if (!circuitBreaker.canAttempt()) {
        return false;
      }

      const poolInfo = this.proxyPool.get(p.id);
      if (poolInfo?.degraded) {
        const degradeDuration = Date.now() - (poolInfo.degradedAt || 0);
        if (degradeDuration < 60000) {
          return false;
        } else {
          this.recoverProxy(p.id);
        }
      }

      return p.status === 'active' || p.status === 'testing' || !p.status;
    });

    if (activeProxies.length === 0) {
      return null;
    }

    if (this.loadMode === 'manual') {
      return activeProxies;
    } else {
      const algorithmKey = this.allowedAlgorithms.has(this.currentAlgorithm) ? this.currentAlgorithm : 'adaptive';
      const algorithm = this.algorithms[algorithmKey] || this.algorithms.adaptive;
      const ordered = await algorithm(activeProxies, targetHost);
      return Array.isArray(ordered) && ordered.length > 0 ? ordered : activeProxies;
    }
  }

  async start (port = 5678) {
    this.server = net.createServer((client) => this.handleConnection(client));

    return new Promise((resolve, reject) => {
      this.server.listen(port, '0.0.0.0', () => {
        console.log(`代理负载均衡服务器运行在 0.0.0.0:${port}`);
        console.log(`当前模式: ${this.loadMode === 'manual' ? '手动模式' : '自动负载均衡'}`);
        console.log(`当前算法: ${getArithmeticName(this.currentAlgorithm)}(${this.currentAlgorithm})`);
        resolve();
      });
      this.server.on('error', reject);
    });
  }

  handleConnection (client) {
    this.connections.add(client);
    const startTime = Date.now();
    let targetHost = null;
    let targetPort = null;
    let selectedProxyId = null;

    const cleanup = () => {
      this.connections.delete(client);
      // 清理该客户端的域名跟踪
      this.clientTargets.delete(client);
      if (selectedProxyId) {
        const current = this.activeConnections.get(selectedProxyId) || 0;
        this.activeConnections.set(selectedProxyId, Math.max(0, current - 1));
      }
    };

    client.on('close', cleanup);
    client.on('error', (err) => {
      cleanup();
      if (this.logRequest && targetHost) {
        this.logRequest(selectedProxyId, targetHost, targetPort, false, Date.now() - startTime, err.message, {
          resultType: 'io_error'
        });
      }
    });

    this.handleSocks5(client, startTime).catch(err => {
      client.destroy();
    });
  }

  async handleSocks5 (client, startTime) {
    let targetHost = null;
    let targetPort = null;

    try {
      client.setTimeout(this.failFast.totalTimeout);

      const handshake = await this.readDataWithTimeout(client, 1000);
      if (!handshake || handshake[0] !== 0x05) {
        this.sendSocks5Error(client, 0x01);
        return;
      }

      client.write(Buffer.from([0x05, 0x00]));

      const requestData = await this.readDataWithTimeout(client, 1000);
      if (!requestData) {
        this.sendSocks5Error(client, 0x01);
        return;
      }

      const request = this.parseSocks5Request(requestData);
      if (!request) {
        this.sendSocks5Error(client, 0x01);
        return;
      }

      targetHost = request.host;
      targetPort = request.port;

      // 应用DNS重写
      const resolvedRequest = await this.resolveTarget(request);

      // 跟踪此客户端连接对应的原始域名
      if (request.addressType === 0x03 && request.host) {
        this.clientTargets.set(client, {
          originalHost: request.host,
          port: request.port,
          resolvedHost: resolvedRequest.host,
          dnsRewritten: !!resolvedRequest.dnsRewritten
        });
      }

      const result = await this.connectWithFailFast(client, resolvedRequest, startTime);

      if (!result.connected) {
        this.sendSocks5Error(client, 0x04);
        if (this.logRequest) {
          this.logRequest(null, targetHost, targetPort, false,
            Date.now() - startTime, result.error || '所有代理连接失败', {
            resultType: 'proxy_exhausted'
          });
        }
      }
    } catch (error) {
      this.sendSocks5Error(client, 0x01);
      if (targetHost && this.logRequest) {
        this.logRequest(null, targetHost, targetPort, false,
          Date.now() - startTime, error.message, {
          resultType: 'proxy_error'
        });
      }
    }
  }

  async connectWithFailFast (client, request, startTime) {
    const totalStartTime = Date.now();

    // 获取当前算法
    const modeSetting = await this.db.get("SELECT value FROM settings WHERE key = 'algorithm'");
    let currentAlgorithm = modeSetting?.value || this.currentAlgorithm;
    if (!this.allowedAlgorithms.has(currentAlgorithm)) {
      currentAlgorithm = 'adaptive';
    }

    // 获取所有可用代理
    const allProxies = await this.getEnabledProxies();
    if (!allProxies || allProxies.length === 0) {
      return { connected: false, error: '没有可用的代理' };
    }

    // 过滤出活跃的代理
    const activeProxies = allProxies.filter(p => {
      const circuitBreaker = this.getCircuitBreaker(p.id);
      return circuitBreaker.canAttempt() && (p.status === 'active' || !p.status);
    });

    // 如果没有活跃代理，尝试使用所有代理
    let proxiesToTry = activeProxies.length > 0 ? activeProxies : allProxies;

    // 为 sticky_host 算法准备 host 关键字
    // request.originalHost 在 DNS 映射生效时存在，否则用 request.host
    const lbHost = (request.originalHost || request.host || '').toLowerCase();

    const algorithm = this.algorithms[currentAlgorithm] || this.algorithms.adaptive;
    const ordered = await algorithm(proxiesToTry, lbHost);
    if (Array.isArray(ordered) && ordered.length > 0) {
      const selectedIds = new Set(ordered.map(p => p.id));
      const rest = proxiesToTry.filter(p => !selectedIds.has(p.id));
      proxiesToTry = [...ordered, ...rest];
    }

    // 其他算法使用原有的失败快速切换逻辑
    const errors = [];

    // 尝试每个代理
    for (const proxy of proxiesToTry) {
      if (Date.now() - totalStartTime > this.failFast.totalTimeout) {
        return { connected: false, error: '总超时：' + errors.join('; ') };
      }

      try {
        const connectPromise = this.attemptProxyConnection(
          client, proxy, request, startTime
        );

        const timeoutPromise = new Promise((_, reject) =>
          setTimeout(() => reject(new Error(`代理${proxy.name}连接超时`)),
            this.failFast.attemptTimeout)
        );

        const result = await Promise.race([connectPromise, timeoutPromise]);

        if (result) {
          return { connected: true, proxyId: proxy.id };
        }

      } catch (error) {
        errors.push(`${proxy.name}: ${error.message}`);

        // 记录失败，触发熔断器
        const circuitBreaker = this.getCircuitBreaker(proxy.id);
        circuitBreaker.recordFailure();

        // 如果不是最后一个代理，等待一下再试下一个
        if (proxiesToTry.indexOf(proxy) < proxiesToTry.length - 1) {
          await this.sleep(this.failFast.betweenAttempts);
        }
      }
    }

    return {
      connected: false,
      error: `所有${proxiesToTry.length}个代理都失败: ${errors.join('; ')}`
    };
  }

  async attemptProxyConnection (client, proxy, request, startTime) {
    let selectedProxyId = proxy.id;

    const current = this.activeConnections.get(proxy.id) || 0;
    this.activeConnections.set(proxy.id, current + 1);

    try {
      const success = await this.connectThroughProxy(client, proxy, request, startTime);

      if (success) {
        const responseTime = Date.now() - startTime;
        this.recordRequest(proxy.id, true, responseTime);
        if (this.logRequest) {
          this.logRequest(proxy.id, request.host, request.port, true, responseTime, null, {
            resultType: 'direct_success',
            proxyName: proxy.name,
            proxyType: proxy.type,
            proxyHost: proxy.host,
            proxyPort: proxy.port
          });
        }
        return true;
      } else {
        throw new Error('连接失败');
      }
    } catch (error) {
      const curr = this.activeConnections.get(proxy.id) || 0;
      this.activeConnections.set(proxy.id, Math.max(0, curr - 1));

      this.recordRequest(proxy.id, false);
      if (this.logRequest) {
        this.logRequest(proxy.id, request.host, request.port, false,
          Date.now() - startTime, error.message, {
          resultType: 'direct_failure',
          proxyName: proxy.name,
          proxyType: proxy.type,
          proxyHost: proxy.host,
          proxyPort: proxy.port
        });
      }

      throw error;
    }
  }

  async getConnectionFromPool (proxyId, createFn, timeout = 5000) {
    return Promise.race([
      this.connectionPool.getConnection(proxyId, createFn),
      new Promise((_, reject) =>
        setTimeout(() => reject(new Error('获取连接超时')), timeout)
      )
    ]);
  }

  async connectThroughProxy (client, proxy, request, reqStartTs) {
    const circuitBreaker = this.getCircuitBreaker(proxy.id);

    try {
      let result;
      if (proxy.type === 'socks5') {
        result = await this.connectThroughSocks5WithPool(client, proxy, request, reqStartTs);
      } else if (proxy.type === 'socks4') {
        result = await this.connectThroughSocks4WithPool(client, proxy, request, reqStartTs);
      } else if (proxy.type === 'http' || proxy.type === 'https') {
        result = await this.connectThroughHttpWithPool(client, proxy, request, reqStartTs);
      }

      if (result) {
        circuitBreaker.recordSuccess();
      } else {
        circuitBreaker.recordFailure();
      }

      return result;
    } catch (error) {
      circuitBreaker.recordFailure();
      throw error;
    }
  }

  async connectThroughSocks5WithPool (client, proxy, request, reqStartTs) {
    try {
      const proxySocket = await this.createSocks5Connection(proxy);

      if (!proxySocket || proxySocket.destroyed) {
        return false;
      }

      proxySocket.setTimeout(500);

      const connectRequest = this.buildSocks5ConnectRequest(request);
      proxySocket.write(connectRequest);

      let connectResponse = await this.readDataWithTimeout(proxySocket, 500);

      if (!connectResponse || connectResponse.length < 10) {
        proxySocket.destroy();
        return false;
      }

      // 检查SOCKS5响应
      if (connectResponse[0] !== 0x05) {
        proxySocket.destroy();
        return false;
      }

      // 状态码检查 - 0x00表示成功
      if (connectResponse[1] !== 0x00) {
        const errorCodes = {
          0x01: '通用SOCKS服务器失败',
          0x02: '规则不允许连接',
          0x03: '网络不可达',
          0x04: '主机不可达',
          0x05: '连接被拒绝',
          0x06: 'TTL过期',
          0x07: '不支持的命令',
          0x08: '不支持的地址类型'
        };
        proxySocket.destroy();
        return false;
      }

      // 发送成功响应给客户端
      client.write(connectResponse);

      // 设置双向管道，并确保错误处理
      client.pipe(proxySocket);
      proxySocket.pipe(client);

      const cleanup = () => {
        try {
          if (!client.destroyed) client.unpipe(proxySocket);
          if (!proxySocket.destroyed) proxySocket.unpipe(client);
          if (!client.destroyed) client.destroy();
          if (!proxySocket.destroyed) proxySocket.destroy();
        } catch (e) {
          // 忽略清理时的错误
        }
      };

      // 使用once避免重复调用
      client.once('error', cleanup);
      client.once('close', cleanup);
      client.once('end', cleanup);
      proxySocket.once('error', (err) => {
        console.error(`代理socket错误: ${err.message}`);
        cleanup();
      });
      proxySocket.once('close', cleanup);
      proxySocket.once('end', cleanup);

      return true;
    } catch (error) {
      return false;
    }
  }

  buildSocks5ConnectRequest (request) {
    if (request.addressType === 0x03) {
      // 域名类型
      const domain = Buffer.from(request.host, 'utf8');
      const port = request.port;

      return Buffer.concat([
        Buffer.from([0x05, 0x01, 0x00, 0x03]),
        Buffer.from([domain.length]),
        domain,
        Buffer.from([
          (port >> 8) & 0xff,
          port & 0xff
        ])
      ]);
    } else if (request.addressType === 0x01) {
      // IPv4地址
      const ipParts = request.host.split('.').map(p => parseInt(p, 10));
      return Buffer.from([
        0x05, 0x01, 0x00, 0x01,
        ...ipParts,
        (request.port >> 8) & 0xff,
        request.port & 0xff
      ]);
    } else if (request.addressType === 0x04) {
      // IPv6地址 - 暂不支持
      throw new Error('暂不支持IPv6地址');
    }

    throw new Error(`不支持的地址类型: ${request.addressType}`);
  }

  setupBidirectionalPipe (client, proxySocket, conn) {
    client.pipe(proxySocket);
    proxySocket.pipe(client);

    const cleanup = () => {
      if (!client.destroyed) client.destroy();
      if (!proxySocket.destroyed) proxySocket.destroy();
      if (conn) this.connectionPool.releaseConnection(conn);
    };

    client.once('error', cleanup);
    client.once('close', cleanup);
    client.once('end', cleanup);
    proxySocket.once('error', cleanup);
    proxySocket.once('close', cleanup);
    proxySocket.once('end', cleanup);
  }

  async createSocks5Connection (proxy) {
    return new Promise((resolve, reject) => {
      const socket = net.createConnection({
        host: proxy.host,
        port: proxy.port
      });

      const timeout = setTimeout(() => {
        socket.destroy();
        reject(new Error('Connection timeout'));
      }, 10000);

      socket.once('connect', async () => {
        try {
          const authMethods = (proxy.username && proxy.password) ?
            [0x00, 0x02] : [0x00];
          socket.write(Buffer.from([0x05, authMethods.length, ...authMethods]));

          const handshakeResponse = await this.readData(socket);
          if (!handshakeResponse || handshakeResponse[0] !== 0x05) {
            socket.destroy();
            clearTimeout(timeout);
            return reject(new Error('SOCKS5 handshake failed'));
          }

          if (handshakeResponse[1] === 0x02 && proxy.username && proxy.password) {
            const authBuffer = Buffer.concat([
              Buffer.from([0x01]),
              Buffer.from([proxy.username.length]),
              Buffer.from(proxy.username),
              Buffer.from([proxy.password.length]),
              Buffer.from(proxy.password)
            ]);
            socket.write(authBuffer);

            const authResponse = await this.readData(socket);
            if (!authResponse || authResponse[1] !== 0x00) {
              socket.destroy();
              clearTimeout(timeout);
              return reject(new Error('SOCKS5 authentication failed'));
            }
          }

          clearTimeout(timeout);
          resolve(socket);
        } catch (error) {
          clearTimeout(timeout);
          socket.destroy();
          reject(error);
        }
      });

      socket.once('error', (err) => {
        clearTimeout(timeout);
        reject(err);
      });
    });
  }

  async connectThroughSocks4WithPool (client, proxy, request, reqStartTs) {
    try {
      let targetIP;
      if (request.addressType === 0x03) {
        try {
          const addresses = await dns.resolve4(request.host);
          if (addresses.length > 0) {
            targetIP = addresses[0];
          } else {
            this.sendSocks5Error(client, 0x04);
            return false;
          }
        } catch (error) {
          this.sendSocks5Error(client, 0x04);
          return false;
        }
      } else {
        targetIP = request.host;
      }

      const conn = await this.getConnectionFromPool(proxy.id, async () => {
        return await this.createSocks4Connection(proxy);
      }, 3000);

      if (!conn || !conn.socket || conn.socket.destroyed) {
        if (conn) this.connectionPool.releaseConnection(conn);
        return false;
      }

      const proxySocket = conn.socket;

      const ipParts = targetIP.split('.').map(p => parseInt(p, 10));
      const connectRequest = Buffer.from([
        0x04, 0x01,
        (request.port >> 8) & 0xff,
        request.port & 0xff,
        ...ipParts,
        0x00
      ]);

      proxySocket.write(connectRequest);

      const response = await this.readDataWithTimeout(proxySocket, 2000);
      if (!response || response[0] !== 0x00 || response[1] !== 0x5a) {
        this.connectionPool.releaseConnection(conn);
        return false;
      }

      const successResponse = Buffer.from([
        0x05, 0x00, 0x00, 0x01,
        0x00, 0x00, 0x00, 0x00,
        0x00, 0x00
      ]);
      client.write(successResponse);

      this.setupBidirectionalPipe(client, proxySocket, conn);

      return true;
    } catch (error) {
      return false;
    }
  }

  async createSocks4Connection (proxy) {
    return new Promise((resolve, reject) => {
      const socket = net.createConnection({
        host: proxy.host,
        port: proxy.port
      });

      const timeout = setTimeout(() => {
        socket.destroy();
        reject(new Error('Connection timeout'));
      }, 10000);

      socket.once('connect', () => {
        clearTimeout(timeout);
        resolve(socket);
      });

      socket.once('error', (err) => {
        clearTimeout(timeout);
        reject(err);
      });
    });
  }

  async connectThroughHttpWithPool (client, proxy, request, reqStartTs) {
    try {
      const conn = await this.getConnectionFromPool(proxy.id, async () => {
        return await this.createHttpConnection(proxy);
      }, 3000);

      if (!conn || !conn.socket || conn.socket.destroyed) {
        if (conn) this.connectionPool.releaseConnection(conn);
        return false;
      }

      const proxySocket = conn.socket;

      let connectRequest = `CONNECT ${request.host}:${request.port} HTTP/1.1\r\n`;
      connectRequest += `Host: ${request.host}:${request.port}\r\n`;

      if (proxy.username && proxy.password) {
        const auth = Buffer.from(`${proxy.username}:${proxy.password}`).toString('base64');
        connectRequest += `Proxy-Authorization: Basic ${auth}\r\n`;
      }

      connectRequest += `\r\n`;
      proxySocket.write(connectRequest);

      const response = await this.readHttpHeader(proxySocket);
      if (!response) {
        this.connectionPool.releaseConnection(conn);
        return false;
      }

      const responseStr = response.toString();
      if (!responseStr.includes('200')) {
        this.connectionPool.releaseConnection(conn);
        return false;
      }

      const successResponse = Buffer.from([
        0x05, 0x00, 0x00, 0x01,
        0x00, 0x00, 0x00, 0x00,
        0x00, 0x00
      ]);
      client.write(successResponse);

      this.setupBidirectionalPipe(client, proxySocket, conn);

      return true;
    } catch (error) {
      return false;
    }
  }

  async createHttpConnection (proxy) {
    return new Promise((resolve, reject) => {
      const socket = net.createConnection({
        host: proxy.host,
        port: proxy.port
      });

      const timeout = setTimeout(() => {
        socket.destroy();
        reject(new Error('Connection timeout'));
      }, 10000);

      socket.once('connect', () => {
        clearTimeout(timeout);
        resolve(socket);
      });

      socket.once('error', (err) => {
        clearTimeout(timeout);
        reject(err);
      });
    });
  }

  readDataWithTimeout (socket, timeout = 2000) {
    return new Promise((resolve, reject) => {
      let buffer = Buffer.alloc(0);
      let timer;

      const cleanup = () => {
        clearTimeout(timer);
        socket.removeListener('data', onData);
        socket.removeListener('error', onError);
        socket.removeListener('end', onEnd);
      };

      // 重置超时
      const resetTimeout = () => {
        clearTimeout(timer);
        timer = setTimeout(() => {
          cleanup();
          // 如果已经接收到部分数据，返回这些数据
          if (buffer.length > 0) {
            resolve(buffer);
          } else {
            reject(new Error('读取超时'));
          }
        }, timeout);
      };

      const onData = (data) => {
        buffer = Buffer.concat([buffer, data]);

        // SOCKS5响应至少需要10个字节
        if (buffer.length >= 10) {
          cleanup();
          resolve(buffer);
        } else {
          // 继续等待更多数据
          resetTimeout();
        }
      };

      const onError = (err) => {
        cleanup();
        reject(err);
      };

      const onEnd = () => {
        cleanup();
        if (buffer.length > 0) {
          resolve(buffer);
        } else {
          reject(new Error('连接意外关闭'));
        }
      };

      resetTimeout();
      socket.on('data', onData);
      socket.once('error', onError);
      socket.once('end', onEnd);
    });
  }

  readData (socket, timeout = 5000) {
    return new Promise((resolve) => {
      const timer = setTimeout(() => resolve(null), timeout);
      const onData = (data) => {
        clearTimeout(timer);
        resolve(data);
      };
      socket.once('data', onData);
      socket.once('error', () => {
        clearTimeout(timer);
        resolve(null);
      });
    });
  }

  readHttpHeader (socket, timeout = 5000) {
    return new Promise((resolve) => {
      let buffer = Buffer.alloc(0);
      const timer = setTimeout(() => {
        cleanup();
        resolve(buffer);
      }, timeout);

      const onData = (data) => {
        buffer = Buffer.concat([buffer, data]);
        if (buffer.includes(Buffer.from('\r\n\r\n'))) {
          cleanup();
          resolve(buffer);
        }
      };

      const cleanup = () => {
        clearTimeout(timer);
        socket.off('data', onData);
      };

      socket.on('data', onData);
      socket.once('error', () => {
        cleanup();
        resolve(null);
      });
    });
  }

  parseSocks5Request (data) {
    if (!data || data.length < 7 || data[0] !== 0x05 || data[1] !== 0x01) {
      return null;
    }

    const addressType = data[3];
    let host, port;

    switch (addressType) {
      case 0x01:
        if (data.length < 10) return null;
        host = `${data[4]}.${data[5]}.${data[6]}.${data[7]}`;
        port = (data[8] << 8) | data[9];
        break;

      case 0x03:
        const domainLength = data[4];
        const end = 5 + domainLength;
        if (data.length < end + 2) return null;
        host = data.toString('utf8', 5, end);
        port = (data[end] << 8) | data[end + 1];
        break;

      case 0x04:
        return null;

      default:
        return null;
    }

    return { cmd: data[1], addressType, host, port };
  }

  isCriticalError (error) {
    const criticalMessages = [
      'ENOTFOUND',
      'ECONNREFUSED',
      'ETIMEDOUT',
      'ENETUNREACH'
    ];

    return criticalMessages.some(msg =>
      error.code === msg || error.message.includes(msg)
    );
  }

  sleep (ms) {
    return new Promise(resolve => setTimeout(resolve, ms));
  }

  sendSocks5Error (client, errorCode) {
    if (client.destroyed) return;

    const response = Buffer.from([
      0x05, errorCode, 0x00, 0x01,
      0x00, 0x00, 0x00, 0x00,
      0x00, 0x00
    ]);

    try {
      client.write(response);
      setTimeout(() => {
        if (!client.destroyed) {
          client.end();
        }
      }, 100);
    } catch (e) {
    }
  }

  getStats () {
    const result = [];
    for (const [proxyId, poolInfo] of this.proxyPool) {
      const metrics = poolInfo.metrics || {};
      result.push({
        proxyId,
        success: metrics.windows?.short?.success || 0,
        failed: metrics.windows?.short?.failed || 0,
        totalTime: 0,
        avgResponseTime: Math.round(metrics.avgResponseTime || 0),
        weight: Math.round((poolInfo.score || 0) * 100) / 100,
        activeConnections: this.activeConnections.get(proxyId) || 0
      });
    }
    return result;
  }

  getWeights () {
    const result = [];
    for (const [proxyId, poolInfo] of this.proxyPool) {
      result.push({
        proxyId,
        weight: Math.round((poolInfo.score || 0) * 100) / 100
      });
    }
    return result;
  }

  stop () {
    if (this.healthCheckTimer) clearInterval(this.healthCheckTimer);
    if (this.performanceTimer) clearInterval(this.performanceTimer);
    if (this.cleanupTimer) clearInterval(this.cleanupTimer);

    if (this.connectionPool) {
      this.connectionPool.cleanup();
    }

    this.circuitBreakers.clear();

    if (this.server) {
      this.connections.forEach(conn => {
        if (!conn.destroyed) {
          try {
            conn.destroy();
          } catch (e) { }
        }
      });
      this.connections.clear();

      try {
        this.server.close();
      } catch (e) { }
    }
  }
}

module.exports = ProxyLoadBalancer;
