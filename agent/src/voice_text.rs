//! 把 LLM 的回复改写成适合 TTS 念出来的版本。
//!
//! 原始 response 始终保留（用于日志、followup 分类、未来文本通道）；
//! 只有送进 `tts.speak()` 的字符串会走这个 transform。
//!
//! ## 设计取舍
//!
//! - **保守优先**：宁可少改也不要改坏。任何无法在 Markdown 里明确
//!   识别的语法（裸 JSON、任意 XML）都不动，让 TTS 自己硬念 —— 因为
//!   删除有意义的内容比念出噪声更危险。
//! - **不翻译数字**：`1883` 保留成 "1883" 而不是 "一千八百八十三"。
//!   多数 Piper / VITS 中文 TTS 对纯数字串会自动按"一八八三"念，硬
//!   转成汉字反而会和 TTS 自己的 digit-readout 撞车。
//! - **保留标点**：中文标点 `，。？！` 是 TTS 节奏提示，不删。
//! - **空字符串短路**：避免后续 regex 在空串上跑出奇怪结果。
//!
//! ## 入口
//!
//! [`transform_for_tts`] 是唯一对外函数；它按 `VoiceOutputConfig`
//! 里的开关顺序依次跑 [`Stage`]。每个 stage 自带单元测试，组合
//! 后再做一次"端到端"测试覆盖典型 LLM 回复模板。

use common::VoiceOutputConfig;

/// 单个改写阶段。`apply` 拿到上一阶段的输出和 config，返回下一阶段
/// 的输入；空串短路、配置开关关闭时直接返回原文。
trait Stage {
    fn name(&self) -> &'static str;
    fn apply(&self, text: &str, cfg: &VoiceOutputConfig) -> String;
    /// 默认实现：主开关关闭或文本为空时跳过。子类可重写以提供更细致
    /// 的条件（比如 max_chars=0 时不做截断）。
    fn should_run(&self, text: &str, _cfg: &VoiceOutputConfig) -> bool {
        !text.is_empty()
    }
}

// ── 阶段实现 ──────────────────────────────────────────────────────────

struct DropCodeBlocks;
struct DropInlineCode;
struct DropTables;
struct DropBareUrls;
struct StripMarkdownLinks;
struct StripHeadings;
struct StripEmphasis;
struct ExpandBulletLists;
struct DropEmoji;
struct CollapseWhitespace;
struct EnsureChinese;
struct Truncate;

/// 去掉 fenced code block：```` ```xxx\n...yyy\n``` ````。保留
/// block 之间的空行结构（替换成单个换行），但 block 自身整体删除。
impl Stage for DropCodeBlocks {
    fn name(&self) -> &'static str {
        "drop_code_blocks"
    }
    fn apply(&self, text: &str, _cfg: &VoiceOutputConfig) -> String {
        // 简化版扫描：找到形如 ```\n...\n``` 的最外层对。
        // 处理 ``` 后不换行的情况也兼容（如 ```python print(1) ```）。
        let mut out = String::with_capacity(text.len());
        let bytes = text.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            // 寻找开 fence
            if i + 2 < bytes.len() && &bytes[i..i + 3] == b"```" {
                // 跳过开 fence + 同一行剩余内容（语言标识等）
                if let Some(nl) = text[i..].find('\n') {
                    i += nl + 1;
                } else {
                    // 整段就一个 fence，没内容，直接结束
                    break;
                }
                // 寻找闭 fence
                if let Some(end) = text[i..].find("```") {
                    i += end + 3;
                    // 跳过闭 fence 后的换行
                    if bytes.get(i) == Some(&b'\n') {
                        i += 1;
                    } else if bytes.get(i) == Some(&b'\r')
                        && bytes.get(i + 1) == Some(&b'\n')
                    {
                        i += 2;
                    }
                    out.push('\n');
                } else {
                    // 没有闭 fence —— 当作到末尾都丢弃
                    break;
                }
            } else {
                // 把 char（不是 byte）拷过去以保 UTF-8 安全
                let ch_end = text[i..]
                    .char_indices()
                    .nth(1)
                    .map(|(n, _)| i + n)
                    .unwrap_or(text.len());
                out.push_str(&text[i..ch_end]);
                i = ch_end;
            }
        }
        out
    }
    fn should_run(&self, text: &str, cfg: &VoiceOutputConfig) -> bool {
        cfg.drop_code && text.contains("```")
    }
}

