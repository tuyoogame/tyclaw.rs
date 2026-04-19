//! CLI 交互循环 —— 基于 rustyline 的交互式 REPL。
//!
//! 终端布局：
//!   上方：固定输入区域（rustyline 提示符 + 用户输入）
//!   分隔线（兼做历史回看状态指示）
//!   下方：滚动输出区域（agent 输出在此区域内自动滚动）
//!
//! ```text
//! ┌──────────────────────────────┐
//! │ ░░░ Logo ░░░                 │  ← 第 1~3 行（固定）
//! │ 🦀 type 'exit' ...           │  ← 第 4 行 tips（固定）
//! │ > 帮我写一个H5小游戏█        │  ← 第 5 行 输入（固定）
//! ├──────────────────────────────┤  ← 第 6 行 分隔线（兼状态指示）
//! │ [轮次 1] 阶段=explore        │  ← 第 7 行~末尾 滚动输出区
//! │ ▌ 分析用户请求...            │
//! │ TyClaw.rs> 已完成            │
//! └──────────────────────────────┘
//! ```
//!
//! ## 历史回看
//! 所有 `cli_print` / `term::scroll_print` 的输出都先入一个固定大小的 ring buffer
//! （`HISTORY_CAP` 行），再按 `view_offset` 决定是否实时写到滚动区：
//! - `view_offset == 0`（跟随模式）：跟过去一样，写一行触发 DECSTBM 上滚。
//! - `view_offset > 0`（回看模式）：只入 buffer 不刷屏，让用户保持在历史位置。
//!
//! 通过 rustyline 的 `bind_sequence` 绑定热键（均非 rustyline 默认占用）：
//! - `PageUp` / `PageDown`：翻一页
//! - `Shift+↑` / `Shift+↓`：翻一行
//! - `Shift+PageUp`：跳到最顶
//! - `Shift+PageDown`：跳到底部并回到跟随模式
//! - 按 `Enter` 提交输入也会自动回到底部跟随

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use rustyline::{
    Cmd, ConditionalEventHandler, Event, EventContext, EventHandler, KeyCode, KeyEvent, Modifiers,
    RepeatCount,
};

use tyclaw_orchestration::{term, BusHandle, InboundMessage};

/// 固定顶部区域的行布局：
///   第 1~3 行：Logo
///   第 4 行：tips
///   第 5 行：输入行（rustyline）
///   第 6 行：分隔线（兼做历史回看状态指示）
///   第 7 行起：滚动输出区
const LOGO_LINES: u16 = 3;
const TIPS_LINE: u16 = LOGO_LINES + 1;          // 4
const INPUT_LINE: u16 = LOGO_LINES + 2;         // 5
const SEP_LINE: u16 = LOGO_LINES + 3;           // 6
const FIXED_TOP_LINES: u16 = LOGO_LINES + 3;    // 6
const PROMPT_WIDTH: u16 = 2;

/// 历史 buffer 最多保留多少行。
const HISTORY_CAP: usize = 5000;

const LOGO: [&str; 3] = [
    "░▀█▀░█░█░█▀▀░█░░░█▀█░█░█░░░░█▀▄░█▀▀",
    "░░█░░░█░░█░░░█░░░█▀█░█▄█░░░░█▀▄░▀▀█",
    "░░▀░░░▀░░▀▀▀░▀▀▀░▀░▀░▀░▀░▀░░▀░▀░▀▀▀",
];

// ---------------------------------------------------------------------------
// 历史滚动状态
// ---------------------------------------------------------------------------

struct ScrollState {
    buffer: VecDeque<String>,
    /// 从最新一行向上的偏移量。0 = 跟随最新；> 0 = 正在回看。
    view_offset: usize,
}

impl ScrollState {
    fn new() -> Self {
        Self {
            buffer: VecDeque::with_capacity(HISTORY_CAP),
            view_offset: 0,
        }
    }

