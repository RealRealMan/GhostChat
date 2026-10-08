// Embeds the app icon and version info into the Windows executable so the
// desktop shortcut and Explorer show the GhostChat icon.
fn main() {
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icons/icon.ico");
        if let Err(e) = res.compile() {
            // A missing Windows SDK must not break development builds.
            println!("cargo:warning=could not embed Windows icon: {e}");
        }
    }
    println!("cargo:rerun-if-changed=assets/icons/icon.ico");
}
