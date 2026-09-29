//! Header-knob robustness: models write `sec = "ETHUSDT"` as often as the
//! tight form, and the lexer used to split that into an empty key plus a
//! phantom value -- reporting "sec needs a symbol" for a syntax slip and
//! burning the studio's whole repair budget on it.

use pine_lite::vet;

#[test]
fn sec_with_spaces_around_the_equals_vets() {
    let src = concat!(
        "//@pine_lite version=1 overlay=false title=\"SMT\" sec = \"ETHUSDT\"\n",
        "rc = request.close()\n",
        "spread = close - rc\n",
        "plot(spread)\n",
    );
    let errs = vet(src).err();
    assert!(
        errs.is_none(),
        "spaced `sec = \"...\"` must lex: {:?}",
        errs.map(|e| e.iter().map(|e| e.message.clone()).collect::<Vec<_>>())
    );
}

#[test]
fn title_with_spaces_around_the_equals_still_parses() {
    let src = "//@pine_lite version=1 overlay=false title = \"My RSI\"\nplot(close)\n";
    let errs = vet(src).err();
    assert!(errs.is_none(), "spaced `title = ...` must lex: {errs:?}");
}

#[test]
fn a_knob_without_equals_is_a_naming_error_not_a_phantom_sec() {
    // `version 1` (no `=`): the old lexer produced an empty key and reported
    // unrelated knob errors; now the error names the `key=value` shape.
    let errs = vet("//@pine_lite version 1\nplot(close)\n").expect_err("malformed knob");
    assert!(
        errs.iter().any(|e| e.message.contains("key=value")),
        "{errs:?}"
    );
}

#[test]
fn tight_form_still_vets() {
    let src = "//@pine_lite version=1 overlay=true title=\"T\" sec=\"ETHUSDT\"\nspread = close - request.close()\nplot(spread)\n";
    assert!(vet(src).is_ok(), "tight form must keep working");
}
