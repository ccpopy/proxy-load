const net = require('net');
const CircuitBreaker = require('./CircuitBreaker');
const ConnectionPool = require('./ConnectionPool');
const DomainMatcher = require('./DomainMatcher');
const { getArithmeticName, methods: utilMethods } = require('./utils');
const algorithmMethods = require('./algorithms');
const protocolMethods = require('./protocols');
const healthMethods = require('./health');
const dnsMethods = require('./dns');

class ProxyLoadBalancer {
  constructor (db, logRequest) {
    this.db = db;
    this.logRequest = logRequest;
    this.server = null;
    this.connections = new Set();
    this.clientTargets = new Map();
    this.loadMode = 'auto';
    this.circuitBreakers = new Map();
    this.dnsCache = new Map();
    this.domainMatcher = new DomainMatcher();

    this.circuitBreakerConfig = {
      threshold: 5,
      timeout: 60000,
      halfOpenAttempts: 2
    };

    this.connectionPool = new ConnectionPool(50, 30000, 10000);

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
      responseTime: 0.30,
      successRate: 0.25,
      connections: 0.20,
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
      degradeThreshold: 0.5,
      recoverThreshold: 0.8
    };

    this.startMonitoring();
  }

  getCircuitBreaker (proxyId) {
    if (!this.circuitBreakers.has(proxyId)) {
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

  async reloadDomainMatcher () {
    await this.domainMatcher.loadFromDb(this.db);
  }

  // ── TCP 服务器 ──

  async start (port = 5678) {
    await this.domainMatcher.loadFromDb(this.db);
    this.server = net.createServer((client) => this.handleConnection(client));

    return new Promise((resolve, reject) => {
      this.server.listen(port, '0.0.0.0', () => {
        console.log(`混合代理负载均衡服务器运行在 0.0.0.0:${port}（SOCKS5/HTTP）`);
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

    this.handleClientProtocol(client, startTime).catch(err => {
      client.destroy();
    });
  }

  async handleClientProtocol (client, startTime) {
    const initialData = await this.readFirstDataWithTimeout(client, 1000);

    if (initialData[0] === 0x05) {
      await this.handleSocks5(client, startTime, initialData);
      return;
    }

    if (this.isHttpProxyRequest(initialData)) {
      await this.handleHttpProxy(client, startTime, initialData);
      return;
    }

    throw new Error('不支持的入站代理协议');
  }

  async handleSocks5 (client, startTime, initialHandshake = null) {
    let targetHost = null;
    let targetPort = null;

    try {
      client.setTimeout(this.failFast.totalTimeout);

      const handshake = initialHandshake || await this.readDataWithTimeout(client, 1000);
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

      const resolvedRequest = await this.resolveTarget(request);

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

  async handleHttpProxy (client, startTime, initialData) {
    let targetHost = null;
    let targetPort = null;

    try {
      client.setTimeout(this.failFast.totalTimeout);

      const { header, rest } = await this.readHttpHeaderWithInitial(client, initialData, 5000);
      const request = this.parseHttpProxyRequest(header);
      targetHost = request.host;
      targetPort = request.port;

      if (request.inboundProtocol === 'http-forward') {
        request.initialPayload = this.buildHttpForwardPayload(header, request, rest);
      } else if (rest.length > 0) {
        request.initialPayload = rest;
      }

      const resolvedRequest = await this.resolveTarget(request);

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
        this.sendHttpProxyError(client, 502, result.error || '所有代理连接失败');
        if (this.logRequest) {
          this.logRequest(null, targetHost, targetPort, false,
            Date.now() - startTime, result.error || '所有代理连接失败', {
              resultType: 'proxy_exhausted'
            });
        }
      }
    } catch (error) {
      this.sendHttpProxyError(client, targetHost ? 502 : 400, error.message);
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

    const modeSetting = await this.db.get("SELECT value FROM settings WHERE key = 'algorithm'");
    let currentAlgorithm = modeSetting?.value || this.currentAlgorithm;
    if (!this.allowedAlgorithms.has(currentAlgorithm)) {
      currentAlgorithm = 'adaptive';
    }

    const allProxies = await this.getEnabledProxies();
    if (!allProxies || allProxies.length === 0) {
      return { connected: false, error: '没有可用的代理' };
    }

    const lbHost = (request.originalHost || request.host || '').toLowerCase();
    const groupProxyIds = await this.getGroupProxyIds(lbHost);
    const scopedProxies = groupProxyIds
      ? allProxies.filter(p => groupProxyIds.has(p.id))
      : allProxies;

    if (scopedProxies.length === 0) {
      return { connected: false, error: `目标 ${lbHost || request.host} 匹配的代理分组没有可用代理` };
    }

    const activeProxies = scopedProxies.filter(p => {
      const circuitBreaker = this.getCircuitBreaker(p.id);
      if (!circuitBreaker.canAttempt() || !(p.status === 'active' || !p.status)) {
        return false;
      }

      const poolInfo = this.proxyPool.get(p.id);
      if (poolInfo?.degraded) {
        const degradeDuration = Date.now() - (poolInfo.degradedAt || 0);
        if (degradeDuration < this.healthCheck.interval) {
          return false;
        }
        this.recoverProxy(p.id);
      }

      return true;
    });

    let proxiesToTry = activeProxies.length > 0 ? activeProxies : scopedProxies;

    const algorithm = this.algorithms[currentAlgorithm] || this.algorithms.adaptive;
    const ordered = await algorithm(proxiesToTry, lbHost);
    if (Array.isArray(ordered) && ordered.length > 0) {
      const selectedIds = new Set(ordered.map(p => p.id));
      const rest = proxiesToTry.filter(p => !selectedIds.has(p.id));
      proxiesToTry = [...ordered, ...rest];
    }

    const maxAttempts = Number.isInteger(Number(this.failFast.maxAttempts))
      ? Math.max(1, Number(this.failFast.maxAttempts))
      : proxiesToTry.length;
    const attemptLimit = this.failFast.enabled === false
      ? 1
      : Math.min(maxAttempts, proxiesToTry.length);
    proxiesToTry = proxiesToTry.slice(0, attemptLimit);

    const errors = [];

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

        const circuitBreaker = this.getCircuitBreaker(proxy.id);
        circuitBreaker.recordFailure();

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

  // ── 对外接口 ──

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

// 挂载 mixin 方法到原型
Object.assign(ProxyLoadBalancer.prototype, algorithmMethods);
Object.assign(ProxyLoadBalancer.prototype, protocolMethods);
Object.assign(ProxyLoadBalancer.prototype, healthMethods);
Object.assign(ProxyLoadBalancer.prototype, dnsMethods);
Object.assign(ProxyLoadBalancer.prototype, utilMethods);

module.exports = ProxyLoadBalancer;
