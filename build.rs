fn main() {
    // Embed the icon in the Windows executable (taskbar, shortcuts, Explorer).
    #[cfg(windows)]
    {
        println!("cargo:rerun-if-changed=packaging/windows/cathode.rc");
        println!("cargo:rerun-if-changed=packaging/windows/cathode.ico");
        embed_resource::compile("packaging/windows/cathode.rc", embed_resource::NONE)
            .manifest_optional()
            .unwrap();
    }
}
