// Embeds the application icon into the Windows executable.
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../packaging/icons/jarvis-core.ico");
        res.set("FileDescription", "Jarvis Core");
        res.set("ProductName", "Jarvis Core");
        if let Err(e) = res.compile() {
            // Cross-compiling without a resource compiler: build without the icon.
            println!("cargo:warning=ícone do Windows não incorporado: {e}");
        }
    }
}
