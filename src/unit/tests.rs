use super::*;

// ---------- parse / classify -----------------------------------------

#[test]
fn parsed_unit_exposes_raw_text() {
    // The renderer keeps the raw UCUM string around as the fallback
    // suffix for `Other` units; verify the accessor returns it.
    let u = parse("  Cel  ").unwrap();
    assert_eq!(u.raw(), "Cel");
}

#[test]
fn parse_rejects_empty_and_whitespace() {
    assert!(parse("").is_none());
    assert!(parse("   ").is_none());
}

#[test]
fn parse_accepts_permissive_ucum_and_classifies_as_other() {
    // `octofhir-ucum` is permissive: `kggg` parses as `k.g.g.g`.
    // We don't fight that — the unit lands in `Other` and is
    // displayed verbatim with no scaling.
    assert_eq!(parse("kggg").map(|u| u.family()), Some(UnitFamily::Other));
}

#[test]
fn classify_bytes_binary_for_base_and_binary_prefixes() {
    assert_eq!(parse("By").unwrap().family(), UnitFamily::BytesBinary);
    assert_eq!(parse("KiBy").unwrap().family(), UnitFamily::BytesBinary);
    assert_eq!(parse("MiBy").unwrap().family(), UnitFamily::BytesBinary);
    assert_eq!(parse("GiBy").unwrap().family(), UnitFamily::BytesBinary);
}

#[test]
fn classify_bytes_decimal_for_decimal_prefixes() {
    assert_eq!(parse("kBy").unwrap().family(), UnitFamily::BytesDecimal);
    assert_eq!(parse("MBy").unwrap().family(), UnitFamily::BytesDecimal);
    assert_eq!(parse("GBy").unwrap().family(), UnitFamily::BytesDecimal);
}

#[test]
fn classify_bits_binary_and_decimal() {
    assert_eq!(parse("bit").unwrap().family(), UnitFamily::BitsBinary);
    assert_eq!(parse("Kibit").unwrap().family(), UnitFamily::BitsBinary);
    assert_eq!(parse("Mibit").unwrap().family(), UnitFamily::BitsBinary);
    assert_eq!(parse("kbit").unwrap().family(), UnitFamily::BitsDecimal);
    assert_eq!(parse("Mbit").unwrap().family(), UnitFamily::BitsDecimal);
}

#[test]
fn classify_time_units() {
    for s in ["ns", "us", "ms", "s", "min", "h", "d"] {
        assert_eq!(
            parse(s).unwrap().family(),
            UnitFamily::Time,
            "expected Time for {s:?}"
        );
    }
}

#[test]
fn classify_frequency_units() {
    for s in ["Hz", "kHz", "MHz", "GHz", "THz"] {
        assert_eq!(
            parse(s).unwrap().family(),
            UnitFamily::Frequency,
            "expected Frequency for {s:?}"
        );
    }
}

#[test]
fn classify_percent_and_dimensionless() {
    assert_eq!(parse("%").unwrap().family(), UnitFamily::Percent);
    assert_eq!(parse("1").unwrap().family(), UnitFamily::Dimensionless);
    // OTEL convention: annotation-only units stay dimensionless.
    assert_eq!(
        parse("{request}").unwrap().family(),
        UnitFamily::Dimensionless
    );
    assert_eq!(
        parse("1{request}").unwrap().family(),
        UnitFamily::Dimensionless
    );
}

#[test]
fn classify_rate_families() {
    assert_eq!(parse("By/s").unwrap().family(), UnitFamily::BytesPerTime);
    assert_eq!(parse("KiBy/s").unwrap().family(), UnitFamily::BytesPerTime);
    assert_eq!(parse("MBy/s").unwrap().family(), UnitFamily::BytesPerTime);
    assert_eq!(parse("bit/s").unwrap().family(), UnitFamily::BitsPerTime);
    assert_eq!(parse("Mbit/s").unwrap().family(), UnitFamily::BitsPerTime);
}

#[test]
fn classify_unknown_families_fall_through_to_other() {
    // Valid UCUM, but intentionally not in our scaling families:
    // offset temperatures must not become kCel or mCel.
    assert_eq!(parse("Cel").unwrap().family(), UnitFamily::Other);
}

