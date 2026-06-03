// 负载均衡算法 + 评分计算 + 代理选择
// 所有方法作为 ProxyLoadBalancer.prototype mixin

module.exports = {
  // ── 评分计算 ──

  calculateProxyScore (proxyId) {
    const metrics = this.getProxyMetrics(proxyId);
    if (!metrics) return 0;

    const rtScore = this.calculateResponseTimeScore(metrics);
    const srScore = this.calculateSuccessRateScore(metrics);
    const connScore = this.calculateConnectionScore(proxyId);
    const stabScore = this.calculateStabilityScore(metrics);
    const trendScore = this.calculateTrendScore(metrics);

    let score =
      rtScore * this.algorithmWeights.responseTime +
      srScore * this.algorithmWeights.successRate +
      connScore * this.algorithmWeights.connections +
      stabScore * this.algorithmWeights.stability +
      trendScore * this.algorithmWeights.recentPerf;

    score = this.applyPenalties(proxyId, score, metrics);

    return Math.max(0.01, Math.min(100, score));
  },

  calculateResponseTimeScore (metrics) {
    const avgRt = metrics.avgResponseTime || 1000;

    if (avgRt <= 200) return 100;
    if (avgRt <= 500) return 90 - (avgRt - 200) * 0.1;
    if (avgRt <= 1000) return 70 - (avgRt - 500) * 0.08;
    if (avgRt <= 2000) return 50 - (avgRt - 1000) * 0.03;
    if (avgRt <= 5000) return 30 - (avgRt - 2000) * 0.005;
    return Math.max(10, 30 - Math.log10(avgRt) * 5);
  },

  calculateSuccessRateScore (metrics) {
    const rate = metrics.successRate || 0;

    if (rate < 0.5) return rate * 40;
    if (rate < 0.8) return 20 + (rate - 0.5) * 100;
    if (rate < 0.95) return 50 + (rate - 0.8) * 200;
    return 80 + (rate - 0.95) * 400;
  },

  calculateConnectionScore (proxyId) {
    const active = this.activeConnections.get(proxyId) || 0;
    const capacity = 100;

    const usage = active / capacity;
    if (usage <= 0.3) return 100;
    if (usage <= 0.5) return 90;
    if (usage <= 0.7) return 70;
    if (usage <= 0.9) return 40;
    return 10;
  },

  calculateStabilityScore (metrics) {
    const variance = metrics.responseTimeVariance || 0;
    const avgRt = metrics.avgResponseTime || 1000;

    const cv = avgRt > 0 ? Math.sqrt(variance) / avgRt : 1;

    if (cv <= 0.1) return 100;
    if (cv <= 0.3) return 80;
    if (cv <= 0.5) return 60;
    if (cv <= 1.0) return 30;
    return 10;
  },

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
  },

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
  },

  // ── 负载均衡算法 ──

  orderProxiesByStickyHost (proxies, hostKey) {
    if (!hostKey || proxies.length <= 1) return proxies;

    const key = hostKey.toLowerCase();

    let hash = 0;
    for (let i = 0; i < key.length; i++) {
      hash = ((hash << 5) - hash) + key.charCodeAt(i);
      hash |= 0;
    }

    const idx = Math.abs(hash) % proxies.length;
    const primary = proxies[idx];
    const rest = proxies.filter((_, i) => i !== idx);

    return [primary, ...rest];
  },

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
  },

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
  },

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
  },

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
  },

  stickyHostSelection (proxies, hostKey) {
    const list = Array.isArray(proxies) ? proxies.slice() : [];
    return this.orderProxiesByStickyHost(list, hostKey);
  },

  // ── 代理选择 ──

  async getEnabledProxies () {
    return await this.db.all(`
      SELECT * FROM proxies
      WHERE enabled = 1
      ORDER BY priority ASC, id ASC
    `);
  },

  async resolveTargetDomain (targetHost) {
    const ipPattern = /^(\d{1,3}\.){3}\d{1,3}$/;
    if (ipPattern.test(targetHost)) {
      const mapping = await this.db.get(
        'SELECT domain FROM dns_mappings WHERE ip = ? AND enabled = 1',
        [targetHost]
      );
      if (mapping) return mapping.domain;
    }
    return targetHost;
  },

  async getGroupProxyIds (targetHost) {
    const domain = await this.resolveTargetDomain(targetHost);
    return this.domainMatcher.match(domain);
  }
};