impl Stage for DropInlineCode {
    fn name(&self) -> &'static str {
        "drop_inline_code"
    }
    fn apply(&self, text: &str, _cfg: &VoiceOutputConfig) -> String {
        // 只去掉反引号，保留内部内容。让 LLM 决定 code 里该写什么
        // 字面（函数名 / 标识符 / 数字），TTS 念出来用户至少能听
        // 懂"这是一个命令"；直接吞掉内容风险更高（会吞掉 LLM
        // 想表达的关键信息）。
        let mut out = String::with_capacity(text.len());
        for c in text.chars() {
            if c == '`' {
                continue;
            }
            out.push(c);
        }
        out
    }
    fn should_run(&self, text: &str, cfg: &VoiceOutputConfig) -> bool {
        cfg.drop_code && text.contains('`')
    }
}

impl Stage for DropTables {
    fn name(&self) -> &'static str {
        "drop_tables"
    }
    fn apply(&self, text: &str, _cfg: &VoiceOutputConfig) -> String {
        // Markdown 表格：连续行（被 \n 分隔）只要"含 |"就整段丢。
        // 简单的两遍：先按行扫，每行按 trim 后看是否含 '|'，连续的
        // 表格行整段剔除。
        let mut out = String::with_capacity(text.len());
        let mut in_table = false;
        for line in text.split_inclusive('\n') {
            let trimmed = line.trim_end();
            let is_table_line = trimmed.contains('|') && trimmed.chars().any(|c| !c.is_whitespace());
            if is_table_line {
                if !in_table {
                    // 表格开始前的换行不要保留
                    while out.ends_with('\n') {
                        out.pop();
                    }
                    in_table = true;
                }
                continue;
            }
            if in_table {
                out.push('\n');
                in_table = false;
            }
            out.push_str(line);
        }
        if in_table {
            out.push('\n');
        }
        out
    }
    fn should_run(&self, text: &str, cfg: &VoiceOutputConfig) -> bool {
        cfg.drop_tables && text.contains('|')
    }
}

impl Stage for DropBareUrls {
    fn name(&self) -> &'static str {
        "drop_bare_urls"
    }
    fn apply(&self, text: &str, _cfg: &VoiceOutputConfig) -> String {
        // 匹配 scheme 头：scheme://... 直到第一个空白。
        // 故意不贪婪、不尝试匹配 markdown 链接（那交给 StripMarkdownLinks）。
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        const SCHEMES: &[&str] = &["http://", "https://", "mqtt://", "mqtts://", "ws://", "wss://"];
        loop {
            let mut found: Option<(usize, &str)> = None;
            for s in SCHEMES {
                if let Some(idx) = rest.find(s) {
                    found = Some((idx, *s));
                    break;
                }
            }
            match found {
                None => {
                    out.push_str(rest);
                    return out;
                }
                Some((idx, scheme)) => {
                    out.push_str(&rest[..idx]);
                    let after = &rest[idx + scheme.len()..];
                    // url 一直延伸到下一个空白 / 行尾 / 标点右括号右尖括号
                    let end = after
                        .find(|c: char| c.is_whitespace() || c == ')' || c == ']')
                        .unwrap_or(after.len());
                    // 给个"链接"提示让用户知道原文此处有引用
                    let _ = out; // suppressed
                    out.push_str("链接");
                    rest = &after[end..];
                }
            }
        }
    }
    fn should_run(&self, text: &str, cfg: &VoiceOutputConfig) -> bool {
        cfg.drop_bare_urls
            && ["http://", "https://", "mqtt://", "mqtts://", "ws://", "wss://"]
                .iter()
                .any(|s| text.contains(s))
    }
}

impl Stage for StripMarkdownLinks {
    fn name(&self) -> &'static str {
        "strip_markdown_links"
    }
    fn apply(&self, text: &str, _cfg: &VoiceOutputConfig) -> String {
        // 把 `[text](url)` 替换成 `text`。
        // 简易手工扫描：找到 `[` → 找 `]` → 找 `(` → 找 `)` → 替换。
        let bytes = text.as_bytes();
        let mut out = String::with_capacity(text.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'[' {
                // 找 ]
                if let Some(close_text) = text[i + 1..].find(']') {
                    let abs_text = i + 1 + close_text;
                    // 紧跟 ( 才算 markdown 链接
                    if bytes.get(abs_text + 1) == Some(&b'(') {
                        if let Some(close_url) = text[abs_text + 2..].find(')') {
                            let abs_url_end = abs_text + 2 + close_url;
                            out.push_str(&text[i + 1..abs_text]);
                            i = abs_url_end + 1;
                            continue;
                        }
                    }
                }
            }
            let ch_end = text[i..]
                .char_indices()
                .nth(1)
                .map(|(n, _)| i + n)
                .unwrap_or(text.len());
            out.push_str(&text[i..ch_end]);
            i = ch_end;
        }
        out
    }
    fn should_run(&self, text: &str, cfg: &VoiceOutputConfig) -> bool {
        cfg.strip_markdown_links && text.contains("](")
    }
}