#[test]
fn classify_si_decimal_engineering_units() {
    for (raw, base) in [
        ("W", SiBase::Watt),
        ("kW", SiBase::Watt),
        ("mW", SiBase::Watt),
        ("V", SiBase::Volt),
        ("A", SiBase::Ampere),
        ("J", SiBase::Joule),
        ("Pa", SiBase::Pascal),
        ("m", SiBase::Metre),
        ("km", SiBase::Metre),
        ("g", SiBase::Gram),
        ("kg", SiBase::Gram),
        ("L", SiBase::Litre),
        ("lx", SiBase::Lux),
        ("klx", SiBase::Lux),
        ("lm", SiBase::Lumen),
        ("mol", SiBase::Mole),
    ] {
        assert_eq!(
            parse(raw).unwrap().family(),
            UnitFamily::SiDecimal(base),
            "expected SI decimal family for {raw:?}"
        );
    }
}

#[test]
fn classify_mass_concentration_normalizes_friendly_cubic_metres() {
    for raw in ["ug/m3", "µg/m3", "μg/m3", "µg/m³", "mg/m3", "kg/m3"] {
        let u = parse(raw).unwrap_or_else(|| panic!("{raw:?} should parse"));
        assert_eq!(u.family(), UnitFamily::MassConcentration, "{raw:?}");
    }
    assert_eq!(parse("µg/m³").unwrap().raw(), "µg/m3");
}

#[test]
fn classify_currency_extensions() {
    for (raw, currency) in [
        ("EUR", iso_currency::Currency::EUR),
        ("USD", iso_currency::Currency::USD),
        ("GBP", iso_currency::Currency::GBP),
        ("JPY", iso_currency::Currency::JPY),
        ("CHF", iso_currency::Currency::CHF),
    ] {
        assert_eq!(
            parse(raw).unwrap().family(),
            UnitFamily::Currency(currency),
            "expected Currency for {raw:?}"
        );
    }
}

// ---------- scale_for: bytes binary ----------------------------------

#[test]
fn scale_bytes_binary_picks_kib_at_kib_threshold() {
    let u = parse("By").unwrap();
    let s = scale_for(Some(&u), 0.0, 2048.0);
    assert_eq!(s.suffix, " KiB");
    // 2048 / 1024 = 2.0
    assert!((2048.0 * s.factor - 2.0).abs() < 1e-9);
}

#[test]
fn scale_bytes_binary_picks_mib_at_megabyte_range() {
    let u = parse("By").unwrap();
    let s = scale_for(Some(&u), 0.0, 2_621_440.0);
    assert_eq!(s.suffix, " MiB");
    // 2_621_440 / 2^20 = 2.5
    assert!((2_621_440.0 * s.factor - 2.5).abs() < 1e-9);
}

#[test]
fn scale_bytes_binary_picks_gib_at_gigabyte_range() {
    let u = parse("By").unwrap();
    let s = scale_for(Some(&u), 0.0, (3u64 << 30) as f64);
    assert_eq!(s.suffix, " GiB");
    assert!(((3u64 << 30) as f64 * s.factor - 3.0).abs() < 1e-9);
}

#[test]
fn scale_bytes_binary_stays_b_for_small_values() {
    let u = parse("By").unwrap();
    let s = scale_for(Some(&u), 0.0, 500.0);
    assert_eq!(s.suffix, " B");
    assert!((500.0 * s.factor - 500.0).abs() < 1e-9);
}

#[test]
fn scale_bytes_binary_input_already_in_mib() {
    // Input unit is already MiBy; the magnitude is in MiBy, not in
    // raw bytes. 1500 MiBy ≈ 1.46 GiB → the picker should promote
    // to GiB. This also exercises the input→base conversion path
    // (1 MiBy = 2^20 By in the base table).
    let u = parse("MiBy").unwrap();
    let s = scale_for(Some(&u), 0.0, 1500.0);
    assert_eq!(s.suffix, " GiB");
    // 1500 MiB / 1024 ≈ 1.4648 GiB
    assert!((1500.0 * s.factor - 1.464_843_75).abs() < 1e-6);
}

// ---------- scale_for: bytes decimal ---------------------------------

#[test]
fn scale_bytes_decimal_picks_mb_at_megabyte_range() {
    let u = parse("kBy").unwrap();
    // 2_500_000 kBy raw == 2.5 GB
    let s = scale_for(Some(&u), 0.0, 2_500_000.0);
    assert_eq!(s.suffix, " GB");
    assert!((2_500_000.0 * s.factor - 2.5).abs() < 1e-6);
}

