//! Machine-ID aliases from tokscale, limited to the supported coding tools.
//! Display labels are join keys within a conversation, never pricing keys.

const ALIASES: &[(&str, &str)] = &[
    ("anthropic/claude-4-5-haiku", "claude-haiku-4-5"),
    ("anthropic/claude-4-5-opus", "claude-opus-4-5"),
    ("anthropic/claude-4-5-sonnet", "claude-sonnet-4-5"),
    ("anthropic/claude-4-6-haiku", "claude-haiku-4-6"),
    ("anthropic/claude-4-6-opus", "claude-opus-4-6"),
    ("anthropic/claude-4-6-sonnet", "claude-sonnet-4-6"),
    ("anthropic/claude-5-5-haiku", "claude-haiku-5-5"),
    ("anthropic/claude-5-5-opus", "claude-opus-5-5"),
    ("anthropic/claude-5-5-sonnet", "claude-sonnet-5-5"),
    ("anthropic/claude-haiku-5-5", "claude-haiku-5-5"),
    ("anthropic/claude-opus-5-5", "claude-opus-5-5"),
    ("anthropic/claude-sonnet-5-5", "claude-sonnet-5-5"),
    ("anthropic/claude-sonnet-5-5-high", "claude-sonnet-5-5"),
    ("anthropic/claude-sonnet-5-5-low", "claude-sonnet-5-5"),
    ("anthropic/claude-sonnet-5-5-medium", "claude-sonnet-5-5"),
    ("claude-haiku-5-5-high", "claude-haiku-5-5"),
    ("claude-haiku-5-5-low", "claude-haiku-5-5"),
    ("claude-haiku-5-5-medium", "claude-haiku-5-5"),
    ("claude-haiku-5-5-thinking", "claude-haiku-5-5"),
    ("claude-opus-4-0", "claude-opus-4"),
    ("claude-opus-4-6-thinking", "claude-opus-4-6"),
    ("claude-opus-41", "claude-opus-4-1"),
    ("claude-opus-5-5-high", "claude-opus-5-5"),
    ("claude-opus-5-5-low", "claude-opus-5-5"),
    ("claude-opus-5-5-medium", "claude-opus-5-5"),
    ("claude-opus-5-5-thinking", "claude-opus-5-5"),
    ("claude-sonnet-4-0", "claude-sonnet-4"),
    ("claude-sonnet-4-6-high", "claude-sonnet-4-6"),
    ("claude-sonnet-4-6-low", "claude-sonnet-4-6"),
    ("claude-sonnet-4-6-medium", "claude-sonnet-4-6"),
    ("claude-sonnet-4-6-thinking", "claude-sonnet-4-6"),
    ("claude-sonnet-5-5-high", "claude-sonnet-5-5"),
    ("claude-sonnet-5-5-low", "claude-sonnet-5-5"),
    ("claude-sonnet-5-5-medium", "claude-sonnet-5-5"),
    ("claude-sonnet-5-5-thinking", "claude-sonnet-5-5"),
    ("daybreak-blue-latest", "gpt-5.6-sol"),
    ("gemini-3-flash", "gemini-3-flash-preview"),
    ("gemini-3-flash-a", "gemini-3.5-flash-high"),
    ("gemini-3-flash-agent", "gemini-3.5-flash-high"),
    ("gemini-3-flash-b", "gemini-3.5-flash-high"),
    ("gemini-3-flash-c", "gemini-3-flash-preview"),
    ("gemini-3-pro-high", "gemini-3-pro"),
    ("gemini-3-pro-low", "gemini-3-pro"),
    ("gemini-3.1-pro-high", "gemini-3.1-pro"),
    ("gemini-3.1-pro-low", "gemini-3.1-pro"),
    ("gemini-3.5-flash-low", "gemini-3.5-flash-medium"),
    ("gemini-pro-agent", "gemini-3.1-pro"),
    ("gemini-pro-default", "gemini-3.1-pro"),
    ("gpt-daybreak-blue-latest", "gpt-5.6-sol"),
    ("kimi-code/k3", "moonshot/kimi-k3"),
    ("kimi-code/k3-256k", "moonshot/kimi-k3"),
    ("model_openai_gpt_oss_120b_medium", "gpt-oss-120b-medium"),
    ("model_placeholder_m132", "gemini-3.5-flash-high"),
    ("model_placeholder_m133", "gemini-3.5-flash-high"),
    ("model_placeholder_m16", "gemini-3.1-pro"),
    ("model_placeholder_m18", "gemini-3-flash-preview"),
    ("model_placeholder_m187", "gemini-3.5-flash-extra-low"),
    ("model_placeholder_m20", "gemini-3.5-flash-medium"),
    ("model_placeholder_m26", "claude-opus-4-6"),
    ("model_placeholder_m35", "claude-sonnet-4-6"),
    ("model_placeholder_m36", "gemini-3.1-pro"),
    ("model_placeholder_m37", "gemini-3.1-pro"),
    ("model_placeholder_m47", "gemini-3-flash-preview"),
    ("model_placeholder_m84", "gemini-3-flash-preview"),
    ("openai/daybreak-blue-latest", "gpt-5.6-sol"),
    ("openai/gpt-daybreak-blue-latest", "gpt-5.6-sol"),
];

/// Resolve a lowercase id with dotted Claude versions already normalized.
pub(super) fn resolve_alias(model: &str) -> Option<&'static str> {
    ALIASES
        .binary_search_by_key(&model, |(alias, _)| *alias)
        .ok()
        .map(|index| ALIASES[index].1)
}

/// Canonical identity used by pricing and Antigravity's ambiguity checks.
pub(crate) fn canonical_id(model: &str) -> String {
    let mut name = model.trim().to_ascii_lowercase();
    let bare_start = name.rfind('/').map_or(0, |cut| cut + 1);
    if name[bare_start..].starts_with("claude-") {
        let normalized = name[bare_start..].replace('.', "-");
        name.replace_range(bare_start.., &normalized);
    }
    resolve_alias(&name).unwrap_or(&name).to_owned()
}
