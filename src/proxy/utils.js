function getArithmeticName (str) {
  const mapName = {
    'adaptive': '自适应算法',
    'weighted_round_robin': '加权轮询',
    'least_connections': '最小连接数',
    'sticky_host': '会话粘滞（按域名）'
  };
  return mapName[str] || str;
}

const HTTP_HEADER_END = Buffer.from('\r\n\r\n');

function isIPv4Address (host) {
  const parts = String(host).split('.');
  if (parts.length !== 4) return false;

  return parts.every(part => {
    if (!/^\d{1,3}$/.test(part)) return false;
    const value = Number(part);
    return value >= 0 && value <= 255;
  });
}

function getAddressType (host) {
  if (!host || String(host).includes(':')) {
    throw new Error('暂不支持IPv6地址');
  }

  return isIPv4Address(host) ? 0x01 : 0x03;
}

function parsePort (value, defaultPort) {
  const raw = value == null || value === '' ? defaultPort : value;
  const port = Number(raw);

  if (!Number.isInteger(port) || port < 1 || port > 65535) {
    throw new Error(`无效端口: ${raw}`);
  }

  return port;
}

function parseAuthority (authority, defaultPort) {
  const value = String(authority || '').trim();
  if (!value) {
    throw new Error('缺少目标主机');
  }

  if (value.startsWith('[')) {
    throw new Error('暂不支持IPv6地址');
  }

  if (value.includes('@')) {
    throw new Error('目标地址不能包含用户信息');
  }

  const colonIndex = value.lastIndexOf(':');
  let host = value;
  let port = defaultPort;

  if (colonIndex !== -1) {
    if (value.indexOf(':') !== colonIndex) {
      throw new Error('暂不支持IPv6地址');
    }

    host = value.slice(0, colonIndex);
    const portValue = value.slice(colonIndex + 1);
    if (!portValue) {
      throw new Error('缺少目标端口');
    }
    port = parsePort(portValue, defaultPort);
  } else {
    port = parsePort(null, defaultPort);
  }

  host = host.trim();
  if (!host) {
    throw new Error('缺少目标主机');
  }

  return { host, port, addressType: getAddressType(host) };
}