    /// 把 msg 按 `\n` 拆成多行推入 buffer。
    ///
    /// agent 回复常常是整段（含嵌入 `\n`）一次传进来；不拆分的话一段 1000 行的
    /// 回复只占 1 个 buffer 条目，`max_offset` 永远是 0。但简单 `split('\n')`
    /// 会把跨行的 SGR 属性丢掉——原消息 `\x1b[1;37m A\n B\n C\x1b[0m`
    /// live 模式是整段写入所以颜色在 `\n` 后继续生效；拆分后 `B` 和 `C` 就裸奔了。
    ///
    /// 所以这里做一个迷你 SGR 解析：扫描 `\x1b[...m` 维护"当前激活的 SGR"，
    /// 碰到 `\n` 就把当前 SGR 作为前缀插到下一行开头。非 SGR 的 CSI（光标移动等）
    /// 原样保留、不入状态。
    fn push(&mut self, msg: &str) {
        let mut lines: Vec<String> = vec![String::new()];
        let mut active_sgr = String::new(); // 如 "1;37"；空串表示默认
        let mut chars = msg.chars().peekable();

        while let Some(c) = chars.next() {
            if c == '\x1b' && chars.peek() == Some(&'[') {
                chars.next();
                let mut params = String::new();
                let mut term = '\0';
                while let Some(nc) = chars.next() {
                    if nc.is_ascii_alphabetic() {
                        term = nc;
                        break;
                    }
                    params.push(nc);
                }
                // 原样回写整条 CSI 到当前行
                let seq = format!("\x1b[{params}{term}");
                lines.last_mut().unwrap().push_str(&seq);
                if term == 'm' {
                    if params.is_empty() || params == "0" {
                        active_sgr.clear();
                    } else {
                        active_sgr = params;
                    }
                }
                continue;
            }
            if c == '\n' {
                lines.push(String::new());
                if !active_sgr.is_empty() {
                    lines
                        .last_mut()
                        .unwrap()
                        .push_str(&format!("\x1b[{active_sgr}m"));
                }
                continue;
            }
            lines.last_mut().unwrap().push(c);
        }

        for line in lines {
            if self.buffer.len() >= HISTORY_CAP {
                self.buffer.pop_front();
            }
            self.buffer.push_back(line);
        }
    }
}

static SCROLL_STATE: OnceLock<Arc<Mutex<ScrollState>>> = OnceLock::new();

fn scroll_state() -> &'static Arc<Mutex<ScrollState>> {
    SCROLL_STATE.get_or_init(|| Arc::new(Mutex::new(ScrollState::new())))
}

/// 交互式 CLI 通道。
pub struct CliChannel {
    user_id: String,
    workspace_id: String,
    startup_lines: Vec<String>,
}

impl CliChannel {
    pub fn new(user_id: impl Into<String>, workspace_id: impl Into<String>) -> Self {
        Self {
            user_id: user_id.into(),
            workspace_id: workspace_id.into(),
            startup_lines: Vec::new(),
        }
    }

    /// 设置启动时在滚动区显示的信息（如配置摘要）。
    pub fn with_startup_lines(mut self, lines: Vec<String>) -> Self {
        self.startup_lines = lines;
        self
    }

