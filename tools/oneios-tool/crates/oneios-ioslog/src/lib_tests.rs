//! Tests of the command line and the time helpers (no log file needed).

use super::*;

#[test]
fn times() {
    let t = parse_time("2026-11-12T14:12:00Z").unwrap();
    assert_eq!(t, 1_794_492_720_000_000_000f64);
    assert_eq!(parse_time("2026-11-12 14:12:00").unwrap(), t);
    assert_eq!(parse_time("2026-11-12 14:12").unwrap(), t);
    assert_eq!(parse_time("2026-11-12T19:42:00+05:30").unwrap(), t);
    assert_eq!(parse_time("2026-11-12 14:12:00.5").unwrap(), t + 5e8);
    assert!(parse_time("yesterday").is_err());
    // the decoder's times are f64 nanoseconds: steps of 256 ns at this date, so only whole milliseconds are exact
    assert_eq!(time_text(t), "2026-11-12 14:12:00.000000Z");
    assert_eq!(time_text(t + 5e8), "2026-11-12 14:12:00.500000Z");
    assert_eq!(base_name("/usr/libexec/backboardd"), "backboardd");
    assert_eq!(uuid_text(Uuid::parse_str("9a6a3124-274a-44b2-9abf-2bc9e4599b3b").unwrap()), "9A6A3124274A44B29ABF2BC9E4599B3B");
}

#[test]
fn command_line() {
    let parse = |args: &[&str]| Cli::try_parse_from(std::iter::once("ioslog").chain(args.iter().copied()));
    assert!(parse(&["show", "data.img", "-f", "csv", "-o", "x.csv", "--boot", "last", "--proc", "backboardd", "--proc", "SpringBoard", "--type", "error,fault"]).is_ok());
    assert!(parse(&["save", "data.img", "x.logarchive", "--volume", "Data"]).is_ok());
    assert!(parse(&["boots", "data.img", "--block0"]).is_ok());
    assert!(parse(&["checks", "data.img", "--boot", "all", "--examples", "5"]).is_ok());
    assert!(parse(&["show"]).is_err() && parse(&["show", "a", "-f", "xml"]).is_err() && parse(&[]).is_err());
}
