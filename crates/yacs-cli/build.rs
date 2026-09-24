//! `yacs update` checks downloads against the same key as the desktop
//! updater, taken from tauri.conf.json so the two can't drift apart.

fn main() {
    let conf = "../../apps/desktop/src-tauri/tauri.conf.json";
    println!("cargo:rerun-if-changed={conf}");
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(conf).expect("reading tauri.conf.json"))
            .expect("tauri.conf.json is JSON");
    let key = json["plugins"]["updater"]["pubkey"]
        .as_str()
        .expect("tauri.conf.json has plugins.updater.pubkey");
    println!("cargo:rustc-env=YACS_UPDATER_PUBKEY={key}");
}
