//! OTEL / UCUM unit handling.
//!
//! OTEL's instrument-unit spec says: "Units should follow the Unified
//! Code for Units of Measure" (UCUM). So every string we might get
//! out of `MetricInfo.unit`, the `otel.metric.unit` series tag, or a
//! `// @unit <expr>` pragma is a UCUM expression.
//!
//! We use [`octofhir_ucum`] to validate the syntax, classify time
//! and frequency units (where dimensional analysis is meaningful),
//! and compute conversion factors for those families. For
//! bytes/bits/percent/annotations we classify textually — UCUM
//! treats them all as dimensionless, so `is_comparable` collapses
//! them into one bucket and can't distinguish bytes from bits from
//! percent from `{request}`. UCUM's registry also has gaps on
//! binary-prefixed bytes (`MiBy`/`GiBy`/`Tibit`/...) so we hard-code
//! the prefix factors there too: that's a tiny IEC table, not
//! "understanding UCUM's logic".
//!
//! Anything UCUM-valid but outside our known families is preserved
//! verbatim as a display suffix without scaling — better to show
//! `123 fortnights` than to drop the unit on the floor. ISO-4217
//! currency codes are a deliberate non-UCUM extension and are parsed
//! via the `iso_currency` crate.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// A validated UCUM unit expression plus its classification into a
/// display family. We keep the raw string for diagnostics and as the
/// fallback display suffix; classification drives the scale picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unit {
    raw: String,
    family: UnitFamily,
}

impl Unit {
    /// The original UCUM string the unit was parsed from. Only used
    /// by tests today — production code routes everything through
    /// [`scale_for`] which already returns the right display suffix.
    /// Keep available for future hover/inspect features.
    #[cfg(test)]
    pub(crate) fn raw(&self) -> &str {
        &self.raw
    }

    /// Classified family. Used by tests; the production scale-picker
    /// reads the field directly within this module.
    #[cfg(test)]
    pub(crate) fn family(&self) -> UnitFamily {
        self.family
    }
}

/// Coarse buckets the display logic understands. We deliberately
/// collapse some UCUM-distinct things (e.g. `Hz` and `1/s`) into one
/// bucket because they share a display story.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitFamily {
    /// Bytes with binary prefixes (Ki/Mi/Gi/...). Selected when the
    /// raw unit uses a binary prefix or the unprefixed `By` (Axiom-web
    /// convention defaults bare bytes to binary).
    BytesBinary,
    /// Bytes with decimal prefixes (k/M/G/...). Selected when the raw
    /// unit explicitly used a decimal prefix like `kBy`/`MBy`.
    BytesDecimal,
    /// Bits, binary-prefix family.
    BitsBinary,
    /// Bits, decimal-prefix family.
    BitsDecimal,
    /// Time/duration. Promotes ns→µs→ms→s→min→h→d by magnitude.
    Time,
    /// Frequency, including `1/s` and equivalents (counter rate).
    /// Promotes Hz→kHz→MHz→GHz→THz.
    Frequency,
    /// Percent. No scaling; the y-axis just gets a `%` suffix.
    Percent,
    /// Dimensionless count: `1`, or any annotation `{request}` etc.
    /// No scaling; suffix preserved verbatim.
    Dimensionless,
    /// Bytes per time (`By/s`, `MiBy/s`, ...). Numerator scales like
    /// bytes; denominator displayed as `/s` after normalising.
    BytesPerTime,
    /// Bits per time.
    BitsPerTime,
    /// Safe SI-prefixable engineering units where decimal scaling is
    /// both conventional and useful in dashboards (W, V, A, J, Pa,
    /// m, g, L, lx, lm, mol). This deliberately stays whitelist-based:
    /// UCUM contains offset/special units (`Cel`, `[degF]`, ...)
    /// where prefixes would be misleading.
    SiDecimal(SiBase),
    /// Mass concentration in grams per cubic metre (`ug/m3`,
    /// `µg/m3`, `mg/m3`, `g/m3`, `kg/m3`). Display uses the nicer
    /// superscript form (`µg/m³`) while parsing keeps UCUM syntax.
    MassConcentration,
    /// ISO-4217 currency extension (`EUR`, `USD`, `GBP`, ...). These
    /// are intentionally not UCUM; parsed and rendered via the
    /// `iso_currency` crate because dashboards commonly plot money.
    Currency(iso_currency::Currency),
    /// UCUM-valid but outside our families (e.g. `Cel`, custom
    /// engineering units). Rendered verbatim, no scaling.
    Other,
}

