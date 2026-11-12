//! Compiler-visible builtin metadata.
//!
//! This registry is the source of truth for vet-time call arity and static
//! result shape. Detailed signatures and host requirements are marked
//! complete per entry; the interpreter remains the execution authority.

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
#[serde(rename_all = "snake_case")]
pub enum HostData {
    Unspecified,
    None,
    Ohlcv,
    SymbolMetadata,
    ExternalSeries,
    PlatformFeed,
    StrategyState,
    ChartRendering,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeSupport {
    Unspecified,
    Implemented,
    VetOnly,
    HostDependent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ParameterType {
    Number,
    Integer,
    Boolean,
    String,
    Color,
    Series,
    Timeframe,
    Symbol,
    Any,
    Enum,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct BuiltinParameter {
    pub name: &'static str,
    pub value_type: ParameterType,
    pub optional: bool,
    pub default_value: Option<&'static str>,
}

const fn param(
    name: &'static str,
    value_type: ParameterType,
    optional: bool,
    default_value: Option<&'static str>,
) -> BuiltinParameter {
    BuiltinParameter {
        name,
        value_type,
        optional,
        default_value,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct BuiltinSpec {
    pub names: &'static [&'static str],
    pub min_args: usize,
    pub max_args: usize,
    pub result: BuiltinReturn,
    pub multi_output_count: Option<usize>,
    pub multi_output_only: bool,
    pub parameters: &'static [BuiltinParameter],
    pub host_data: HostData,
    pub runtime_support: RuntimeSupport,
    pub metadata_complete: bool,
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
        parameters: &[],
        host_data: HostData::Unspecified,
        runtime_support: RuntimeSupport::Unspecified,
        metadata_complete: false,
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
        parameters: &[],
        host_data: HostData::Unspecified,
        runtime_support: RuntimeSupport::Unspecified,
        metadata_complete: false,
    }
}

const fn metadata(
    mut entry: BuiltinSpec,
    parameters: &'static [BuiltinParameter],
    host_data: HostData,
    runtime_support: RuntimeSupport,
) -> BuiltinSpec {
    entry.parameters = parameters;
    entry.host_data = host_data;
    entry.runtime_support = runtime_support;
    entry.metadata_complete = true;
    entry
}

const SERIES_LENGTH: &[BuiltinParameter] = &[
    param("source", ParameterType::Series, false, None),
    param("length", ParameterType::Integer, false, None),
];
const ATR_ARGS: &[BuiltinParameter] = &[param("length", ParameterType::Integer, false, None)];
const SECURITY_ARGS: &[BuiltinParameter] = &[
    param("symbol", ParameterType::Symbol, false, None),
    param("timeframe", ParameterType::Timeframe, false, None),
    param("expression", ParameterType::Any, false, None),
    param("gaps", ParameterType::Enum, true, Some("barmerge.gaps_off")),
    param(
        "lookahead",
        ParameterType::Enum,
        true,
        Some("barmerge.lookahead_off"),
    ),
];
const LINE_ARGS: &[BuiltinParameter] = &[
    param("bar1", ParameterType::Number, false, None),
    param("price1", ParameterType::Number, false, None),
    param("bar2", ParameterType::Number, false, None),
    param("price2", ParameterType::Number, false, None),
    param("color", ParameterType::Color, true, Some("0xFF94A3B8")),
    param("style", ParameterType::Enum, true, Some("solid")),
    param("width", ParameterType::Number, true, Some("1")),
    param("extend", ParameterType::Enum, true, Some("none")),
];
const TIME_LINE_ARGS: &[BuiltinParameter] = &[
    param("time1", ParameterType::Number, false, None),
    param("price1", ParameterType::Number, false, None),
    param("time2", ParameterType::Number, false, None),
    param("price2", ParameterType::Number, false, None),
    param("color", ParameterType::Color, true, Some("0xFF94A3B8")),
    param("style", ParameterType::Enum, true, Some("solid")),
    param("width", ParameterType::Number, true, Some("1")),
    param("extend", ParameterType::Enum, true, Some("none")),
];
const BOX_ARGS: &[BuiltinParameter] = &[
    param("left", ParameterType::Number, false, None),
    param("top", ParameterType::Number, false, None),
    param("right", ParameterType::Number, false, None),
    param("bottom", ParameterType::Number, false, None),
    param("color", ParameterType::Color, true, Some("0x3394A3B8")),
    param("border_color", ParameterType::Color, true, None),
    param("border_width", ParameterType::Number, true, Some("1")),
    param("border_style", ParameterType::Enum, true, Some("solid")),
];
const TIME_BOX_ARGS: &[BuiltinParameter] = &[
    param("left_time", ParameterType::Number, false, None),
    param("top", ParameterType::Number, false, None),
    param("right_time", ParameterType::Number, false, None),
    param("bottom", ParameterType::Number, false, None),
    param("color", ParameterType::Color, true, Some("0x3394A3B8")),
    param("border_color", ParameterType::Color, true, None),
    param("border_width", ParameterType::Number, true, Some("1")),
    param("border_style", ParameterType::Enum, true, Some("solid")),
];
const BAR_LABEL_ARGS: &[BuiltinParameter] = &[
    param("bar", ParameterType::Number, false, None),
    param("price", ParameterType::Number, false, None),
    param("text", ParameterType::String, false, None),
    param("color", ParameterType::Color, true, Some("0xFF94A3B8")),
];
const TIME_LABEL_ARGS: &[BuiltinParameter] = &[
    param("time", ParameterType::Number, false, None),
    param("price", ParameterType::Number, false, None),
    param("text", ParameterType::String, false, None),
    param("color", ParameterType::Color, true, Some("0xFF94A3B8")),
];

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
    metadata(
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
        SERIES_LENGTH,
        HostData::Ohlcv,
        RuntimeSupport::Implemented,
    ),
    metadata(
        spec(&["ta.atr"], 1, 1, BuiltinReturn::Number),
        ATR_ARGS,
        HostData::Ohlcv,
        RuntimeSupport::Implemented,
    ),
    metadata(
        spec(&["ta.change"], 1, 1, BuiltinReturn::Number),
        &[param("source", ParameterType::Series, false, None)],
        HostData::Ohlcv,
        RuntimeSupport::Implemented,
    ),
    metadata(
        spec(&["ta.tr", "ta.vwap"], 0, 0, BuiltinReturn::Number),
        &[],
        HostData::Ohlcv,
        RuntimeSupport::Implemented,
    ),
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
    metadata(
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
        &[],
        HostData::ExternalSeries,
        RuntimeSupport::HostDependent,
    ),
    metadata(
        spec(&["request.security"], 3, 5, BuiltinReturn::Number),
        SECURITY_ARGS,
        HostData::ExternalSeries,
        RuntimeSupport::HostDependent,
    ),
    metadata(
        spec(&["line.new"], 4, 8, BuiltinReturn::Number),
        LINE_ARGS,
        HostData::ChartRendering,
        RuntimeSupport::Implemented,
    ),
    metadata(
        spec(&["line.new_time"], 4, 8, BuiltinReturn::Number),
        TIME_LINE_ARGS,
        HostData::ChartRendering,
        RuntimeSupport::Implemented,
    ),
    metadata(
        spec(&["label.new"], 3, 4, BuiltinReturn::Number),
        BAR_LABEL_ARGS,
        HostData::ChartRendering,
        RuntimeSupport::Implemented,
    ),
    metadata(
        spec(&["label.new_time"], 3, 4, BuiltinReturn::Number),
        TIME_LABEL_ARGS,
        HostData::ChartRendering,
        RuntimeSupport::Implemented,
    ),
    metadata(
        spec(&["box.new"], 4, 8, BuiltinReturn::Number),
        BOX_ARGS,
        HostData::ChartRendering,
        RuntimeSupport::Implemented,
    ),
    metadata(
        spec(&["box.new_time"], 4, 8, BuiltinReturn::Number),
        TIME_BOX_ARGS,
        HostData::ChartRendering,
        RuntimeSupport::Implemented,
    ),
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
    metadata(
        spec(&["request.data"], 1, 1, BuiltinReturn::Number),
        &[param("name", ParameterType::String, false, None)],
        HostData::PlatformFeed,
        RuntimeSupport::HostDependent,
    ),
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
    schema_version: 2,
    builtins: SPECS,
};

#[must_use]
pub fn lookup(name: &str) -> Option<&'static BuiltinSpec> {
    SPECS.iter().find(|entry| entry.names.contains(&name))
}

#[must_use]
pub fn prompt_reference() -> String {
    let mut reference = String::from(
        "Compiler-registered call bounds, catalog schema v2 (count includes named arguments); this list is authoritative for vet-time arity and static result shape. Parameter and host metadata are authoritative only where marked complete; unspecified entries must be checked against the language reference:\n",
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
        if entry.metadata_complete {
            let _ = write!(
                reference,
                "; parameters: {}",
                entry
                    .parameters
                    .iter()
                    .map(|parameter| {
                        let optional = if parameter.optional {
                            parameter.default_value.map_or_else(
                                || " optional".to_string(),
                                |value| format!(" = {value}"),
                            )
                        } else {
                            String::new()
                        };
                        format!(
                            "{}: {}{}",
                            parameter.name,
                            serde_json::to_value(parameter.value_type)
                                .expect("enum serializes")
                                .as_str()
                                .expect("snake-case enum"),
                            optional
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            let _ = write!(
                reference,
                "; host data `{}`; runtime `{}`",
                serde_json::to_value(entry.host_data)
                    .expect("enum serializes")
                    .as_str()
                    .expect("snake-case enum"),
                serde_json::to_value(entry.runtime_support)
                    .expect("enum serializes")
                    .as_str()
                    .expect("snake-case enum"),
            );
        } else {
            reference.push_str("; parameter/host metadata incomplete");
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
        assert_eq!(value["schema_version"], 2);
        assert!(!value["builtins"].as_array().unwrap().is_empty());
        assert_eq!(value["builtins"][0]["metadata_complete"], false);
    }

    #[test]
    fn catalog_exposes_complete_metadata_for_mtf_and_time_drawings() {
        let security = lookup("request.security").expect("registered security call");
        assert!(security.metadata_complete);
        assert_eq!(
            security
                .parameters
                .iter()
                .map(|parameter| parameter.name)
                .collect::<Vec<_>>(),
            ["symbol", "timeframe", "expression", "gaps", "lookahead"]
        );
        assert_eq!(security.host_data, HostData::ExternalSeries);
        assert_eq!(security.runtime_support, RuntimeSupport::HostDependent);

        let box_time = lookup("box.new_time").expect("registered time-anchored box");
        assert!(box_time.metadata_complete);
        assert_eq!(box_time.host_data, HostData::ChartRendering);
        assert_eq!(
            box_time.parameters.len(),
            box_time.max_args,
            "box constructor metadata includes its named optional drawing arguments"
        );
        assert_eq!(box_time.parameters[0].name, "left_time");
        assert_eq!(
            lookup("label.new").unwrap().parameters[0].name,
            "bar",
            "the bar-index label and absolute-time label must not be interchanged"
        );
        assert_eq!(lookup("label.new_time").unwrap().parameters[0].name, "time");
        assert!(SPECS
            .iter()
            .filter(|entry| entry.metadata_complete)
            .all(|entry| entry.parameters.len() == entry.max_args));
    }

    #[test]
    fn documented_named_arguments_for_registered_mtf_and_drawing_calls_vet() {
        let source = concat!(
            "//@pine_lite version=1 overlay=true\n",
            "htf = request.security(\"BTCUSDT\", \"4h\", close, gaps=barmerge.gaps_off, lookahead=barmerge.lookahead_off)\n",
            "line.new_time(time, htf, time + 1000000000.0, htf, color=color.teal, style=\"solid\", width=2, extend=extend.right)\n",
            "box.new_time(time, high, time + 1000000000.0, low, color=color.teal, border_width=2, border_style=\"dashed\")\n",
            "label.new_time(time, high, \"HTF\", color=color.teal)\n",
        );
        crate::vet(source).expect("registered call metadata and runtime support must agree");
    }
}