    /// 运行交互式 CLI 循环（REPL）。
    pub async fn run(
        &self,
        bus_handle: BusHandle,
        timer_service: &tyclaw_tools::timer::TimerService,
    ) {
        let (input_tx, mut input_rx) = tokio::sync::mpsc::channel::<String>(1);
        let startup_lines = self.startup_lines.clone();

        // 让 orchestration 的 scroll_print 也走历史 buffer。
        term::set_hook(|msg| cli_print(msg));

        tokio::task::spawn_blocking(move || {
            use rustyline::error::ReadlineError;
            use rustyline::DefaultEditor;

            // 设置终端布局：Logo + 输入在顶部固定，输出在下方滚动
            setup_scroll_region();

            // 在滚动区打印启动信息（配置摘要等）
            for line in &startup_lines {
                cli_print(&format!("\x1b[2m{line}\x1b[0m"));
            }

            let mut rl = DefaultEditor::new().expect("Failed to init readline");
            let history_path = cli_history_path();
            rl.load_history(&history_path).ok();

            bind_scroll_keys(&mut rl);

            loop {
                // 每次 readline 前：强制光标回到第 1 行，清除残留，重画分隔线
                // （rustyline 按回车会打 \n 把光标推下去，这里纠正回来）
                reset_input_area();

                match rl.readline("> ") {
                    Ok(line) => {
                        // 提交时回到底部跟随模式
                        snap_to_bottom();
                        let trimmed = line.trim().to_string();
                        if trimmed.is_empty() {
                            continue;
                        }
                        rl.add_history_entry(&trimmed).ok();
                        if trimmed.eq_ignore_ascii_case("exit")
                            || trimmed.eq_ignore_ascii_case("quit")
                        {
                            restore_scroll_region();
                            println!("Goodbye!");
                            break;
                        }
                        // 把用户输入 echo 到输出区（像聊天记录）
                        cli_print(&format!("\x1b[1;37mYou> {trimmed}\x1b[0m"));
                        if input_tx.blocking_send(trimmed).is_err() {
                            break;
                        }
                    }
                    Err(ReadlineError::Interrupted) => {
                        continue;
                    }
                    Err(e) => {
                        restore_scroll_region();
                        println!("Goodbye! (reason: {e})");
                        let _ = input_tx.blocking_send("__exit__".into());
                        break;
                    }
                }
            }
            rl.save_history(&history_path).ok();
        });

        while let Some(input) = input_rx.recv().await {
            if input == "__exit__" {
                break;
            }

            let msg = InboundMessage {
                content: input,
                user_id: self.user_id.clone(),
                user_name: "cli_user".into(),
                workspace_id: self.workspace_id.clone(),
                channel: "cli".into(),
                chat_id: "direct".into(),
                conversation_id: None,
                images: vec![],
                files: vec![],
                reply_tx: None,
                is_timer: false,
                emotion_context: None,
            };

            if let Err(e) = bus_handle.send(msg).await {
                cli_print(&format!("\x1b[2;31mError: failed to send message to bus: {e}\x1b[0m"));
            }
        }

        timer_service.stop();
    }
}

// ---------------------------------------------------------------------------
// 终端滚动区域管理
// ---------------------------------------------------------------------------

/// 获取终端尺寸 (width, height)。
fn terminal_size() -> (u16, u16) {
    #[cfg(unix)]
    {
        unsafe {
            let mut ws: libc::winsize = std::mem::zeroed();
            if libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) == 0
                && ws.ws_col > 0
                && ws.ws_row > 0
            {
                return (ws.ws_col, ws.ws_row);
            }
        }
    }
    let cols = std::env::var("COLUMNS").ok().and_then(|v| v.parse().ok()).unwrap_or(80);
    let rows = std::env::var("LINES").ok().and_then(|v| v.parse().ok()).unwrap_or(24);
    (cols, rows)
}

/// 输出区的起始行号（紧接分隔线之后）。
fn output_start_row() -> u16 {
    FIXED_TOP_LINES + 1 // logo + tips + input + separator 之后
}

/// 滚动区可见行数（可能为 0，当终端很小时）。
fn visible_rows() -> usize {
    let (_, height) = terminal_size();
    let scroll_start = output_start_row();
    if height < scroll_start {
        0
    } else {
        (height - scroll_start + 1) as usize
    }
}

/// 设置终端布局：
///   第 1~3 行：Logo（固定）
///   第 4 行：提示信息（固定）
///   第 5 行：输入行 rustyline（固定）
///   第 6 行：分隔线（固定）
///   第 7 行 ~ 末尾：滚动输出区域
fn setup_scroll_region() {
    let (width, height) = terminal_size();
    let scroll_start = output_start_row();

    // 清屏
    eprint!("\x1b[2J");
    // 设置滚动区域
    eprint!("\x1b[{scroll_start};{height}r");
    // 画 Logo + Tips + 分隔线
    draw_fixed_top(width);
    // 光标移到输入行
    eprint!("\x1b[{INPUT_LINE};1H");
    flush_stderr();
}

/// 画固定顶部区域（Logo + Tips + 分隔线）。
fn draw_fixed_top(width: u16) {
    // Logo（第 1~3 行）
    for (i, line) in LOGO.iter().enumerate() {
        let row = i as u16 + 1;
        eprint!("\x1b[{row};1H\x1b[K");
        for ch in line.chars() {
            if ch == '░' {
                eprint!("\x1b[2m{ch}\x1b[0m");
            } else {
                eprint!("\x1b[1;36m{ch}\x1b[0m");
            }
        }
    }
    // Tips（第 4 行）
    eprint!(
        "\x1b[{TIPS_LINE};1H\x1b[K  🦀 \x1b[32m'exit'\x1b[0m quit · \
         \x1b[32m↑↓\x1b[0m input · \
         \x1b[32mPgUp/PgDn\x1b[0m scroll history · \
         \x1b[32mCtrl+R\x1b[0m search"
    );
    // 分隔线（第 6 行）—— 当在回看模式时会被 draw_separator 覆盖
    draw_separator(width, None);
}

