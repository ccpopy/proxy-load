const axios = require('axios');
const tls = require('tls');
const { SocksClient } = require('socks');
const { SocksProxyAgent } = require('socks-proxy-agent');
const { HttpProxyAgent, HttpsProxyAgent } = require('hpagent');

function isTruthyFlag (value) {
  return value === 1 || value === true;
}

function getDefaultPort (url) {
  if (url.port) return Number(url.port);
  if (url.protocol === 'https:') return 443;
  if (url.protocol === 'http:') return 80;
  throw new Error(`不支持的测试地址协议: ${url.protocol}`);
}

function formatHostHeader (url) {
  const port = getDefaultPort(url);
  const defaultPort = url.protocol === 'https:' ? 443 : 80;
  return port === defaultPort ? url.hostname : `${url.hostname}:${port}`;
}

function getRequestPath (url) {
  return `${url.pathname || '/'}${url.search || ''}`;
}

function withTimeout (promise, timeout, message, onTimeout) {
  let timer;
  const timeoutPromise = new Promise((_, reject) => {
    timer = setTimeout(() => {
      if (onTimeout) onTimeout();
      reject(new Error(message));
    }, timeout);
  });

  return Promise.race([promise, timeoutPromise]).finally(() => clearTimeout(timer));
}

async function createSocks5Tunnel (proxy, targetUrl, timeout) {
  const port = getDefaultPort(targetUrl);
  const proxyOptions = {
    host: proxy.host,
    port: Number(proxy.port),
    type: 5
  };

  if (proxy.username) {
    proxyOptions.userId = proxy.username;
    proxyOptions.password = proxy.password || '';
  }

  let result;
  try {
    result = await SocksClient.createConnection({
      proxy: proxyOptions,
      command: 'connect',
      destination: {
        host: targetUrl.hostname,
        port
      },
      timeout
    });
  } catch (error) {
    throw new Error(`SOCKS5隧道建立失败: ${error.message}`);
  }

  return result.socket;
}

function createTlsSocket (socket, targetUrl, timeout, skipCertificateVerification) {
  return withTimeout(new Promise((resolve, reject) => {
    const tlsSocket = tls.connect({
      socket,
      servername: targetUrl.hostname,
      rejectUnauthorized: !skipCertificateVerification
    });

    tlsSocket.once('secureConnect', () => resolve(tlsSocket));
    tlsSocket.once('error', reject);
  }), timeout, 'TLS握手超时', () => socket.destroy());
}

function readHttpResponseHeader (socket, timeout) {
  return withTimeout(new Promise((resolve, reject) => {
    let buffer = Buffer.alloc(0);

    const cleanup = () => {
      socket.removeListener('data', onData);
      socket.removeListener('error', onError);
      socket.removeListener('end', onEnd);
      socket.removeListener('close', onClose);
    };

    const onData = (data) => {
      buffer = Buffer.concat([buffer, data]);
      const headerEnd = buffer.indexOf(Buffer.from('\r\n\r\n'));
      if (headerEnd !== -1) {
        cleanup();
        resolve(buffer.subarray(0, headerEnd + 4).toString('latin1'));
      }
    };

    const onError = (error) => {
      cleanup();
      reject(error);
    };

    const onEnd = () => {
      cleanup();
      reject(new Error('目标服务器在返回响应头前关闭连接'));
    };

    const onClose = () => {
      cleanup();
      reject(new Error('连接在返回响应头前关闭'));
    };

    socket.on('data', onData);
    socket.once('error', onError);
    socket.once('end', onEnd);
    socket.once('close', onClose);
  }), timeout, '读取HTTP响应超时', () => socket.destroy());
}

async function testSocks5Proxy (proxy, testUrl, timeout) {
  const startTime = Date.now();
  let rawSocket;
  let requestSocket;

  try {
    const targetUrl = new URL(testUrl);
    const skipCertificateVerification = isTruthyFlag(proxy.skip_cert_verify);

    rawSocket = await createSocks5Tunnel(proxy, targetUrl, timeout);
    rawSocket.setNoDelay(true);
    rawSocket.setTimeout(timeout, () => rawSocket.destroy(new Error('SOCKS5隧道空闲超时')));

    requestSocket = targetUrl.protocol === 'https:'
      ? await createTlsSocket(rawSocket, targetUrl, timeout, skipCertificateVerification)
      : rawSocket;

    const request = [
      `GET ${getRequestPath(targetUrl)} HTTP/1.1`,
      `Host: ${formatHostHeader(targetUrl)}`,
      'User-Agent: zwfw-load-health-check/1.0',
      'Accept: */*',
      'Connection: close',
      '',
      ''
    ].join('\r\n');

    requestSocket.write(request);
    const header = await readHttpResponseHeader(requestSocket, timeout);
    const statusLine = header.split('\r\n')[0] || '';
    const statusMatch = statusLine.match(/^HTTP\/\d(?:\.\d)?\s+(\d{3})\b/);

    if (!statusMatch) {
      throw new Error(`无效HTTP响应: ${statusLine}`);
    }

    const statusCode = Number(statusMatch[1]);
    if (statusCode < 200 || statusCode >= 500) {
      throw new Error(`HTTP状态码不符合连通性策略: ${statusCode}`);
    }

    return {
      success: true,
      responseTime: Date.now() - startTime,
      statusCode
    };
  } catch (error) {
    return {
      success: false,
      responseTime: Date.now() - startTime,
      error: error.message
    };
  } finally {
    if (requestSocket && !requestSocket.destroyed) requestSocket.destroy();
    if (rawSocket && rawSocket !== requestSocket && !rawSocket.destroyed) rawSocket.destroy();
  }
}

// 测试代理
async function testProxy (proxy, testUrl, timeout) {
  const startTime = Date.now();

  try {
    if (proxy.type === 'socks5') {
      return await testSocks5Proxy(proxy, testUrl, timeout);
    }

    let agent;
    const skipCertificateVerification = isTruthyFlag(proxy.skip_cert_verify);

    if (proxy.type === 'socks4' || proxy.type === 'socks5') {
      const auth = proxy.username ?
        `${encodeURIComponent(proxy.username)}:${encodeURIComponent(proxy.password)}@` : '';
      const protocol = proxy.type === 'socks5' ? 'socks5h' : proxy.type;
      const proxyUrl = `${protocol}://${auth}${proxy.host}:${proxy.port}`;
      agent = new SocksProxyAgent(proxyUrl, {
        rejectUnauthorized: !skipCertificateVerification
      });
    } else if (proxy.type === 'http' || proxy.type === 'https') {
      const auth = proxy.username ?
        `${encodeURIComponent(proxy.username)}:${encodeURIComponent(proxy.password)}@` : '';
      const proxyUrl = `${proxy.type}://${auth}${proxy.host}:${proxy.port}`;
      const isHttps = testUrl.startsWith('https');
      agent = isHttps ?
        new HttpsProxyAgent({ proxy: proxyUrl, rejectUnauthorized: !skipCertificateVerification }) :
        new HttpProxyAgent({ proxy: proxyUrl });
    }

    const response = await axios.get(testUrl, {
      httpAgent: agent,
      httpsAgent: agent,
      timeout,
      proxy: false,
      maxRedirects: 5,
      validateStatus: (status) => status >= 200 && status < 500
    });

    const responseTime = Date.now() - startTime;

    const result = {
      success: true,
      responseTime,
      statusCode: response.status
    };

    return result;
  } catch (error) {
    return {
      success: false,
      responseTime: Date.now() - startTime,
      error: error.message
    };
  }
}

module.exports = { testProxy };
