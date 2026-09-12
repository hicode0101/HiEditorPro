// Windows 资源编译：为 exe 内嵌图标（资源管理器/任务栏/快捷方式显示）
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.compile().expect("compile windows resources failed");
    }
}