/// 画分隔线，可选地显示回看状态。
fn draw_separator(width: u16, status: Option<&str>) {
    eprint!("\x1b[{SEP_LINE};1H\x1b[K");
    let bar = "─".repeat(width as usize);
    match status {
        None => eprint!("\x1b[2m{bar}\x1b[0m"),
        Some(s) => {
            // 状态格式：────── [history ...] ──────
            let label = format!(" {s} ");
            let lw = label.chars().count();
            let total = width as usize;
            if lw + 6 > total {
                // 终端太窄，直接画状态
                eprint!("\x1b[2;33m{label}\x1b[0m");
            } else {
                let left = 3;
                let right = total - lw - left;
                eprint!(
                    "\x1b[2m{}\x1b[0;33m{label}\x1b[2m{}\x1b[0m",
                    "─".repeat(left),
                    "─".repeat(right)
                );
            }
        }
    }
}

/// 恢复终端为正常模式。
fn restore_scroll_region() {
    let (_, height) = terminal_size();
    eprint!("\x1b[r");
    eprint!("\x1b[{height};1H");
    flush_stderr();
}

/// 重置输入区 + 刷新固定顶部 + 滚动区域。
///
/// 在每次 readline 前调用，处理：
/// 1. rustyline 按回车后光标位置错乱
/// 2. 终端 resize 后布局失效
fn reset_input_area() {
    let (width, height) = terminal_size();
    let scroll_start = output_start_row();
    eprint!("\x1b[{scroll_start};{height}r");
    draw_fixed_top(width);
    eprint!("\x1b[{INPUT_LINE};1H\x1b[K");
    flush_stderr();
}

/// 向滚动区底部追加一行（跟随模式下的实时写入）。
fn append_to_scroll_region(msg: &str) {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    let (_, height) = terminal_size();
    let scroll_start = output_start_row();
    let _ = write!(out, "\x1b[{scroll_start};{height}r");
    // 写入前后都复位 SGR，避免继承 redraw 或上一次 append 遗留的颜色属性。
    let _ = write!(out, "\x1b[{height};1H\x1b[0m");
    let _ = writeln!(out, "{msg}\x1b[0m");
    let col = PROMPT_WIDTH + 1;
    let _ = write!(out, "\x1b[{INPUT_LINE};{col}H");
    let _ = out.flush();
}

/// 按当前 view_offset 重绘整个滚动区。
///
/// 关键约定：最底行始终留空——跟 live-mode `append_to_scroll_region` 的稳态一致
/// （每次 `\n` 触发 DECSTBM 上滚后，最底行总是空的）。这样 redraw 之后再来 append
/// 才不会把原本的最后一行覆盖掉。
fn redraw_scroll_region(state: &ScrollState) {
    use std::io::Write;
    let (_width, height) = terminal_size();
    let scroll_start = output_start_row();
    if height < scroll_start {
        return;
    }
    let rows = (height - scroll_start + 1) as usize;
    let usable = rows.saturating_sub(1); // 最底行留空
    let len = state.buffer.len();
    let end = len.saturating_sub(state.view_offset);
    let start = end.saturating_sub(usable);

    let mut out = std::io::stdout().lock();
    // 确保滚动区域设置（终端 resize 后旧值失效）
    let _ = write!(out, "\x1b[{scroll_start};{height}r");
    // 关掉自动换行（DECAWM），保证"1 buffer 行 = 1 屏幕行"，长行在右边界截断。
    let _ = write!(out, "\x1b[?7l");
    for i in 0..rows {
        let row = scroll_start + i as u16;
        // 每行清空前后都复位 SGR，避免前一行末尾的颜色/背景属性
        // 渗透到 \x1b[K 的清行底色或下一行内容。
        let _ = write!(out, "\x1b[{row};1H\x1b[0m\x1b[K");
        if i < usable {
            let idx = start + i;
            if idx < end {
                let _ = write!(out, "{}\x1b[0m", &state.buffer[idx]);
            }
        }
    }
    // 恢复自动换行 + 光标回输入行（最后再复位一次，确保 append 起点干净）
    let _ = write!(out, "\x1b[0m\x1b[?7h");
    let col = PROMPT_WIDTH + 1;
    let _ = write!(out, "\x1b[{INPUT_LINE};{col}H");
    let _ = out.flush();
}

