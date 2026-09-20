fn main() {
    println!("cargo:rerun-if-env-changed=PROXY_LOAD_UPDATE_PUBLIC_KEY");
    println!("cargo:rerun-if-env-changed=PROXY_LOAD_UPDATE_MODE");
    tauri_build::build()
}