/// SI-prefixable base units we scale with decimal engineering
/// prefixes. The variants encode display semantics; parsing still
/// accepts UCUM strings such as `kW`, `mlx`, or `mmol`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SiBase {
    Watt,
    Volt,
    Ampere,
    Joule,
    Pascal,
    Metre,
    Gram,
    Litre,
    Lux,
    Lumen,
    Mole,
}

impl SiBase {
    fn ucum_base(self) -> &'static str {
        match self {
            Self::Watt => "W",
            Self::Volt => "V",
            Self::Ampere => "A",
            Self::Joule => "J",
            Self::Pascal => "Pa",
            Self::Metre => "m",
            Self::Gram => "g",
            Self::Litre => "L",
            Self::Lux => "lx",
            Self::Lumen => "lm",
            Self::Mole => "mol",
        }
    }

    fn display_base(self) -> &'static str {
        self.ucum_base()
    }
}

/// Result of picking a display prefix for a value range.
#[derive(Debug, Clone, PartialEq)]
pub struct Scaled {
    /// Prefix to prepend before the value. Empty for normal units;
    /// currency uses this for symbols like `€`/`$`/`£`.
    pub prefix: String,
    /// Multiply the raw value by this factor to get the displayed
    /// number (e.g. `1.0 / (1<<20)` to go from bytes to MiB).
    pub factor: f64,
    /// Suffix to append after the value, including a leading space
    /// for letter-style units (`s`, `MiB`) and no space for `%`.
    pub suffix: String,
}

impl Scaled {
    /// Identity — display the raw value with no suffix. Used when the
    /// caller has no unit at all.
    pub fn none() -> Self {
        Self {
            prefix: String::new(),
            factor: 1.0,
            suffix: String::new(),
        }
    }
}

/// Parse a unit expression. Empty / whitespace-only strings yield
/// `None`. ISO-4217 currency codes are accepted as a non-UCUM
/// extension; otherwise the string must validate as UCUM. Anything
/// unrecognised yields `None` — we never want a bad unit to take
/// down the chart; absence is the safe outcome.
pub fn parse(s: &str) -> Option<Unit> {
    let raw = normalize_unit_input(s.trim());
    if raw.is_empty() {
        return None;
    }
    if let Some(currency) = iso_currency::Currency::from_code(&raw) {
        return Some(Unit {
            raw,
            family: UnitFamily::Currency(currency),
        });
    }
    // Syntax check via the UCUM library. The library is permissive
    // on some malformed strings (e.g. `kggg` parses as `k.g.g.g`),
    // but for our purposes a permissive parse is fine — the unit
    // just lands in `Other` if classification can't make sense of
    // it.
    if octofhir_ucum::parse_expression(&raw).is_err() {
        return None;
    }
    let family = classify(&raw);
    Some(Unit { raw, family })
}

