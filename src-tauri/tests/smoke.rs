//! 会读写真实的 ~/.clade、~/.claude 下的 junction 目标和凭据管理器（只用 zz- 前缀的测试数据，结束后清理）。
//! 运行：cargo test --test smoke -- --include-ignored --test-threads=1

use clade_lib::core::{self, ApiFormat, Auth, Card, CardKind, CardModel, Conn, Mode, Pick, Profile, Store, Switch};
use serde_json::Value;
use std::{fs, path::Path, process::Command};

fn read_json(p: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(p).unwrap()).unwrap()
}

fn args_of(cmd: &Command) -> Vec<String> {
    cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect()
}

fn envs_of(cmd: &Command) -> Vec<(String, Option<String>)> {
    cmd.get_envs()
        .map(|(k, v)| (k.to_string_lossy().into_owned(), v.map(|v| v.to_string_lossy().into_owned())))
        .collect()
}

/// 测试会改写真实的 clade.json：先备份，结束时（包括中途失败）原样还原。
struct StoreGuard(Option<Vec<u8>>);

impl StoreGuard {
    fn new() -> Self {
        Self(fs::read(core::root().join("clade.json")).ok())
    }
}

impl Drop for StoreGuard {
    fn drop(&mut self) {
        let path = core::root().join("clade.json");
        let _ = match &self.0 {
            Some(bytes) => fs::write(&path, bytes),
            None => fs::remove_file(&path),
        };
    }
}

fn test_card() -> Card {
    Card {
        id: "zz-card".into(),
        name: "测试卡".into(),
        base_url: "https://example.com/anthropic".into(),
        model_list: vec![model("m-big", 128_000), model("m-small", 200_000), model("m-judge", 200_000)],
        ..Default::default()
    }
}

fn model(id: &str, context: u64) -> CardModel {
    CardModel { id: id.into(), context }
}

fn pick(card: &str, id: &str) -> Pick {
    Pick { card: card.into(), id: id.into() }
}

