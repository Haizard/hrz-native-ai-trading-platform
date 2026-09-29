//! The pasted-script failure from the browser: a CRLF script's header
//! carried `\r` into the knob scanner (`is_whitespace` eats it, fine), but
//! the real failure was the header parser consuming line 2 — `title="S"` at
//! end-of-line must END the knob scan, not run past the newline.

use pine_lite::vet;

#[test]
fn a_crlf_script_with_request_security_vets() {
    let src = "//@pine_lite version=1 overlay=false title=\"SecForm Test\"\r\npair = request.security(\"ETHUSDT\", \"1m\", request.close())\r\nplot(pair)\r\n";
    let errs = vet(src).err();
    assert!(
        errs.is_none(),
        "CRLF + request.security must vet: {:?}",
        errs.map(|e| e.iter().map(|e| e.message.clone()).collect::<Vec<_>>())
    );
}

#[test]
fn title_at_end_of_line_does_not_swallow_the_next_line() {
    // The regression: knobs were scanned past the header's own line when the
    // title quote closed exactly at EOL, so line 2's text became phantom
    // knobs ("`\"1m\"` has no `=`").
    let src = "//@pine_lite version=1 overlay=false title=\"S\"\npair = request.security(\"ETHUSDT\", \"1m\", request.close())\nplot(pair)\n";
    let errs = vet(src).err();
    assert!(errs.is_none(), "{errs:?}");
}
