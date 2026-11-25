/// Opt-in only. `cargo test` reports this as ignored, which is not a passed live call.
#[test]
#[ignore = "requires COMPANY_AI_LIVE=1, live.toml, and explicitly requested smoke"]
fn live_smoke() {
    assert_eq!(std::env::var("COMPANY_AI_LIVE").ok().as_deref(), Some("1"));
    assert!(std::path::Path::new("live.toml").is_file());
}
