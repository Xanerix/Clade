// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> std::process::ExitCode {
    // 开发模式下 clade-gui 本身是控制台子系统，GUI 启动环境时直接复用当前二进制，
    // 让标准 `bun run tauri dev` 无需预先单独构建 clade.exe。
    #[cfg(debug_assertions)]
    {
        let mut args = std::env::args().skip(1);
        if args.next().as_deref() == Some("--clade-cli") {
            if let Some(name) = args.next() {
                let extra: Vec<String> = args.collect();
                return clade_lib::cli::run_cli(&name, &extra);
            }
        }
    }

    clade_lib::run();
    std::process::ExitCode::SUCCESS
}