/// Classify a UCUM string into one of our display families.
///
/// UCUM's dimensional analysis (`is_comparable`) is only useful for
/// time and frequency in our scope. Bytes, bits, percent, and
/// annotations are all dimensionless in the UCUM model and so
/// indistinguishable via comparability. We classify those textually
/// by inspecting the prefix + base-unit suffix; this is the same
/// information the OTEL spec uses to define instrument units.
fn classify(raw: &str) -> UnitFamily {
    // Cheap textual special cases.
    if raw == "%" {
        return UnitFamily::Percent;
    }
    if raw == "1" || is_annotation_only(raw) {
        return UnitFamily::Dimensionless;
    }

    // Rate forms: `<num>/<denom>`. We only special-case denominators
    // that scale as time (`s`, `min`, `h`, `ms`, `us`, `ns`, `d`). For
    // numerator detection we reuse the byte/bit textual rules.
    if let Some((num, denom)) = raw.split_once('/')
        && denom_is_time(denom)
    {
        if let Some(byte_family) = bytes_family(num) {
            return match byte_family {
                UnitFamily::BytesBinary | UnitFamily::BytesDecimal => UnitFamily::BytesPerTime,
                _ => UnitFamily::Other,
            };
        }
        if let Some(bit_family) = bits_family(num) {
            return match bit_family {
                UnitFamily::BitsBinary | UnitFamily::BitsDecimal => UnitFamily::BitsPerTime,
                _ => UnitFamily::Other,
            };
        }
        // `1/s` and friends → frequency-shaped.
        if num == "1" || is_annotation_only(num) {
            return UnitFamily::Frequency;
        }
        // Fall through: unknown numerator over a time denominator.
    }

    if let Some(f) = bytes_family(raw) {
        return f;
    }
    if let Some(f) = bits_family(raw) {
        return f;
    }
    if mass_concentration_factor(raw).is_some() {
        return UnitFamily::MassConcentration;
    }
    if let Some(f) = si_family(raw) {
        return f;
    }

    // Time vs frequency by dimensional analysis. `is_comparable`
    // returns true for any unit dimensionally equal to the
    // representative; e.g. `min`/`h` compare equal to `s`. `Hz` is
    // dimensionally `1/s` so it lands in Frequency, not Time.
    if octofhir_ucum::is_comparable(raw, "s").unwrap_or(false) {
        return UnitFamily::Time;
    }
    if octofhir_ucum::is_comparable(raw, "Hz").unwrap_or(false) {
        return UnitFamily::Frequency;
    }

    UnitFamily::Other
}

/// `true` for UCUM strings that are nothing but a `{annotation}` token,
/// optionally with a leading `1`. Per the OTEL spec, these are
/// dimensionless counters whose annotation is preserved as a suffix.
fn is_annotation_only(raw: &str) -> bool {
    let trimmed = raw.trim();
    let body = trimmed.strip_prefix('1').unwrap_or(trimmed).trim();
    body.starts_with('{') && body.ends_with('}')
}

/// `true` when `denom` is a UCUM time unit suitable as the
/// denominator of a rate (`s`, `min`, `h`, `d`, `ms`, `us`, `ns`).
fn denom_is_time(denom: &str) -> bool {
    matches!(denom, "s" | "min" | "h" | "d" | "ms" | "us" | "µs" | "ns")
}

/// If `raw` is a byte unit (`By` family), return `BytesBinary` or
/// `BytesDecimal` based on its prefix. `None` if it isn't bytes at
/// all.
fn bytes_family(raw: &str) -> Option<UnitFamily> {
    let prefix = raw.strip_suffix("By")?;
    Some(if is_binary_prefix(prefix) {
        UnitFamily::BytesBinary
    } else {
        UnitFamily::BytesDecimal
    })
}

/// If `raw` is a bit unit (`bit` family), return `BitsBinary` or
/// `BitsDecimal` based on its prefix. `None` if it isn't bits at
/// all.
fn bits_family(raw: &str) -> Option<UnitFamily> {
    let prefix = raw.strip_suffix("bit")?;
    Some(if is_binary_prefix(prefix) {
        UnitFamily::BitsBinary
    } else {
        UnitFamily::BitsDecimal
    })
}

/// Classify a prefix string: empty (bare base unit) and any IEC
/// binary prefix (`Ki`/`Mi`/`Gi`/`Ti`/`Pi`/`Ei`) are binary; any
/// other non-empty prefix (`k`/`M`/`G`/`T`/`P`/`E`/`Z`/`Y`) is
/// decimal.
fn is_binary_prefix(prefix: &str) -> bool {
    if prefix.is_empty() {
        return true;
    }
    matches!(
        prefix,
        "Ki" | "Mi" | "Gi" | "Ti" | "Pi" | "Ei" | "Zi" | "Yi"
    )
}

