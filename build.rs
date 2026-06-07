// Embute o ícone (assets/abyss.ico) no executável no Windows.
fn main() {
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/abyss.ico");
        // Caminhos do toolchain MinGW (mesmo do .cargo/config.toml).
        res.set_windres_path("C:/msys64/mingw64/bin/windres.exe");
        res.set_ar_path("C:/msys64/mingw64/bin/ar.exe");
        if let Err(e) = res.compile() {
            println!("cargo:warning=Falha ao embutir o ícone no .exe: {e}");
        }
    }
    println!("cargo:rerun-if-changed=assets/abyss.ico");
}
