// Windows: embed a manifest asking for admin rights, which openconnect needs
// to create the wintun adapter, set routes and DNS. Windows shows one UAC
// prompt when FortiBar starts instead of one per connect.
fn main() {
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        use embed_manifest::manifest::ExecutionLevel;
        use embed_manifest::{embed_manifest, new_manifest};
        embed_manifest(new_manifest("FortiBar").requested_execution_level(ExecutionLevel::RequireAdministrator))
            .expect("unable to embed manifest");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
