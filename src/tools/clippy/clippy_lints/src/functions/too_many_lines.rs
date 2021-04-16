use rustc_hir as hir;
use rustc_lint::{LateContext, LintContext};
use rustc_middle::lint::in_external_macro;
use rustc_span::Span;

use clippy_utils::diagnostics::span_lint;
use clippy_utils::source::snippet;

use super::TOO_MANY_LINES;

pub(super) fn check_fn(cx: &LateContext<'_>, span: Span, body: &'tcx hir::Body<'_>, too_many_lines_threshold: u64) {
    if in_external_macro(cx.sess(), span) {
        return;
    }

    let code_snippet = snippet(cx, body.value.span, "..");
    let mut line_count: u64 = 0;
    let mut in_comment = false;
    let mut code_in_line;

    // Skip the surrounding function decl.
    let start_brace_idx = code_snippet.find('{').map_or(0, |i| i + 1);
    let end_brace_idx = code_snippet.rfind('}').unwrap_or_else(|| code_snippet.len());
    let function_lines = code_snippet[start_brace_idx..end_brace_idx].lines();

    for mut line in function_lines {
        code_in_line = false;
        loop {
            line = line.trim_start();
            if line.is_empty() {
                break;
            }
            if in_comment {
                if let Some(i) = line.find("*/") {
                    line = &line[i + 2..];
                    in_comment = false;
                    continue;
                }
            } else {
                let multi_idx = line.find("/*").unwrap_or_else(|| line.len());
                let single_idx = line.find("//").unwrap_or_else(|| line.len());
                code_in_line |= multi_idx > 0 && single_idx > 0;
                // Implies multi_idx is below line.len()
                if multi_idx < single_idx {
                    line = &line[multi_idx + 2..];
                    in_comment = true;
                    continue;
                }
            }
            break;
        }
        if code_in_line {
            line_count += 1;
        }
    }

    if line_count > too_many_lines_threshold {
        span_lint(
            cx,
            TOO_MANY_LINES,
            span,
            &format!(
                "this function has too many lines ({}/{})",
                line_count, too_many_lines_threshold
            ),
        )
    }
}
