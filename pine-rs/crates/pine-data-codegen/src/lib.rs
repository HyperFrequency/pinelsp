//! `pine-data-codegen` — the Pine v6 builtins database, embedded at compile time.
//!
//! The JSON under `data/` is emitted from the canonical TypeScript `pine-data`
//! modules by `scripts/dump-pine-data.mjs` (re-run after the pipeline
//! regenerates pine-data). We embed it with `include_str!` and deserialize once
//! behind `LazyLock`. At ~480 KB / 457 functions the one-time parse is
//! sub-millisecond, so this is favored over compile-time `phf` maps for
//! simplicity and debuggability; `phf` remains a possible optimization if
//! startup ever shows up in a profile.

use serde::Deserialize;
use std::collections::HashMap;
use std::sync::LazyLock;

/// A single parameter of a builtin function. `ty` carries Pine type strings such
/// as `"series float"` or `"series int/float"`; parsing those into a structured
/// type happens later in `pine-core`.
#[derive(Debug, Clone, Deserialize)]
pub struct Param {
    pub name: String,
    #[serde(rename = "type", default)]
    pub ty: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub default: Option<String>,
}

/// Behavior flags that drive checker rules (top-level-only enforcement,
/// variadics, polymorphism). Mirrors the TS `FunctionFlags` (camelCase in JSON).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FunctionFlags {
    #[serde(default)]
    pub top_level_only: bool,
    #[serde(default)]
    pub series_returning: bool,
    #[serde(default)]
    pub variadic: bool,
    #[serde(default)]
    pub min_args: Option<u32>,
    #[serde(default)]
    pub max_args: Option<u32>,
    pub polymorphic: Option<String>,
}

/// A builtin function. `name` is the full, possibly dotted name (e.g.
/// `"ta.sma"`, `"request.security"`); `namespace` is the leading segment when
/// present (`"ta"`, `"request"`).
#[derive(Debug, Clone, Deserialize)]
pub struct BuiltinFunction {
    pub name: String,
    #[serde(default)]
    pub namespace: Option<String>,
    #[serde(default)]
    pub syntax: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub parameters: Vec<Param>,
    #[serde(default)]
    pub returns: String,
    pub flags: Option<FunctionFlags>,
    pub deprecated: Option<String>,
    pub since: Option<String>,
    pub example: Option<String>,
}

/// A builtin variable (e.g. `close`, `bar_index`, `syminfo.tickerid`).
#[derive(Debug, Clone, Deserialize)]
pub struct BuiltinVariable {
    pub name: String,
    #[serde(default)]
    pub namespace: Option<String>,
    #[serde(rename = "type", default)]
    pub ty: String,
    #[serde(default)]
    pub qualifier: String,
    #[serde(default)]
    pub description: String,
    pub since: Option<String>,
}

/// A builtin constant (e.g. `color.red`, `plot.style_line`).
#[derive(Debug, Clone, Deserialize)]
pub struct BuiltinConstant {
    pub name: String,
    #[serde(default)]
    pub namespace: String,
    #[serde(rename = "shortName", default)]
    pub short_name: String,
    #[serde(rename = "type", default)]
    pub ty: String,
    pub description: Option<String>,
}

/// Reserved words, kept categorized as in the TS source.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Keywords {
    pub all: Vec<String>,
    pub control: Vec<String>,
    pub declaration: Vec<String>,
    pub operator: Vec<String>,
    pub literal: Vec<String>,
    #[serde(rename = "type")]
    pub type_: Vec<String>,
}

const FUNCTIONS_JSON: &str = include_str!("../data/functions.json");
const VARIABLES_JSON: &str = include_str!("../data/variables.json");
const CONSTANTS_JSON: &str = include_str!("../data/constants.json");
const KEYWORDS_JSON: &str = include_str!("../data/keywords.json");

