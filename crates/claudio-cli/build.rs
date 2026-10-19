// Embeds the application icon into the Windows executable.
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../packaging/icons/claudio-code.ico");
        res.set("FileDescription", "Claudio Code");
        res.set("ProductName", "Claudio Code");
        if let Err(e) = res.compile() {
            // Cross-compiling without a resource compiler: build without the icon.
            println!("cargo:warning=ícone do Windows não incorporado: {e}");
        }
    }
}
