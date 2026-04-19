//! 终端输出工具 —— 在 ANSI 滚动区域内打印，不破坏固定区域布局。
//!
//! 与 tyclaw_channel::cli::cli_print 功能相同，但不依赖 channel crate。
//!
//! ## 可选 hook
//! CLI 通道在启动时通过 [`set_hook`] 注入一个回调，用于把所有输出送进
//! 历史缓冲（供回看功能使用）；注入后 `scroll_print` 就不再直接操作终端，
//! 由 hook 内部决定是否以及如何绘制。未注入时走原有的 DECSTBM 光标逻辑，
//! 保持与旧行为一致（例如 tyclaw-client 这类无滚动区的场景）。

use std::io::Write;
use std::sync::OnceLock;

type Hook = Box<dyn Fn(&str) + Send + Sync + 'static>;

static HOOK: OnceLock<Hook> = OnceLock::new();

/// 注入 scroll_print 钩子。只在首次调用时生效。
pub fn set_hook(f: impl Fn(&str) + Send + Sync + 'static) {
    let _ = HOOK.set(Box::new(f));
}

/// 输入行行号（与 cli.rs INPUT_LINE 一致）。
const INPUT_LINE: u16 = 5;
/// prompt "> " 宽度。
const PROMPT_WIDTH: u16 = 2;

/// 在终端滚动区域内打印一行。
///
/// 有 hook 时走 hook（通常会先入历史 buffer，再决定是否刷屏）；
/// 无 hook 时：移到终端底部 → 打印（触发滚动区内上滚）→ 光标回到输入行。
pub fn scroll_print(msg: &str) {
    if let Some(hook) = HOOK.get() {
        hook(msg);
        return;
    }
    let mut out = std::io::stdout().lock();
    let height = terminal_height();
    let _ = write!(out, "\x1b[{height};1H");
    let _ = writeln!(out, "{msg}");
    let col = PROMPT_WIDTH + 1;
    let _ = write!(out, "\x1b[{INPUT_LINE};{col}H");
    let _ = out.flush();
}

fn terminal_height() -> u16 {
    #[cfg(unix)]
    {
        unsafe {
            let mut ws: libc::winsize = std::mem::zeroed();
            if libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) == 0 && ws.ws_row > 0 {
                return ws.ws_row;
            }
        }
    }
    std::env::var("LINES").ok().and_then(|v| v.parse().ok()).unwrap_or(24)
}
