const express = require('express');
const { DEFAULT_ADVANCED_CONFIG } = require('../constants');

module.exports = function ({ db, applyRuntimeConfig }) {
  const router = express.Router();

  // 获取高级配置
  router.get('/', async (req, res) => {
    try {
      const configKeys = Object.keys(DEFAULT_ADVANCED_CONFIG);
      const settings = await db.all('SELECT key, value FROM settings WHERE key IN (' + configKeys.map(() => '?').join(',') + ')', configKeys);

      const config = { ...DEFAULT_ADVANCED_CONFIG };

      settings.forEach(setting => {
        try {
          // 尝试解析JSON（对于复杂对象如algorithm_weights）
          const value = JSON.parse(setting.value);
          config[setting.key] = value;
        } catch {
          // 如果不是JSON，直接使用值
          const numValue = Number(setting.value);
          config[setting.key] = isNaN(numValue) ? setting.value : numValue;
        }
      });

      res.json(config);
    } catch (error) {
      res.status(500).json({ error: error.message });
    }
  });

  // 保存高级配置
  router.post('/', async (req, res) => {
    try {
      const config = req.body;
      let requiresRestart = false;

      // 检查是否修改了需要重启的配置
      const currentProxyPort = await db.get("SELECT value FROM settings WHERE key = 'proxy_port'");
      if (currentProxyPort && Number(currentProxyPort.value) !== config.proxy_port) {
        requiresRestart = true;
      }

      // 保存配置到数据库
      for (const [key, value] of Object.entries(config)) {
        let saveValue = value;

        // 对于对象类型，转换为JSON字符串
        if (typeof value === 'object' && value !== null) {
          saveValue = JSON.stringify(value);
        }

        await db.run(
          'INSERT OR REPLACE INTO settings (key, value) VALUES (?, ?)',
          [key, String(saveValue)]
        );
      }

      // 应用配置到运行时
      applyRuntimeConfig(config);

      res.json({
        success: true,
        requiresRestart,
        message: requiresRestart ? '部分配置需要重启服务才能生效' : '配置已应用'
      });
    } catch (error) {
      res.status(500).json({ error: error.message });
    }
  });

  // 重置为默认配置
  router.post('/reset', async (req, res) => {
    try {
      // 删除所有高级配置
      const configKeys = Object.keys(DEFAULT_ADVANCED_CONFIG);
      await db.run('DELETE FROM settings WHERE key IN (' + configKeys.map(() => '?').join(',') + ')', configKeys);

      // 应用默认配置到运行时
      applyRuntimeConfig(DEFAULT_ADVANCED_CONFIG);

      res.json({ success: true, message: '已恢复默认配置' });
    } catch (error) {
      res.status(500).json({ error: error.message });
    }
  });

  // 导出配置
  router.get('/export', async (req, res) => {
    try {
      const allSettings = await db.all('SELECT key, value FROM settings');
      const config = {};

      allSettings.forEach(setting => {
        try {
          config[setting.key] = JSON.parse(setting.value);
        } catch {
          const numValue = Number(setting.value);
          config[setting.key] = isNaN(numValue) ? setting.value : numValue;
        }
      });

      // 添加代理列表
      const proxies = await db.all('SELECT * FROM proxies ORDER BY priority ASC, id ASC');
      config.proxies = proxies;

      res.json({
        version: '1.0.0',
        exportTime: new Date().toISOString(),
        config
      });
    } catch (error) {
      res.status(500).json({ error: error.message });
    }
  });

  return router;
};
