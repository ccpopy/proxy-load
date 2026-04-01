const express = require('express');
const { VALID_ALGORITHMS } = require('../constants');

module.exports = function ({ db, getProxyServer }) {
  const router = express.Router();

  router.get('/', async (req, res) => {
    try {
      const settings = await db.all('SELECT * FROM settings');
      const result = {};
      settings.forEach(s => { result[s.key] = s.value; });
      res.json(result);
    } catch (error) {
      res.status(500).json({ error: error.message });
    }
  });

  router.post('/', async (req, res) => {
    try {
      for (const [key, value] of Object.entries(req.body)) {
        if (key === 'algorithm') {
          const normalized = VALID_ALGORITHMS.has(value) ? value : 'adaptive';
          await db.run(
            'INSERT OR REPLACE INTO settings (key, value) VALUES (?, ?)',
            [key, normalized]
          );

          const proxyServer = getProxyServer();
          if (proxyServer) {
            proxyServer.currentAlgorithm = normalized;
            console.log(`切换到${normalized}算法`);
          }
          continue;
        }

        await db.run(
          'INSERT OR REPLACE INTO settings (key, value) VALUES (?, ?)',
          [key, value]
        );
      }
      res.json({ message: '设置已保存' });
    } catch (error) {
      res.status(500).json({ error: error.message });
    }
  });

  return router;
};
