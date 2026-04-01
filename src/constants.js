// 默认高级配置
const DEFAULT_ADVANCED_CONFIG = {
  // 基础配置
  proxy_port: Number(process.env.PROXY_PORT) || 5678,
  periodic_test_interval: 5 * 60 * 1000, // 5分钟
  log_retention_days: 7,
  stats_retention_days: 30,

  // 连接池配置
  pool_max_size: 50,
  pool_idle_timeout: 30000, // 30秒
  pool_wait_timeout: 10000, // 10秒

  // 熔断器配置
  circuit_failure_threshold: 5,
  circuit_timeout: 60000, // 60秒
  circuit_half_open_attempts: 2,

  // 健康检查配置
  health_check_interval: 30000, // 30秒
  health_degrade_threshold: 0.5,
  health_recover_threshold: 0.8,

  // 快速失败配置
  failfast_enabled: true,
  failfast_max_attempts: 3,
  failfast_attempt_timeout: 10000,
  failfast_total_timeout: 30000,

  // 算法权重配置
  algorithm_weights: {
    responseTime: 0.25,
    successRate: 0.20,
    bandwidth: 0.15,
    connections: 0.15,
    stability: 0.15,
    recentPerf: 0.10
  }
};

const VALID_ALGORITHMS = new Set([
  'weighted_round_robin',
  'least_connections',
  'adaptive',
  'sticky_host'
]);

const TRAFFIC_LOG_LIMIT = 100;

module.exports = { DEFAULT_ADVANCED_CONFIG, VALID_ALGORITHMS, TRAFFIC_LOG_LIMIT };