pub static FUNCTIONS: LazyLock<Vec<BuiltinFunction>> =
    LazyLock::new(|| serde_json::from_str(FUNCTIONS_JSON).expect("parse functions.json"));
pub static VARIABLES: LazyLock<Vec<BuiltinVariable>> =
    LazyLock::new(|| serde_json::from_str(VARIABLES_JSON).expect("parse variables.json"));
pub static CONSTANTS: LazyLock<Vec<BuiltinConstant>> =
    LazyLock::new(|| serde_json::from_str(CONSTANTS_JSON).expect("parse constants.json"));
pub static KEYWORDS: LazyLock<Keywords> =
    LazyLock::new(|| serde_json::from_str(KEYWORDS_JSON).expect("parse keywords.json"));

static FUNCTIONS_BY_NAME: LazyLock<HashMap<&'static str, &'static BuiltinFunction>> =
    LazyLock::new(|| FUNCTIONS.iter().map(|f| (f.name.as_str(), f)).collect());
static VARIABLES_BY_NAME: LazyLock<HashMap<&'static str, &'static BuiltinVariable>> =
    LazyLock::new(|| VARIABLES.iter().map(|v| (v.name.as_str(), v)).collect());
static CONSTANTS_BY_NAME: LazyLock<HashMap<&'static str, &'static BuiltinConstant>> =
    LazyLock::new(|| CONSTANTS.iter().map(|c| (c.name.as_str(), c)).collect());

/// Look up a builtin function by full name, e.g. `"ta.sma"`.
pub fn function(name: &str) -> Option<&'static BuiltinFunction> {
    FUNCTIONS_BY_NAME.get(name).copied()
}

/// Look up a builtin variable by full name, e.g. `"close"` or `"syminfo.tickerid"`.
pub fn variable(name: &str) -> Option<&'static BuiltinVariable> {
    VARIABLES_BY_NAME.get(name).copied()
}

/// Look up a builtin constant by full name, e.g. `"color.red"`.
pub fn constant(name: &str) -> Option<&'static BuiltinConstant> {
    CONSTANTS_BY_NAME.get(name).copied()
}

/// True if `name` is a Pine reserved word.
pub fn is_keyword(name: &str) -> bool {
    KEYWORDS.all.iter().any(|k| k == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_match_source() {
        assert_eq!(FUNCTIONS.len(), 457, "function count");
        assert_eq!(VARIABLES.len(), 90, "variable count");
        assert_eq!(CONSTANTS.len(), 237, "constant count");
        assert_eq!(KEYWORDS.all.len(), 28, "keyword count");
    }

    #[test]
    fn name_indexes_are_complete() {
        assert_eq!(
            FUNCTIONS_BY_NAME.len(),
            FUNCTIONS.len(),
            "no duplicate fn names"
        );
        assert_eq!(
            VARIABLES_BY_NAME.len(),
            VARIABLES.len(),
            "no duplicate var names"
        );
        assert_eq!(
            CONSTANTS_BY_NAME.len(),
            CONSTANTS.len(),
            "no duplicate const names"
        );
    }

    #[test]
    fn spot_check_ta_sma() {
        let f = function("ta.sma").expect("ta.sma present");
        assert_eq!(f.namespace.as_deref(), Some("ta"));
        assert!(!f.parameters.is_empty(), "ta.sma should have parameters");
    }

    #[test]
    fn request_security_has_gaps_and_lookahead() {
        let f = function("request.security").expect("request.security present");
        let params: Vec<&str> = f.parameters.iter().map(|p| p.name.as_str()).collect();
        assert!(params.contains(&"gaps"), "params: {params:?}");
        assert!(params.contains(&"lookahead"), "params: {params:?}");
    }

    #[test]
    fn builtin_variable_close_present() {
        assert!(
            variable("close").is_some(),
            "close should be a builtin variable"
        );
    }

    #[test]
    fn keywords_categorized() {
        assert!(is_keyword("if"));
        assert!(!KEYWORDS.declaration.is_empty());
    }
}