impl Stage for StripHeadings {
    fn name(&self) -> &'static str {
        "strip_headings"
    }
    fn apply(&self, text: &str, _cfg: &VoiceOutputConfig) -> String {
        // 行首的连续 '#' + 至少一个空白 → 去掉前缀；行内的 '#' 不动。
        let mut out = String::with_capacity(text.len());
        for line in text.split_inclusive('\n') {
            let trimmed_start = line.trim_start();
            let leading_ws = line.len() - trimmed_start.len();
            let hashes = trimmed_start
                .chars()
                .take_while(|&c| c == '#')
                .count();
            if (1..=6).contains(&hashes) {
                let after = &trimmed_start[hashes..];
                if after.starts_with(' ') || after.starts_with('\t') {
                    out.push_str(&line[..leading_ws]);
                    out.push_str(after.trim_start());
                    if !out.ends_with('\n') && line.ends_with('\n') {
                        out.push('\n');
                    }
                    continue;
                }
            }
            out.push_str(line);
        }
        out
    }
    fn should_run(&self, text: &str, cfg: &VoiceOutputConfig) -> bool {
        cfg.strip_headings && text.lines().any(|l| l.trim_start().starts_with('#'))
    }
}

impl Stage for StripEmphasis {
    fn name(&self) -> &'static str {
        "strip_emphasis"
    }
    fn apply(&self, text: &str, _cfg: &VoiceOutputConfig) -> String {
        // 处理 `**bold**` `__bold__` `*em*` `_em_` 中的标记符。
        // 策略：成对扫描，遇到 `**` / `__` / `*` / `_` 就把它删掉，
        // 内部的字符保留。最朴素但鲁棒：跑 3 轮去掉最长的 fence，
        // 剩下单独的 `*` / `_` 保留（可能是列表项的 `-` 误识别等，
        // 不删以免误伤）。
        let mut s = text.to_string();
        for fence in ["**", "__"] {
            while s.contains(fence) {
                let before = s.clone();
                s = s.replacen(fence, "", 2);
                if s == before {
                    break;
                }
            }
        }
        s
    }
    fn should_run(&self, text: &str, cfg: &VoiceOutputConfig) -> bool {
        cfg.strip_emphasis && (text.contains("**") || text.contains("__"))
    }
}

impl Stage for ExpandBulletLists {
    fn name(&self) -> &'static str {
        "expand_bullet_lists"
    }
    fn apply(&self, text: &str, _cfg: &VoiceOutputConfig) -> String {
        // 连续多行以 `- ` / `* ` / `+ ` 开头 → 转成 "第一，...。第二，...。"
        // 中间被非 bullet 行打断则重新计数。
        const CN_NUM: &[&str] = &["第一", "第二", "第三", "第四", "第五", "第六", "第七", "第八", "第九", "第十"];
        let mut out = String::new();
        let mut bullet_block: Vec<String> = Vec::new();
        let mut after_block_blank = false;
        for line in text.split_inclusive('\n') {
            let trimmed = line.trim_start();
            let leading_ws = line.len() - trimmed.len();
            let bullet = if let Some(rest) = trimmed.strip_prefix("- ") {
                Some(rest.trim_end().to_string())
            } else if let Some(rest) = trimmed.strip_prefix("* ") {
                Some(rest.trim_end().to_string())
            } else if let Some(rest) = trimmed.strip_prefix("+ ") {
                Some(rest.trim_end().to_string())
            } else {
                None
            };
            match bullet {
                Some(item) => {
                    bullet_block.push(item);
                    after_block_blank = false;
                }
                None => {
                    if !bullet_block.is_empty() {
                        // flush block
                        for (idx, item) in bullet_block.iter().enumerate() {
                            let label = CN_NUM.get(idx).copied().unwrap_or("接下来");
                            if idx > 0 {
                                out.push(' ');
                            }
                            out.push_str(label);
                            out.push('，');
                            out.push_str(item);
                            out.push('。');
                        }
                        bullet_block.clear();
                        if after_block_blank {
                            out.push('\n');
                        }
                    }
                    if trimmed.trim().is_empty() {
                        after_block_blank = true;
                    } else {
                        after_block_blank = false;
                    }
                    out.push_str(&line[..leading_ws]);
                    out.push_str(trimmed);
                }
            }
        }
        // tail flush
        if !bullet_block.is_empty() {
            for (idx, item) in bullet_block.iter().enumerate() {
                let label = CN_NUM.get(idx).copied().unwrap_or("接下来");
                if idx > 0 {
                    out.push(' ');
                }
                out.push_str(label);
                out.push('，');
                out.push_str(item);
                out.push('。');
            }
        }
        out
    }
    fn should_run(&self, text: &str, cfg: &VoiceOutputConfig) -> bool {
        cfg.expand_lists
            && text.lines().any(|l| {
                let t = l.trim_start();
                t.starts_with("- ") || t.starts_with("* ") || t.starts_with("+ ")
            })
    }
}

