//! Compiler-visible builtin metadata.
//!
//! This registry is the source of truth for vet-time call arity and static
//! result shape. The interpreter still owns execution.

use std::fmt::Write as _;

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BuiltinReturn {
    Number,
    Bool,
    String,
    Color,
    InputKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct BuiltinSpec {
    pub names: &'static [&'static str],
    pub min_args: usize,
    pub max_args: usize,
    pub result: BuiltinReturn,
    pub multi_output_count: Option<usize>,
    pub multi_output_only: bool,
}

const fn spec(
    names: &'static [&'static str],
    min_args: usize,
    max_args: usize,
    result: BuiltinReturn,
) -> BuiltinSpec {
    BuiltinSpec {
        names,
        min_args,
        max_args,
        result,
        multi_output_count: None,
        multi_output_only: false,
    }
}

const fn multi(
    names: &'static [&'static str],
    min_args: usize,
    max_args: usize,
    count: usize,
    multi_output_only: bool,
) -> BuiltinSpec {
    BuiltinSpec {
        names,
        min_args,
        max_args,
        result: BuiltinReturn::Number,
        multi_output_count: Some(count),
        multi_output_only,
    }
}

pub const SPECS: &[BuiltinSpec] = &[
    spec(&["na", "nz"], 1, 2, BuiltinReturn::Number),
    spec(&["fixnan"], 1, 1, BuiltinReturn::Number),
    spec(
        &["hour", "minute", "dayofweek", "year", "month", "dayofmonth"],
        1,
        1,
        BuiltinReturn::Number,
    ),
    spec(&["time"], 1, 1, BuiltinReturn::Number),
    spec(&["timestamp"], 5, 5, BuiltinReturn::Number),
    spec(
        &[
            "ta.sma",
            "ta.ema",
            "ta.rma",
            "ta.wma",
            "ta.rsi",
            "ta.highest",
            "ta.lowest",
            "ta.mom",
            "ta.roc",
            "ta.stdev",
            "ta.variance",
            "ta.hma",
            "ta.vwma",
            "ta.tema",
            "ta.deviation",
            "ta.median",
            "ta.mode",
            "ta.percentrank",
            "ta.range",
        ],
        2,
        2,
        BuiltinReturn::Number,
    ),
    spec(&["ta.atr", "ta.change"], 1, 1, BuiltinReturn::Number),
    spec(&["ta.tr", "ta.vwap"], 0, 0, BuiltinReturn::Number),
    multi(&["ta.stoch"], 2, 2, 2, false),
    multi(&["ta.macd"], 3, 4, 3, true),
    multi(&["ta.bb"], 2, 3, 3, true),
    spec(
        &[
            "ta.percentile_linear_interpolation",
            "ta.percentile_nearest_rank",
            "ta.correlation",
            "ta.covariance",
            "ta.lsma",
            "ta.linreg",
        ],
        3,
        3,
        BuiltinReturn::Number,
    ),
    spec(
        &["ta.obv", "ta.pvt", "ta.nvi", "ta.wad", "ta.ad"],
        0,
        0,
        BuiltinReturn::Number,
    ),
    spec(&["ta.swma"], 1, 1, BuiltinReturn::Number),
    spec(&["ta.alma"], 2, 4, BuiltinReturn::Number),
    spec(
        &[
            "ta.cci",
            "ta.mfi",
            "ta.willr",
            "ta.highestbars",
            "ta.lowestbars",
            "ta.barssince",
            "ta.cum",
        ],
        1,
        1,
        BuiltinReturn::Number,
    ),
    spec(
        &["ta.valuewhen", "ta.pivothigh", "ta.pivotlow"],
        3,
        3,
        BuiltinReturn::Number,
    ),
    multi(&["ta.dmi"], 1, 1, 3, true),
    multi(&["ta.kc"], 2, 2, 3, true),
    multi(&["ta.supertrend"], 2, 2, 2, true),
    spec(
        &["ta.crossover", "ta.crossunder", "ta.cross"],
        2,
        2,
        BuiltinReturn::Bool,
    ),
    spec(&["ta.rising", "ta.falling"], 2, 2, BuiltinReturn::Bool),
    spec(&["str.tostring"], 1, 2, BuiltinReturn::String),
    spec(&["str.format"], 1, 9, BuiltinReturn::String),
    spec(
        &[
            "math.round_to_mintick",
            "math.abs",
            "math.floor",
            "math.ceil",
            "math.round",
            "math.sqrt",
            "math.log",
            "math.exp",
            "math.sign",
            "math.sin",
            "math.cos",
            "math.tan",
            "math.asin",
            "math.acos",
            "math.atan",
            "math.toradians",
            "math.todegrees",
        ],
        1,
        1,
        BuiltinReturn::Number,
    ),
    spec(&["math.atan2"], 2, 2, BuiltinReturn::Number),
    spec(&["timeframe.change"], 1, 1, BuiltinReturn::Bool),
    spec(&["alert"], 1, 2, BuiltinReturn::Number),
    spec(
        &["math.min", "math.max", "math.avg"],
        1,
        8,
        BuiltinReturn::Number,
    ),
    spec(&["math.sum", "math.pow"], 2, 2, BuiltinReturn::Number),
    spec(&["array.new", "map.new"], 0, 0, BuiltinReturn::Number),
    spec(
        &["array.get", "array.includes"],
        2,
        2,
        BuiltinReturn::Number,
    ),
    spec(
        &[
            "array.size",
            "array.first",
            "array.last",
            "array.min",
            "array.max",
            "array.avg",
            "map.size",
            "map.clear",
            "array.pop",
            "array.shift",
            "array.clear",
        ],
        1,
        1,
        BuiltinReturn::Number,
    ),
    spec(&["matrix.new"], 2, 3, BuiltinReturn::Number),
    spec(&["matrix.get"], 3, 3, BuiltinReturn::Number),
    spec(&["matrix.set"], 4, 4, BuiltinReturn::Number),
    spec(
        &["matrix.rows", "matrix.columns"],
        1,
        1,
        BuiltinReturn::Number,
    ),
    spec(&["matrix.fill"], 2, 2, BuiltinReturn::Number),
    spec(&["map.put"], 3, 3, BuiltinReturn::Number),
    spec(&["map.get", "map.remove"], 2, 2, BuiltinReturn::Number),
    spec(&["map.contains"], 2, 2, BuiltinReturn::Bool),
    spec(&["array.new_string"], 0, 0, BuiltinReturn::Number),
    spec(&["array.from"], 0, 8, BuiltinReturn::Number),
    spec(&["array.slice"], 3, 3, BuiltinReturn::Number),
    spec(&["array.join"], 1, 2, BuiltinReturn::String),
    spec(
        &["array.indexof", "array.concat"],
        2,
        2,
        BuiltinReturn::Number,
    ),
    spec(&["array.fill"], 2, 4, BuiltinReturn::Number),
    spec(&["array.push"], 2, 2, BuiltinReturn::Number),
    spec(&["array.set"], 3, 3, BuiltinReturn::Number),
    spec(
        &[
            "request.symbol",
            "request.open",
            "request.high",
            "request.low",
            "request.close",
            "request.volume",
            "request.time",
        ],
        0,
        0,
        BuiltinReturn::Number,
    ),
    spec(&["request.security"], 3, 3, BuiltinReturn::Number),
    spec(&["line.new", "line.new_time"], 4, 7, BuiltinReturn::Number),
    spec(
        &["label.new", "label.new_time"],
        3,
        4,
        BuiltinReturn::Number,
    ),
    spec(&["box.new", "box.new_time"], 4, 8, BuiltinReturn::Number),
    spec(
        &[
            "line.set_x1",
            "line.set_x2",
            "line.set_y1",
            "line.set_y2",
            "line.set_color",
            "line.set_extend",
            "box.set_left",
            "box.set_right",
            "box.set_top",
            "box.set_bottom",
            "box.set_border_color",
            "box.set_border_width",
            "box.set_bgcolor",
            "label.set_x",
            "label.set_y",
            "label.set_text",
            "label.set_color",
        ],
        2,
        2,
        BuiltinReturn::Number,
    ),
    spec(
        &["line.delete", "label.delete", "box.delete"],
        1,
        1,
        BuiltinReturn::Number,
    ),
    spec(&["linefill.new"], 3, 3, BuiltinReturn::Number),
    spec(
        &["linefill.delete", "polyline.delete"],
        1,
        1,
        BuiltinReturn::Number,
    ),
    spec(&["polyline.new"], 1, 7, BuiltinReturn::Number),
    spec(&["chart.point.new"], 2, 2, BuiltinReturn::Number),
    spec(&["table.new"], 3, 5, BuiltinReturn::Number),
    spec(&["table.cell"], 4, 6, BuiltinReturn::Number),
    spec(&["color.new"], 2, 2, BuiltinReturn::Color),
    spec(&["color.rgb"], 3, 4, BuiltinReturn::Color),
    spec(&["color.from_gradient"], 4, 5, BuiltinReturn::Color),
    spec(&["fib.new"], 4, 5, BuiltinReturn::Number),
    spec(&["request.data"], 1, 1, BuiltinReturn::Number),
    spec(
        &["plot", "plotshape", "plotchar", "plotarrow"],
        1,
        8,
        BuiltinReturn::Number,
    ),
    spec(&["plotcandle", "plotbar"], 4, 8, BuiltinReturn::Number),
    spec(&["hline"], 1, 4, BuiltinReturn::Number),
    spec(&["fill"], 2, 6, BuiltinReturn::Number),
    spec(&["bgcolor", "barcolor"], 1, 4, BuiltinReturn::Number),
    spec(
        &[
            "input.int",
            "input.float",
            "input.bool",
            "input.string",
            "input.color",
            "input.source",
            "input.timeframe",
            "input.time",
            "input.price",
            "input.symbol",
        ],
        0,
        6,
        BuiltinReturn::InputKind,
    ),
    spec(
        &[
            "strategy.entry",
            "strategy.exit",
            "strategy.close",
            "strategy.cancel",
        ],
        1,
        9,
        BuiltinReturn::Number,
    ),
    spec(
        &[
            "strategy.risk.max_position_size",
            "strategy.risk.max_intraday_loss",
            "strategy.risk.max_drawdown",
            "strategy.risk.allow_entry_in",
        ],
        1,
        1,
        BuiltinReturn::Number,
    ),
    spec(&["strategy.close_all"], 0, 6, BuiltinReturn::Number),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct BuiltinCatalog {
    pub schema_version: u32,
    pub builtins: &'static [BuiltinSpec],
}

pub const CATALOG: BuiltinCatalog = BuiltinCatalog {
    schema_version: 1,
    builtins: SPECS,
};

#[must_use]
pub fn lookup(name: &str) -> Option<&'static BuiltinSpec> {
    SPECS.iter().find(|entry| entry.names.contains(&name))
}