// ProxyLoadBalancer prototype mixin 方法
const methods = {
  readFirstDataWithTimeout (socket, timeout = 2000) {
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        cleanup();
        reject(new Error('读取入站协议超时'));
      }, timeout);

      const cleanup = () => {
        clearTimeout(timer);
        socket.removeListener('data', onData);
        socket.removeListener('error', onError);
        socket.removeListener('end', onEnd);
      };

      const onData = (data) => {
        cleanup();
        resolve(data);
      };

      const onError = (err) => {
        cleanup();
        reject(err);
      };

      const onEnd = () => {
        cleanup();
        reject(new Error('连接意外关闭'));
      };

      socket.once('data', onData);
      socket.once('error', onError);
      socket.once('end', onEnd);
    });
  },

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

      const resetTimeout = () => {
        clearTimeout(timer);
        timer = setTimeout(() => {
          cleanup();
          if (buffer.length > 0) {
            resolve(buffer);
          } else {
            reject(new Error('读取超时'));
          }
        }, timeout);
      };

      const onData = (data) => {
        buffer = Buffer.concat([buffer, data]);

        if (buffer.length >= 10) {
          cleanup();
          resolve(buffer);
        } else {
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
  },

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
  },

  readHttpHeaderWithInitial (socket, initialData, timeout = 5000) {
    return new Promise((resolve, reject) => {
      let buffer = Buffer.from(initialData || []);
      let timer;

      const cleanup = () => {
        clearTimeout(timer);
        socket.removeListener('data', onData);
        socket.removeListener('error', onError);
        socket.removeListener('end', onEnd);
      };

      const resolveIfComplete = () => {
        const headerEnd = buffer.indexOf(HTTP_HEADER_END);
        if (headerEnd === -1) return false;

        cleanup();
        const end = headerEnd + HTTP_HEADER_END.length;
        resolve({
          header: buffer.subarray(0, end).toString('latin1'),
          rest: buffer.subarray(end)
        });
        return true;
      };

      const onData = (data) => {
        buffer = Buffer.concat([buffer, data]);
        resolveIfComplete();
      };

      const onError = (err) => {
        cleanup();
        reject(err);
      };

      const onEnd = () => {
        cleanup();
        reject(new Error('HTTP请求头未完整读取'));
      };

      if (resolveIfComplete()) return;

      timer = setTimeout(() => {
        cleanup();
        reject(new Error('读取HTTP请求头超时'));
      }, timeout);

      socket.on('data', onData);
      socket.once('error', onError);
      socket.once('end', onEnd);
    });
  },

  isHttpProxyRequest (data) {
    if (!data || data.length === 0) return false;

    const prefix = data.toString('latin1', 0, Math.min(data.length, 16)).toUpperCase();
    return /^(CONNECT|GET|POST|HEAD|PUT|DELETE|OPTIONS|PATCH|TRACE)\s/.test(prefix);
  },

  parseHttpProxyRequest (header) {
    const lines = String(header || '').split('\r\n');
    const requestLine = lines[0] || '';
    const match = requestLine.match(/^([A-Za-z]+)\s+(\S+)\s+(HTTP\/\d\.\d)$/);

    if (!match) {
      throw new Error('无效HTTP代理请求行');
    }

    const method = match[1].toUpperCase();
    const target = match[2];
    const version = match[3];
    const headers = new Map();

    for (const line of lines.slice(1)) {
      if (!line) continue;

      const separator = line.indexOf(':');
      if (separator === -1) continue;

      const name = line.slice(0, separator).trim().toLowerCase();
      const value = line.slice(separator + 1).trim();
      if (name) headers.set(name, value);
    }

    if (method === 'CONNECT') {
      const targetInfo = parseAuthority(target, 443);
      return {
        cmd: 0x01,
        ...targetInfo,
        originalHost: targetInfo.host,
        inboundProtocol: 'http-connect',
        httpMethod: method,
        httpTarget: target,
        httpVersion: version
      };
    }

    let targetInfo;
    let httpPath;

    if (/^https?:\/\//i.test(target)) {
      const url = new URL(target);
      if (url.protocol !== 'http:') {
        throw new Error('普通HTTP代理请求只支持http://目标，HTTPS请使用CONNECT隧道');
      }

      targetInfo = parseAuthority(url.host, 80);
      httpPath = `${url.pathname || '/'}${url.search || ''}`;
    } else {
      const hostHeader = headers.get('host');
      if (!hostHeader) {
        throw new Error('普通HTTP代理请求缺少Host头');
      }

      targetInfo = parseAuthority(hostHeader, 80);
      httpPath = target || '/';
    }

    return {
      cmd: 0x01,
      ...targetInfo,
      originalHost: targetInfo.host,
      inboundProtocol: 'http-forward',
      httpMethod: method,
      httpTarget: target,
      httpPath,
      httpVersion: version
    };
  },

  buildHttpForwardPayload (header, request, rest = Buffer.alloc(0)) {
    const lines = String(header || '').replace(/\r\n\r\n$/, '').split('\r\n');
    const rewritten = [
      `${request.httpMethod} ${request.httpPath || '/'} ${request.httpVersion || 'HTTP/1.1'}`
    ];

    for (const line of lines.slice(1)) {
      if (/^Proxy-(Connection|Authorization)\s*:/i.test(line)) continue;
      rewritten.push(line);
    }

    return Buffer.concat([
      Buffer.from(`${rewritten.join('\r\n')}\r\n\r\n`, 'latin1'),
      rest
    ]);
  },

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
  },

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

      case 0x03: {
        const domainLength = data[4];
        const end = 5 + domainLength;
        if (data.length < end + 2) return null;
        host = data.toString('utf8', 5, end);
        port = (data[end] << 8) | data[end + 1];
        break;
      }

      case 0x04:
        return null;

      default:
        return null;
    }

    return { cmd: data[1], addressType, host, port };
  },

  buildSocks5ConnectRequest (request) {
    if (request.addressType === 0x03) {
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
      const ipParts = request.host.split('.').map(p => parseInt(p, 10));
      return Buffer.from([
        0x05, 0x01, 0x00, 0x01,
        ...ipParts,
        (request.port >> 8) & 0xff,
        request.port & 0xff
      ]);
    } else if (request.addressType === 0x04) {
      throw new Error('暂不支持IPv6地址');
    }

    throw new Error(`不支持的地址类型: ${request.addressType}`);
  },

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
  },

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
  },

  sendHttpProxyError (client, statusCode, message) {
    if (client.destroyed) return;

    const reasonMap = {
      400: 'Bad Request',
      502: 'Bad Gateway'
    };
    const reason = reasonMap[statusCode] || 'Proxy Error';
    const body = `${statusCode} ${reason}\n${message || ''}\n`;
    const bodyLength = Buffer.byteLength(body);
    const response = [
      `HTTP/1.1 ${statusCode} ${reason}`,
      'Connection: close',
      'Content-Type: text/plain; charset=utf-8',
      `Content-Length: ${bodyLength}`,
      '',
      body
    ].join('\r\n');

    try {
      client.end(response);
    } catch (e) {
    }
  },

  completeClientProxyHandshake (client, proxySocket, request, socks5Response = null) {
    const inboundProtocol = request.inboundProtocol || 'socks5';

    if (inboundProtocol === 'socks5') {
      const response = socks5Response || Buffer.from([
        0x05, 0x00, 0x00, 0x01,
        0x00, 0x00, 0x00, 0x00,
        0x00, 0x00
      ]);
      client.write(response);
    } else if (inboundProtocol === 'http-connect') {
      client.write('HTTP/1.1 200 Connection Established\r\n\r\n');
    } else if (inboundProtocol !== 'http-forward') {
      throw new Error(`不支持的入站代理协议: ${inboundProtocol}`);
    }

    if (request.initialPayload && request.initialPayload.length > 0) {
      proxySocket.write(request.initialPayload);
    }
  },

  sleep (ms) {
    return new Promise(resolve => setTimeout(resolve, ms));
  }
};

module.exports = { getArithmeticName, methods };