// ---------- scale_for: time ------------------------------------------

#[test]
fn scale_time_promotes_ms_to_seconds_at_one_second() {
    let u = parse("ms").unwrap();
    // 5000 ms = 5 s, well above the 1s threshold. The magnitude
    // arrives in the *input* unit (ms), so we pass 5000, not 5.
    let s = scale_for(Some(&u), 0.0, 5000.0);
    assert_eq!(s.suffix, " s");
    // 5000 ms × (1/1000) = 5 s
    assert!((5000.0 * s.factor - 5.0).abs() < 1e-9);
}

#[test]
fn scale_time_promotes_seconds_to_minutes_at_two_minutes() {
    let u = parse("s").unwrap();
    let s = scale_for(Some(&u), 0.0, 180.0);
    assert_eq!(s.suffix, " min");
    assert!((180.0 * s.factor - 3.0).abs() < 1e-9);
}

#[test]
fn scale_time_demotes_to_microseconds_under_one_ms() {
    let u = parse("s").unwrap();
    let s = scale_for(Some(&u), 0.0, 5e-4);
    // 0.5ms is below the 1ms threshold, so the picker falls one
    // step further to microseconds (500 µs).
    assert_eq!(s.suffix, " µs");
    assert!((5e-4 * s.factor - 500.0).abs() < 1e-6);
}

// ---------- scale_for: frequency -------------------------------------

#[test]
fn scale_frequency_promotes_hz_to_ghz() {
    let u = parse("Hz").unwrap();
    let s = scale_for(Some(&u), 0.0, 2.5e9);
    assert_eq!(s.suffix, " GHz");
    assert!((2.5e9 * s.factor - 2.5).abs() < 1e-3);
}

// ---------- scale_for: byte/bit rates --------------------------------

#[test]
fn scale_byte_rate_picks_mib_per_second_by_magnitude() {
    let u = parse("By/s").unwrap();
    let s = scale_for(Some(&u), 0.0, 2_621_440.0);
    assert_eq!(s.suffix, " MiB/s");
    assert!((2_621_440.0 * s.factor - 2.5).abs() < 1e-9);
}

#[test]
fn scale_byte_rate_respects_input_denominator() {
    let u = parse("MiBy/min").unwrap();
    // 120 MiBy/min = 2 MiB/s. Values arrive in input units, so the
    // raw magnitude is 120.
    let s = scale_for(Some(&u), 0.0, 120.0);
    assert_eq!(s.suffix, " MiB/s");
    assert!((120.0 * s.factor - 2.0).abs() < 1e-9);
}

#[test]
fn scale_bit_rate_picks_mbit_per_second_by_magnitude() {
    let u = parse("bit/s").unwrap();
    let s = scale_for(Some(&u), 0.0, 2_500_000.0);
    assert_eq!(s.suffix, " Mibit/s");
    assert!((2_500_000.0 * s.factor - 2.384_185_791).abs() < 1e-6);
}

// ---------- scale_for: SI decimal ------------------------------------

#[test]
fn scale_si_decimal_power_promotes_watts_to_kw() {
    let u = parse("W").unwrap();
    let s = scale_for(Some(&u), 0.0, 1_500.0);
    assert_eq!(s.suffix, " kW");
    assert!((1_500.0 * s.factor - 1.5).abs() < 1e-9);
}

#[test]
fn scale_si_decimal_power_demotes_watts_to_mw() {
    let u = parse("W").unwrap();
    let s = scale_for(Some(&u), 0.0, 0.25);
    assert_eq!(s.suffix, " mW");
    assert!((0.25 * s.factor - 250.0).abs() < 1e-9);
}

#[test]
fn scale_si_decimal_lux_promotes_to_klx() {
    let u = parse("lx").unwrap();
    let s = scale_for(Some(&u), 0.0, 1_500.0);
    assert_eq!(s.suffix, " klx");
    assert!((1_500.0 * s.factor - 1.5).abs() < 1e-9);
}

#[test]
fn scale_si_decimal_lux_demotes_to_mlx() {
    let u = parse("lx").unwrap();
    let s = scale_for(Some(&u), 0.0, 0.25);
    assert_eq!(s.suffix, " mlx");
    assert!((0.25 * s.factor - 250.0).abs() < 1e-9);
}