#[must_use]
pub fn prompt_reference() -> String {
    let mut reference = String::from(
        "Compiler-registered call bounds, catalog schema v1 (count includes named arguments); this list is authoritative for vet-time arity and static result shape:\n",
    );
    for entry in SPECS {
        let names = entry.names.join(", ");
        let bounds = if entry.min_args == entry.max_args {
            entry.min_args.to_string()
        } else {
            format!("{}..={}", entry.min_args, entry.max_args)
        };
        let _ = write!(
            reference,
            "- `{names}`: {bounds} argument(s); result `{}`",
            match entry.result {
                BuiltinReturn::Number => "number",
                BuiltinReturn::Bool => "bool",
                BuiltinReturn::String => "string",
                BuiltinReturn::Color => "color",
                BuiltinReturn::InputKind => "input-dependent",
            }
        );
        if let Some(count) = entry.multi_output_count {
            let _ = write!(reference, "; destructures to {count} values");
            if entry.multi_output_only {
                reference.push_str("; cannot be read as one value");
            } else {
                reference.push_str("; scalar use is also supported");
            }
        }
        reference.push('\n');
    }
    reference
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn builtin_names_are_unique_and_have_valid_arity() {
        let mut names = HashSet::new();
        for entry in SPECS {
            assert!(entry.min_args <= entry.max_args);
            for name in entry.names {
                assert!(names.insert(name), "duplicate builtin `{name}`");
                assert_eq!(lookup(name).map(|found| found.names), Some(entry.names));
            }
        }
    }

    #[test]
    fn multi_output_metadata_matches_existing_pine_forms() {
        assert_eq!(lookup("ta.macd").unwrap().multi_output_count, Some(3));
        assert!(lookup("ta.macd").unwrap().multi_output_only);
        assert_eq!(lookup("ta.stoch").unwrap().multi_output_count, Some(2));
        assert!(!lookup("ta.stoch").unwrap().multi_output_only);
        assert_eq!(lookup("ta.dmi").unwrap().min_args, 1);
        assert_eq!(lookup("ta.dmi").unwrap().max_args, 1);
    }

    #[test]
    fn catalog_serializes_with_its_schema_version() {
        let value = serde_json::to_value(CATALOG).expect("catalog serializes");
        assert_eq!(value["schema_version"], 1);
        assert!(!value["builtins"].as_array().unwrap().is_empty());
    }
}
