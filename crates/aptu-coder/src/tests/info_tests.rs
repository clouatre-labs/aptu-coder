use super::helpers::make_analyzer;
use rmcp::ServerHandler;

#[test]
fn instructions_are_a_short_capability_summary() {
    // Arrange: a default analyzer instance.
    let analyzer = make_analyzer();

    // Act: read the server info instructions.
    let info = analyzer.get_info();
    let instructions = info.instructions.expect("server must provide instructions");

    // Assert: one short paragraph pointing at docs/METRICS.md, no per-tool
    // workflow steps that would re-introduce always-on context cost.
    assert!(
        instructions.len() <= 300,
        "instructions must stay short, got {} chars: {instructions}",
        instructions.len()
    );
    assert!(
        instructions.contains("docs/METRICS.md"),
        "instructions must point at docs/METRICS.md: {instructions}"
    );
}