#[test]
fn scale_si_decimal_respects_prefixed_input_unit() {
    let u = parse("kW").unwrap();
    let s = scale_for(Some(&u), 0.0, 2_500.0);
    assert_eq!(s.suffix, " MW");
    assert!((2_500.0 * s.factor - 2.5).abs() < 1e-9);
}

#[test]
fn scale_si_decimal_displays_micro_prefix() {
    let u = parse("A").unwrap();
    let s = scale_for(Some(&u), 0.0, 0.000_002);
    assert_eq!(s.suffix, " µA");
    assert!((0.000_002 * s.factor - 2.0).abs() < 1e-9);
}

#[test]
fn scale_si_decimal_keeps_temperature_unscaled() {
    let u = parse("Cel").unwrap();
    let s = scale_for(Some(&u), 0.0, 1_500.0);
    assert_eq!(s.suffix, " Cel");
    assert!((1_500.0 * s.factor - 1_500.0).abs() < 1e-9);
}

// ---------- scale_for: mass concentration ----------------------------

#[test]
fn scale_mass_concentration_promotes_micrograms_to_milligrams() {
    let u = parse("µg/m³").unwrap();
    let s = scale_for(Some(&u), 0.0, 1_500.0);
    assert_eq!(s.suffix, " mg/m³");
    assert!((1_500.0 * s.factor - 1.5).abs() < 1e-9);
}

#[test]
fn scale_mass_concentration_demotes_milligrams_to_micrograms() {
    let u = parse("mg/m3").unwrap();
    let s = scale_for(Some(&u), 0.0, 0.25);
    assert_eq!(s.suffix, " µg/m³");
    assert!((0.25 * s.factor - 250.0).abs() < 1e-9);
}

#[test]
fn scale_mass_concentration_respects_prefixed_input() {
    let u = parse("kg/m3").unwrap();
    let s = scale_for(Some(&u), 0.0, 0.002);
    assert_eq!(s.suffix, " g/m³");
    assert!((0.002 * s.factor - 2.0).abs() < 1e-9);
}

// ---------- scale_for: currency --------------------------------------

#[test]
fn scale_currency_renders_symbol_prefix() {
    let u = parse("EUR").unwrap();
    let s = scale_for(Some(&u), 0.0, 1_234.5);
    assert_eq!(s.prefix, "€");
    assert_eq!(s.suffix, "");
    assert_eq!(format_value(1_234.5, &s, 2), "€1234.50");
}

#[test]
fn scale_currency_uses_library_symbol() {
    let u = parse("CHF").unwrap();
    let s = scale_for(Some(&u), 0.0, 12.0);
    assert_eq!(s.prefix, iso_currency::Currency::CHF.symbol().to_string());
    assert_eq!(format_value(12.0, &s, 2), "₣12.00");
}

// ---------- scale_for: percent / dimensionless -----------------------

#[test]
fn scale_percent_is_identity_with_percent_suffix() {
    let u = parse("%").unwrap();
    let s = scale_for(Some(&u), 0.0, 73.0);
    assert_eq!(s.suffix, "%");
    assert!((73.0 * s.factor - 73.0).abs() < 1e-9);
}

#[test]
fn scale_dimensionless_keeps_raw_suffix() {
    let u = parse("{request}").unwrap();
    let s = scale_for(Some(&u), 0.0, 100.0);
    assert!(
        s.suffix.contains("{request}"),
        "expected annotation in suffix, got {:?}",
        s.suffix
    );
    assert!((100.0 * s.factor - 100.0).abs() < 1e-9);
}

#[test]
fn scale_other_keeps_raw_suffix() {
    let u = parse("Cel").unwrap();
    let s = scale_for(Some(&u), 0.0, 42.0);
    assert!(s.suffix.contains("Cel"));
}

#[test]
fn scale_none_returns_identity() {
    let s = scale_for(None, 0.0, 1e9);
    assert_eq!(s, Scaled::none());
}

// ---------- format_value ---------------------------------------------

#[test]
fn format_value_applies_factor_and_suffix() {
    let u = parse("By").unwrap();
    let s = scale_for(Some(&u), 0.0, 2_621_440.0);
    let out = format_value(2_621_440.0, &s, 1);
    assert_eq!(out, "2.5 MiB");
}

#[test]
fn format_value_no_unit_just_number() {
    let s = Scaled::none();
    let out = format_value(42.5, &s, 2);
    assert_eq!(out, "42.50");
}