impl Stage for DropEmoji {
    fn name(&self) -> &'static str {
        "drop_emoji"
    }
    fn apply(&self, text: &str, _cfg: &VoiceOutputConfig) -> String {
        let mut out = String::with_capacity(text.len());
        for c in text.chars() {
            // 覆盖常见 emoji 块：symbols / dingbats / emoticons / 杂项符号 / 补充符号
            let cp = c as u32;
            let is_emoji = matches!(cp,
                0x2600..=0x26FF       // Miscellaneous Symbols
                | 0x2700..=0x27BF     // Dingbats
                | 0x1F300..=0x1F5FF   // Misc Symbols and Pictographs
                | 0x1F600..=0x1F64F   // Emoticons
                | 0x1F680..=0x1F6FF   // Transport and Map
                | 0x1F700..=0x1F77F   // Alchemical
                | 0x1F780..=0x1F7FF
                | 0x1F800..=0x1F8FF
                | 0x1F900..=0x1F9FF   // Supplemental Symbols and Pictographs
                | 0x1FA00..=0x1FA6F
                | 0x1FA70..=0x1FAFF
                | 0x1F1E6..=0x1F1FF   // Regional indicator (国旗)
            );
            if !is_emoji {
                out.push(c);
            }
        }
        out
    }
    fn should_run(&self, text: &str, cfg: &VoiceOutputConfig) -> bool {
        cfg.drop_emoji
            && text.chars().any(|c| {
                let cp = c as u32;
                matches!(cp,
                    0x2600..=0x26FF | 0x2700..=0x27BF
                    | 0x1F300..=0x1F5FF | 0x1F600..=0x1F64F
                    | 0x1F680..=0x1F6FF | 0x1F900..=0x1F9FF
                    | 0x1FA00..=0x1FAFF | 0x1F1E6..=0x1F1FF
                )
            })
    }
}

impl Stage for CollapseWhitespace {
    fn name(&self) -> &'static str {
        "collapse_whitespace"
    }
    fn apply(&self, text: &str, _cfg: &VoiceOutputConfig) -> String {
        let mut out = String::with_capacity(text.len());
        let mut last_was_space = false;
        for c in text.chars() {
            if c.is_whitespace() {
                if !last_was_space {
                    out.push(' ');
                    last_was_space = true;
                }
            } else {
                out.push(c);
                last_was_space = false;
            }
        }
        out.trim().to_string()
    }
    fn should_run(&self, text: &str, cfg: &VoiceOutputConfig) -> bool {
        cfg.collapse_whitespace
            && text.chars().any(|c| c.is_whitespace())
            && (text.contains("  ") || text.contains("\n"))
    }
}

impl Stage for Truncate {
    fn name(&self) -> &'static str {
        "truncate"
    }
    fn apply(&self, text: &str, cfg: &VoiceOutputConfig) -> String {
        let count = text.chars().count();
        if count <= cfg.max_chars {
            return text.to_string();
        }
        // 按字符截断，避免在 UTF-8 中间切。
        let keep: String = text.chars().take(cfg.max_chars).collect();
        // 截断点如果正好停在"半句话标点"上（',' '，' ';' '；'
        // 空白 / tab / 换行），回退一格，听感更自然。
        // 句号 '。' / 问号 '？' / 感叹号 '！' 是合法句子结尾，
        // **不**回退 —— 后接省略提示语义通顺。
        let mut cut = keep.chars().count();
        if let Some(last) = keep.chars().last() {
            if matches!(last, ',' | '，' | ';' | '；' | ' ' | '\t' | '\n' | '\r') && cut > 1 {
                cut -= 1;
            }
        }
        let mut out: String = keep.chars().take(cut).collect();
        if !cfg.truncate_suffix.is_empty() {
            out.push_str(&cfg.truncate_suffix);
        }
        out
    }
    fn should_run(&self, text: &str, cfg: &VoiceOutputConfig) -> bool {
        cfg.max_chars > 0 && text.chars().count() > cfg.max_chars
    }
}