/// Normalize friendly user input into UCUM-ish syntax before
/// validation. This is intentionally tiny and display-driven: users
/// type `µg/m³`, UCUM wants `µg/m3`; Greek mu `μ` is normalised to
/// micro sign `µ`, which the UCUM crate accepts.
fn normalize_unit_input(raw: &str) -> String {
    raw.replace('μ', "µ").replace('³', "3").replace('²', "2")
}

/// If `raw` is one of the SI-prefixable engineering units we support,
/// return its family. This is intentionally textual: it keeps offset
/// units like `Cel` out of the scaling path, and avoids depending on
/// UCUM canonicalisation for display semantics.
fn si_family(raw: &str) -> Option<UnitFamily> {
    SI_BASES
        .iter()
        .copied()
        .find(|base| si_prefix_factor(raw, *base).is_some())
        .map(UnitFamily::SiDecimal)
}

/// Factor from `raw` to the SI family's base unit. Examples:
/// `kW → 1000 W`, `mlx → 0.001 lx`, `kg → 1000 g`.
fn si_prefix_factor(raw: &str, base: SiBase) -> Option<f64> {
    let suffix = base.ucum_base();
    let prefix = raw.strip_suffix(suffix)?;
    SI_PREFIXES
        .iter()
        .find(|p| p.ucum == prefix)
        .map(|p| p.factor)
}

/// Factor from `raw` to the mass-concentration base `g/m3`.
fn mass_concentration_factor(raw: &str) -> Option<f64> {
    let (mass, volume) = raw.split_once('/')?;
    if volume != "m3" {
        return None;
    }
    si_prefix_factor(mass, SiBase::Gram)
}

/// Prefix for currency display. The library returns `¤` for codes
/// without a common symbol; in that case the ISO code is clearer in
/// a chart axis than the generic currency sign.
fn currency_symbol_prefix(currency: iso_currency::Currency) -> String {
    let symbol = currency.symbol().to_string();
    if symbol == "¤" {
        format!("{} ", currency.code())
    } else {
        symbol
    }
}

/// Pick a display scale for a value range under the given unit.
/// `None` for `unit` means "no unit known"; we return identity.
///
/// `range_lo` / `range_hi` are the y-axis bounds in the *raw* input
/// unit; we use `max(|lo|, |hi|)` to drive the prefix choice so a
/// chart that swings through zero still picks the prefix of the
/// bigger magnitude.
pub fn scale_for(unit: Option<&Unit>, range_lo: f64, range_hi: f64) -> Scaled {
    let Some(u) = unit else {
        return Scaled::none();
    };
    let mag_input = range_lo.abs().max(range_hi.abs());
    match u.family {
        UnitFamily::BytesBinary => pick_bytes_bits(u, BYTES_BINARY, mag_input),
        UnitFamily::BytesDecimal => pick_bytes_bits(u, BYTES_DECIMAL, mag_input),
        UnitFamily::BitsBinary => pick_bytes_bits(u, BITS_BINARY, mag_input),
        UnitFamily::BitsDecimal => pick_bytes_bits(u, BITS_DECIMAL, mag_input),
        UnitFamily::Time => pick_via_ucum(u, TIME, mag_input),
        UnitFamily::Frequency => pick_via_ucum(u, FREQUENCY, mag_input),
        UnitFamily::BytesPerTime => pick_rate(u, BYTES_RATE_TABLE, mag_input),
        UnitFamily::BitsPerTime => pick_rate(u, BITS_RATE_TABLE, mag_input),
        UnitFamily::SiDecimal(base) => pick_si_decimal(u, base, mag_input),
        UnitFamily::MassConcentration => pick_mass_concentration(u, mag_input),
        UnitFamily::Currency(currency) => Scaled {
            prefix: currency_symbol_prefix(currency),
            factor: 1.0,
            suffix: String::new(),
        },
        UnitFamily::Percent => Scaled {
            prefix: String::new(),
            factor: 1.0,
            suffix: "%".to_string(),
        },
        UnitFamily::Dimensionless | UnitFamily::Other => Scaled {
            prefix: String::new(),
            factor: 1.0,
            suffix: format!(" {}", u.raw),
        },
    }
}

