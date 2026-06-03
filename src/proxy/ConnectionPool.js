class ConnectionPool {
  constructor (maxSize = 50, maxIdleTime = 30000, maxWaitTime = 10000) {
    this.pools = new Map();
    this.maxSize = maxSize;
    this.maxIdleTime = maxIdleTime;
    this.maxWaitTime = maxWaitTime;
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
      }, this.maxWaitTime);

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

module.exports = ConnectionPool;
