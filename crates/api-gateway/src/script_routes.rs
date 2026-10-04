//! Pine-lite script routes (`docs/23` Phase 9).
//!
//! `POST /scripts/vet` is the gate between the studio's chat and the chart:
//! it runs the same lex/parse/typecheck/limits pipeline `pine_lite::vet`
//! runs everywhere else, and returns the *full* error list so the agent's
//! repair loop can fix everything at once. Vetting here is what makes "the
//! platform vets scripts" true end to end: a source that never passed this
//! route has no path to a chart, a backtest, or a bot.
//!
//! There is deliberately no persistence here: scripts are stored as
//! indicator-workspace revisions (the same table, a new representation), so
//! the revision lifecycle -- append-only, restore, promote -- is inherited
//! rather than reinvented.

use axum::Json;
use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::extract::ApiJson;

/// Body of `POST /scripts/vet`.
#[derive(Debug, Deserialize)]
pub struct VetRequest {
    /// The script source, including its `//@pine_lite` header.
    pub source: String,
}

/// One vetting failure.
#[derive(Debug, Serialize)]
pub struct ScriptIssue {
    /// Which layer refused: `lex | parse | type | limit`.
    pub kind: String,
    /// Line, 1-indexed.
    pub line: usize,
    /// Column, 1-indexed.
    pub col: usize,
    /// The message, written to be fed back to the author (human or model).
    pub message: String,
}

/// One `input.*` declaration, for the settings UI.
#[derive(Debug, Serialize)]
pub struct ScriptInput {
    /// The variable the input binds.
    pub name: String,
    /// The input kind (`int`, `float`, `bool`, `string`, `color`).
    pub kind: String,
    /// The declared default, JSON-typed like the value the settings UI will
    /// send back (docs/25): a number for `int`/`float`, a bool for `bool`, a
    /// string for `string`, and a `#rrggbb` string for `color`. Absent when
    /// the declaration carried no literal default.
    pub default: Option<serde_json::Value>,
    /// The declared title, when any.
    pub title: Option<String>,
}

/// Response of `POST /scripts/vet`.
#[derive(Debug, Serialize)]
pub struct VetResponse {
    /// Whether the script passed every layer.
    pub valid: bool,
    /// Every problem, when it did not.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub issues: Vec<ScriptIssue>,
    /// The header's knobs, when it did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header: Option<ScriptHeader>,
    /// The script's `input.*` declarations, when it did, for the settings UI.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<ScriptInput>,
    /// Whether the script plots onto the price pane.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overlay: Option<bool>,
}

/// The `//@pine_lite` header, echoed.
#[derive(Debug, Serialize)]
pub struct ScriptHeader {
    /// The annotation version.
    pub version: u32,
    /// The display title.
    pub title: Option<String>,
    /// The history budget.
    pub max_bars_back: usize,
}

/// `POST /scripts/vet`.
///
/// # Errors
/// Never a transport error: a vetting failure is a 200 with `valid: false`
/// and the issue list, because "your script is wrong" is the route's answer,
/// not an error the HTTP layer should shape.
pub async fn vet(ApiJson(request): ApiJson<VetRequest>) -> Result<Json<VetResponse>, ApiError> {
    Ok(Json(vet_response(&request.source)))
}

/// The vetting pipeline, shared by the route and tests.
#[must_use]
pub fn vet_response(source: &str) -> VetResponse {
    match pine_lite::vet(source) {
        Ok((header, script)) => {
            // The header-aware check, with the script's OWN header: `request.*`
            // legality depends on `sec=` in the header, so rechecking against
            // the default header would refuse every honest multi-symbol
            // script (the `sec` knob is known only to the header).
            let type_errors = pine_lite::typecheck::check_with_header(&script, &header);
            VetResponse {
                valid: type_errors.is_empty(),
                issues: type_errors
                    .into_iter()
                    .map(|e| ScriptIssue {
                        kind: pine_lite::kind_name(e.kind).to_string(),
                        line: e.span.line,
                        col: e.span.col,
                        message: e.message,
                    })
                    .collect(),
                header: Some(ScriptHeader {
                    version: header.version,
                    title: header.title.clone(),
                    max_bars_back: header.max_bars_back,
                }),
                inputs: collect_inputs(&script),
                overlay: Some(header.overlay),
            }
        }
        Err(errs) => VetResponse {
            valid: false,
            issues: errs
                .iter()
                .map(|e| ScriptIssue {
                    kind: pine_lite::kind_name(e.kind).to_string(),
                    line: e.span.line,
                    col: e.span.col,
                    message: e.message.clone(),
                })
                .collect(),
            header: None,
            inputs: Vec::new(),
            overlay: None,
        },
    }
}

