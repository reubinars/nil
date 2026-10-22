//! Gives `bp-gui.exe` the nil icon in Explorer and on the taskbar before it runs.

fn main() {
    println!("cargo:rerun-if-changed=assets/icon/nil.ico");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon/nil.ico");
        // A missing resource compiler only costs the Explorer icon; the window icon is
        // set at run time either way.
        if let Err(e) = res.compile() {
            println!("cargo:warning=no exe icon: {e}");
        }
    }
}
