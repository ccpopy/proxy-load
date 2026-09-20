// IPC cannot be cancelled; stale completions must be ignored instead.
export function createLatestRequestGuard() {
  let revision = 0
  let currentKey = ""
  return {
    begin(key: string) {
      const ticket = ++revision
      currentKey = key
      return () => ticket === revision && key === currentKey
    },
  }
}
