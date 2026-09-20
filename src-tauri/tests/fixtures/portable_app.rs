// Isolated process fixture for the real Windows release update helper (no GUI/network).
fn main() {
    let exe = std::env::current_exe().unwrap();
    let root = exe.parent().unwrap();
    let data = std::env::var_os("DATA_DIR").map(std::path::PathBuf::from).unwrap_or_else(|| root.join("data"));
    assert!(data.join("proxy.db").is_file());
    std::fs::write(root.join("fixture-paths.txt"), format!("{}\n{}", root.display(), data.display())).unwrap();
    if let Some(path) = std::env::var_os("PROXY_LOAD_UPDATE_READY_FILE") {
        std::fs::write(path, std::process::id().to_string()).unwrap();
    }
    std::thread::sleep(std::time::Duration::from_secs(2));
}