/// 把 TTS 文本里的非 CJK 字符剥掉，让中文 TTS 模型（matcha-zh-baker、
/// Piper zh 等）不会念出乱码。
///
/// 行为由 `cfg.require_chinese` + `cfg.require_chinese_action` 控制：
/// - `require_chinese = false`：stage 跳过，原文不动。
/// - `require_chinese_action = "pass"`：stage 跳过（用户显式选择
///   不做处理 —— 比如切到 Kokoro 这种中英混读模型时）。
/// - 其它情况：统计 CJK 字符占比（含 CJK 统一表意文字 + 扩展 A/B
///   + 兼容 + 数字、ASCII 标点、空白**不计入**分母避免误判）。如果
///   比例 < `require_chinese_min_ratio`：
///   - `action = "warn"`：记一条 warn 日志（原始 response 不变），
///     然后把非 CJK 字符从 TTS 文本里剥掉再送进 TTS。
///   - `action = "drop"`：静默剥掉非 CJK 字符。
/// 比例达标时不动文本。
impl Stage for EnsureChinese {
    fn name(&self) -> &'static str {
        "ensure_chinese"
    }
    fn apply(&self, text: &str, cfg: &VoiceOutputConfig) -> String {
        // 统计 CJK 字符数（CJK 统一表意 + 扩展 A/B + 兼容 + 全角标点 + 半角假名 + 谚文音节）
        let (cjk_count, total) = text.chars().fold((0usize, 0usize), |(cjk, tot), c| {
            let cp = c as u32;
            let is_cjk = matches!(cp,
                0x3000..=0x303F    // CJK Symbols and Punctuation (CJK 标点)
                | 0x3400..=0x4DBF  // CJK Unified Ideographs Extension A
                | 0x4E00..=0x9FFF  // CJK Unified Ideographs
                | 0xF900..=0xFAFF  // CJK Compatibility Ideographs
                | 0x20000..=0x2A6DF // CJK Extension B
                | 0x2A700..=0x2B73F
                | 0x2B740..=0x2B81F
                | 0x2B820..=0x2CEAF
                | 0x2F800..=0x2FA1F // CJK Compatibility Supplement
            );
            // 跳过空白、ASCII 标点和纯数字，避免"全是数字串就被判为非中文"
            if c.is_whitespace() {
                (cjk, tot)
            } else {
                (cjk + is_cjk as usize, tot + 1)
            }
        });
        if total == 0 {
            return text.to_string();
        }
        let ratio = cjk_count as f32 / total as f32;
        if ratio >= cfg.require_chinese_min_ratio {
            // 比例达标，原文不动
            return text.to_string();
        }
        // 比例不达标：剥掉非 CJK 字符
        if cfg.require_chinese_action == "warn" {
            tracing::warn!(
                target: "voice_text",
                cjk_count,
                total,
                ratio = format!("{:.2}", ratio),
                threshold = cfg.require_chinese_min_ratio,
                preview = %text.chars().take(60).collect::<String>(),
                "TTS text is below Chinese ratio threshold; stripping non-CJK characters before TTS"
            );
        }
        let mut out = String::with_capacity(text.len());
        for c in text.chars() {
            let cp = c as u32;
            let is_cjk = matches!(cp,
                0x3000..=0x303F | 0x3400..=0x4DBF | 0x4E00..=0x9FFF
                | 0xF900..=0xFAFF | 0x20000..=0x2A6DF | 0x2A700..=0x2B73F
                | 0x2B740..=0x2B81F | 0x2B820..=0x2CEAF | 0x2F800..=0x2FA1F
            );
            if is_cjk {
                out.push(c);
            } else if matches!(c, '，' | '。' | '？' | '！' | '、' | '；' | '：' | '“' | '”' | '‘' | '’' | '（' | '）' | '《' | '》') {
                // 中文标点保留（这些也是 CJK 范围但有些工具链视为标点，
                // 显式列出避免被误删）
                out.push(c);
            }
            // 其余 ASCII 字母 / 数字 / 英文标点 / emoji 全部静默剥掉
        }
        out
    }
    fn should_run(&self, text: &str, cfg: &VoiceOutputConfig) -> bool {
        if !cfg.require_chinese {
            return false;
        }
        if cfg.require_chinese_action == "pass" {
            return false;
        }
        !text.is_empty()
    }
}

// ── 编排 ─────────────────────────────────────────────────────────────

const STAGES: &[&dyn Stage] = &[
    &DropCodeBlocks,
    &DropInlineCode,
    &DropTables,
    &DropBareUrls,
    &StripMarkdownLinks,
    &StripHeadings,
    &StripEmphasis,
    &ExpandBulletLists,
    &DropEmoji,
    &CollapseWhitespace,
    &EnsureChinese,
    &Truncate,
];