#[test]
#[ignore]
fn apply_launch_and_cleanup() {
    let _guard = StoreGuard::new();
    let name = "zz-test";
    let skill = core::lib_dir("skills").join("zz-skill");
    let mcp = core::lib_dir("mcp").join("zz-mcp.json");
    fs::create_dir_all(&skill).unwrap();
    fs::write(skill.join("SKILL.md"), "test").unwrap();
    fs::create_dir_all(mcp.parent().unwrap()).unwrap();
    fs::write(&mcp, r#"{"command":"echo"}"#).unwrap();
    let dir = core::profile_dir(name);
    let _ = fs::remove_dir_all(&dir);

    let mut store = Store::default();
    store.global.assets.mcp = vec!["zz-mcp".into()];
    store.global.switches.insert("DISABLE_TELEMETRY".into(), Switch::Off);
    store.global.switches.insert("CLAUDE_CODE_DISABLE_AUTO_MEMORY".into(), Switch::On); // profile 级开关，放全局无效
    store.cards.push(test_card());
    let mut p = Profile {
        name: name.into(),
        card: "zz-card".into(),
        compat: vec!["CLAUDE_CODE_DISABLE_THINKING".into()],
        ..Default::default()
    };
    assert!(core::apply(&store, &p).is_err(), "用第三方卡时主模型必填");
    p.models.main = pick("zz-card", "m-big");
    p.models.haiku = pick("", "m-small"); // 直连模式不看 card
    p.models.opus = pick("zz-card", "nope");
    assert!(core::apply(&store, &p).is_err(), "模型位只能从卡的清单里选");
    p.models.opus = Pick::default();
    p.assets.skills.add = vec!["zz-skill".into()];
    p.shared.sessions = Some(true);
    p.switches.insert("DISABLE_COMPACT".into(), Switch::On);
    p.switches.insert("DISABLE_AUTOUPDATER".into(), Switch::Off); // 全局级开关，放 profile 无效
    core::apply(&store, &p).unwrap();

    let env = &read_json(&dir.join("settings.json"))["env"];
    assert_eq!(env["ANTHROPIC_BASE_URL"], "https://example.com/anthropic");
    assert_eq!(env["ANTHROPIC_MODEL"], "m-big");
    assert_eq!(env["ANTHROPIC_DEFAULT_HAIKU_MODEL"], "m-small");
    for k in ["SONNET", "OPUS", "FABLE"] {
        assert_eq!(env[format!("ANTHROPIC_DEFAULT_{k}_MODEL")], "m-big", "空的模型位跟随 main");
    }
    assert_eq!(env["CLAUDE_CODE_MAX_CONTEXT_TOKENS"], "128000");
    assert_eq!(env["CLAUDE_CODE_DISABLE_THINKING"], "1");
    assert!(env.get("DISABLE_PROMPT_CACHING").is_none(), "没勾的兼容开关要删掉");
    assert_eq!(env["DISABLE_COMPACT"], "1");
    assert_eq!(env["DISABLE_AUTOUPDATER"], "1", "默认全局开");
    assert!(env.get("DISABLE_TELEMETRY").is_none(), "关 = 删掉");
    assert!(env.get("CLAUDE_CODE_DISABLE_AUTO_MEMORY").is_none());
    assert_eq!(env["CLAUDE_CODE_AUTO_MODE_SERVER"], "0", "第三方要关掉服务端审查，auto 才能用");
    assert!(env.get("CLAUDE_CODE_AUTO_MODE_MODEL").is_none(), "分类器留空就不写");
    assert!(junction::exists(dir.join("skills/zz-skill")).unwrap());
    assert_eq!(fs::read_to_string(dir.join("skills/zz-skill/SKILL.md")).unwrap(), "test");
    assert!(fs::read_to_string(dir.join("clade-mcp.json")).unwrap().contains("\"zz-mcp\""));
    let sess = junction::get_target(dir.join("sessions")).unwrap();
    assert!(sess.ends_with(".claude\\sessions"), "{}", sess.display());
    let global_md = dirs::home_dir().unwrap().join(".claude/CLAUDE.md");
    if global_md.is_file() {
        assert_eq!(fs::read(dir.join("CLAUDE.md")).unwrap(), fs::read(&global_md).unwrap(), "继承全局 CLAUDE.md");
    }

    // 排除全局素材
    p.assets.mcp.exclude = vec!["zz-mcp".into()];
    core::apply(&store, &p).unwrap();
    assert!(!fs::read_to_string(dir.join("clade-mcp.json")).unwrap().contains("zz-mcp"));

    // 共享目录：有内容的改名成 _projects，空的直接删；切回隔离时改回来
    fs::create_dir_all(dir.join("projects/x")).unwrap();
    fs::create_dir_all(dir.join("file-history")).unwrap();
    fs::create_dir_all(dir.join("plugins")).unwrap();
    fs::create_dir_all(dir.join("mods")).unwrap();
    p.shared.projects = Some(true);
    p.shared.file_history = Some(true);
    p.shared.plugins = Some(true);
    p.shared.mods = Some(true);
    core::apply(&store, &p).unwrap();
    assert!(junction::exists(dir.join("projects")).unwrap());
    assert!(dir.join("_projects/x").is_dir(), "有内容的目录改名，不删");
    assert!(junction::exists(dir.join("file-history")).unwrap());
    assert!(!dir.join("_file-history").exists(), "空目录直接删");
    assert!(junction::exists(dir.join("plugins")).unwrap());
    assert!(junction::get_target(dir.join("plugins")).unwrap().ends_with(".claude\\plugins"));
    assert!(junction::exists(dir.join("mods")).unwrap());
    assert!(junction::get_target(dir.join("mods")).unwrap().ends_with(".claude\\mods"));
    p.shared.projects = Some(false);
    p.shared.file_history = None; // 跟随全局（不共享）
    p.shared.plugins = None;
    p.shared.mods = None;
    core::apply(&store, &p).unwrap();
    assert!(!junction::exists(dir.join("projects")).unwrap_or(false));
    assert!(dir.join("projects/x").is_dir(), "切回隔离时改回原名");
    assert!(!dir.join("_projects").exists());
    assert!(!dir.join("file-history").exists());
    assert!(!dir.join("plugins").exists());
    assert!(!dir.join("mods").exists());
    assert!(dirs::home_dir().unwrap().join(".claude/projects").is_dir(), "删 junction 不能删到目标");
    assert!(dirs::home_dir().unwrap().join(".claude/plugins").is_dir(), "删 junction 不能删到目标");
    assert!(dirs::home_dir().unwrap().join(".claude/mods").is_dir(), "删 junction 不能删到目标");

    // 全新模式：用户手加的 settings 字段要保留；换成官方账号后模型卡的键去掉
    let mut s = read_json(&dir.join("settings.json"));
    s["zzUserKey"] = "kept".into();
    fs::write(dir.join("settings.json"), s.to_string()).unwrap();
    p.inherit_settings = false;
    p.assets.skills.add.clear();
    p.shared.sessions = None;
    p.card.clear();
    core::apply(&store, &p).unwrap();
    let settings = read_json(&dir.join("settings.json"));
    assert_eq!(settings["zzUserKey"], "kept");
    assert!(settings["env"].get("ANTHROPIC_BASE_URL").is_none());
    assert!(settings["env"].get("CLAUDE_CODE_MAX_CONTEXT_TOKENS").is_none());
    assert!(settings["env"].get("CLAUDE_CODE_AUTO_MODE_SERVER").is_none());
    assert!(!dir.join("skills/zz-skill").exists());
    assert!(skill.join("SKILL.md").exists(), "删 junction 不能删到素材本身");
    assert!(!dir.join("sessions").exists());

    // 禁用 auto / bypass：跟随默认不动；禁用写 "disable"；允许删掉（两种写法都删），permissions 里别的不动
    let mut s = read_json(&dir.join("settings.json"));
    s["permissions"] = serde_json::json!({"allow": ["Bash(ls)"], "disableAutoMode": "disable"});
    fs::write(dir.join("settings.json"), s.to_string()).unwrap();
    core::apply(&store, &p).unwrap();
    assert_eq!(read_json(&dir.join("settings.json"))["permissions"]["disableAutoMode"], "disable", "跟随默认不动");
    p.disable_auto_mode = Some(Switch::Off);
    p.disable_bypass_mode = Some(Switch::On);
    core::apply(&store, &p).unwrap();
    let settings = read_json(&dir.join("settings.json"));
    assert!(settings.get("disableAutoMode").is_none());
    assert!(settings["permissions"].get("disableAutoMode").is_none(), "允许 = 删掉");
    assert_eq!(settings["permissions"]["disableBypassPermissionsMode"], "disable");
    assert_eq!(settings["permissions"]["allow"], serde_json::json!(["Bash(ls)"]));
    p.disable_auto_mode = Some(Switch::On);
    p.disable_bypass_mode = Some(Switch::Off);
    core::apply(&store, &p).unwrap();
    let settings = read_json(&dir.join("settings.json"));
    assert_eq!(settings["disableAutoMode"], "disable");
    assert!(settings["permissions"].get("disableBypassPermissionsMode").is_none());
    p.disable_auto_mode = None;
    p.disable_bypass_mode = None;

    // 署名：跟随默认不动；去掉写三个子键；保留删掉 attribution 和 includeCoAuthoredBy
    let mut s = read_json(&dir.join("settings.json"));
    s["includeCoAuthoredBy"] = false.into();
    fs::write(dir.join("settings.json"), s.to_string()).unwrap();
    core::apply(&store, &p).unwrap();
    assert_eq!(read_json(&dir.join("settings.json"))["includeCoAuthoredBy"], false, "跟随默认不动");
    p.hide_attribution = Some(Switch::On);
    core::apply(&store, &p).unwrap();
    assert_eq!(
        read_json(&dir.join("settings.json"))["attribution"],
        serde_json::json!({"commit": "", "pr": "", "sessionUrl": false})
    );
    p.hide_attribution = Some(Switch::Off);
    core::apply(&store, &p).unwrap();
    let settings = read_json(&dir.join("settings.json"));
    assert!(settings.get("attribution").is_none() && settings.get("includeCoAuthoredBy").is_none());
    p.hide_attribution = None;

    // 继承模式：settings.json 重新生成，手加的字段消失，覆盖文件生效，clade 的键最后写入
    fs::write(dir.join("clade-settings.json"), r#"{"zzOverride":1,"env":{"ZZ_X":"1"}}"#).unwrap();
    p.inherit_settings = true;
    p.card = "zz-card".into();
    p.classifier_model = pick("zz-card", "m-judge");
    core::apply(&store, &p).unwrap();
    let settings = read_json(&dir.join("settings.json"));
    assert_eq!(settings["env"]["CLAUDE_CODE_AUTO_MODE_MODEL"], "m-judge");
    assert!(settings.get("zzUserKey").is_none(), "继承模式每次重新生成");
    assert_eq!(settings["zzOverride"], 1);
    assert_eq!(settings["env"]["ZZ_X"], "1");
    assert_eq!(settings["env"]["ANTHROPIC_BASE_URL"], "https://example.com/anthropic");
    let global_path = dirs::home_dir().unwrap().join(".claude/settings.json");
    if global_path.exists() {
        // 全局里非服务商的顶层键原样继承
        for (k, v) in read_json(&global_path).as_object().unwrap() {
            if k != "env" && k != "model" && k != "apiKeyHelper" && k != "zzOverride" {
                assert_eq!(&settings[k], v, "{k} 应从全局继承");
            }
        }
    }
    fs::remove_file(dir.join("clade-settings.json")).unwrap();

    // 全局 CLAUDE.md 加载开关：全局默认 + profile 覆盖；且原路径 ~/.claude/CLAUDE.md 无论开关都进 claudeMdExcludes 且不重复
    let profile_md = dir.join("CLAUDE.md");
    let global_md_file = core::global_claude_md();
    if global_md_file.is_file() {
        let _ = fs::remove_file(&profile_md);
        store.global.inherit_claude_md = false;
        p.inherit_claude_md = None;
        core::apply(&store, &p).unwrap();
        assert!(!profile_md.exists(), "跟随全局关时不复制");

        p.inherit_claude_md = Some(true);
        core::apply(&store, &p).unwrap();
        assert!(profile_md.is_file(), "profile 覆盖开时复制");

        fs::write(&profile_md, "custom-profile-md").unwrap();
        store.global.inherit_claude_md = true;
        p.inherit_claude_md = Some(false);
        core::apply(&store, &p).unwrap();
        assert_eq!(fs::read_to_string(&profile_md).unwrap(), "custom-profile-md", "profile 覆盖关时不覆盖自有 CLAUDE.md");

        p.inherit_claude_md = None;
        core::apply(&store, &p).unwrap();
        assert_eq!(fs::read(&profile_md).unwrap(), fs::read(&global_md_file).unwrap(), "跟随全局开时重新复制");
    }

    // key 按模型卡存
    core::set_key("zz-card", "sk-test").unwrap();
    assert_eq!(core::get_key("zz-card").unwrap().as_deref(), Some("sk-test"));

    // 启动命令（需要 store 落盘）
    p.card.clear(); // 先按官方 profile 测
    p.claude_md_excludes = vec![r"C:\zz\CLAUDE.md".into()];
    store.profiles.push(p.clone());
    core::save(&store).unwrap();
    let cwd = dir.clone(); // 工作目录用 profile 目录本身，结束时一起删

    let cmd = core::command(name, &cwd, &["--version".into()]).unwrap();
    let args = args_of(&cmd);
    let envs = envs_of(&cmd);
    let global_md = core::global_claude_md().display().to_string().replace('\\', "/");
    assert!(!args.iter().any(|a| a.starts_with("--mcp-config")), "mcp 被排除光了就不传");
    assert!(!args.contains(&"--session-id".to_string()), "不选卡不补 scratchpad");
    assert!(args.contains(&format!(
        "--settings={}",
        serde_json::json!({"claudeMdExcludes": ["C:/zz/CLAUDE.md", global_md]})
    )));
    assert_eq!(args.last().unwrap(), "--version");
    assert_eq!(cmd.get_current_dir(), Some(cwd.as_path()));
    assert!(envs.contains(&("CLAUDE_CONFIG_DIR".into(), Some(dir.display().to_string()))));
    assert!(envs.contains(&("ANTHROPIC_AUTH_TOKEN".into(), None)), "不选卡不注入 key");
    let home_claude = dirs::home_dir().unwrap().join(".claude").display().to_string();
    assert!(envs.contains(&("CLAUDE_SECURESTORAGE_CONFIG_DIR".into(), Some(home_claude.clone()))), "不选卡用 ~/.claude 的登录");
    assert!(envs.contains(&("CLAUDE_CODE_REMOTE_MEMORY_DIR".into(), None)), "未共享 projects 时不注入远程记忆目录");
    for k in core::CONFLICT_ENV.iter().filter(|k| **k != "CLAUDE_SECURESTORAGE_CONFIG_DIR") {
        assert!(envs.contains(&(k.to_string(), None)), "{k} 应被清除");
    }
    // 共享 projects 时注入 CLAUDE_CODE_REMOTE_MEMORY_DIR=~/.claude，避免写 memory 经过 junction 触发 symlink 确认
    store.profiles.last_mut().unwrap().shared.projects = Some(true);
    core::save(&store).unwrap();
    let shared_envs = envs_of(&core::command(name, &cwd, &["--version".into()]).unwrap());
    assert!(shared_envs.contains(&("CLAUDE_CODE_REMOTE_MEMORY_DIR".into(), Some(home_claude))));
    store.profiles.last_mut().unwrap().shared.projects = Some(false);
    core::save(&store).unwrap();
    core::apply(&store, store.profiles.last().unwrap()).unwrap();
    assert!(envs.contains(&("DISABLE_TELEMETRY".into(), None)), "关掉的开关从进程环境里也清除");
    assert!(!envs.iter().any(|(k, _)| k == "DISABLE_AUTOUPDATER"), "开关写在 settings.json，不走进程环境");
    assert_eq!(core::recent_dirs(name), vec![cwd.display().to_string()]);

    // 父目录 CLAUDE.md 提醒：已排除的不再报
    let check = core::check_dir(name, &cwd).unwrap();
    assert!(!check.nested_claude_md.iter().any(|f| f.eq_ignore_ascii_case(r"C:\zz\CLAUDE.md")));
    assert!(!check.scratchpad_skipped);

    // 第三方 profile：注入 key、补 scratchpad
    store.profiles.last_mut().unwrap().card = "zz-card".into();
    core::save(&store).unwrap();
    let hash = core::scratchpad_hash(&cwd).unwrap();
    let cmd = core::command(name, &cwd, &["--version".into()]).unwrap();
    let args = args_of(&cmd);
    assert!(envs_of(&cmd).contains(&("ANTHROPIC_AUTH_TOKEN".into(), Some("sk-test".into()))));
    assert!(envs_of(&cmd).contains(&("CLAUDE_SECURESTORAGE_CONFIG_DIR".into(), None)), "直连卡不用官方登录");
    let i = args.iter().position(|a| a == "--session-id").expect("应传 --session-id");
    let pad = std::env::temp_dir().join("claude").join(&hash).join(&args[i + 1]).join("scratchpad");
    assert!(pad.is_dir(), "{}", pad.display());
    let rule = format!("{}/**", core::rule_path(&pad));
    assert!(rule.starts_with("//c/"), "{rule}");
    assert!(args.contains(&format!("--allowedTools=Read({rule}),Edit({rule})")));
    assert!(!args.iter().any(|a| a.starts_with("--add-dir")), "不能用 --add-dir，会每次弹信任确认");
    let k = args.iter().position(|a| a == "--append-system-prompt").unwrap();
    assert!(args[k + 1].starts_with(&format!("Scratchpad directory: {}", pad.display())));
    assert_eq!(args.last().unwrap(), "--version");

    // x-api-key 认证注入 ANTHROPIC_API_KEY
    store.cards[0].auth = Auth::XApiKey;
    core::save(&store).unwrap();
    let envs = envs_of(&core::command(name, &cwd, &["--continue".into()]).unwrap());
    assert!(envs.contains(&("ANTHROPIC_API_KEY".into(), Some("sk-test".into()))));
    assert!(envs.contains(&("ANTHROPIC_AUTH_TOKEN".into(), None)));

    // 代理开关：跟随默认不动，不走删掉，走就把环境里缺的补成系统代理
    let proxy_envs = |store: &Store| {
        core::save(store).unwrap();
        let envs = envs_of(&core::command(name, &cwd, &["--continue".into()]).unwrap());
        core::PROXY_ENV.map(|k| envs.iter().find(|(e, _)| e.eq_ignore_ascii_case(k)).map(|(_, v)| v.clone()))
    };
    store.global.proxy = None;
    assert_eq!(proxy_envs(&store), [None, None]);
    store.global.proxy = Some(Switch::Off);
    assert_eq!(proxy_envs(&store), [Some(None), Some(None)]);
    store.global.proxy = Some(Switch::On);
    let want = core::PROXY_ENV.map(|k| match (core::system_proxy(), std::env::var(k).ok().filter(|v| !v.trim().is_empty())) {
        (Some(sys), None) => Some(Some(sys)),
        _ => None,
    });
    assert_eq!(proxy_envs(&store), want);
    store.global.proxy = None;
    core::save(&store).unwrap();

    // 恢复会话时不传 --session-id，用户自己给的 --session-id 直接用
    let args = args_of(&core::command(name, &cwd, &["--continue".into()]).unwrap());
    assert!(!args.contains(&"--session-id".to_string()));
    assert!(args.iter().any(|a| a.starts_with("--allowedTools=")));
    let id = "11111111-2222-3333-4444-555555555555";
    let args = args_of(&core::command(name, &cwd, &["--session-id".into(), id.into()]).unwrap());
    assert_eq!(args.iter().filter(|a| *a == "--session-id").count(), 1);
    assert!(args.iter().any(|a| a.contains(id) && a.starts_with("--allowedTools=")));
    fs::remove_dir_all(std::env::temp_dir().join("claude").join(&hash)).unwrap();

    // 聚合模式：多张卡经网关转发，模型位可以跨卡选
    let mut card2 = test_card();
    card2.id = "zz-card2".into();
    card2.auth = Auth::Bearer;
    card2.model_list = vec![model("k1", 1_000_000)];
    store.cards.push(card2);
    store.cards.push(Card { id: "zz-off2".into(), kind: CardKind::Official, ..Default::default() });
    {
        let ap = store.profiles.last_mut().unwrap();
        ap.mode = Mode::Aggregate;
        ap.cards = vec!["zz-card".into(), "zz-off2".into()];
        ap.models = Default::default();
        ap.models.main = pick("zz-card2", "k1");
        ap.models.haiku = pick("zz-card", "m-small");
        ap.classifier_model = Pick::default();
    }
    core::save(&store).unwrap();
    assert!(core::command(name, &cwd, &[]).is_err(), "官方卡不能聚合");
    store.profiles.last_mut().unwrap().cards = vec!["zz-card".into()];
    core::save(&store).unwrap();
    assert!(core::command(name, &cwd, &[]).is_err(), "模型位的卡要在所选的卡里");
    store.profiles.last_mut().unwrap().cards = vec!["zz-card".into(), "zz-card2".into()];
    core::save(&store).unwrap();
    core::set_key("zz-card2", "").unwrap(); // 上次中途失败可能留下
    assert!(core::command(name, &cwd, &[]).is_err(), "zz-card2 还没有 key");
    core::set_key("zz-card2", "sk-test2").unwrap();
    let cmd = core::command(name, &cwd, &["--version".into()]).unwrap();
    let envs: std::collections::HashMap<String, Option<String>> = envs_of(&cmd).into_iter().collect();
    let env = |k: &str| envs.get(k).cloned().flatten().unwrap_or_default();
    let base = env("ANTHROPIC_BASE_URL");
    assert!(base.starts_with("http://127.0.0.1:"), "{base}");
    let token = env("ANTHROPIC_AUTH_TOKEN");
    assert!(!token.is_empty() && token != "sk-test" && token != "sk-test2", "claude 只拿到随机 token");
    assert_eq!(envs.get("ANTHROPIC_API_KEY"), Some(&None));
    assert_eq!(env("ANTHROPIC_MODEL"), "cl-claude-zz-card2--k1[1m]");
    assert_eq!(env("ANTHROPIC_DEFAULT_SONNET_MODEL"), "cl-claude-zz-card2--k1[1m]", "空的跟随 main");
    assert_eq!(env("ANTHROPIC_DEFAULT_HAIKU_MODEL"), "cl-claude-zz-card--m-small");
    assert_eq!(env("CLAUDE_CODE_MAX_CONTEXT_TOKENS"), "128000", "main 是 1M 时取其余模型里最小的窗口");
    assert_eq!(env("CLAUDE_CODE_AUTO_MODE_SERVER"), "0");
    assert_eq!(env("CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY"), "1");
    assert!(env("NO_PROXY").split(',').any(|h| h == "127.0.0.1"), "连本机网关不走代理");
    assert!(args_of(&cmd).contains(&"--session-id".to_string()), "聚合也补 scratchpad");
    let cli: Vec<Value> = args_of(&cmd)
        .iter()
        .filter_map(|a| a.strip_prefix("--settings="))
        .map(|j| serde_json::from_str(j).unwrap())
        .collect();
    assert_eq!(cli.len(), 1, "--settings 只传一次");
    assert_eq!(cli[0]["claudeMdExcludes"], serde_json::json!(["C:/zz/CLAUDE.md", global_md]));
    let picker = &cli[0]["modelPicker"];
    assert_eq!(picker["replaceBuiltInOptions"], true, "/model 不显示内置的四档");
    assert_eq!(picker["options"].as_array().unwrap().len(), 4);
    assert_eq!(picker["options"][3], serde_json::json!({"model": "cl-claude-zz-card2--k1[1m]", "label": "k1", "description": "测试卡 · 1M"}));
    let settings = read_json(&dir.join("settings.json"));
    for k in ["ANTHROPIC_BASE_URL", "ANTHROPIC_MODEL", "CLAUDE_CODE_MAX_CONTEXT_TOKENS", "CLAUDE_CODE_AUTO_MODE_SERVER"] {
        assert!(settings["env"].get(k).is_none(), "聚合时 settings.json 里不能有 {k}（会压过进程 env）");
    }
    assert_eq!(settings["env"]["CLAUDE_CODE_DISABLE_THINKING"], "1", "兼容开关照样写");
    // 网关真的起来了
    let http = reqwest::blocking::Client::builder().no_proxy().build().unwrap();
    let v: Value = http.get(format!("{base}/v1/models")).bearer_auth(&token).send().unwrap().json().unwrap();
    assert_eq!(v["data"].as_array().unwrap().len(), 4);
    assert!(dir.join("clade-gateway.log").exists());
    assert!(core::delete_card(&mut store, "zz-card2").is_err(), "聚合在用的卡也不能删");
    {
        let dp = store.profiles.last_mut().unwrap();
        dp.mode = Mode::Direct;
        dp.models.main = pick("zz-card", "m-big");
        dp.models.haiku = Pick::default();
    }
    core::delete_card(&mut store, "zz-card2").unwrap();
    core::delete_card(&mut store, "zz-off2").unwrap();
    core::save(&store).unwrap();
    fs::remove_dir_all(std::env::temp_dir().join("claude").join(&hash)).unwrap();

    // 克隆：复制登记和配置文件，目录已存在时拒绝
    let clone = core::clone_profile(&store, name, "zz-test2").unwrap();
    let dir2 = core::profile_dir("zz-test2");
    assert_eq!(clone.card, "zz-card");
    assert_eq!(fs::read(dir2.join("settings.json")).unwrap(), fs::read(dir.join("settings.json")).unwrap());
    assert!(!dir2.join("projects").exists(), "不复制历史记录");
    assert!(core::clone_profile(&store, name, "zz-test2").is_err(), "目录已存在");
    fs::remove_dir_all(&dir2).unwrap();

    // 删卡：还有 profile 在用就拒绝；删掉时连 key 一起删
    assert!(core::delete_card(&mut store, "zz-card").is_err());
    store.profiles.retain(|x| x.name != name);
    core::delete_card(&mut store, "zz-card").unwrap();
    assert!(core::get_key("zz-card").unwrap().is_none());

    // 清理
    fs::remove_dir_all(&dir).unwrap();
    fs::remove_dir_all(&skill).unwrap();
    fs::remove_file(&mcp).unwrap();
}

#[test]
fn name_rules() {
    assert!(core::validate_name("deepseek").is_ok());
    assert!(core::validate_name("glm-4").is_ok());
    assert!(core::validate_name("claude").is_ok());
    for bad in ["", "Deep", "-x", "a b", "a;rm"] {
        assert!(core::validate_name(bad).is_err(), "{bad}");
    }
}

#[test]
fn card_rules() {
    assert!(core::validate_card(&test_card()).is_ok());
    let mut c = test_card();
    c.model_list.clear();
    assert!(core::validate_card(&c).is_ok(), "清单可以先空着");
    let mut c = test_card();
    c.base_url = "example.com".into();
    assert!(core::validate_card(&c).is_err());
    let mut c = test_card();
    c.model_list[1].context = 0;
    assert!(core::validate_card(&c).is_err());
    let mut c = test_card();
    c.model_list.push(model("m-big", 1));
    assert!(core::validate_card(&c).is_err(), "模型不能重复");
}

#[test]
fn defaults() {
    let store = Store::default();
    assert_eq!(store.version, core::STORE_VERSION);
    assert_eq!(store.global.switches.get("DISABLE_AUTOUPDATER"), Some(&Switch::On), "禁用自动更新默认开");
    assert!(store.global.inherit_claude_md, "全局 CLAUDE.md 默认加载");
    let p: Profile = serde_json::from_str(r#"{"name":"x"}"#).unwrap();
    assert!(p.inherit_settings && p.inherit_claude_md.is_none());
    let p_off: Profile = serde_json::from_str(r#"{"name":"x","inheritClaudeMd":false}"#).unwrap();
    assert_eq!(p_off.inherit_claude_md, Some(false), "旧布尔字段照常反序列化");
    let c: Card = serde_json::from_str(r#"{"id":"x","models":{"main":{"id":"m"}},"compat":[]}"#).unwrap();
    assert!(c.kind == CardKind::Api && c.model_list.is_empty(), "旧格式的卡照样能读，旧字段丢掉");
    let c: Card = serde_json::from_str(r#"{"id":"x","kind":"direct","modelList":[{"id":"m"}]}"#).unwrap();
    assert!(c.kind == CardKind::Api, "旧的 direct 当作第三方卡");
    assert_eq!(c.protocol, ApiFormat::Anthropic, "旧数据默认 anthropic 协议");
    assert_eq!(c.model_list[0].context, core::DEFAULT_CONTEXT);
    assert_eq!(serde_json::to_value(Auth::XApiKey).unwrap(), "x-api-key");
    assert_eq!(serde_json::to_value(ApiFormat::Anthropic).unwrap(), "anthropic");
    assert_eq!(serde_json::to_value(ApiFormat::OpenAiChat).unwrap(), "openai_chat");
    assert_eq!(serde_json::to_value(ApiFormat::OpenAiResponses).unwrap(), "openai_responses");
    assert_eq!(serde_json::to_value(ApiFormat::GeminiNative).unwrap(), "gemini_native");
}

#[test]
fn protocol_resolve_rules() {
    let mut store = Store::default();
    let mut oa_chat = test_card();
    oa_chat.id = "zz-oa".into();
    oa_chat.protocol = ApiFormat::OpenAiChat;
    let mut oa_resp = test_card();
    oa_resp.id = "zz-resp".into();
    oa_resp.protocol = ApiFormat::OpenAiResponses;
    let mut gem = test_card();
    gem.id = "zz-gem".into();
    gem.protocol = ApiFormat::GeminiNative;
    store.cards.push(oa_chat);
    store.cards.push(oa_resp);
    store.cards.push(gem);

    // 直连拒绝非 Anthropic 卡
    let mut p = Profile { name: "zz-p".into(), mode: Mode::Direct, card: "zz-oa".into(), ..Default::default() };
    p.models.main = pick("zz-oa", "m-big");
    let err = core::resolve(&store, &p).err().expect("直连应拒绝 OpenAI 卡");
    assert!(err.to_string().contains("聚合模式"), "{err}");

    p.card = "zz-gem".into();
    p.models.main = pick("zz-gem", "m-big");
    let err = core::resolve(&store, &p).err().expect("直连应拒绝 Gemini 卡");
    assert!(err.to_string().contains("聚合模式"), "{err}");

    // 聚合支持 OpenAiChat、OpenAiResponses 和 GeminiNative 且保留卡的协议
    p.mode = Mode::Aggregate;
    p.cards = vec!["zz-oa".into(), "zz-resp".into(), "zz-gem".into()];
    p.models.main = pick("zz-oa", "m-big");
    match core::resolve(&store, &p).unwrap() {
        Conn::Aggregate(cards, _) => {
            assert_eq!(cards[0].protocol, ApiFormat::OpenAiChat);
            assert_eq!(cards[1].protocol, ApiFormat::OpenAiResponses);
            assert_eq!(cards[2].protocol, ApiFormat::GeminiNative);
        }
        _ => panic!("应为聚合模式"),
    }
}

#[test]
fn effective_lists() {
    use core::ListOverride;
    let g = vec!["a".to_string(), "b".into()];
    let o = ListOverride { add: vec!["c".into(), "a".into()], exclude: vec!["b".into()] };
    assert_eq!(core::effective_list(&g, &o), vec!["a", "c"]);
}

#[test]
fn switch_lists_disjoint() {
    let mut keys: Vec<&str> = core::GLOBAL_SWITCHES
        .iter()
        .chain(core::PROFILE_SWITCHES)
        .chain(core::COMPAT_SWITCHES)
        .map(|d| d.key)
        .collect();
    let n = keys.len();
    keys.sort();
    keys.dedup();
    assert_eq!(keys.len(), n, "同一个开关不能出现在两份清单里");
}

#[test]
fn scratchpad_hash_matches_claude() {
    // 实测算法：把非字母数字（按 UTF-16 单元）替换为 '-' 并小写后取 sha256 前 16 位
    let h = core::scratchpad_hash(Path::new(r"C:\Users\Dev\Projects\clade"));
    assert_eq!(h.as_deref(), Some("f86d2d702ce6a116"));
    // 大小写不影响；非 BMP 字符算两个 UTF-16 单元
    assert_eq!(core::scratchpad_hash(Path::new(r"c:\users\dev\projects\clade")), h);
    assert_eq!(core::scratchpad_hash(Path::new("a😀")), core::scratchpad_hash(Path::new("a--")));
    assert_ne!(core::scratchpad_hash(Path::new("a中")), core::scratchpad_hash(Path::new("a--")));
    // 替换后超过 200 个字符就放弃
    assert!(core::scratchpad_hash(Path::new(&"x".repeat(200))).is_some());
    assert!(core::scratchpad_hash(Path::new(&"x".repeat(201))).is_none());
}

#[test]
fn nested_claude_md() {
    let root = std::env::temp_dir().join("clade-zz-nested");
    let _ = fs::remove_dir_all(&root);
    let cwd = root.join("a/b/c");
    fs::create_dir_all(cwd.join(".claude")).unwrap();
    fs::create_dir_all(root.join("a/b/.claude")).unwrap();
    for f in ["a/CLAUDE.md", "a/b/.claude/CLAUDE.md", "a/b/c/CLAUDE.md", "a/b/c/.claude/CLAUDE.md"] {
        fs::write(root.join(f), "x").unwrap();
    }
    let found = core::nested_claude_md(&cwd);
    assert!(found.contains(&root.join("a/CLAUDE.md")));
    assert!(found.contains(&root.join("a/b/.claude/CLAUDE.md")));
    assert!(!found.iter().any(|f| f.starts_with(&cwd)), "工作目录自己的不算");
    fs::remove_dir_all(&root).unwrap();

    // ~/.claude/CLAUDE.md 由开关单独控制且始终自动排除，无论在 ~ 还是子目录都不再提醒
    let home = dirs::home_dir().unwrap();
    let global = core::global_claude_md();
    assert!(!core::nested_claude_md(&home).contains(&global));
    assert!(!core::nested_claude_md(&home.join("Documents")).contains(&global));
}

#[test]
fn path_contains() {
    let path = r"C:\Windows;C:\Tools\clade\;%USERPROFILE%\bin";
    assert!(core::path_contains(path, r"c:\tools\clade"));
    assert!(core::path_contains(path, r"C:\Windows\"));
    assert!(!core::path_contains(path, r"C:\Tools"));
    assert!(!core::path_contains("", r"C:\Tools"));
}

#[test]
fn parse_proxy_server() {
    let p = |s| core::parse_proxy_server(s);
    assert_eq!(p("127.0.0.1:7890").as_deref(), Some("http://127.0.0.1:7890"));
    assert_eq!(p(" socks5://127.0.0.1:1080 ").as_deref(), Some("socks5://127.0.0.1:1080"));
    assert_eq!(p("http=a:1;https=b:2;socks=c:3").as_deref(), Some("http://b:2"));
    assert_eq!(p("http=a:1;socks=c:3").as_deref(), Some("http://a:1"));
    assert_eq!(p("socks=c:3"), None);
    assert_eq!(p(""), None);
}

#[test]
fn inherited_settings_rules() {
    use serde_json::json;
    let global = json!({
        "theme": "dark",
        "model": "claude-opus-5-5",
        "apiKeyHelper": "x",
        "statusLine": {"type": "command", "command": "hud"},
        "permissions": {"defaultMode": "auto", "allow": ["Bash(ls)"]},
        "env": {
            "DISABLE_TELEMETRY": "1",
            "ANTHROPIC_DEFAULT_OPUS_MODEL": "claude-opus-5-5",
            "AWS_REGION": "us-east-1",
            "CLAUDE_CODE_USE_BEDROCK": "1",
            "CLAUDE_CODE_SKIP_BEDROCK_AUTH": "1",
            "CLAUDE_CODE_OAUTH_TOKEN": "t"
        }
    });
    let overrides = json!({
        "statusLine": null,
        "permissions": {"allow": ["Bash(pwd)"]},
        "env": {"ANTHROPIC_DEFAULT_SONNET_MODEL": "minimax-m3"}
    });
    let out = Value::Object(core::inherited_settings(global.as_object().unwrap().clone(), overrides.as_object().unwrap()));
    assert_eq!(out["theme"], "dark");
    assert!(out.get("model").is_none());
    assert!(out.get("apiKeyHelper").is_none());
    assert!(out.get("statusLine").is_none(), "null 表示删除");
    assert_eq!(out["permissions"]["defaultMode"], "auto", "对象逐层合并");
    assert_eq!(out["permissions"]["allow"], json!(["Bash(pwd)"]), "数组整个替换");
    assert_eq!(out["env"], json!({"DISABLE_TELEMETRY": "1", "ANTHROPIC_DEFAULT_SONNET_MODEL": "minimax-m3"}));
}

#[test]
#[ignore]
fn official_card() {
    let _guard = StoreGuard::new();
    let name = "zz-off-p";
    let dir = core::profile_dir(name);
    let _ = fs::remove_dir_all(&dir);
    let card = Card { id: "zz-off".into(), kind: CardKind::Official, ..Default::default() };
    assert!(core::validate_card(&card).is_ok(), "官方卡可以不填模型和地址");
    let mut store = Store::default();
    store.cards.push(card);
    // 官方卡不用模型位，profile 里残留的也不写
    let mut p = Profile { name: name.into(), card: "zz-off".into(), ..Default::default() };
    p.models.main = pick("", "leftover");
    core::apply(&store, &p).unwrap();
    let env = &read_json(&dir.join("settings.json"))["env"];
    for k in ["ANTHROPIC_MODEL", "ANTHROPIC_BASE_URL", "ANTHROPIC_DEFAULT_HAIKU_MODEL", "CLAUDE_CODE_MAX_CONTEXT_TOKENS", "CLAUDE_CODE_AUTO_MODE_SERVER"] {
        assert!(env.get(k).is_none(), "官方卡不写 {k}");
    }

    store.profiles.push(p);
    core::save(&store).unwrap();
    let long = std::env::temp_dir().join("x".repeat(220)); // 路径太长也不提示，官方模式 claude 自己给 scratchpad
    assert!(!core::check_dir(name, &long).unwrap().scratchpad_skipped);
    let cmd = core::command(name, &dir, &[]).unwrap();
    let envs = envs_of(&cmd);
    let card_dir = core::card_dir("zz-off").display().to_string();
    assert!(envs.contains(&("CLAUDE_SECURESTORAGE_CONFIG_DIR".into(), Some(card_dir))), "登录跟卡走");
    assert!(envs.contains(&("ANTHROPIC_AUTH_TOKEN".into(), None)));
    assert!(!args_of(&cmd).contains(&"--session-id".to_string()), "官方卡不补 scratchpad");

    // 删官方卡连卡目录一起删
    fs::create_dir_all(core::card_dir("zz-off")).unwrap();
    store.profiles.clear();
    core::delete_card(&mut store, "zz-off").unwrap();
    assert!(!core::card_dir("zz-off").exists());

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
#[ignore]
fn oauth_keyring_roundtrip_and_chunking() {
    use clade_lib::oauth;
    use clade_lib::openai_oauth::OpenAiOAuthCreds;

    let id = "zz-oauth-kr";
    let _ = oauth::delete_creds(id);

    // 1. 普通大小凭据（单条存储）
    let small = OpenAiOAuthCreds {
        access_token: "at-small".into(),
        refresh_token: "rt-small".into(),
        chatgpt_account_id: "acct_1".into(),
        email: "a@example.com".into(),
        plan_type: "plus".into(),
        expires_at_ms: 2_000_000_000_000,
        updated_at_ms: 1_900_000_000_000,
    };
    oauth::save_creds_json(id, &small).unwrap();
    let loaded: OpenAiOAuthCreds = oauth::load_creds_json(id).unwrap().unwrap();
    assert_eq!(loaded.access_token, "at-small");
    assert_eq!(loaded.refresh_token, "rt-small");

    // 2. 超长凭据（> 2400 字节，自动分片存储并完整还原）
    let big = OpenAiOAuthCreds {
        access_token: "x".repeat(5200),
        refresh_token: "rt-big".into(),
        ..small.clone()
    };
    oauth::save_creds_json(id, &big).unwrap();
    let loaded_big: OpenAiOAuthCreds = oauth::load_creds_json(id).unwrap().unwrap();
    assert_eq!(loaded_big.access_token.len(), 5200);
    assert_eq!(loaded_big.refresh_token, "rt-big");

    // 3. 缩回单条存储 + 删除清理
    oauth::save_creds_json(id, &small).unwrap();
    let loaded_small: OpenAiOAuthCreds = oauth::load_creds_json(id).unwrap().unwrap();
    assert_eq!(loaded_small.access_token, "at-small");

    oauth::delete_creds(id).unwrap();
    assert!(oauth::load_creds_json::<OpenAiOAuthCreds>(id).unwrap().is_none());
}

/// .claude.json 的 cachedUsageUtilization.utilization（2.1.330 实测结构；opus 和 limits 里的 weekly_scoped 是按 cc-switch 的写法构造的）
#[test]
fn usage_parse() {
    let u: Value = serde_json::from_str(
        r#"{
        "five_hour": {"utilization": 2, "resets_at": "2026-11-22T19:29:59.506817+00:00"},
        "seven_day": {"utilization": 40.5, "resets_at": "2026-11-26T12:59:59.506840+00:00"},
        "seven_day_opus": {"utilization": 10, "resets_at": null},
        "seven_day_sonnet": null,
        "tangelo": null,
        "limits": [
            {"kind": "session", "group": "session", "percent": 2, "scope": null},
            {"kind": "weekly_scoped", "group": "weekly", "percent": 12, "resets_at": "2026-11-26T12:59:59Z",
             "scope": {"model": {"display_name": "Opus"}, "surface": null}},
            {"kind": "weekly_scoped", "group": "weekly", "percent": 89, "resets_at": "2026-11-26T12:59:59Z",
             "scope": {"model": {"display_name": "Fable"}, "surface": null}},
            {"kind": "weekly_scoped", "group": "weekly", "percent": 50,
             "scope": {"model": {"display_name": "Fable"}, "surface": "cowork"}}
        ]
    }"#,
    )
    .unwrap();
    let w = core::parse_usage(&u);
    let got: Vec<(&str, f64, &str)> = w.iter().map(|x| (x.label.as_str(), x.percent, x.resets_at.as_str())).collect();
    assert_eq!(
        got,
        [
            ("当前会话（5 小时）", 2.0, "2026-11-22T19:29:59.506817+00:00"),
            ("本周（所有模型）", 40.5, "2026-11-26T12:59:59.506840+00:00"),
            ("本周（仅 Opus）", 12.0, "2026-11-26T12:59:59Z"),
            ("本周（仅 Fable）", 89.0, "2026-11-26T12:59:59Z"),
        ]
    );
    assert!(core::parse_usage(&Value::Null).is_empty());
}

#[test]
fn effective_classifier_prompt_inheritance() {
    let mut g = core::Global::default();
    let mut p = Profile::default();
    assert_eq!(core::effective_classifier_prompt(&g, &p), None);

    g.classifier_prompt = "  global mini prompt  ".into();
    assert_eq!(core::effective_classifier_prompt(&g, &p).as_deref(), Some("global mini prompt"));

    // profile 显式设为空串 => 覆盖全局，强制不替换
    p.classifier_prompt = Some("".into());
    assert_eq!(core::effective_classifier_prompt(&g, &p), None);

    // profile 显式设自定义内容 => 覆盖全局
    p.classifier_prompt = Some("profile custom prompt".into());
    assert_eq!(core::effective_classifier_prompt(&g, &p).as_deref(), Some("profile custom prompt"));
}

#[test]
fn effective_prompt_policies_precedence() {
    use core::PromptPolicy;

    let mut g = core::Global::default();
    let mut p = Profile::default();
    let mut api_card = Card { id: "a".into(), kind: CardKind::Api, ..Default::default() };
    let agy_card = Card { id: "g".into(), kind: CardKind::Antigravity, ..Default::default() };

    // 1. 全部未设置：Antigravity 默认 Strip，其余 Keep；memory 默认 true；reminder 默认 Keep
    assert_eq!(core::effective_system_prompt_policy(&g, &p, &api_card), PromptPolicy::Keep);
    assert_eq!(core::effective_system_prompt_policy(&g, &p, &agy_card), PromptPolicy::Strip);
    assert!(core::effective_preserve_memory_prompt(&g, &p, &api_card));
    assert_eq!(core::effective_system_reminder_policy(&g, &p, &api_card), PromptPolicy::Keep);

    // 2. 全局设置生效
    g.system_prompt_policy = Some(PromptPolicy::Custom { text: "g-sys".into() });
    g.preserve_memory_prompt = false;
    g.system_reminder_policy = Some(PromptPolicy::Strip);
    assert_eq!(core::effective_system_prompt_policy(&g, &p, &api_card), PromptPolicy::Custom { text: "g-sys".into() });
    assert_eq!(core::effective_system_prompt_policy(&g, &p, &agy_card), PromptPolicy::Custom { text: "g-sys".into() });
    assert!(!core::effective_preserve_memory_prompt(&g, &p, &api_card));
    assert_eq!(core::effective_system_reminder_policy(&g, &p, &api_card), PromptPolicy::Strip);

    // 3. 卡覆盖全局（旧字段 strip_system_prompt 也能兜底覆盖全局）
    api_card.strip_system_prompt = Some(true);
    assert_eq!(core::effective_system_prompt_policy(&g, &p, &api_card), PromptPolicy::Strip);
    api_card.system_prompt_policy = Some(PromptPolicy::Keep);
    api_card.preserve_memory_prompt = Some(true);
    api_card.system_reminder_policy = Some(PromptPolicy::Custom { text: "c-rem".into() });
    assert_eq!(core::effective_system_prompt_policy(&g, &p, &api_card), PromptPolicy::Keep);
    assert!(core::effective_preserve_memory_prompt(&g, &p, &api_card));
    assert_eq!(core::effective_system_reminder_policy(&g, &p, &api_card), PromptPolicy::Custom { text: "c-rem".into() });

    // 4. Profile 优先级最高
    p.system_prompt_policy = Some(PromptPolicy::Custom { text: "p-sys".into() });
    p.preserve_memory_prompt = Some(false);
    p.system_reminder_policy = Some(PromptPolicy::Keep);
    assert_eq!(core::effective_system_prompt_policy(&g, &p, &api_card), PromptPolicy::Custom { text: "p-sys".into() });
    assert!(!core::effective_preserve_memory_prompt(&g, &p, &api_card));
    assert_eq!(core::effective_system_reminder_policy(&g, &p, &api_card), PromptPolicy::Keep);
}

#[test]
fn import_from_claude_skills_plugins_and_mcp() {
    use serde_json::json;

    let base = std::env::temp_dir().join(format!("clade-import-test-{}", uuid::Uuid::new_v4()));
    let fake_home = base.join("home");
    let lib = base.join("lib");

    // 1. skill
    let skill_src = fake_home.join(".claude").join("skills").join("demo-skill");
    fs::create_dir_all(&skill_src).unwrap();
    fs::write(skill_src.join("SKILL.md"), "hello").unwrap();

    // 2. installed_plugins.json：一个有效插件，一个路径不存在的跳过，同键带 user/project 两项优先 user
    let plug_user = fake_home.join(".claude").join("plugins").join("cache").join("my-plug").join("1.0.0");
    let plug_proj = fake_home.join(".claude").join("plugins").join("cache").join("my-plug").join("0.9.0");
    fs::create_dir_all(plug_user.join(".claude-plugin")).unwrap();
    fs::write(plug_user.join(".claude-plugin").join("plugin.json"), r#"{"name":"my-plug","version":"1.0.0"}"#).unwrap();
    fs::create_dir_all(&plug_proj).unwrap();
    fs::write(plug_proj.join("old.txt"), "old").unwrap();

    let installed = fake_home.join(".claude").join("plugins").join("installed_plugins.json");
    fs::write(
        &installed,
        serde_json::to_string_pretty(&json!({
            "version": 2,
            "plugins": {
                "my-plug@official": [
                    { "scope": "project", "installPath": plug_proj.to_string_lossy() },
                    { "scope": "user", "installPath": plug_user.to_string_lossy() }
                ],
                "missing-plug@official": [
                    { "scope": "user", "installPath": fake_home.join("no-such-dir").to_string_lossy() }
                ]
            }
        }))
        .unwrap(),
    )
    .unwrap();

    // 3. ~/.claude.json 顶层 mcpServers（含非法文件名字符、非对象项）+ projects 下 mcpServers（不导入）
    fs::write(
        fake_home.join(".claude.json"),
        serde_json::to_string_pretty(&json!({
            "mcpServers": {
                "demo-mcp": { "command": "npx", "args": ["-y", "demo"] },
                "scope/bad:name": { "type": "http", "url": "https://example.com/mcp" },
                "not-object": "ignored"
            },
            "projects": {
                "C:/proj": {
                    "mcpServers": {
                        "proj-only": { "command": "echo" }
                    }
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();

    // 预置一个同名 MCP，验证不会覆盖已有项
    fs::create_dir_all(lib.join("mcp")).unwrap();
    fs::write(lib.join("mcp").join("demo-mcp.json"), r#"{"command":"keep-existing"}"#).unwrap();

    let imported = core::import_from_claude_into(&fake_home, &lib).unwrap();
    assert!(imported.contains(&"skill: demo-skill".to_string()), "{imported:?}");
    assert!(imported.contains(&"插件: my-plug".to_string()), "{imported:?}");
    assert!(imported.contains(&"MCP: scope-bad-name".to_string()), "{imported:?}");
    assert!(!imported.iter().any(|x| x.contains("demo-mcp")), "已存在的不覆盖: {imported:?}");
    assert!(!imported.iter().any(|x| x.contains("missing-plug")), "{imported:?}");
    assert!(!imported.iter().any(|x| x.contains("proj-only")), "不导入项目级 MCP: {imported:?}");

    // 检查导入产物内容
    assert_eq!(fs::read_to_string(lib.join("skills/demo-skill/SKILL.md")).unwrap(), "hello");
    assert!(lib.join("plugins/my-plug/.claude-plugin/plugin.json").is_file(), "优先导入 user scope");
    assert!(!lib.join("plugins/my-plug/old.txt").exists());
    assert_eq!(read_json(&lib.join("mcp/demo-mcp.json"))["command"], "keep-existing");
    assert_eq!(read_json(&lib.join("mcp/scope-bad-name.json"))["url"], "https://example.com/mcp");

    // 再次导入应当全跳过
    let second = core::import_from_claude_into(&fake_home, &lib).unwrap();
    assert!(second.is_empty(), "重复导入应为空: {second:?}");

    let _ = fs::remove_dir_all(&base);
}

#[test]
fn sync_claude_state_inherits_ui_prefs_only() {
    use serde_json::json;

    let base = std::env::temp_dir().join(format!("clade-state-test-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&base).unwrap();
    let global = base.join("global.claude.json");
    let dst = base.join("profile.claude.json");

    fs::write(
        &global,
        serde_json::to_string_pretty(&json!({
            "copyOnSelect": false,
            "editorMode": "vim",
            "verbose": true,
            "oauthAccount": { "accountUuid": "secret-uuid" },
            "mcpServers": { "foo": { "command": "bar" } },
            "projects": { "C:/a": {} }
        }))
        .unwrap(),
    )
    .unwrap();

    // 1. 不继承：仅创建 hasCompletedOnboarding，不同步偏好
    assert!(core::sync_claude_state_from(&global, &dst, false).unwrap());
    let d1 = read_json(&dst);
    assert_eq!(d1["hasCompletedOnboarding"], true);
    assert!(d1.get("copyOnSelect").is_none());
    assert!(d1.get("oauthAccount").is_none());
    assert!(!core::sync_claude_state_from(&global, &dst, false).unwrap(), "无变化时不重写");

    // 2. 往目标文件写入 profile 自己的状态字段，开启继承后保留自有状态、只同步白名单偏好
    let mut cur = d1.as_object().unwrap().clone();
    cur.insert("userID".into(), json!("profile-uid"));
    cur.insert("numStartups".into(), json!(7));
    fs::write(&dst, serde_json::to_string_pretty(&cur).unwrap()).unwrap();

    assert!(core::sync_claude_state_from(&global, &dst, true).unwrap());
    let d2 = read_json(&dst);
    assert_eq!(d2["hasCompletedOnboarding"], true);
    assert_eq!(d2["userID"], "profile-uid", "保留 profile 自己的状态");
    assert_eq!(d2["numStartups"], 7);
    assert_eq!(d2["copyOnSelect"], false);
    assert_eq!(d2["editorMode"], "vim");
    assert_eq!(d2["verbose"], true);
    assert!(d2.get("oauthAccount").is_none(), "不同步账号信息");
    assert!(d2.get("mcpServers").is_none(), "不同步 mcpServers");
    assert!(d2.get("projects").is_none(), "不同步 projects");

    // 3. 再次同步幂等，不触发重写
    assert!(!core::sync_claude_state_from(&global, &dst, true).unwrap());

    let _ = fs::remove_dir_all(&base);
}

#[test]
fn writeback_ui_prefs_diff_and_isolation() {
    use serde_json::json;

    let base = std::env::temp_dir().join(format!("clade-wb-test-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&base).unwrap();
    let global = base.join("global.claude.json");
    let profile = base.join("profile.claude.json");

    fs::write(
        &global,
        serde_json::to_string_pretty(&json!({
            "copyOnSelect": false,
            "verbose": false,
            "theme": "dark",
            "oauthAccount": { "accountUuid": "keep-me" },
            "mcpServers": { "mt": { "command": "mt" } }
        }))
        .unwrap(),
    )
    .unwrap();

    // 启动前先同步，再拍基线快照
    core::sync_claude_state_from(&global, &profile, true).unwrap();
    let before = core::snapshot_ui_prefs_from(&profile).unwrap();

    // 1. 没改动时直接跳过
    assert!(core::writeback_ui_prefs_between(&before, &profile, &global).unwrap().is_empty());

    // 2. 模拟会话期间：
    //    - 用户在 profile 改了 copyOnSelect -> true、新增 editorMode -> "vim"、删掉 verbose，并写入敏感/非白名单键
    //    - 另一个进程在全局里把 theme 改成了 "light"（本会话没动 theme，写回时不能把全局的 "light" 覆盖回 "dark"）
    let mut g_cur = read_json(&global).as_object().unwrap().clone();
    g_cur.insert("theme".into(), json!("light"));
    fs::write(&global, serde_json::to_string_pretty(&g_cur).unwrap()).unwrap();

    let mut p_cur = read_json(&profile).as_object().unwrap().clone();
    p_cur.insert("copyOnSelect".into(), json!(true));
    p_cur.insert("editorMode".into(), json!("vim"));
    p_cur.insert("workflowSizeGuideline".into(), json!("medium"));
    p_cur.remove("verbose");
    p_cur.insert("oauthAccount".into(), json!({ "accountUuid": "pollute" }));
    p_cur.insert("numStartups".into(), json!(99));
    fs::write(&profile, serde_json::to_string_pretty(&p_cur).unwrap()).unwrap();

    let written = core::writeback_ui_prefs_between(&before, &profile, &global).unwrap();
    assert_eq!(written, vec!["verbose", "editorMode", "copyOnSelect", "workflowSizeGuideline"]);

    let g_after = read_json(&global);
    assert_eq!(g_after["copyOnSelect"], true);
    assert_eq!(g_after["editorMode"], "vim");
    assert_eq!(g_after["workflowSizeGuideline"], "medium");
    assert!(g_after.get("verbose").is_none(), "会话中删除的白名单键应从全局移除");
    assert_eq!(g_after["theme"], "light", "本会话未改动的键不能覆盖全局新值");
    assert_eq!(g_after["oauthAccount"]["accountUuid"], "keep-me", "绝不污染账号信息");
    assert_eq!(g_after["mcpServers"]["mt"]["command"], "mt", "绝不污染 MCP");
    assert!(g_after.get("numStartups").is_none(), "非白名单键不写回");

    // 3. 再次用同一快照写回，全局已一致，不再写入
    assert!(core::writeback_ui_prefs_between(&before, &profile, &global).unwrap().is_empty());

    let _ = fs::remove_dir_all(&base);
}

#[test]
fn writeback_settings_delta_filters_managed_and_overrides() {
    use serde_json::json;

    let base = std::env::temp_dir().join(format!("clade-wb-settings-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&base).unwrap();
    let global = base.join("global-settings.json");
    let profile = base.join("profile-settings.json");
    let overrides = base.join("clade-settings.json");

    // 全局原有 model（服务商键，继承时被剥离）、theme、permissions、env
    fs::write(
        &global,
        serde_json::to_string_pretty(&json!({
            "model": "claude-opus-5-5",
            "theme": "dark",
            "verbose": false,
            "awaySummaryEnabled": false,
            "permissions": { "defaultMode": "auto", "allow": ["Bash(git status)"] },
            "env": { "MY_CUSTOM_ENV": "1", "ANTHROPIC_BASE_URL": "https://orig.example" }
        }))
        .unwrap(),
    )
    .unwrap();
    // clade-settings.json 覆写项：不应被写回全局
    fs::write(
        &overrides,
        serde_json::to_string_pretty(&json!({
            "language": "zh",
            "permissions": { "allow": ["Bash(pwd)"] }
        }))
        .unwrap(),
    )
    .unwrap();

    let p = Profile {
        hide_attribution: Some(Switch::On),
        disable_bypass_mode: Some(Switch::Off),
        ..Profile::default()
    };

    // 模拟启动前生成的 profile settings.json
    let before = json!({
        "theme": "dark",
        "verbose": false,
        "awaySummaryEnabled": false,
        "language": "zh",
        "attribution": { "commit": "", "pr": "", "sessionUrl": false },
        "permissions": { "defaultMode": "auto", "allow": ["Bash(pwd)"] },
        "env": { "MY_CUSTOM_ENV": "1", "ANTHROPIC_MODEL": "m-card", "DISABLE_AUTOUPDATER": "1" }
    });
    let before_map = before.as_object().unwrap().clone();
    fs::write(&profile, serde_json::to_string_pretty(&before).unwrap()).unwrap();

    // 1. 无改动时不写回
    assert!(core::writeback_settings_between(&before_map, &profile, &overrides, &global, &p).unwrap().is_empty());

    // 2. 模拟会话期间：
    //    - 另一个进程在全局改了 theme -> "light"（本会话没动 theme，写回时不能覆回 "dark"）
    let mut g_cur = read_json(&global).as_object().unwrap().clone();
    g_cur.insert("theme".into(), json!("light"));
    fs::write(&global, serde_json::to_string_pretty(&g_cur).unwrap()).unwrap();

    //    - 本会话用户在 /config 改了：
    //      * 允许回写：verbose -> true、新增 autoScrollEnabled -> false、删掉 awaySummaryEnabled、
    //                  permissions.defaultMode -> "plan"、env.MY_CUSTOM_ENV -> "2"
    //      * 禁止回写：model（服务商顶层键）、env.ANTHROPIC_MODEL / env.DISABLE_AUTOUPDATER（clade 管理 env）、
    //                  attribution（profile 显式锁了 hide_attribution）、permissions.disableBypassPermissionsMode（显式锁了）、
    //                  language / permissions.allow（命中 clade-settings.json 覆写）
    let after = json!({
        "model": "polluted-model",
        "theme": "dark",
        "verbose": true,
        "autoScrollEnabled": false,
        "language": "en",
        "attribution": { "commit": "x" },
        "permissions": {
            "defaultMode": "plan",
            "allow": ["Bash(ls)"],
            "disableBypassPermissionsMode": "disable"
        },
        "env": {
            "MY_CUSTOM_ENV": "2",
            "ANTHROPIC_MODEL": "changed-card",
            "DISABLE_AUTOUPDATER": "0"
        }
    });
    fs::write(&profile, serde_json::to_string_pretty(&after).unwrap()).unwrap();

    let written = core::writeback_settings_between(&before_map, &profile, &overrides, &global, &p).unwrap();
    assert_eq!(
        written,
        vec![
            "autoScrollEnabled",
            "awaySummaryEnabled",
            "env.MY_CUSTOM_ENV",
            "permissions.defaultMode",
            "verbose"
        ]
    );

    let g_after = read_json(&global);
    assert_eq!(g_after["model"], "claude-opus-5-5", "全局原有服务商键不能被覆盖或删除");
    assert_eq!(g_after["theme"], "light", "未改动键不能覆盖全局新值");
    assert_eq!(g_after["verbose"], true);
    assert_eq!(g_after["autoScrollEnabled"], false);
    assert!(g_after.get("awaySummaryEnabled").is_none());
    assert_eq!(g_after["permissions"]["defaultMode"], "plan");
    assert_eq!(g_after["permissions"]["allow"], json!(["Bash(git status)"]), "clade-settings.json 覆盖项不写回");
    assert!(g_after["permissions"].get("disableBypassPermissionsMode").is_none());
    assert!(g_after.get("attribution").is_none());
    assert!(g_after.get("language").is_none());
    assert_eq!(g_after["env"]["MY_CUSTOM_ENV"], "2");
    assert_eq!(g_after["env"]["ANTHROPIC_BASE_URL"], "https://orig.example");
    assert!(g_after["env"].get("ANTHROPIC_MODEL").is_none());
    assert!(g_after["env"].get("DISABLE_AUTOUPDATER").is_none());

    // 3. 幂等
    assert!(core::writeback_settings_between(&before_map, &profile, &overrides, &global, &p).unwrap().is_empty());

    let _ = fs::remove_dir_all(&base);
}

/// 要求 ~/.claude 已用订阅账号登录
#[test]
#[ignore]
fn usage_global() {
    let u = core::usage("").unwrap();
    assert!(!u.windows.is_empty(), "没取到额度");
    // 没用过的窗口（比如 5 小时会话）resets_at 是 null，有量了才有重置时间
    assert!(u.windows.iter().all(|w| w.percent <= 100.0 && (w.percent == 0.0 || !w.resets_at.is_empty())));
    assert!(u.fetched_at_ms > 0 && !u.credential_expires_at.is_empty());
}
