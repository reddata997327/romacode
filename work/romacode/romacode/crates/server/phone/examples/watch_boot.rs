use phone::{boot, PhoneConfig};
use std::time::Duration;
fn main() {
    let data = std::env::var_os("WATCH_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp/watch-boot"));
    let cfg = PhoneConfig {
        port: 5588,
        data_dir: data,
        display: (720, 1560),
        density: 420,
        ..Default::default()
    };
    let handle = boot(cfg).expect("boot");
    for i in 0..20 {
        std::thread::sleep(Duration::from_millis(700));
        let st = handle.state.lock().unwrap();
        eprintln!(
            "t={}ms boot={:?} dmesg={} fb_gen={} installer={}",
            i * 700,
            st.boot,
            st.dmesg.len(),
            st.fb_gen,
            st.installer.is_some()
        );
    }
    handle.shutdown();
}
