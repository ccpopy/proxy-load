const express = require('express');

module.exports = function ({ db, broadcast }) {
  const router = express.Router();

  // 获取所有分组
  router.get('/', async (req, res) => {
    try {
      const groups = await db.all('SELECT * FROM proxy_groups ORDER BY is_default DESC, id ASC');
      for (const group of groups) {
        group.domains = await db.all('SELECT * FROM proxy_group_domains WHERE group_id = ?', group.id);
        group.members = await db.all(`
          SELECT pgm.proxy_id, p.name, p.type, p.host, p.port, p.status, p.enabled
          FROM proxy_group_members pgm
          JOIN proxies p ON p.id = pgm.proxy_id
          WHERE pgm.group_id = ?
        `, group.id);
      }
      res.json(groups);
    } catch (error) {
      res.status(500).json({ error: error.message });
    }
  });

  // 创建分组
  router.post('/', async (req, res) => {
    try {
      const { name, domains = [], proxy_ids = [], is_default = 0, enabled = 1 } = req.body;
      if (!name) return res.status(400).json({ error: '分组名称不能为空' });

      // 如果设为默认组，先取消其他默认组
      if (is_default) {
        await db.run('UPDATE proxy_groups SET is_default = 0');
      }

      const result = await db.run(
        'INSERT INTO proxy_groups (name, is_default, enabled) VALUES (?, ?, ?)',
        [name, is_default ? 1 : 0, enabled ? 1 : 0]
      );
      const groupId = result.lastID;

      for (const domain of domains) {
        if (domain.trim()) {
          await db.run('INSERT INTO proxy_group_domains (group_id, domain) VALUES (?, ?)', [groupId, domain.trim().toLowerCase()]);
        }
      }
      for (const proxyId of proxy_ids) {
        await db.run('INSERT OR IGNORE INTO proxy_group_members (group_id, proxy_id) VALUES (?, ?)', [groupId, proxyId]);
      }

      const group = await db.get('SELECT * FROM proxy_groups WHERE id = ?', groupId);
      group.domains = await db.all('SELECT * FROM proxy_group_domains WHERE group_id = ?', groupId);
      group.members = await db.all(`
        SELECT pgm.proxy_id, p.name, p.type, p.host, p.port, p.status, p.enabled
        FROM proxy_group_members pgm JOIN proxies p ON p.id = pgm.proxy_id
        WHERE pgm.group_id = ?
      `, groupId);

      broadcast('proxy_group_created', group);
      res.json(group);
    } catch (error) {
      res.status(500).json({ error: error.message });
    }
  });

  // 更新分组
  router.put('/:id', async (req, res) => {
    try {
      const { name, domains, proxy_ids, is_default, enabled } = req.body;
      const groupId = req.params.id;

      const existing = await db.get('SELECT * FROM proxy_groups WHERE id = ?', groupId);
      if (!existing) return res.status(404).json({ error: '分组不存在' });

      // 如果设为默认组，先取消其他默认组
      if (is_default) {
        await db.run('UPDATE proxy_groups SET is_default = 0');
      }

      await db.run(
        'UPDATE proxy_groups SET name = ?, is_default = ?, enabled = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ?',
        [name ?? existing.name, is_default !== undefined ? (is_default ? 1 : 0) : existing.is_default, enabled !== undefined ? (enabled ? 1 : 0) : existing.enabled, groupId]
      );

      if (domains !== undefined) {
        await db.run('DELETE FROM proxy_group_domains WHERE group_id = ?', groupId);
        for (const domain of domains) {
          if (domain.trim()) {
            await db.run('INSERT INTO proxy_group_domains (group_id, domain) VALUES (?, ?)', [groupId, domain.trim().toLowerCase()]);
          }
        }
      }

      if (proxy_ids !== undefined) {
        await db.run('DELETE FROM proxy_group_members WHERE group_id = ?', groupId);
        for (const proxyId of proxy_ids) {
          await db.run('INSERT OR IGNORE INTO proxy_group_members (group_id, proxy_id) VALUES (?, ?)', [groupId, proxyId]);
        }
      }

      const group = await db.get('SELECT * FROM proxy_groups WHERE id = ?', groupId);
      group.domains = await db.all('SELECT * FROM proxy_group_domains WHERE group_id = ?', groupId);
      group.members = await db.all(`
        SELECT pgm.proxy_id, p.name, p.type, p.host, p.port, p.status, p.enabled
        FROM proxy_group_members pgm JOIN proxies p ON p.id = pgm.proxy_id
        WHERE pgm.group_id = ?
      `, groupId);

      broadcast('proxy_group_updated', group);
      res.json(group);
    } catch (error) {
      res.status(500).json({ error: error.message });
    }
  });

  // 删除分组
  router.delete('/:id', async (req, res) => {
    try {
      await db.run('DELETE FROM proxy_groups WHERE id = ?', req.params.id);
      broadcast('proxy_group_deleted', { id: req.params.id });
      res.json({ message: '分组已删除' });
    } catch (error) {
      res.status(500).json({ error: error.message });
    }
  });

  return router;
};