/// Generic picker for time / frequency families: thresholds are in
/// the family's canonical base unit (s for time, Hz for frequency),
/// magnitudes come in via the input unit, so we first convert the
/// input magnitude to canonical, then pick the row, then ask UCUM
/// for the input→target factor.
fn pick_via_ucum(unit: &Unit, table: &[PrefixRow], mag_input: f64) -> Scaled {
    let to_canonical = canonical_factor(&unit.raw).unwrap_or(1.0);
    let mag_canon = mag_input * to_canonical;
    let row = pick_row(table, mag_canon);
    let factor = conversion_factor_via_canonical(&unit.raw, row.target).unwrap_or(1.0);
    Scaled {
        prefix: String::new(),
        factor,
        suffix: format!(" {}", row.display),
    }
}

/// Bytes/bits picker. UCUM's registry has gaps for binary prefixes
/// (`MiBy`/`GiBy`/`Tibit` come back canon=None) so we ignore UCUM
/// here and compute factors from the family's own prefix table — the
/// IEC/SI definitions for bytes and bits are well-known constants.
fn pick_bytes_bits(unit: &Unit, table: &[BinPrefixRow], mag_input: f64) -> Scaled {
    let to_base = table_factor(table, &unit.raw).unwrap_or(1.0);
    let mag_base = mag_input * to_base;
    // Pick by base-unit magnitude (largest threshold ≤ mag_base).
    let row = table
        .iter()
        .find(|r| mag_base >= r.threshold_factor)
        .or_else(|| table.last())
        .expect("byte/bit tables are never empty");
    let factor = to_base / row.threshold_factor;
    Scaled {
        prefix: String::new(),
        factor,
        suffix: format!(" {}", row.display),
    }
}

/// Rate picker: normalise every input rate to "per second" of the
/// family's base, then pick the numerator prefix from the
/// per-second magnitude. Picking thresholds by per-second magnitude
/// matches user intuition — "show me MiB/s when the value is around
/// the MiB-per-second range".
fn pick_rate(unit: &Unit, table: RateFamily, mag_input: f64) -> Scaled {
    // Strip the denominator from the raw and find the numerator's
    // factor to the family base (e.g. KiBy → 1024 By).
    let (num, denom) = unit.raw.split_once('/').unwrap_or((unit.raw.as_str(), "s"));
    let num_factor = table_factor(table.numerator, num).unwrap_or(1.0);
    // Convert the denominator to seconds via UCUM. `s` is the
    // family-base denominator we display.
    let denom_to_s = canonical_factor(denom).unwrap_or(1.0);
    // Input units per second = (num_factor base / 1 denom_unit) × (1 denom_unit / denom_to_s s)
    //                        = num_factor / denom_to_s  base/s
    let factor_to_base_per_s = num_factor / denom_to_s;
    let mag_base_per_s = mag_input * factor_to_base_per_s;
    let row = table
        .numerator
        .iter()
        .find(|r| mag_base_per_s >= r.threshold_factor)
        .or_else(|| table.numerator.last())
        .expect("rate numerator tables are never empty");

    Scaled {
        prefix: String::new(),
        factor: factor_to_base_per_s / row.threshold_factor,
        suffix: format!(" {}/s", row.display),
    }
}

