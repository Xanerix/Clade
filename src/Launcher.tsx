import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import type { DirCheck, State } from "./types";
import { ModalDialog, MsgLine, type Msg } from "./ui";

type Pwsh = { pwsh: string | null; winget: boolean };
type DirItem = { dir: string; check: DirCheck };

const PWSH_URL = "https://learn.microsoft.com/powershell/scripting/install/installing-powershell-on-windows";

const hasWarn = (c: DirCheck) => c.nestedClaudeMd.length > 0 || c.scratchpadSkipped;

type Props = {
  name: string;
  /** 若提供，则直接针对该目录检查：有提醒时弹窗确认，无提醒直接启动 */
  initialDir?: string;
  refresh: () => Promise<State>;
  onClose: (text?: string) => void;
};

/** 启动流程：先确认有 pwsh7（没有就引导安装），再选工作目录（或检查指定目录）。 */
export default function Launcher({ name, initialDir, refresh, onClose }: Props) {
  const [noPwsh, setNoPwsh] = useState<{ winget: boolean; installing: boolean } | null>(null);
  const [recent, setRecent] = useState<DirItem[] | null>(null);
  const [picked, setPicked] = useState<DirItem | null>(null);
  const [msg, setMsg] = useState<Msg>(null);
  const launchingRef = useRef(false);

  const fail = (e: unknown) => setMsg({ text: String(e), bad: true });
  const check = (dir: string) => invoke<DirCheck>("check_dir", { name, dir });

  async function launchIn(dir: string) {
    if (launchingRef.current) return;
    launchingRef.current = true;
    try {
      await invoke("launch", { name, dir });
      onClose(`已在 ${dir} 启动 ${name}`);
    } catch (e) {
      launchingRef.current = false;
      fail(e);
    }
  }

  async function start(isCancelled: () => boolean = () => false) {
    const st = await invoke<Pwsh>("pwsh_status");
    if (isCancelled()) return;
    if (!st.pwsh) {
      setNoPwsh({ winget: st.winget, installing: false });
      return;
    }
    setNoPwsh(null);
    if (initialDir) {
      const c = await check(initialDir);
      if (isCancelled()) return;
      if (hasWarn(c)) {
        setPicked({ dir: initialDir, check: c });
        setRecent([]);
      } else {
        await launchIn(initialDir);
      }
      return;
    }
    const dirs = await invoke<string[]>("recent_dirs", { name });
    if (isCancelled()) return;
    const checked = await Promise.all(dirs.map(async (dir) => ({ dir, check: await check(dir) })));
    if (isCancelled()) return;
    setRecent(checked);
  }

  useEffect(() => {
    let cancelled = false;
    start(() => cancelled).catch((e) => {
      if (!cancelled) fail(e);
    });
    return () => {
      cancelled = true;
    };
  }, []);

  async function browse() {
    try {
      const dir = await open({ directory: true, title: "选择工作目录" });
      if (typeof dir !== "string") return;
      const c = await check(dir);
      if (hasWarn(c)) setPicked({ dir, check: c });
      else await launchIn(dir);
    } catch (e) {
      fail(e);
    }
  }

  // 把父目录里的 CLAUDE.md 加进本 profile 的排除列表，然后重新检查
  async function exclude(file: string) {
    try {
      const s = await refresh();
      const p = s.profiles.find((x) => x.name === name);
      if (!p) throw new Error(`没有名为 ${name} 的环境`);
      if (!p.claudeMdExcludes.includes(file)) {
        await invoke("save_profile", { profile: { ...p, claudeMdExcludes: [...p.claudeMdExcludes, file] }, isNew: false });
        await refresh();
      }
      if (recent && recent.length > 0) {
        setRecent(await Promise.all(recent.map(async (r) => ({ dir: r.dir, check: await check(r.dir) }))));
      }
      if (picked) setPicked({ dir: picked.dir, check: await check(picked.dir) });
    } catch (e) {
      fail(e);
    }
  }

  async function installPwsh() {
    setNoPwsh((m) => m && { ...m, installing: true });
    try {
      const st = await invoke<Pwsh>("install_pwsh");
      if (!st.pwsh) {
        setNoPwsh({ winget: st.winget, installing: false });
        setMsg({ text: "安装结束了，但还是没找到 pwsh.exe", bad: true });
        return;
      }
      setMsg({ text: "PowerShell 7 已安装" });
      await start();
    } catch (e) {
      setNoPwsh((m) => m && { ...m, installing: false });
      fail(e);
    }
  }

  const warnings = (c: DirCheck) => {
    if (!hasWarn(c)) return null;
    return (
      <div className="dir-warn-box">
        {c.nestedClaudeMd.length > 0 && (
          <>
            <div style={{ fontWeight: 500 }}>父目录里的这些 CLAUDE.md 会被当作项目指令加载：</div>
            {c.nestedClaudeMd.map((f) => (
              <label key={f} className="dir-warn-check">
                <input type="checkbox" checked={false} onChange={() => exclude(f)} />
                <span className="mono" style={{ fontSize: 12, wordBreak: "break-all" }}>
                  {f}
                </span>
                <span className="set-sub">以后都排除</span>
              </label>
            ))}
          </>
        )}
        {c.scratchpadSkipped && <div>路径太长，本次不提供 scratchpad</div>}
      </div>
    );
  };

  if (noPwsh) {
    return (
      <ModalDialog label="需要 PowerShell 7" canClose={!noPwsh.installing} onClose={() => onClose()}>
        <div className="modal-card narrow">
          <div className="mc-head flat">
            <div className="mc-top">
              <h2 className="mc-title">需要 PowerShell 7</h2>
              <button type="button" className="btn-circle" aria-label="关闭" disabled={noPwsh.installing} onClick={() => onClose()}>
                ✕
              </button>
            </div>
          </div>
          <div className="mc-body">
            <p style={{ margin: 0 }}>
              clade 只在 PowerShell 7（pwsh）里启动 Claude Code，这是 Claude Code 在 Windows 上官方推荐的终端。当前没有找到 pwsh.exe。
            </p>
            {noPwsh.winget ? (
              <span className="set-sub">
                点「一键安装」会新开一个窗口运行 <span className="mono">winget install --id Microsoft.PowerShell</span>，装完自动重新检测。
              </span>
            ) : (
              <span className="set-sub">
                没有找到 winget，请从官方页面下载安装，装好后点「重新检测」：
                <br />
                <span className="mono">{PWSH_URL}</span>
              </span>
            )}
            <MsgLine msg={msg} />
          </div>
          <div className="mc-foot">
            <span />
            <div style={{ display: "flex", gap: 8 }}>
              <button type="button" className="btn btn-sm" disabled={noPwsh.installing} onClick={() => onClose()}>
                取消
              </button>
              {noPwsh.winget ? (
                <button type="button" className="btn btn-primary btn-sm" disabled={noPwsh.installing} onClick={installPwsh}>
                  {noPwsh.installing ? "正在安装…" : "一键安装"}
                </button>
              ) : (
                <>
                  <button type="button" className="btn btn-sm" onClick={() => start().catch(fail)}>
                    重新检测
                  </button>
                  <button
                    type="button"
                    className="btn btn-primary btn-sm"
                    onClick={() => invoke("open_url", { url: PWSH_URL }).catch(fail)}
                  >
                    打开下载页
                  </button>
                </>
              )}
            </div>
          </div>
        </div>
      </ModalDialog>
    );
  }

  if (!recent) {
    if (!msg) return null;
    return (
      <ModalDialog label={`启动 ${name}`} onClose={() => onClose()}>
        <div className="modal-card narrow">
          <div className="mc-head flat">
            <div className="mc-top">
              <h2 className="mc-title">启动 {name}</h2>
              <button type="button" className="btn-circle" aria-label="关闭" onClick={() => onClose()}>
                ✕
              </button>
            </div>
          </div>
          <div className="mc-body">
            <MsgLine msg={msg} />
          </div>
          <div className="mc-foot">
            <span />
            <button type="button" className="btn btn-primary btn-sm" onClick={() => onClose()}>
              关闭
            </button>
          </div>
        </div>
      </ModalDialog>
    );
  }

  return (
    <ModalDialog label={picked ? `启动前提醒 · ${name}` : `选择工作目录 · ${name}`} onClose={() => onClose()}>
      <div className="modal-card">
        <div className="mc-head flat">
          <div className="mc-top">
            <div>
              <h2 className="mc-title">{picked ? `启动前提醒 · ${name}` : `在哪个目录启动 ${name}？`}</h2>
              {picked && <div className="mc-sub">{picked.dir}</div>}
            </div>
            <button type="button" className="btn-circle" aria-label="关闭" onClick={() => onClose()}>
              ✕
            </button>
          </div>
        </div>

        <div className="mc-body">
          {picked ? (
            <>
              {warnings(picked.check) ?? <span className="set-sub">所有提醒均已处理，可以直接启动。</span>}
            </>
          ) : (
            <>
              {recent.length === 0 && <span className="set-sub">还没有用过的目录，点下方「浏览…」选一个。</span>}
              {recent.map((r) => (
                <div key={r.dir} className="dir-pick-row">
                  <button type="button" className="dir-pick-btn" onClick={() => launchIn(r.dir)}>
                    {r.dir}
                  </button>
                  {warnings(r.check)}
                </div>
              ))}
            </>
          )}
          <MsgLine msg={msg} />
        </div>

        <div className="mc-foot">
          {picked ? (
            <>
              {!initialDir ? (
                <button type="button" className="btn btn-sm" onClick={() => setPicked(null)}>
                  返回
                </button>
              ) : (
                <span />
              )}
              <div style={{ display: "flex", gap: 8 }}>
                <button type="button" className="btn btn-sm" onClick={() => onClose()}>
                  取消
                </button>
                <button type="button" className="btn btn-primary btn-sm" onClick={() => launchIn(picked.dir)}>
                  {hasWarn(picked.check) ? "仍然启动" : "启动"}
                </button>
              </div>
            </>
          ) : (
            <>
              <button type="button" className="btn btn-sm" onClick={browse}>
                浏览…
              </button>
              <button type="button" className="btn btn-sm" onClick={() => onClose()}>
                取消
              </button>
            </>
          )}
        </div>
      </div>
    </ModalDialog>
  );
}
