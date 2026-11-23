fn main() {
    slint_build::compile("ui/app.slint").expect("ui/app.slint failed to compile");
    // Gives the Windows executable its icon in Explorer and the taskbar.
    #[cfg(windows)]
    winresource::WindowsResource::new()
        .set_icon("../assets/app.ico")
        .compile()
        .expect("the executable icon could not be embedded");
}
