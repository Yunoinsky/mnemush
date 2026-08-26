//! Live integration tests against real LLM providers. All gated on env vars.
//!
//!   MNEMUSH_LIVE_LLM=1          — enables basic live tests
//!   MNEMUSH_LIVE_LLM_COMPARE=1  — enables the 3-provider comparison
//!
//! Required env for the comparison run:
//!   MINIMAX_API_KEY / DEEPSEEK_API_KEY — cloud credentials
//!   Ollama running at http://localhost:11434 with the local model loaded

use mnemush::config::Config;
use mnemush::llm;

#[test]
fn live_ollama_chat_round_trip() {
    if std::env::var("MNEMUSH_LIVE_LLM").ok().as_deref() != Some("1") {
        eprintln!("skipping (set MNEMUSH_LIVE_LLM=1)");
        return;
    }
    let cfg = Config::load().unwrap();
    let msgs = vec![
        llm::ChatMsg::system("You are concise."),
        llm::ChatMsg::user("Reply with exactly the word OK."),
    ];
    let (text, usage) = llm::chat_with_usage_cfg(&msgs, &cfg).unwrap();
    assert!(!text.trim().is_empty());
    assert!(text.to_uppercase().contains("OK"), "got: {text}");
    eprintln!("round-trip: text={:?} usage={:?}", text.trim(), usage);
}

/// Side-by-side comparison of the three providers on a small
/// dream-like prompt. Prints results to stderr. Pass `MNEMUSH_LIVE_LLM_COMPARE=1`.
#[test]
fn compare_three_providers_on_dream_prompt() {
    if std::env::var("MNEMUSH_LIVE_LLM_COMPARE").ok().as_deref() != Some("1") {
        eprintln!("skipping compare (set MNEMUSH_LIVE_LLM_COMPARE=1)");
        return;
    }

    let system = r#"你是记忆库巩固者。分析以下候选记忆,输出 JSON 动作列表。
         巩固: update({id,content,reason}) 修订内容; link({source,target,etype,strength}) source指向target 建边; merge({keep,absorb}) 重复记忆合并; insight({title,content,links}) 发现跨簇新模式; neuropilize({id,path}) 将可结构化记忆归档。
         主动遗忘: decay(降权)/ forget(软删)。
         双阈值: confidence<0.4 的记忆低证据即可遗忘; confidence>=0.4 需明确矛盾/过时证据。
         保护规则: importance>=0.7 / never_prune / identity / 7 天内创建 → 禁止 decay/forget。
         动作 type 只能是: update/link/merge/insight/decay/forget/neuropilize。
         所有 id 必须原样使用候选列表中的完整 id。
         输出严格 JSON, 示例: {"actions":[{"type":"link","source":"A","target":"B","etype":"related","strength":0.6}]}。不要 markdown 代码块。

候选记忆:
[0] id=019f1840-aa31-7721-9404-aafcc2b08b3e category=note importance=0.50 confidence=1.00 created=2026-08-25
title: 测试记忆
content: 简短测试条目
---
[1] id=019f18ce-1837-7b21-9629-2a79cc501404 category=note importance=0.50 confidence=1.00 created=2026-08-25
title: 重启验证
content: 验证 mnemush 重启后状态保留
---
[2] id=019f1919-1769-7e03-8a5f-04ccebf13b2a category=note importance=0.50 confidence=1.00 created=2026-08-25
title: 最终回归测试
content: 跑完所有 192 测试
---
[3] id=019f196e-1f9d-7d40-add5-1bb859669c50 category=tool_quirk importance=0.50 confidence=1.00 created=2026-08-25
title: pi extension: user_prompt_submit
content: 插件钩子文档
---"#;
    let msgs = vec![
        llm::ChatMsg::system(system),
        llm::ChatMsg::user("请分析并输出动作。"),
    ];

    let cfg = Config::load().unwrap();
    let candidates: Vec<(&str, llm::ProviderKind, String)> = vec![
        (
            "minimax",
            llm::ProviderKind::MiniMax,
            std::env::var("MINIMAX_API_KEY")
                .ok()
                .or_else(|| llm::minimax_key())
                .unwrap_or_default(),
        ),
        (
            "deepseek",
            llm::ProviderKind::DeepSeek,
            std::env::var("DEEPSEEK_API_KEY").unwrap_or_default(),
        ),
        ("local", llm::ProviderKind::Local, String::new()),
    ];

    for (name, kind, api_key) in candidates {
        let p = llm::Provider {
            name: name.into(),
            base_url: match kind {
                llm::ProviderKind::MiniMax => llm::MINIMAX_CHAT_URL.into(),
                llm::ProviderKind::DeepSeek => llm::DEEPSEEK_CHAT_URL.into(),
                llm::ProviderKind::Local => cfg.llm.local_base_url.clone(),
            },
            model: match kind {
                llm::ProviderKind::MiniMax => llm::minimax_model(),
                llm::ProviderKind::DeepSeek => llm::deepseek_model(),
                llm::ProviderKind::Local => cfg.llm.local_model.clone(),
            },
            api_key: if api_key.is_empty() { None } else { Some(api_key) },
            kind,
        };
        let t0 = std::time::Instant::now();
        match llm::chat_with_provider(&p, &msgs) {
            Ok((text, usage)) => {
                let dt = t0.elapsed();
                let preview: String = text.chars().take(200).collect();
                // Count action types
                let action_counts = count_actions(&text);
                eprintln!(
                    "\n========== [{}] ==========\ntime:       {:.2}s\nprompt:     {} tok\ncompletion: {} tok\nreasoning:  {} tok\nmodel:      {}\n--- response (first 200 chars) ---\n{}{}\n--- action counts: {}\n",
                    name,
                    dt.as_secs_f64(),
                    usage.prompt_tokens,
                    usage.completion_tokens,
                    usage.reasoning_tokens,
                    p.model,
                    preview,
                    if text.len() > 200 { "..." } else { "" },
                    action_counts
                );
            }
            Err(e) => {
                eprintln!("\n========== [{}] ==========\nERR in {:.1}s: {}\n", name, t0.elapsed().as_secs_f64(), e);
            }
        }
    }
}

fn count_actions(text: &str) -> String {
    // Try parse the JSON. If it fails, report raw length.
    let trimmed = text.trim();
    // The model might wrap with ```json fences or extra text; find first '{' to last '}'.
    let start = trimmed.find('{');
    let end = trimmed.rfind('}');
    let json_str = match (start, end) {
        (Some(s), Some(e)) if e > s => &trimmed[s..=e],
        _ => return format!("(parse fail, len={})", trimmed.len()),
    };
    match serde_json::from_str::<serde_json::Value>(json_str) {
        Ok(v) => {
            let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
            if let Some(arr) = v.get("actions").and_then(|x| x.as_array()) {
                for a in arr {
                    let t = a.get("type").and_then(|x| x.as_str()).unwrap_or("?");
                    *counts.entry(t.to_string()).or_insert(0) += 1;
                }
                let total: usize = counts.values().sum();
                format!("total={} by_type={:?}", total, counts)
            } else {
                "(no actions array)".into()
            }
        }
        Err(e) => format!("(json parse: {})", e),
    }
}
