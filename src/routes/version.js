const express = require('express');
const { getVersion } = require('../../version');

module.exports = function () {
  const router = express.Router();

  router.get('/', (req, res) => {
    try {
      const versionInfo = getVersion();
      res.json(versionInfo);
    } catch (error) {
      res.status(500).json({ error: error.message });
    }
  });

  return router;
};