/// Pull the `input.*` declarations out of a parsed script. Shared with the
/// workspace revision routes, which store the declarations in the revision's
/// validation record so an attach needs no re-vet (docs/25).
pub(crate) fn collect_inputs(script: &pine_lite::parse::Script) -> Vec<ScriptInput> {
    let mut out = Vec::new();
    for item in &script.items {
        if let pine_lite::parse::Item::Assign { name, expr, .. } = item {
            if let pine_lite::parse::ExprKind::Call { callee, args } = &expr.kind {
                if let Some(kind) = callee.strip_prefix("input.") {
                    // The default, JSON-typed the way the settings UI will
                    // send the value back: number, bool, string, and a
                    // `#rrggbb` string for a color literal. A non-literal
                    // default (an expression) is not a value the UI can
                    // prefill, so the field stays absent and the script's own
                    // evaluation of it stands.
                    let default = args.iter().find_map(|a| {
                        if a.name.as_deref() != Some("defval") {
                            return None;
                        }
                        match &a.value.kind {
                            pine_lite::parse::ExprKind::Num(n) => {
                                Some(serde_json::Value::from(*n))
                            }
                            pine_lite::parse::ExprKind::Bool(b) => {
                                Some(serde_json::Value::from(*b))
                            }
                            pine_lite::parse::ExprKind::Str(s) => {
                                Some(serde_json::Value::from(s.clone()))
                            }
                            pine_lite::parse::ExprKind::Color(c) => {
                                // Packed 0xAARRGGBB (the lexer's `| 0xFF000000`
                                // for the 6-digit form); the UI speaks
                                // `#rrggbb`, so the alpha byte comes off.
                                Some(serde_json::Value::from(format!("#{:06x}", c & 0xFF_FFFF)))
                            }
                            _ => None,
                        }
                    });
                    let title = args.iter().find_map(|a| {
                        if a.name.as_deref() == Some("title") {
                            if let pine_lite::parse::ExprKind::Str(s) = &a.value.kind {
                                return Some(s.clone());
                            }
                        }
                        None
                    });
                    out.push(ScriptInput {
                        name: name.clone(),
                        kind: kind.to_string(),
                        default,
                        title,
                    });
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_valid_script_reports_header_and_inputs() {
        let src = concat!(
            "//@pine_lite version=1 overlay=false title=\"My RSI\"\n",
            "len = input.int(defval=14, title=\"Length\")\n",
            "r = ta.rsi(close, len)\n",
            "plot(r, title=\"RSI\")\n",
        );
        let response = vet_response(src);
        assert!(response.valid, "{:?}", response.issues);
        assert_eq!(response.header.as_ref().expect("header").title.as_deref(), Some("My RSI"));
        assert_eq!(response.inputs.len(), 1);
        assert_eq!(response.inputs[0].name, "len");
        assert_eq!(response.inputs[0].default, Some(serde_json::json!(14.0)));
        assert_eq!(response.overlay, Some(false));
    }

    #[test]
    fn input_defaults_are_typed_the_way_the_settings_ui_sends_them() {
        // docs/25: the settings popover prefills from `default` and sends the
        // same JSON type back, so a bool default must arrive as a bool (not
        // 1.0), a string as a string, and a color literal as `#rrggbb` with
        // the packed alpha byte removed.
        let src = concat!(
            "//@pine_lite version=1 overlay=false title=\"typed\"\n",
            "n = input.int(defval=14)\n",
            "b = input.bool(defval=true)\n",
            "s = input.string(defval=\"L\")\n",
            "c = input.color(defval=#ff0000)\n",
            "plot(close)\n",
        );
        let response = vet_response(src);
        assert!(response.valid, "{:?}", response.issues);
        let by_name: std::collections::HashMap<_, _> = response
            .inputs
            .iter()
            .map(|i| (i.name.as_str(), &i.default))
            .collect();
        assert_eq!(by_name["n"], &Some(serde_json::json!(14.0)));
        assert_eq!(by_name["b"], &Some(serde_json::json!(true)));
        assert_eq!(by_name["s"], &Some(serde_json::json!("L")));
        assert_eq!(by_name["c"], &Some(serde_json::json!("#ff0000")));
    }

    #[test]
    fn a_broken_script_lists_every_issue() {
        let src = concat!(
            "//@pine_lite version=1\n",
            "a = first_unknown\n",
            "b = second_unknown\n",
        );
        let response = vet_response(src);
        assert!(!response.valid);
        assert!(response.issues.len() >= 2, "{:?}", response.issues);
        assert!(response.header.is_none());
    }

    #[test]
    fn a_source_without_the_annotation_is_refused() {
        let response = vet_response("plot(close)\n");
        assert!(!response.valid);
        assert!(response.issues.iter().any(|i| i.message.contains("@pine_lite")));
    }

    #[test]
    fn a_sec_header_script_vets_clean() {
        // A multi-symbol script vets with its OWN header: the stale default-
        // header recheck used to refuse every honest `request.*` script here.
        let src = concat!(
            "//@pine_lite version=1 overlay=false title=\"SMT\" sec=\"ETHUSDT\"\n",
            "rc = request.close()\n",
            "spread = close - rc\n",
            "plot(spread)\n",
        );
        let response = vet_response(src);
        assert!(response.valid, "{:?}", response.issues);
    }
}
