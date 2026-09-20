export const groupAlgorithms: Record<string, string> = {
  inherit: "继承全局设置",
  adaptive: "自适应算法",
  round_robin: "轮询",
  least_connections: "最小连接数",
  sticky_host: "按目标主机粘滞",
}

export function groupPolicyPayload(algorithm: string, hold: string) {
  const seconds = Number(hold)
  if (!Object.hasOwn(groupAlgorithms, algorithm)) throw new Error("请选择有效的分组算法")
  if (hold.trim() === "" || !Number.isSafeInteger(seconds) || seconds < 0 || seconds > 86400) {
    throw new Error("故障切换保持期必须为 0 到 86400 之间的整数")
  }
  return { algorithm_override: algorithm === "inherit" ? null : algorithm, sticky_failover_seconds: seconds }
}
