//! Render key phone screens to PNG files for visual inspection — headless,
//! no display server needed.
//!
//! ```text
//! cargo run --release -p phone --example snapshot_png -- <out-dir>
//! ```

use std::path::PathBuf;

use phone::state::{AppId, BootPhase, OsState};
use phone::{compose, persist};

fn save_png(path: &PathBuf, w: u32, h: u32, rgba: &[u8]) {
    let mut enc = png::Encoder::new(std::fs::File::create(path).expect("create png"), w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header().expect("png header");
    writer.write_image_data(rgba).expect("png data");
    println!("wrote {}", path.display());
}

fn frame_of(st: &mut OsState) -> (u32, u32, Vec<u8>) {
    compose::tick(st);
    let (w, h) = st.effective_size();
    (w, h, st.fb.clone())
}

fn main() {
    let out = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("snapshots"));
    std::fs::create_dir_all(&out).unwrap();

    let dir = out.join("data");
    std::fs::create_dir_all(&dir).unwrap();

    // 1. boot animation mid-flight
    let mut st = persist::fresh(&dir);
    st.boot = BootPhase::Splash {
        t0: std::time::SystemTime::now() - std::time::Duration::from_millis(700),
    };
    // fake elapsed by constructing a past t0 via rewind: just render twice
    let (w, h, px) = frame_of(&mut st);
    save_png(&out.join("boot.png"), w, h, &px);

    // 2. home launcher
    let mut st = persist::fresh(&dir);
    st.boot = BootPhase::Home;
    st.dirty = true;
    let (w, h, px) = frame_of(&mut st);
    save_png(&out.join("home.png"), w, h, &px);

    // 3. messages app with keyboard
    let mut st = persist::fresh(&dir);
    st.boot = BootPhase::Home;
    st.dirty = true;
    st.launch_app(AppId::Messages);
    st.app.messages.kb.visible = true;
    let (w, h, px) = frame_of(&mut st);
    save_png(&out.join("messages_kb.png"), w, h, &px);

    // 4. recents
    let mut st = persist::fresh(&dir);
    st.boot = BootPhase::Home;
    st.launch_app(AppId::Settings);
    st.launch_app(AppId::Calculator);
    st.launch_app(AppId::Clock);
    st.recents_screen_open = true;
    st.dirty = true;
    let (w, h, px) = frame_of(&mut st);
    save_png(&out.join("recents.png"), w, h, &px);

    // 5. termux
    let mut st = persist::fresh(&dir);
    st.boot = BootPhase::Home;
    st.launch_app(AppId::Termux);
    st.app.termux.lines.push(phone::state::TermuxLine {
        text: "Welcome to Termux (roma-os)".into(),
        scale: 3,
        color: [129, 201, 149],
    });
    st.app.termux.lines.push(phone::state::TermuxLine {
        text: "~ $ uname -a".into(),
        scale: 3,
        color: [232, 234, 237],
    });
    st.app.termux.lines.push(phone::state::TermuxLine {
        text: "Linux localhost 5.15.104-android13 #1 SMP".into(),
        scale: 3,
        color: [154, 160, 166],
    });
    st.app.termux.kb.visible = true;
    st.dirty = true;
    let (w, h, px) = frame_of(&mut st);
    save_png(&out.join("termux.png"), w, h, &px);
}
