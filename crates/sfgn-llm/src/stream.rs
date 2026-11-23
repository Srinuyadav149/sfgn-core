use crate::{TokenUsage, TurnResponse, TurnStreamEvent};

/// Demuxes `llama-server` SSE deltas into separated `thinking` and `content` channels.
///
/// Handles both:
/// 1. Split-channel responses (`delta.reasoning_content` + `delta.content`)
/// 2. Inline `<think>...</think>` tags inside `delta.content` across arbitrary chunk boundaries
#[derive(Debug, Default)]
pub struct StreamDemuxer {
    thinking: String,
    content: String,
    finish_reason: Option<String>,
    usage: TokenUsage,
    in_inline_think: bool,
    tag_carry: String,
}

impl StreamDemuxer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Processes a token delta from `choices[0].delta.reasoning_content`.
    pub fn push_reasoning_delta(
        &mut self,
        delta: &str,
        on_event: &mut dyn FnMut(TurnStreamEvent<'_>),
    ) {
        if delta.is_empty() {
            return;
        }
        let cleaned = delta.replace("<think>", "").replace("</think>", "");
        if !cleaned.is_empty() {
            on_event(TurnStreamEvent::Thinking(&cleaned));
            self.thinking.push_str(&cleaned);
        }
    }

    /// Processes a token delta from `choices[0].delta.content`, splitting `<think>...</think>`
    /// blocks from the final JSON action even when tags span multiple SSE chunks.
    pub fn push_content_delta(
        &mut self,
        delta: &str,
        on_event: &mut dyn FnMut(TurnStreamEvent<'_>),
    ) {
        if delta.is_empty() {
            return;
        }

        let mut buf = String::with_capacity(self.tag_carry.len() + delta.len());
        buf.push_str(&self.tag_carry);
        buf.push_str(delta);
        self.tag_carry.clear();

        let mut rest = buf.as_str();
        while !rest.is_empty() {
            if self.in_inline_think {
                if let Some(idx) = rest.find("</think>") {
                    let before = &rest[..idx];
                    if !before.is_empty() {
                        on_event(TurnStreamEvent::Thinking(before));
                        self.thinking.push_str(before);
                    }
                    self.in_inline_think = false;
                    rest = &rest[idx + "</think>".len()..];
                } else {
                    let hold = trailing_tag_prefix_len(rest, "</think>");
                    let safe_end = rest.len() - hold;
                    if safe_end > 0 {
                        let safe = &rest[..safe_end];
                        on_event(TurnStreamEvent::Thinking(safe));
                        self.thinking.push_str(safe);
                    }
                    if hold > 0 {
                        self.tag_carry.push_str(&rest[safe_end..]);
                    }
                    break;
                }
            } else if let Some(idx) = rest.find("<think>") {
                let before = &rest[..idx];
                if !before.is_empty() {
                    on_event(TurnStreamEvent::Content(before));
                    self.content.push_str(before);
                }
                self.in_inline_think = true;
                rest = &rest[idx + "<think>".len()..];
            } else {
                let hold = trailing_tag_prefix_len(rest, "<think>");
                let safe_end = rest.len() - hold;
                if safe_end > 0 {
                    let safe = &rest[..safe_end];
                    on_event(TurnStreamEvent::Content(safe));
                    self.content.push_str(safe);
                }
                if hold > 0 {
                    self.tag_carry.push_str(&rest[safe_end..]);
                }
                break;
            }
        }
    }

    pub fn set_finish_reason(&mut self, reason: impl Into<String>) {
        self.finish_reason = Some(reason.into());
    }

    pub fn set_usage(&mut self, usage: TokenUsage) {
        self.usage = usage;
    }

    /// Flushes any trailing partial tag buffer and materializes the final [`TurnResponse`].
    pub fn finish(
        mut self,
        ttft: std::time::Duration,
        elapsed: std::time::Duration,
        on_event: &mut dyn FnMut(TurnStreamEvent<'_>),
    ) -> TurnResponse {
        if !self.tag_carry.is_empty() {
            if self.in_inline_think {
                on_event(TurnStreamEvent::Thinking(&self.tag_carry));
                self.thinking.push_str(&self.tag_carry);
            } else {
                on_event(TurnStreamEvent::Content(&self.tag_carry));
                self.content.push_str(&self.tag_carry);
            }
        }

        TurnResponse {
            thinking: self.thinking,
            content: self.content.trim().to_owned(),
            finish_reason: self.finish_reason,
            usage: self.usage,
            ttft,
            elapsed,
        }
    }
}

/// Returns the byte length of the longest suffix of `haystack` that is a proper prefix of `tag`.
fn trailing_tag_prefix_len(haystack: &str, tag: &str) -> usize {
    let max_check = (tag.len() - 1).min(haystack.len());
    for len in (1..=max_check).rev() {
        if haystack.ends_with(&tag[..len]) {
            return len;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn demuxes_split_reasoning_and_content_channels() {
        let mut demux = StreamDemuxer::new();
        let mut noop = |_ev: TurnStreamEvent<'_>| {};

        demux.push_reasoning_delta("<think>Step 1.", &mut noop);
        demux.push_reasoning_delta(" Step 2.</think>", &mut noop);
        demux.push_content_delta("{\"action\":\"finish\",\"summary\":\"done\"}", &mut noop);

        let resp = demux.finish(
            Duration::from_millis(10),
            Duration::from_millis(50),
            &mut noop,
        );
        assert_eq!(resp.thinking, "Step 1. Step 2.");
        assert_eq!(resp.content, "{\"action\":\"finish\",\"summary\":\"done\"}");
    }

    #[test]
    fn demuxes_inline_think_tags_split_across_chunks() {
        let mut demux = StreamDemuxer::new();
        let mut noop = |_ev: TurnStreamEvent<'_>| {};

        demux.push_content_delta("<thi", &mut noop);
        demux.push_content_delta("nk>Inspecting file.</th", &mut noop);
        demux.push_content_delta(
            "ink>\n{\"action\":\"inspect_node\",\"address\":\"src/lib.rs\"}",
            &mut noop,
        );

        let resp = demux.finish(
            Duration::from_millis(10),
            Duration::from_millis(50),
            &mut noop,
        );
        assert_eq!(resp.thinking, "Inspecting file.");
        assert_eq!(
            resp.content,
            "{\"action\":\"inspect_node\",\"address\":\"src/lib.rs\"}"
        );
    }
}
