const net = require('net');
const dns = require('dns').promises;

// 协议处理方法 - ProxyLoadBalancer.prototype mixin

module.exports = {
  async getConnectionFromPool (proxyId, createFn, timeout) {
    const waitTimeout = timeout ?? this.connectionPool?.maxWaitTime ?? 5000;
    return Promise.race([
      this.connectionPool.getConnection(proxyId, createFn),
      new Promise((_, reject) =>
        setTimeout(() => reject(new Error('获取连接超时')), waitTimeout)
      )
    ]);
  },

  async attemptProxyConnection (client, proxy, request, startTime) {
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
  },

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
  },

  // ── SOCKS5 ──

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

      if (connectResponse[0] !== 0x05) {
        proxySocket.destroy();
        return false;
      }

      if (connectResponse[1] !== 0x00) {
        proxySocket.destroy();
        return false;
      }

      this.completeClientProxyHandshake(client, proxySocket, request, connectResponse);

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
  },

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
  },

  // ── SOCKS4 ──

  async connectThroughSocks4WithPool (client, proxy, request, reqStartTs) {
    try {
      let targetIP;
      if (request.addressType === 0x03) {
        try {
          const addresses = await dns.resolve4(request.host);
          if (addresses.length > 0) {
            targetIP = addresses[0];
          } else {
            return false;
          }
        } catch (error) {
          return false;
        }
      } else {
        targetIP = request.host;
      }

      const conn = await this.getConnectionFromPool(proxy.id, async () => {
        return await this.createSocks4Connection(proxy);
      });

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

      this.completeClientProxyHandshake(client, proxySocket, request);

      this.setupBidirectionalPipe(client, proxySocket, conn);

      return true;
    } catch (error) {
      return false;
    }
  },

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
  },

  // ── HTTP/HTTPS ──

  async connectThroughHttpWithPool (client, proxy, request, reqStartTs) {
    try {
      const conn = await this.getConnectionFromPool(proxy.id, async () => {
        return await this.createHttpConnection(proxy);
      });

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

      this.completeClientProxyHandshake(client, proxySocket, request);

      this.setupBidirectionalPipe(client, proxySocket, conn);

      return true;
    } catch (error) {
      return false;
    }
  },

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
};
