const axios = require('axios');
const { SocksProxyAgent } = require('socks-proxy-agent');
const { HttpProxyAgent, HttpsProxyAgent } = require('hpagent');

// 带宽测速功能
async function measureBandwidth (proxy, testUrls) {
  const urls = [
    "https://cms.zjzwfw.gov.cn/ucenter_files/tempfile/logo/logo.svg",
    "https://cms.zjzwfw.gov.cn/ucenter_files/tempfile/logo/logo.svg",
    "https://cms.zjzwfw.gov.cn/ucenter_files/tempfile/logo/logo.svg",
    "https://cms.zjzwfw.gov.cn/ucenter_files/tempfile/logo/logo.svg",
  ];

  try {
    let agent;

    if (proxy.type === 'socks4' || proxy.type === 'socks5') {
      const auth = proxy.username ?
        `${encodeURIComponent(proxy.username)}:${encodeURIComponent(proxy.password)}@` : '';
      const proxyUrl = `${proxy.type}://${auth}${proxy.host}:${proxy.port}`;
      agent = new SocksProxyAgent(proxyUrl);
    } else if (proxy.type === 'http' || proxy.type === 'https') {
      const auth = proxy.username ?
        `${encodeURIComponent(proxy.username)}:${encodeURIComponent(proxy.password)}@` : '';
      const proxyUrl = `${proxy.type}://${auth}${proxy.host}:${proxy.port}`;
      const isHttps = urls[0].startsWith('https');
      agent = isHttps ?
        new HttpsProxyAgent({ proxy: proxyUrl }) :
        new HttpProxyAgent({ proxy: proxyUrl });
    }

    const results = [];
    for (const url of urls) {
      const startTime = Date.now();

      try {
        const response = await axios.get(url, {
          httpAgent: agent,
          httpsAgent: agent,
          timeout: 30000,
          proxy: false,
          responseType: 'arraybuffer'
        });

        const endTime = Date.now();
        const duration = (endTime - startTime) / 1000; // 秒
        const bytes = response.data.length;
        const bits = bytes * 8;
        const bps = bits / duration;

        results.push({
          url,
          bytes,
          duration,
          bps
        });
      } catch (error) {
        console.error(`带宽测试失败 ${url}:`, error.message);
      }
    }

    if (results.length === 0) {
      return { success: false, error: '所有测试都失败' };
    }

    // 计算平均带宽
    const avgBps = results.reduce((sum, r) => sum + r.bps, 0) / results.length;

    return {
      success: true,
      throughputBps: avgBps,
      throughputMbps: avgBps / 1048576,
      results
    };
  } catch (error) {
    return { success: false, error: error.message };
  }
}

// 测试代理
async function testProxy (proxy, testUrl, timeout, measureBandwidthFlag = false) {
  const startTime = Date.now();

  try {
    let agent;

    if (proxy.type === 'socks4' || proxy.type === 'socks5') {
      const auth = proxy.username ?
        `${encodeURIComponent(proxy.username)}:${encodeURIComponent(proxy.password)}@` : '';
      const proxyUrl = `${proxy.type}://${auth}${proxy.host}:${proxy.port}`;
      agent = new SocksProxyAgent(proxyUrl);
    } else if (proxy.type === 'http' || proxy.type === 'https') {
      const auth = proxy.username ?
        `${encodeURIComponent(proxy.username)}:${encodeURIComponent(proxy.password)}@` : '';
      const proxyUrl = `${proxy.type}://${auth}${proxy.host}:${proxy.port}`;
      const isHttps = testUrl.startsWith('https');
      agent = isHttps ?
        new HttpsProxyAgent({ proxy: proxyUrl }) :
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

    // 如果需要测量带宽
    if (measureBandwidthFlag) {
      const bandwidthResult = await measureBandwidth(proxy);
      result.bandwidth = bandwidthResult;
    }

    return result;
  } catch (error) {
    return {
      success: false,
      responseTime: Date.now() - startTime,
      error: error.message
    };
  }
}

module.exports = { testProxy, measureBandwidth };
