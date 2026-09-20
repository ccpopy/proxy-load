fn main() {
    println!("cargo:rerun-if-env-changed=PROXY_LOAD_UPDATE_PUBLIC_KEY");
    tauri_build::build()
}
