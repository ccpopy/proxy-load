export const UPDATE_MIRROR_STORAGE_KEY = "proxy-load-update-mirror"
export const UPDATE_AUTO_CHECK_STORAGE_KEY = "proxy-load-update-auto-check"

export function readBooleanPreference(key: string) {
  return localStorage.getItem(key) === "1"
}

export function writeBooleanPreference(key: string, value: boolean) {
  localStorage.setItem(key, value ? "1" : "0")
}
