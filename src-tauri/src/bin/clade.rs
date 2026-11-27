//! 控制台启动器：clade <profile> [claude 参数...]；不带参数时打开 GUI。
//! GUI 是 windows 子系统，没法在终端里正常占用控制台，所以单独做一个控制台程序。

use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(name) = args.next() else {
        return open_gui();
    };
    let extra: Vec<String> = args.collect();
    clade_lib::cli::run_cli(&name, &extra)
}

fn open_gui() -> ExitCode {
    let gui = std::env::current_exe().unwrap().with_file_name("clade-gui.exe");
    match Command::new(&gui).spawn() {
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("clade: 打开 {} 失败: {e}", gui.display());
            eprintln!("用法: clade <profile> [claude 参数...]");
            ExitCode::from(1)
        }
    }
}