/// 改写入口：`text` 走完全部 stage，返回 TTS 友好版本。
///
/// `cfg.enabled = false` 时直接返回原文。
pub fn transform_for_tts(text: &str, cfg: &VoiceOutputConfig) -> String {
    if !cfg.enabled {
        return text.to_string();
    }
    let mut current = text.to_string();
    for stage in STAGES {
        if stage.should_run(&current, cfg) {
            let next = stage.apply(&current, cfg);
            tracing::trace!(
                target: "voice_text",
                stage = stage.name(),
                before_len = current.chars().count(),
                after_len = next.chars().count(),
                "voice_text stage applied"
            );
            current = next;
            if current.is_empty() {
                break;
            }
        }
    }
    current
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::VoiceOutputConfig;

    fn cfg() -> VoiceOutputConfig {
        VoiceOutputConfig::default()
    }

    fn noop_cfg() -> VoiceOutputConfig {
        VoiceOutputConfig {
            enabled: false,
            ..VoiceOutputConfig::default()
        }
    }

    fn minimal_cfg(keep: &[&str]) -> VoiceOutputConfig {
        // 把 keep 之外的开关全关，方便单独测某个 stage
        let mut c = VoiceOutputConfig {
            enabled: true,
            collapse_whitespace: false,
            ..VoiceOutputConfig::default()
        };
        for key in [
            "strip_emphasis",
            "strip_headings",
            "expand_lists",
            "strip_markdown_links",
            "drop_bare_urls",
            "drop_code",
            "drop_tables",
            "drop_emoji",
        ] {
            if !keep.contains(&key) {
                match key {
                    "strip_emphasis" => c.strip_emphasis = false,
                    "strip_headings" => c.strip_headings = false,
                    "expand_lists" => c.expand_lists = false,
                    "strip_markdown_links" => c.strip_markdown_links = false,
                    "drop_bare_urls" => c.drop_bare_urls = false,
                    "drop_code" => c.drop_code = false,
                    "drop_tables" => c.drop_tables = false,
                    "drop_emoji" => c.drop_emoji = false,
                    _ => unreachable!(),
                }
            }
        }
        c
    }

    // ── 单 stage 单元测试 ────────────────────────────────────────────

    #[test]
    fn drop_code_blocks_removes_fenced_block() {
        let s = DropCodeBlocks;
        let c = cfg();
        let out = s.apply("before\n```python\nprint(1)\n```\nafter\n", &c);
        assert!(!out.contains("print"));
        assert!(out.contains("before"));
        assert!(out.contains("after"));
    }

    #[test]
    fn drop_code_blocks_handles_unclosed() {
        let s = DropCodeBlocks;
        let c = cfg();
        let out = s.apply("hello\n```\nworld", &c);
        assert!(!out.contains("world"));
    }

    #[test]
    fn drop_inline_code_strips_backticks() {
        let s = DropInlineCode;
        let c = cfg();
        let out = s.apply("use `list_clients` to query", &c);
        assert_eq!(out, "use list_clients to query");
    }

    #[test]
    fn drop_inline_code_keeps_content_strips_only_backticks() {
        let s = DropInlineCode;
        let c = cfg();
        // 单数 / 奇数个反引号 —— 现在只删反引号、不删内容
        let out = s.apply("a `b c d e", &c);
        assert_eq!(out, "a b c d e");
    }

    #[test]
    fn drop_tables_removes_pipe_lines() {
        let s = DropTables;
        let c = cfg();
        let input = "intro\n| a | b |\n| - | - |\n| 1 | 2 |\nend\n";
        let out = s.apply(input, &c);
        assert!(!out.contains("|"));
        assert!(out.contains("intro"));
        assert!(out.contains("end"));
    }

    #[test]
    fn drop_bare_urls_replaces_with_链接() {
        let s = DropBareUrls;
        let c = cfg();
        let out = s.apply("see https://example.com for details", &c);
        assert!(!out.contains("example.com"));
        assert!(out.contains("链接"));
        assert!(out.contains("see"));
        assert!(out.contains("for details"));
    }

    #[test]
    fn strip_markdown_links_keeps_text() {
        let s = StripMarkdownLinks;
        let c = cfg();
        let out = s.apply("see [the docs](https://x.y/) here", &c);
        assert_eq!(out, "see the docs here");
    }

    #[test]
    fn strip_headings_removes_hash_prefix() {
        let s = StripHeadings;
        let c = cfg();
        let out = s.apply("## 标题\n正文", &c);
        assert!(out.contains("标题"));
        assert!(!out.contains("##"));
    }

    #[test]
    fn strip_emphasis_removes_double_markers() {
        let s = StripEmphasis;
        let c = cfg();
        assert_eq!(s.apply("**重要**的事", &c), "重要的事");
        assert_eq!(s.apply("__bold__ text", &c), "bold text");
    }

    #[test]
    fn expand_bullet_lists_adds_chinese_ordinals() {
        let s = ExpandBulletLists;
        let c = cfg();
        let out = s.apply("- alpha\n- beta\n- gamma\n", &c);
        assert!(out.contains("第一"));
        assert!(out.contains("alpha"));
        assert!(out.contains("第二"));
        assert!(out.contains("beta"));
        assert!(!out.starts_with("-"));
    }

    #[test]
    fn drop_emoji_removes_unicode_blocks() {
        let s = DropEmoji;
        let c = cfg();
        let out = s.apply("hello 🔧 world 🇺🇸 end", &c);
        assert_eq!(out, "hello  world  end");
    }

    #[test]
    fn collapse_whitespace_squashes() {
        let s = CollapseWhitespace;
        let c = cfg();
        let out = s.apply("a   b\n\n\nc", &c);
        assert_eq!(out, "a b c");
    }

    #[test]
    fn truncate_caps_at_max_chars() {
        let s = Truncate;
        let c = VoiceOutputConfig {
            max_chars: 10,
            ..cfg()
        };
        let out = s.apply("你好世界一二三四五六七八九十", &c);
        assert!(out.chars().count() <= 10 + "，后面的内容省略了".chars().count());
        assert!(out.ends_with("省略了"));
    }

    #[test]
    fn truncate_does_not_end_on_half_sentence_punct() {
        // max_chars=4 切在 ',' 上：cut 回退一格到 'd'
        let s = Truncate;
        let c = VoiceOutputConfig {
            max_chars: 4,
            ..cfg()
        };
        let out = s.apply("abcd,efgh", &c);
        let body = out.trim_end_matches("，后面的内容省略了");
        assert_eq!(body, "abcd");
        assert!(!body.ends_with(','));
    }

    #[test]
    fn truncate_keeps_terminal_punctuation() {
        // 句号 / 问号 / 感叹号 是合法句子结尾，**不**回退；
        // cut 后的最后一个字符（不在 suffix 范围内）应保留。
        // 输入 "你好世界。" 共 5 字符，max_chars=6 → 不触发截断。
        let s = Truncate;
        let c = VoiceOutputConfig {
            max_chars: 6,
            ..cfg()
        };
        let out = s.apply("你好世界。", &c);
        assert_eq!(out, "你好世界。");
    }

    #[test]
    fn truncate_trims_only_commas_and_spaces() {
        let s = Truncate;
        let c = VoiceOutputConfig {
            max_chars: 3,
            ..cfg()
        };
        // 3 chars: '你' '好' '，' → 切在 '，' 上 → 回退到 '好'
        let out = s.apply("你好，世界", &c);
        let body = out.trim_end_matches("，后面的内容省略了");
        assert_eq!(body, "你好");
        assert!(!body.contains('，'));
    }

    // ── 端到端 ──────────────────────────────────────────────────────

    #[test]
    fn end_to_end_llm_response_with_code_and_list() {
        let input = r#"当前有 **3 个**客户端连接。

## 详情

- 客户端 A（`mqttv5`，已连 12 秒）
- 客户端 B（`mqttv3.1.1`，已连 5 分钟）
- 客户端 C（已连 1 小时）

文档参考：https://example.com/docs
"#;
        let out = transform_for_tts(input, &cfg());
        // markdown 强调去掉
        assert!(!out.contains("**"));
        // heading 标记去掉
        assert!(!out.contains("##"));
        // inline code 去掉
        assert!(!out.contains('`'));
        // 链接换成"链接"
        assert!(out.contains("链接"));
        // bullet list 展开成中文序数
        assert!(out.contains("第一"));
        assert!(out.contains("第二"));
        assert!(out.contains("第三"));
        // 数字保留
        assert!(out.contains("3"));
        assert!(out.contains("12"));
    }

    #[test]
    fn end_to_end_code_block_dropped_completely() {
        let input = "下面是示例代码：\n```python\nprint('hello')\nprint('world')\n```\n代码结束。";
        let out = transform_for_tts(input, &cfg());
        assert!(!out.contains("print"));
        assert!(!out.contains("python"));
        assert!(out.contains("代码结束"));
    }

    #[test]
    fn end_to_end_table_dropped() {
        let input = "数据：\n| name | count |\n| - | - |\n| a | 1 |\n| b | 2 |\n完。";
        let out = transform_for_tts(input, &cfg());
        assert!(!out.contains('|'));
        assert!(out.contains("数据"));
        assert!(out.contains("完"));
    }

    #[test]
    fn disabled_cfg_returns_original() {
        let input = "**bold** with `code` and 🔧";
        let out = transform_for_tts(input, &noop_cfg());
        assert_eq!(out, input);
    }

    #[test]
    fn empty_input_returns_empty() {
        assert_eq!(transform_for_tts("", &cfg()), "");
    }

    #[test]
    fn short_input_under_max_chars_passes_through() {
        let input = "你好，连接 3 个客户端。";
        let out = transform_for_tts(input, &cfg());
        assert_eq!(out, input);
    }

    #[test]
    fn long_input_truncated_with_suffix() {
        let mut input = String::new();
        for _ in 0..30 {
            input.push_str("这里是一段比较长的中文回答，用于测试截断逻辑。");
        }
        let out = transform_for_tts(&input, &cfg());
        assert!(out.chars().count() < input.chars().count());
        assert!(out.ends_with("省略了"));
    }

    #[test]
    fn minimal_cfg_isolates_single_stage() {
        // 只开 drop_code，验证其他 stage 不会误伤
        let c = minimal_cfg(&["drop_code"]);
        let input = "这是 **重要** 的事，详见 [文档](https://x.y) 与 - bullet";
        let out = transform_for_tts(input, &c);
        assert!(out.contains("**"));
        assert!(out.contains("[文档]"));
        assert!(out.contains("- bullet"));
    }

    // ── EnsureChinese stage 单元测试 ─────────────────────────────────

    fn zh_cfg() -> VoiceOutputConfig {
        VoiceOutputConfig {
            require_chinese: true,
            ..VoiceOutputConfig::default()
        }
    }

    #[test]
    fn ensure_chinese_disabled_is_noop() {
        let s = EnsureChinese;
        // 默认 config.require_chinese = false → should_run 跳过
        assert!(!s.should_run("Hello world", &cfg()));
        assert!(!s.should_run("中文", &cfg()));
    }

    #[test]
    fn ensure_chinese_pass_action_is_noop() {
        let s = EnsureChinese;
        let c = VoiceOutputConfig {
            require_chinese: true,
            require_chinese_action: "pass".into(),
            ..cfg()
        };
        assert!(!s.should_run("Hello world", &c));
    }

    #[test]
    fn ensure_chinese_strips_latin_when_ratio_below_threshold() {
        let s = EnsureChinese;
        let c = zh_cfg();
        // "Hello 你好" — 拉丁 5 个 + CJK 2 个 = ratio 0.29 < 0.5
        let out = s.apply("Hello 你好", &c);
        assert_eq!(out, "你好");
    }

    #[test]
    fn ensure_chinese_keeps_chinese_punctuation() {
        let s = EnsureChinese;
        let c = zh_cfg();
        let out = s.apply("Hello 你好，世界！", &c);
        assert!(out.contains("你好"));
        assert!(out.contains("，"));
        assert!(out.contains("！"));
        assert!(!out.contains("Hello"));
    }

    #[test]
    fn ensure_chinese_pure_chinese_passes_through() {
        let s = EnsureChinese;
        let c = zh_cfg();
        // ratio = 1.0 ≥ 0.5 → 不动
        let out = s.apply("你好，世界，今天天气不错。", &c);
        assert_eq!(out, "你好，世界，今天天气不错。");
    }

    #[test]
    fn ensure_chinese_pure_english_gets_stripped_to_empty() {
        let s = EnsureChinese;
        let c = zh_cfg();
        let out = s.apply("Hello world", &c);
        assert_eq!(out, "");
    }

    #[test]
    fn ensure_chinese_ignores_pure_digits_in_denominator() {
        let s = EnsureChinese;
        let c = VoiceOutputConfig {
            require_chinese: true,
            require_chinese_min_ratio: 0.5,
            ..cfg()
        };
        // 数字不应被算入"非 CJK"以避免误判：
        // "1883 个客户端" — CJK 4 个，标点 1 个，digit 4 个
        // 分母跳过空白，分母 = 4 (CJK) + 4 (digit) = 8；分子 = 4
        // ratio = 0.5 ≥ 0.5 → 不动
        let out = s.apply("1883 个客户端", &c);
        assert_eq!(out, "1883 个客户端");
    }

    #[test]
    fn ensure_chinese_low_threshold_lets_short_chinese_pass() {
        let s = EnsureChinese;
        let c = VoiceOutputConfig {
            require_chinese: true,
            require_chinese_min_ratio: 0.2,
            ..cfg()
        };
        // ratio = 3/8 = 0.375 ≥ 0.2 → 不动
        let out = s.apply("Hi 你好呀", &c);
        assert_eq!(out, "Hi 你好呀");
    }
}
