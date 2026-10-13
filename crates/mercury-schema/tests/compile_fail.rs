//! `SafeText`, `DisplayName` and `RedactedQuestion` can't be built outside the redaction module:
//! every attempt below must fail to compile.

#[test]
fn text_types_cannot_be_built_outside_the_redaction_module() {
    let cases = trybuild::TestCases::new();
    cases.compile_fail("tests/compile_fail/*.rs");
}
