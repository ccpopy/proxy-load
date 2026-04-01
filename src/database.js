const path = require('path');
const fs = require('fs');
const { open } = require('sqlite');
const sqlite3 = require('sqlite3').verbose();

async function initDatabase () {
  const DATA_DIR = process.env.DATA_DIR || path.join(process.cwd(), 'data');
  await fs.promises.mkdir(DATA_DIR, { recursive: true });
  const DB_FILE = path.join(DATA_DIR, 'proxy.db');

  const db = await open({ filename: DB_FILE, driver: sqlite3.Database });

  await db.exec(`PRAGMA journal_mode = WAL;`);
  await db.exec(`PRAGMA foreign_keys = ON;`);
  await db.exec(`PRAGMA synchronous = NORMAL;`);

  // 创建表结构
  await db.exec(`
    CREATE TABLE IF NOT EXISTS proxies (
      id INTEGER PRIMARY KEY AUTOINCREMENT,
      name TEXT NOT NULL,
      type TEXT NOT NULL,
      host TEXT NOT NULL,
      port INTEGER NOT NULL,
      username TEXT,
      password TEXT,
      status TEXT DEFAULT 'unknown',
      last_test DATETIME,
      response_time INTEGER,
      success_count INTEGER DEFAULT 0,
      fail_count INTEGER DEFAULT 0,
      priority INTEGER DEFAULT 999,
      enabled INTEGER DEFAULT 1,
      bandwidth_bps INTEGER DEFAULT NULL,
      bandwidth_test_time DATETIME DEFAULT NULL,
      created_at DATETIME DEFAULT CURRENT_TIMESTAMP
    )
  `);

  await db.exec(`
    CREATE TABLE IF NOT EXISTS settings (
      key TEXT PRIMARY KEY,
      value TEXT NOT NULL
    )
  `);

  await db.exec(`
    CREATE TABLE IF NOT EXISTS request_logs (
      id INTEGER PRIMARY KEY AUTOINCREMENT,
      proxy_id INTEGER,
      target_host TEXT,
      target_port INTEGER,
      success BOOLEAN,
      response_time INTEGER,
      error_message TEXT,
      result_type TEXT,
      created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
      FOREIGN KEY (proxy_id) REFERENCES proxies(id) ON DELETE CASCADE
    )
  `);

  await db.exec(`
    CREATE TABLE IF NOT EXISTS load_stats (
      id INTEGER PRIMARY KEY AUTOINCREMENT,
      proxy_id INTEGER,
      weight REAL,
      success_rate REAL,
      avg_response_time INTEGER,
      requests_count INTEGER,
      timestamp DATETIME DEFAULT CURRENT_TIMESTAMP,
      FOREIGN KEY (proxy_id) REFERENCES proxies(id) ON DELETE CASCADE
    )
  `);

  // 创建DNS映射表
  await db.exec(`
    CREATE TABLE IF NOT EXISTS dns_mappings (
      id INTEGER PRIMARY KEY AUTOINCREMENT,
      domain TEXT NOT NULL UNIQUE,
      ip TEXT NOT NULL,
      description TEXT,
      enabled INTEGER DEFAULT 1,
      created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
      updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
    )
  `);

  // 创建索引
  await db.exec(`CREATE INDEX IF NOT EXISTS idx_logs_created_at ON request_logs (created_at);`);
  await db.exec(`CREATE INDEX IF NOT EXISTS idx_logs_proxy_id ON request_logs (proxy_id);`);
  await db.exec(`CREATE INDEX IF NOT EXISTS idx_logs_target ON request_logs (target_host, created_at);`);
  await db.exec(`CREATE INDEX IF NOT EXISTS idx_load_stats_proxy ON load_stats (proxy_id, timestamp);`);
  await db.exec(`CREATE INDEX IF NOT EXISTS idx_dns_domain ON dns_mappings (domain);`);
  await db.exec(`CREATE INDEX IF NOT EXISTS idx_dns_ip ON dns_mappings (ip);`);

  // 创建代理分组表
  await db.exec(`
    CREATE TABLE IF NOT EXISTS proxy_groups (
      id INTEGER PRIMARY KEY AUTOINCREMENT,
      name TEXT NOT NULL,
      is_default INTEGER DEFAULT 0,
      enabled INTEGER DEFAULT 1,
      created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
      updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
    )
  `);

  await db.exec(`
    CREATE TABLE IF NOT EXISTS proxy_group_domains (
      id INTEGER PRIMARY KEY AUTOINCREMENT,
      group_id INTEGER NOT NULL,
      domain TEXT NOT NULL,
      FOREIGN KEY (group_id) REFERENCES proxy_groups(id) ON DELETE CASCADE
    )
  `);

  await db.exec(`
    CREATE TABLE IF NOT EXISTS proxy_group_members (
      id INTEGER PRIMARY KEY AUTOINCREMENT,
      group_id INTEGER NOT NULL,
      proxy_id INTEGER NOT NULL,
      FOREIGN KEY (group_id) REFERENCES proxy_groups(id) ON DELETE CASCADE,
      FOREIGN KEY (proxy_id) REFERENCES proxies(id) ON DELETE CASCADE,
      UNIQUE(group_id, proxy_id)
    )
  `);

  await db.exec(`CREATE INDEX IF NOT EXISTS idx_group_domains_group ON proxy_group_domains (group_id);`);
  await db.exec(`CREATE INDEX IF NOT EXISTS idx_group_domains_domain ON proxy_group_domains (domain);`);
  await db.exec(`CREATE INDEX IF NOT EXISTS idx_group_members_group ON proxy_group_members (group_id);`);
  await db.exec(`CREATE INDEX IF NOT EXISTS idx_group_members_proxy ON proxy_group_members (proxy_id);`);

  // 添加带宽列（如果不存在）
  const columns = await db.all("PRAGMA table_info(proxies)");
  const hasBandwidth = columns.some(col => col.name === 'bandwidth_bps');
  if (!hasBandwidth) {
    await db.exec(`ALTER TABLE proxies ADD COLUMN bandwidth_bps INTEGER DEFAULT NULL`);
    await db.exec(`ALTER TABLE proxies ADD COLUMN bandwidth_test_time DATETIME DEFAULT NULL`);
  }

  // 添加代理专属测试配置列（如果不存在）
  const hasTestUrl = columns.some(col => col.name === 'test_url');
  if (!hasTestUrl) {
    await db.exec(`ALTER TABLE proxies ADD COLUMN test_url TEXT DEFAULT NULL`);
    await db.exec(`ALTER TABLE proxies ADD COLUMN test_timeout INTEGER DEFAULT NULL`);
  }

  const logColumns = await db.all("PRAGMA table_info(request_logs)");
  const hasResultType = logColumns.some(col => col.name === 'result_type');
  if (!hasResultType) {
    await db.exec(`ALTER TABLE request_logs ADD COLUMN result_type TEXT`);
  }

  // 初始化默认设置
  const testUrl = await db.get("SELECT value FROM settings WHERE key = 'test_url'");
  if (!testUrl) {
    await db.run("INSERT INTO settings (key, value) VALUES ('test_url', 'https://cms.zjzwfw.gov.cn/favicon.ico')");
    await db.run("INSERT INTO settings (key, value) VALUES ('timeout', '10')");
    await db.run("INSERT INTO settings (key, value) VALUES ('load_mode', 'auto')");
    await db.run("INSERT INTO settings (key, value) VALUES ('algorithm', 'adaptive')");
  }

  return db;
}

module.exports = { initDatabase };