/// Decimal-SI picker for whitelisted engineering units. UCUM syntax
/// accepts both `u` and `µ`; display always uses the typographic `µ`.
fn pick_si_decimal(unit: &Unit, base: SiBase, mag_input: f64) -> Scaled {
    let input_factor = si_prefix_factor(&unit.raw, base).unwrap_or(1.0);
    let mag_base = mag_input * input_factor;
    let row = SI_PREFIXES
        .iter()
        .find(|p| mag_base >= p.factor)
        .or_else(|| SI_PREFIXES.last())
        .expect("SI prefix table is never empty");
    Scaled {
        prefix: String::new(),
        factor: input_factor / row.factor,
        suffix: format!(" {}{}", row.display, base.display_base()),
    }
}

/// Mass-concentration picker. Internally we normalise to `g/m3` and
/// display with typographic cubic metre (`m³`).
fn pick_mass_concentration(unit: &Unit, mag_input: f64) -> Scaled {
    let input_factor = mass_concentration_factor(&unit.raw).unwrap_or(1.0);
    let mag_base = mag_input * input_factor;
    let row = MASS_CONCENTRATION
        .iter()
        .find(|r| mag_base >= r.factor)
        .or_else(|| MASS_CONCENTRATION.last())
        .expect("mass concentration table is never empty");
    Scaled {
        prefix: String::new(),
        factor: input_factor / row.factor,
        suffix: format!(" {}/m³", row.display),
    }
}

/// Pick the largest row whose threshold ≤ `mag`. Falls back to the
/// smallest row when nothing matches (e.g. zero magnitude).
fn pick_row(table: &[PrefixRow], mag: f64) -> &PrefixRow {
    table
        .iter()
        .find(|r| mag >= r.threshold)
        .or_else(|| table.last())
        .expect("prefix tables are never empty")
}

/// UCUM canonical factor for `expr`: how many canonical-base units
/// one `expr` represents. Cached per-process because every axis
/// redraw asks the same handful of questions.
fn canonical_factor(expr: &str) -> Option<f64> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<f64>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(v) = cache.lock().expect("canonical_factor cache").get(expr) {
        return *v;
    }
    let v = octofhir_ucum::get_canonical_units(expr)
        .ok()
        .map(|c| c.factor);
    cache
        .lock()
        .expect("canonical_factor cache")
        .insert(expr.to_string(), v);
    v
}

/// Multiplier `from → to` via UCUM canonical forms. `None` if either
/// fails to canonicalise or they don't share a canonical unit.
fn conversion_factor_via_canonical(from: &str, to: &str) -> Option<f64> {
    let f = canonical_factor(from)?;
    let t = canonical_factor(to)?;
    // Same canonical string is required, but the cached factor map
    // doesn't store the unit string. Fall back to one fresh
    // `get_canonical_units` per call here — cheap enough on the
    // axis-redraw path.
    let cf = octofhir_ucum::get_canonical_units(from).ok()?;
    let ct = octofhir_ucum::get_canonical_units(to).ok()?;
    if cf.unit != ct.unit {
        return None;
    }
    let _ = (f, t); // already used via the .ok() calls above
    Some(cf.factor / ct.factor)
}

/// Look up a unit string in a byte/bit prefix table, returning the
/// factor from that unit to the family's base unit (e.g. `KiBy → 1024`,
/// `MiBy → 1_048_576`, `By → 1`).
fn table_factor(table: &[BinPrefixRow], unit_str: &str) -> Option<f64> {
    table
        .iter()
        .find(|r| r.target == unit_str)
        .map(|r| r.threshold_factor)
}

/// One row of a time / frequency prefix table.
struct PrefixRow {
    /// UCUM unit string for this row's target. Used with
    /// [`conversion_factor_via_canonical`] to compute the multiplier
    /// from input unit to this display unit.
    target: &'static str,
    /// What we render after the scaled value (e.g. `ms`, `MHz`).
    display: &'static str,
    /// Minimum raw-value magnitude (in the family's *canonical base*
    /// unit) at which this prefix is preferred. Rows are listed
    /// largest threshold first so a linear scan finds the match.
    threshold: f64,
}

