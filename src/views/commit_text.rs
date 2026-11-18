//! Commit subject presentation shared by the history list and the commit header.

use gpui::{AnyElement, FontWeight, HighlightStyle, Rgba, StyledText, div, prelude::*, px};

use crate::services::project_git::workflow;
use crate::theme::{self, terminal_manager as chrome};

pub(super) fn tint(color: Rgba, alpha: f32) -> Rgba {
    Rgba { a: alpha, ..color }
}

/// Flat label for a Conventional Commits prefix. Tone only reinforces the text: the kind
/// is semibold and the scope is plain muted text, so both read without color.
pub(super) fn prefix_badge(prefix: workflow::CommitPrefix<'_>) -> gpui::Div {
    let kind = prefix.kind.to_ascii_lowercase();
    let tone = match kind.as_str() {
        _ if prefix.breaking => theme::error(),
        "feat" => theme::success(),
        "fix" | "hotfix" => theme::modified(),
        "revert" => theme::error(),
        _ => theme::ash(),
    };
    div()
        .flex_shrink_0()
        .max_w(px(140.0))
        .h(px(18.0))
        .px(px(6.0))
        .flex()
        .items_center()
        .gap(px(5.0))
        .overflow_hidden()
        .rounded(px(4.0))
        .bg(tint(tone, 0.12))
        .font_family(chrome::CHROME_FONT)
        .text_size(px(chrome::DETAIL_TEXT_SIZE))
        .line_height(px(14.0))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(tone)
        .child(div().flex_shrink_0().child(kind))
        .when_some(prefix.scope, |badge, scope| {
            badge.child(
                div()
                    .min_w_0()
                    .truncate()
                    .font_weight(FontWeight::NORMAL)
                    .text_color(theme::ash())
                    .child(scope.to_owned()),
            )
        })
        .when(prefix.breaking, |badge| {
            badge.child(div().flex_shrink_0().text_color(theme::error()).child("!"))
        })
}

/// Subject text with `` `code` `` spans set on a soft fill instead of literal backticks.
pub(super) fn subject(text: &str) -> AnyElement {
    let Some((text, spans)) = workflow::inline_code(text) else {
        return text.to_owned().into_any_element();
    };
    let code = HighlightStyle {
        background_color: Some(tint(theme::bone(), 0.1).into()),
        ..Default::default()
    };
    StyledText::new(text)
        .with_highlights(spans.into_iter().map(|span| (span, code)))
        .into_any_element()
}
