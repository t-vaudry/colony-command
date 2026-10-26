//! How full a session's context is, and when that is worth saying.
//!
//! The size of a session's context is the input side of its newest main-thread API
//! response: `input_tokens + cache_read_input_tokens + cache_creation_input_tokens`.
//! Every request re-sends it, so a large one is the biggest silent cost of a session.

use crate::usage::Tokens;

/// Context window of a model run normally.
pub const DEFAULT_WINDOW: u64 = 200_000;
/// Context window of a model run with the `[1m]` suffix.
pub const LONG_WINDOW: u64 = 1_000_000;
/// Share of the window, in percent, at which Colony speaks up, unless settings say otherwise.
pub const DEFAULT_NUDGE_PCT: u32 = 70;

/// What a response put into the context: the whole input side.
pub fn context_of(t: &Tokens) -> u64 {
    t.input + t.cache_read + t.cache_creation
}

/// The context window, in tokens. 1M when the model id has the `[1m]` suffix (that is how
/// a session asks for it, in `--model` and in the hook's `SessionStart` payload). Transcripts
/// record the model without the suffix, so a context past 200k is the other evidence: it
/// cannot fit in the small window.
pub fn window(model: Option<&str>, context: u64) -> u64 {
    let long = model.is_some_and(|m| m.to_ascii_lowercase().contains("[1m]"));
    if long || context > DEFAULT_WINDOW {
        LONG_WINDOW
    } else {
        DEFAULT_WINDOW
    }
}

/// Whole percent of the window used, rounded down (so "71%" never means 70.9).
pub fn percent(context: u64, window: u64) -> u32 {
    if window == 0 {
        return 0;
    }
    u32::try_from(context.saturating_mul(100) / window).unwrap_or(u32::MAX)
}

/// Settings accept 10 to 95 percent; anything else falls back to the default.
pub fn clamp_pct(pct: u32) -> u32 {
    if (10..=95).contains(&pct) {
        pct
    } else {
        DEFAULT_NUDGE_PCT
    }
}

/// The token count at which a session started on `model` gets nudged.
pub fn nudge_tokens(model: Option<&str>, pct: u32) -> u64 {
    window(model, 0) * u64::from(clamp_pct(pct)) / 100
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_is_200k_unless_the_model_or_the_size_says_1m() {
        assert_eq!(window(None, 0), DEFAULT_WINDOW);
        assert_eq!(window(Some("claude-sonnet-5-5"), 150_000), DEFAULT_WINDOW);
        assert_eq!(window(Some("claude-sonnet-5-5[1m]"), 10), LONG_WINDOW);
        assert_eq!(window(Some("Claude-Opus-5-5[1M]"), 10), LONG_WINDOW);
        assert_eq!(window(Some("claude-opus-5-5"), 200_001), LONG_WINDOW);
        assert_eq!(window(Some("claude-opus-5-5"), 200_000), DEFAULT_WINDOW);
    }

    #[test]
    fn percent_rounds_down() {
        assert_eq!(percent(142_000, 200_000), 71);
        assert_eq!(percent(139_999, 200_000), 69);
        assert_eq!(percent(5, 0), 0);
    }

    #[test]
    fn context_is_the_whole_input_side() {
        assert_eq!(context_of(&Tokens { input: 10, output: 999, cache_read: 100_000, cache_creation: 2_000 }), 102_010);
    }

    #[test]
    fn nudge_point_follows_the_window_and_the_setting() {
        assert_eq!(nudge_tokens(Some("sonnet"), 70), 140_000);
        assert_eq!(nudge_tokens(Some("claude-sonnet-5-5[1m]"), 50), 500_000);
        assert_eq!(nudge_tokens(None, 0), 140_000, "out of range: default");
        assert_eq!(nudge_tokens(None, 99), 140_000);
    }
}
