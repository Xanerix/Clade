pub mod antigravity;
pub mod convert;
pub mod core;
pub mod gateway;
pub mod oauth;
pub mod openai_oauth;

use core::{Auth, AuthStatus, Card, CardKind, DirCheck, Global, GlobalPolicyState, Library, Profile, Switch, SwitchDef, Usage};
use openai_oauth::DeviceFlowStart;
use serde::Serialize;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Command,
};

type Res<T> = Result<T, String>;

fn err(e: anyhow::Error) -> String {
    format!("{e:#}")
}

/// 控制台启动器 clade.exe 与 GUI 放在同一目录
fn exe_dir() -> PathBuf {
    std::env::current_exe().unwrap().parent().unwrap().to_path_buf()
}

fn ps_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SwitchDefs {
    global: &'static [SwitchDef],
    profile: &'static [SwitchDef],
    compat: &'static [SwitchDef],
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct State {
    root: String,
    global: Global,
    cards: Vec<Card>,
    profiles: Vec<Profile>,
    library: Library,
    /// 模型卡 id -> 是否已存 key
    has_key: BTreeMap<String, bool>,
    /// 环境里存在、启动时会被清除的冲突变量名
    conflict_env: Vec<&'static str>,
    switches: SwitchDefs,
    /// 注册表里的系统代理地址（不看 ProxyEnable）
    system_proxy: Option<String>,
    /// 推荐的精简 auto 模式分类器提示词模板
    default_classifier_prompt: &'static str,
    /// 从 ~/.claude/settings.json 读出的权限与署名当前状态
    global_policy: GlobalPolicyState,
}

#[tauri::command]
fn get_state() -> Res<State> {
    let store = core::load().map_err(err)?;
    let mut has_key = BTreeMap::new();
    for c in &store.cards {
        has_key.insert(c.id.clone(), core::get_key(&c.id).map_err(err)?.is_some());
    }
    Ok(State {
        root: core::root().display().to_string(),
        library: core::library().map_err(err)?,
        global: store.global,
        cards: store.cards,
        profiles: store.profiles,
        has_key,
        conflict_env: core::conflict_env(),
        switches: SwitchDefs {
            global: core::GLOBAL_SWITCHES,
            profile: core::PROFILE_SWITCHES,
            compat: core::COMPAT_SWITCHES,
        },
        system_proxy: core::system_proxy(),
        default_classifier_prompt: core::DEFAULT_CLASSIFIER_PROMPT,
        global_policy: core::global_policy_state(),
    })
}

#[tauri::command]
fn save_global(global: Global) -> Res<()> {
    let mut store = core::load().map_err(err)?;
    store.global = global;
    core::save(&store).map_err(err)
}

/// key: None 不改，Some("") 删除，其余为新值
#[tauri::command]
fn save_card(card: Card, key: Option<String>, is_new: bool) -> Res<()> {
    core::validate_card(&card).map_err(err)?;
    let mut store = core::load().map_err(err)?;
    let pos = store.cards.iter().position(|c| c.id == card.id);
    match (pos, is_new) {
        (Some(_), true) => return Err(format!("模型卡 {} 已存在", card.id)),
        (None, false) => return Err(format!("模型卡 {} 不存在", card.id)),
        _ => {}
    }
    if let Some(k) = key {
        core::set_key(&card.id, k.trim()).map_err(err)?;
    }
    match pos {
        Some(i) => store.cards[i] = card,
        None => store.cards.push(card),
    }
    core::save(&store).map_err(err)
}

#[tauri::command]
async fn delete_card(id: String) -> Res<()> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut store = core::load()?;
        core::delete_card(&mut store, &id)?;
        core::save(&store)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(err)
}

/// key 为空时用 card_id 已保存的 key；OpenAI / Antigravity 登录卡走各自订阅模型接口。
#[tauri::command]
async fn fetch_models(base_url: String, auth: Auth, key: String, card_id: String) -> Res<Vec<String>> {
    let store = core::load().map_err(err)?;
    if let Some(c) = store.cards.iter().find(|c| c.id == card_id) {
        match c.kind {
            CardKind::OpenAiOAuth => return openai_oauth::fetch_models(&card_id, &store.global).await.map_err(err),
            CardKind::Antigravity => return antigravity::fetch_models(&card_id, &store.global).await.map_err(err),
            _ => {}
        }
    }
    let key = if key.trim().is_empty() {
        core::get_key(&card_id).map_err(err)?.ok_or("先填 API key")?
    } else {
        key.trim().to_string()
    };
    tauri::async_runtime::spawn_blocking(move || core::fetch_models(&base_url, auth, &key))
        .await
        .map_err(|e| e.to_string())?
        .map_err(err)
}