/// 根据 view_offset 更新分隔线状态（跟随 vs 回看）。
fn redraw_status_separator(state: &ScrollState) {
    let (width, _) = terminal_size();
    if state.view_offset == 0 {
        draw_separator(width, None);
    } else {
        let label = format!(
            "history ↑{} / {} lines  (press Enter or PgDn to follow)",
            state.view_offset,
            state.buffer.len()
        );
        draw_separator(width, Some(&label));
    }
    // 光标回到输入行
    let col = PROMPT_WIDTH + 1;
    eprint!("\x1b[{INPUT_LINE};{col}H");
    flush_stderr();
}

// ---------------------------------------------------------------------------
// 滚动导航
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum ScrollCmd {
    PageUp,
    PageDown,
}

fn apply_scroll(cmd: ScrollCmd) {
    let rows = visible_rows();
    let usable = rows.saturating_sub(1); // 与 redraw 保持一致：最底行留空
    let state = scroll_state();
    let mut s = state.lock().unwrap();
    let len = s.buffer.len();
    let max_offset = len.saturating_sub(usable);
    let old = s.view_offset;
    let step_page = usable.max(1);
    let new = match cmd {
        ScrollCmd::PageUp => s.view_offset.saturating_add(step_page).min(max_offset),
        ScrollCmd::PageDown => s.view_offset.saturating_sub(step_page),
    };
    tracing::info!(?cmd, visible_rows = rows, usable, buffer_len = len, max_offset, old, new, "scroll key handled");
    if rows == 0 || new == old {
        return;
    }
    s.view_offset = new;
    redraw_scroll_region(&s);
    redraw_status_separator(&s);
}

/// 提交输入后调用：强制回到底部跟随模式并重绘一次。
fn snap_to_bottom() {
    let state = scroll_state();
    let mut s = state.lock().unwrap();
    if s.view_offset != 0 {
        s.view_offset = 0;
        redraw_scroll_region(&s);
        redraw_status_separator(&s);
    }
}

// ---------------------------------------------------------------------------
// 对外的打印入口
// ---------------------------------------------------------------------------

/// 打印一行到输出区。跟随模式下即时写入，回看模式只入 buffer。
pub fn cli_print(msg: &str) {
    let state = scroll_state();
    let mut s = state.lock().unwrap();
    s.push(msg);
    let follow = s.view_offset == 0;
    drop(s);
    if follow {
        append_to_scroll_region(msg);
    }
}

// ---------------------------------------------------------------------------
// 热键绑定
// ---------------------------------------------------------------------------

struct ScrollKey(ScrollCmd);

impl ConditionalEventHandler for ScrollKey {
    fn handle(
        &self,
        _evt: &Event,
        _n: RepeatCount,
        _positive: bool,
        _ctx: &EventContext<'_>,
    ) -> Option<Cmd> {
        apply_scroll(self.0);
        Some(Cmd::Noop)
    }
}

fn bind(rl: &mut rustyline::DefaultEditor, key: KeyEvent, cmd: ScrollCmd) {
    rl.bind_sequence(
        Event::KeySeq(vec![key]),
        EventHandler::Conditional(Box::new(ScrollKey(cmd))),
    );
}

fn bind_scroll_keys(rl: &mut rustyline::DefaultEditor) {
    bind(rl, KeyEvent(KeyCode::PageUp, Modifiers::NONE), ScrollCmd::PageUp);
    bind(rl, KeyEvent(KeyCode::PageDown, Modifiers::NONE), ScrollCmd::PageDown);
}

// ---------------------------------------------------------------------------

fn flush_stderr() {
    let _ = std::io::Write::flush(&mut std::io::stderr());
}

fn cli_history_path() -> PathBuf {
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".tyclaw_cli_history")
}
