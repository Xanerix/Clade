//! 控制台启动器核心逻辑：供 `clade.exe` 与开发模式下 `clade-gui --clade-cli` 共用。

use crate::core;
use anyhow::Context;
use std::process::ExitCode;

pub fn run_cli(name: &str, extra: &[String]) -> ExitCode {
    // Ctrl+C 交给 claude 处理，启动器自己不退出
    let _ = ctrlc::set_handler(|| {});

    match run(name, extra) {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("clade: {e:#}");
            ExitCode::from(1)
        }
    }
}

fn run(name: &str, extra: &[String]) -> anyhow::Result<u8> {
    if core::find_claude().is_none() {
        anyhow::bail!(
            "未检测到 Claude Code CLI（claude）。请先安装 Claude Code，并在终端确认 claude --version 可用。\n安装命令（PowerShell）：irm https://claude.ai/install.ps1 | iex"
        );
    }
    let cwd = std::env::current_dir()?;

    let conflicts = core::conflict_env();
    if !conflicts.is_empty() {
        eprintln!("clade: 已清除冲突的环境变量：{}", conflicts.join(", "));
    }
    let check = core::check_dir(name, &cwd)?;
    if !check.nested_claude_md.is_empty() {
        eprintln!("clade: 注意，父目录里的这些 CLAUDE.md 会被当作项目指令加载：");
        for f in &check.nested_claude_md {
            eprintln!("  {f}");
        }
    }
    if check.scratchpad_skipped {
        eprintln!("clade: 工作目录路径太长，本次不提供 scratchpad");
    }
    if let Some((k, env, sys)) = core::proxy_mismatch(&core::load()?.global) {
        eprintln!("clade: 警告，终端里的 {k}={env} 和系统代理 {sys} 不一致");
        eprint!("继续用终端的代理运行？[y/N] ");
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        if !line.trim().eq_ignore_ascii_case("y") {
            return Ok(1);
        }
    }

    let mut cmd = core::command(name, &cwd, extra)?;
    let wb_before = core::snapshot_profile_writeback(name).unwrap_or_else(|e| {
        eprintln!("clade: 读取设置写回快照失败：{e:#}");
        None
    });
    let status = cmd.status().context("启动 Claude Code CLI 失败，请确认 claude 已正确安装")?;
    if let Some(before) = wb_before {
        match core::writeback_profile(name, &before) {
            Ok(res) => {
                if !res.ui_prefs.is_empty() {
                    eprintln!("clade: 已将界面偏好写回 ~/.claude.json：{}", res.ui_prefs.join(", "));
                }
                if !res.settings.is_empty() {
                    eprintln!("clade: 已将设置改动写回 ~/.claude/settings.json：{}", res.settings.join(", "));
                }
            }
            Err(e) => eprintln!("clade: 写回设置失败：{e:#}"),
        }
    }
    Ok(status.code().unwrap_or(1) as u8)
}
