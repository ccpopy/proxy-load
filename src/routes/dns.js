const express = require('express');

module.exports = function ({ db, broadcast, getProxyServer }) {
  const router = express.Router();

  // 获取所有DNS映射
  router.get('/', async (req, res) => {
    try {
      const mappings = await db.all('SELECT * FROM dns_mappings ORDER BY domain ASC');
      res.json(mappings);
    } catch (error) {
      res.status(500).json({ error: error.message });
    }
  });

  // 创建DNS映射
  router.post('/', async (req, res) => {
    try {
      const { domain, ip, description } = req.body;

      if (!domain || !ip) {
        return res.status(400).json({ error: '域名和IP地址不能为空' });
      }

      // 验证IP格式
      const ipRegex = /^(\d{1,3}\.){3}\d{1,3}$/;
      if (!ipRegex.test(ip)) {
        return res.status(400).json({ error: 'IP地址格式不正确' });
      }

      const result = await db.run(
        'INSERT INTO dns_mappings (domain, ip, description) VALUES (?, ?, ?)',
        [domain.toLowerCase(), ip, description || null]
      );

      // 重新加载DNS映射缓存
      const proxyServer = getProxyServer();
      if (proxyServer) {
        await proxyServer.loadDNSMappings();
        // 清除连接池，确保新的DNS映射立即生效
        proxyServer.connectionPool.cleanup();
        // 关闭该域名的现存隧道，确保"秒级"生效
        if (typeof proxyServer.flushConnectionsForDomain === 'function') {
          proxyServer.flushConnectionsForDomain(domain.toLowerCase());
        }
      }

      broadcast('dns_mapping_added', { id: result.lastID, domain, ip, description });
      res.json({ id: result.lastID, domain, ip, description, enabled: 1 });
    } catch (error) {
      if (error.message.includes('UNIQUE')) {
        res.status(400).json({ error: '该域名已存在' });
      } else {
        res.status(500).json({ error: error.message });
      }
    }
  });

  // 更新DNS映射
  router.put('/:id', async (req, res) => {
    try {
      const { id } = req.params;
      const { domain, ip, description, enabled } = req.body;
      // 取旧域名，便于更新后刷新旧/新两侧连接
      const before = await db.get('SELECT domain FROM dns_mappings WHERE id = ?', [id]);

      if (!domain || !ip) {
        return res.status(400).json({ error: '域名和IP地址不能为空' });
      }

      // 验证IP格式
      const ipRegex = /^(\d{1,3}\.){3}\d{1,3}$/;
      if (!ipRegex.test(ip)) {
        return res.status(400).json({ error: 'IP地址格式不正确' });
      }

      await db.run(
        'UPDATE dns_mappings SET domain = ?, ip = ?, description = ?, enabled = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ?',
        [domain.toLowerCase(), ip, description || null, enabled !== undefined ? enabled : 1, id]
      );

      // 重新加载DNS映射缓存
      const proxyServer = getProxyServer();
      if (proxyServer) {
        await proxyServer.loadDNSMappings();
        // 清除连接池，确保DNS映射修改立即生效
        proxyServer.connectionPool.cleanup();
        // 关闭旧/新域名相关的现存隧道
        if (typeof proxyServer.flushConnectionsForDomain === 'function') {
          if (before?.domain) proxyServer.flushConnectionsForDomain(String(before.domain).toLowerCase());
          proxyServer.flushConnectionsForDomain(domain.toLowerCase());
        }
      }

      broadcast('dns_mapping_updated', { id, domain, ip, description, enabled });
      res.json({ success: true });
    } catch (error) {
      if (error.message.includes('UNIQUE')) {
        res.status(400).json({ error: '该域名已存在' });
      } else {
        res.status(500).json({ error: error.message });
      }
    }
  });

  // 删除DNS映射
  router.delete('/:id', async (req, res) => {
    try {
      const { id } = req.params;
      const before = await db.get('SELECT domain FROM dns_mappings WHERE id = ?', [id]);
      await db.run('DELETE FROM dns_mappings WHERE id = ?', [id]);

      // 重新加载DNS映射缓存
      const proxyServer = getProxyServer();
      if (proxyServer) {
        await proxyServer.loadDNSMappings();
        // 清除连接池，确保DNS映射删除立即生效
        proxyServer.connectionPool.cleanup();
        // 关闭该域名的现存隧道
        if (typeof proxyServer.flushConnectionsForDomain === 'function' && before?.domain) {
          proxyServer.flushConnectionsForDomain(String(before.domain).toLowerCase());
        }
      }

      broadcast('dns_mapping_deleted', { id });
      res.json({ success: true });
    } catch (error) {
      res.status(500).json({ error: error.message });
    }
  });

  // 切换DNS映射启用状态
  router.put('/:id/toggle', async (req, res) => {
    try {
      const { id } = req.params;
      const mapping = await db.get('SELECT domain, enabled FROM dns_mappings WHERE id = ?', [id]);

      if (!mapping) {
        return res.status(404).json({ error: 'DNS映射不存在' });
      }

      const newEnabled = mapping.enabled === 1 ? 0 : 1;
      await db.run('UPDATE dns_mappings SET enabled = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ?', [newEnabled, id]);

      // 重新加载DNS映射缓存
      const proxyServer = getProxyServer();
      if (proxyServer) {
        await proxyServer.loadDNSMappings();
        // 清除连接池，确保DNS映射启用/禁用立即生效
        proxyServer.connectionPool.cleanup();
        // 关闭该域名的现存隧道
        if (typeof proxyServer.flushConnectionsForDomain === 'function' && mapping?.domain) {
          proxyServer.flushConnectionsForDomain(String(mapping.domain).toLowerCase());
        }
      }

      broadcast('dns_mapping_toggled', { id, enabled: newEnabled });
      res.json({ success: true, enabled: newEnabled });
    } catch (error) {
      res.status(500).json({ error: error.message });
    }
  });

  return router;
};