/// One row of a byte / bit prefix table. Same shape as [`PrefixRow`]
/// but the factor to the base unit is also the threshold (e.g. 1 MiB
/// is preferred at ≥1,048,576 bytes), so we store it once.
struct BinPrefixRow {
    target: &'static str,
    display: &'static str,
    /// Factor from this prefix to the family base. Doubles as the
    /// threshold for switching to this prefix.
    threshold_factor: f64,
}

#[derive(Clone, Copy)]
struct RateFamily {
    numerator: &'static [BinPrefixRow],
}

struct SiPrefixRow {
    ucum: &'static str,
    display: &'static str,
    factor: f64,
}

const BYTES_RATE_TABLE: RateFamily = RateFamily {
    numerator: BYTES_BINARY,
};

const BITS_RATE_TABLE: RateFamily = RateFamily {
    numerator: BITS_BINARY,
};

const SI_BASES: &[SiBase] = &[
    SiBase::Watt,
    SiBase::Volt,
    SiBase::Ampere,
    SiBase::Joule,
    SiBase::Pascal,
    SiBase::Metre,
    SiBase::Gram,
    SiBase::Litre,
    SiBase::Lux,
    SiBase::Lumen,
    SiBase::Mole,
];

const SI_PREFIXES: &[SiPrefixRow] = &[
    SiPrefixRow {
        ucum: "T",
        display: "T",
        factor: 1e12,
    },
    SiPrefixRow {
        ucum: "G",
        display: "G",
        factor: 1e9,
    },
    SiPrefixRow {
        ucum: "M",
        display: "M",
        factor: 1e6,
    },
    SiPrefixRow {
        ucum: "k",
        display: "k",
        factor: 1e3,
    },
    SiPrefixRow {
        ucum: "",
        display: "",
        factor: 1.0,
    },
    SiPrefixRow {
        ucum: "m",
        display: "m",
        factor: 1e-3,
    },
    SiPrefixRow {
        ucum: "u",
        display: "µ",
        factor: 1e-6,
    },
    SiPrefixRow {
        ucum: "µ",
        display: "µ",
        factor: 1e-6,
    },
    SiPrefixRow {
        ucum: "n",
        display: "n",
        factor: 1e-9,
    },
];

const MASS_CONCENTRATION: &[SiPrefixRow] = &[
    SiPrefixRow {
        ucum: "kg",
        display: "kg",
        factor: 1e3,
    },
    SiPrefixRow {
        ucum: "g",
        display: "g",
        factor: 1.0,
    },
    SiPrefixRow {
        ucum: "mg",
        display: "mg",
        factor: 1e-3,
    },
    SiPrefixRow {
        ucum: "ug",
        display: "µg",
        factor: 1e-6,
    },
    SiPrefixRow {
        ucum: "ng",
        display: "ng",
        factor: 1e-9,
    },
];

// `threshold_factor` for byte/bit prefixes = the factor from this
// row's unit to the base unit, which doubles as the threshold for
// switching to this row.

const BYTES_BINARY: &[BinPrefixRow] = &[
    BinPrefixRow {
        target: "PiBy",
        display: "PiB",
        threshold_factor: (1u64 << 50) as f64,
    },
    BinPrefixRow {
        target: "TiBy",
        display: "TiB",
        threshold_factor: (1u64 << 40) as f64,
    },
    BinPrefixRow {
        target: "GiBy",
        display: "GiB",
        threshold_factor: (1u64 << 30) as f64,
    },
    BinPrefixRow {
        target: "MiBy",
        display: "MiB",
        threshold_factor: (1u64 << 20) as f64,
    },
    BinPrefixRow {
        target: "KiBy",
        display: "KiB",
        threshold_factor: (1u64 << 10) as f64,
    },
    BinPrefixRow {
        target: "By",
        display: "B",
        threshold_factor: 1.0,
    },
];