#[tauri::command]
async fn card_status(id: String) -> Res<AuthStatus> {
    tauri::async_runtime::spawn_blocking(move || core::card_auth_status(&id))
        .await
        .map_err(|e| e.to_string())?
        .map_err(err)
}

#[tauri::command]
async fn card_logout(id: String) -> Res<()> {
    tauri::async_runtime::spawn_blocking(move || core::card_logout(&id))
        .await
        .map_err(|e| e.to_string())?
        .map_err(err)
}

/// card 为空时查 ~/.claude 的登录
#[tauri::command]
async fn usage(card: String) -> Res<Usage> {
    tauri::async_runtime::spawn_blocking(move || core::usage(&card))
        .await
        .map_err(|e| e.to_string())?
        .map_err(err)
}

/// 新开一个 pwsh7 窗口，在官方卡目录里运行 claude auth login --claudeai（要在浏览器里完成授权）。
/// 成功就自动关窗口，失败停住让用户看报错；等窗口关掉后返回最新的登录状态。
#[tauri::command]
async fn card_login(id: String) -> Res<AuthStatus> {
    core::validate_name(&id).map_err(err)?;
    let pwsh = core::find_pwsh().ok_or("找不到 pwsh7")?;
    let dir = core::card_dir(&id);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let d = ps_quote(&dir.to_string_lossy());
    let mut clear: Vec<String> = core::CONFLICT_ENV.iter().map(|k| format!("Remove-Item Env:{k} -ErrorAction Ignore")).collect();
    // 代理开关和 core::apply_proxy 一样处理；pwsh 配置文件里设的代理变量在这之前已经生效
    match core::load().map_err(err)?.global.proxy {
        Some(Switch::Off) => clear.extend(core::PROXY_ENV.map(|k| format!("Remove-Item Env:{k} -ErrorAction Ignore"))),
        Some(Switch::On) => {
            if let Some(p) = core::system_proxy() {
                let p = ps_quote(&p);
                clear.extend(core::PROXY_ENV.map(|k| format!("if (-not $env:{k}) {{ $env:{k}={p} }}")));
            }
        }
        None => {}
    }
    let script = format!(
        "{}; $env:CLAUDE_CONFIG_DIR={d}; $env:CLAUDE_SECURESTORAGE_CONFIG_DIR={d}; claude auth login --claudeai; if ($LASTEXITCODE) {{ Read-Host '登录没有成功，按回车关闭窗口' }}",
        clear.join("; ")
    );
    let cmdline = format!("\"{}\" -NoLogo -Command \"{script}\"", pwsh.display());
    tauri::async_runtime::spawn_blocking(move || {
        core::spawn_console(&cmdline, Some(&dir), true)?;
        core::card_auth_status(&id)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(err)
}

#[tauri::command]
async fn openai_browser_login(id: String) -> Res<AuthStatus> {
    core::validate_name(&id).map_err(err)?;
    let global = core::load().map_err(err)?.global;
    openai_oauth::browser_login(&id, &global).await.map_err(err)
}

#[tauri::command]
fn openai_cancel_login() {
    openai_oauth::cancel_browser_login();
}

#[tauri::command]
async fn openai_device_start() -> Res<DeviceFlowStart> {
    let global = core::load().map_err(err)?.global;
    openai_oauth::start_device_flow(&global).await.map_err(err)
}

#[tauri::command]
async fn openai_device_poll(id: String, device_auth_id: String, user_code: String) -> Res<Option<AuthStatus>> {
    core::validate_name(&id).map_err(err)?;
    let global = core::load().map_err(err)?.global;
    openai_oauth::poll_device_flow(&id, &device_auth_id, &user_code, &global)
        .await
        .map_err(err)
}

#[tauri::command]
async fn antigravity_browser_login(id: String) -> Res<AuthStatus> {
    core::validate_name(&id).map_err(err)?;
    let global = core::load().map_err(err)?.global;
    antigravity::browser_login(&id, &global).await.map_err(err)
}

#[tauri::command]
fn antigravity_cancel_login() {
    antigravity::cancel_browser_login();
}

#[tauri::command]
fn save_profile(profile: Profile, is_new: bool) -> Res<()> {
    core::validate_name(&profile.name).map_err(err)?;
    let mut store = core::load().map_err(err)?;
    let pos = store.profiles.iter().position(|p| p.name == profile.name);
    match (pos, is_new) {
        (Some(_), true) => return Err(format!("{} 已存在", profile.name)),
        (None, false) => return Err(format!("{} 不存在", profile.name)),
        _ => {}
    }
    core::apply(&store, &profile).map_err(err)?;
    match pos {
        Some(i) => store.profiles[i] = profile,
        None => store.profiles.push(profile),
    }
    core::save(&store).map_err(err)
}

#[tauri::command]
fn clone_profile(src: String, name: String) -> Res<()> {
    let mut store = core::load().map_err(err)?;
    let p = core::clone_profile(&store, &src, &name).map_err(err)?;
    core::apply(&store, &p).map_err(err)?;
    store.profiles.push(p);
    core::save(&store).map_err(err)
}

/// 只移除登记，目录保留（里面有历史记录和登录凭据）
#[tauri::command]
fn delete_profile(name: String) -> Res<()> {
    let mut store = core::load().map_err(err)?;
    store.profiles.retain(|p| p.name != name);
    core::save(&store).map_err(err)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PwshStatus {
    pwsh: Option<String>,
    winget: bool,
}

#[tauri::command]
fn pwsh_status() -> PwshStatus {
    PwshStatus {
        pwsh: core::find_pwsh().map(|p| p.display().to_string()),
        winget: core::has_winget(),
    }
}

/// 在新控制台窗口里跑 winget 安装 pwsh7（用户能看到进度、回答协议提示），装完返回重新检测的结果。
#[tauri::command]
async fn install_pwsh() -> Res<PwshStatus> {
    tauri::async_runtime::spawn_blocking(|| core::spawn_console("winget install --id Microsoft.PowerShell", None, true))
        .await
        .map_err(|e| e.to_string())?
        .map_err(err)?;
    Ok(pwsh_status())
}

#[tauri::command]
fn recent_dirs(name: String) -> Vec<String> {
    core::recent_dirs(&name)
}

#[tauri::command]
fn check_dir(name: String, dir: String) -> Res<DirCheck> {
    core::check_dir(&name, Path::new(&dir)).map_err(err)
}

/// 新开一个 pwsh7 窗口，在 dir 里运行 clade <name>；claude 退出后窗口停在提示符。
#[tauri::command]
fn launch(name: String, dir: String) -> Res<()> {
    let clade = exe_dir().join("clade.exe");
    if !clade.exists() {
        return Err(format!("找不到 {}，请先 cargo build --bin clade", clade.display()));
    }
    let pwsh = core::find_pwsh().ok_or("找不到 pwsh7")?;
    // name 已通过 validate_name，只含小写字母、数字和 -
    let script = format!("& {} {}", ps_quote(&clade.to_string_lossy()), name);
    // Windows 路径里不会有双引号，直接包一层即可
    let cmdline = format!("\"{}\" -NoLogo -NoExit -Command \"{script}\"", pwsh.display());
    core::spawn_console(&cmdline, Some(Path::new(&dir)), false).map_err(err)
}

/// name 为空时打开数据目录 ~/.clade
#[tauri::command]
fn open_dir(name: String) -> Res<()> {
    let dir = if name.is_empty() { core::root() } else { core::profile_dir(&name) };
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Command::new("explorer.exe").arg(dir).spawn().map_err(|e| e.to_string())?;
    Ok(())
}

/// 用默认浏览器打开网址
#[tauri::command]
fn open_url(url: String) -> Res<()> {
    Command::new("explorer.exe").arg(url).spawn().map_err(|e| e.to_string())?;
    Ok(())
}

/// 把 clade 所在目录加入用户级 PATH。返回 (目录, 是否新加入)。
#[tauri::command]
fn add_to_path() -> Res<(String, bool)> {
    let dir = exe_dir();
    let added = core::add_to_user_path(&dir).map_err(err)?;
    Ok((dir.display().to_string(), added))
}

#[tauri::command]
fn import_claude() -> Res<Vec<String>> {
    core::import_from_claude().map_err(err)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            get_state,
            save_global,
            save_card,
            delete_card,
            fetch_models,
            card_status,
            card_logout,
            card_login,
            openai_browser_login,
            openai_cancel_login,
            openai_device_start,
            openai_device_poll,
            antigravity_browser_login,
            antigravity_cancel_login,
            usage,
            save_profile,
            clone_profile,
            delete_profile,
            pwsh_status,
            install_pwsh,
            recent_dirs,
            check_dir,
            launch,
            open_dir,
            open_url,
            add_to_path,
            import_claude
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