const BYTES_DECIMAL: &[BinPrefixRow] = &[
    BinPrefixRow {
        target: "PBy",
        display: "PB",
        threshold_factor: 1e15,
    },
    BinPrefixRow {
        target: "TBy",
        display: "TB",
        threshold_factor: 1e12,
    },
    BinPrefixRow {
        target: "GBy",
        display: "GB",
        threshold_factor: 1e9,
    },
    BinPrefixRow {
        target: "MBy",
        display: "MB",
        threshold_factor: 1e6,
    },
    BinPrefixRow {
        target: "kBy",
        display: "kB",
        threshold_factor: 1e3,
    },
    BinPrefixRow {
        target: "By",
        display: "B",
        threshold_factor: 1.0,
    },
];

const BITS_BINARY: &[BinPrefixRow] = &[
    BinPrefixRow {
        target: "Tibit",
        display: "Tibit",
        threshold_factor: (1u64 << 40) as f64,
    },
    BinPrefixRow {
        target: "Gibit",
        display: "Gibit",
        threshold_factor: (1u64 << 30) as f64,
    },
    BinPrefixRow {
        target: "Mibit",
        display: "Mibit",
        threshold_factor: (1u64 << 20) as f64,
    },
    BinPrefixRow {
        target: "Kibit",
        display: "Kibit",
        threshold_factor: (1u64 << 10) as f64,
    },
    BinPrefixRow {
        target: "bit",
        display: "bit",
        threshold_factor: 1.0,
    },
];

const BITS_DECIMAL: &[BinPrefixRow] = &[
    BinPrefixRow {
        target: "Tbit",
        display: "Tbit",
        threshold_factor: 1e12,
    },
    BinPrefixRow {
        target: "Gbit",
        display: "Gbit",
        threshold_factor: 1e9,
    },
    BinPrefixRow {
        target: "Mbit",
        display: "Mbit",
        threshold_factor: 1e6,
    },
    BinPrefixRow {
        target: "kbit",
        display: "kbit",
        threshold_factor: 1e3,
    },
    BinPrefixRow {
        target: "bit",
        display: "bit",
        threshold_factor: 1.0,
    },
];

// Time: canonical base is `s`. Thresholds are in seconds.
// "Promote at 2× the next unit" rule keeps the display from flipping
// at single-unit boundaries (e.g. 70s is shown as "70 s", not "1.17
// min"), matching how dashboards-as-glance work in practice.
const TIME: &[PrefixRow] = &[
    PrefixRow {
        target: "d",
        display: "d",
        threshold: 2.0 * 86_400.0,
    },
    PrefixRow {
        target: "h",
        display: "h",
        threshold: 2.0 * 3_600.0,
    },
    PrefixRow {
        target: "min",
        display: "min",
        threshold: 2.0 * 60.0,
    },
    PrefixRow {
        target: "s",
        display: "s",
        threshold: 1.0,
    },
    PrefixRow {
        target: "ms",
        display: "ms",
        threshold: 1e-3,
    },
    PrefixRow {
        target: "us",
        display: "µs",
        threshold: 1e-6,
    },
    PrefixRow {
        target: "ns",
        display: "ns",
        threshold: 0.0,
    },
];

// Frequency: canonical base is `Hz`. Thresholds are in Hz.
const FREQUENCY: &[PrefixRow] = &[
    PrefixRow {
        target: "THz",
        display: "THz",
        threshold: 1e12,
    },
    PrefixRow {
        target: "GHz",
        display: "GHz",
        threshold: 1e9,
    },
    PrefixRow {
        target: "MHz",
        display: "MHz",
        threshold: 1e6,
    },
    PrefixRow {
        target: "kHz",
        display: "kHz",
        threshold: 1e3,
    },
    PrefixRow {
        target: "Hz",
        display: "Hz",
        threshold: 0.0,
    },
];

/// Format a value using a chosen scale. `decimals` controls the
/// fractional digits of the scaled (i.e. post-multiplication) number.
pub fn format_value(v: f64, scaled: &Scaled, decimals: usize) -> String {
    let scaled_v = v * scaled.factor;
    format!(
        "{}{:.prec$}{}",
        scaled.prefix,
        scaled_v,
        scaled.suffix,
        prec = decimals
    )
}

pub mod pragma;

#[cfg(test)]
mod tests;
