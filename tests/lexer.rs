use sfgn_core::lexer::{IndentState, Lexer};
use sfgn_core::{ErrorCode, LineCol};

#[test]
fn rejects_utf8_bom_and_invalid_utf8_with_e001() {
    let with_bom = b"\xEF\xBB\xBF>:\n";
    let err = Lexer::from_bytes(with_bom).unwrap_err();
    assert_eq!(err.code, ErrorCode::E001);
    assert_eq!(err.loc, LineCol::new(1, 1));

    let invalid_utf8 = b">:\n  \"k\": \xFF\xFE\n";
    let err = Lexer::from_bytes(invalid_utf8).unwrap_err();
    assert_eq!(err.code, ErrorCode::E001);
    assert_eq!(err.loc, LineCol::new(2, 8));
}

#[test]
fn rejects_standalone_cr_and_control_bytes_with_e001() {
    let mut lex = Lexer::new(">:\r\"a\": 1\n").unwrap();
    lex.advance_ascii(2);
    let err = lex.expect_eol_or_eof(ErrorCode::E009).unwrap_err();
    assert_eq!(err.code, ErrorCode::E001);
    assert_eq!(err.loc, LineCol::new(1, 3));

    let mut lex_crlf = Lexer::new(">:\r\n\"a\": 1\n").unwrap();
    lex_crlf.advance_ascii(2);
    lex_crlf.expect_eol_or_eof(ErrorCode::E009).unwrap();
    assert_eq!(lex_crlf.loc(), LineCol::new(2, 1));

    let mut lex_ctrl = Lexer::new("\"bad\x07str\"").unwrap();
    let err = lex_ctrl.scan_quoted_string().unwrap_err();
    assert_eq!(err.code, ErrorCode::E001);
}

#[test]
fn indentation_locking_spaces_and_errors() {
    let mut lex1 = Lexer::new(
        r###" "a": 1
"###,
    )
    .unwrap();
    let err = lex1.consume_depth1_indent(false).unwrap_err();
    assert_eq!(err.code, ErrorCode::E004);

    let mut lex_mix = Lexer::new("  \t\"a\": 1\n").unwrap();
    let err = lex_mix.consume_depth1_indent(false).unwrap_err();
    assert_eq!(err.code, ErrorCode::E004);

    let mut lex = Lexer::new(
        r###" # comment
  "a": 1
   "b": 2
"###,
    )
    .unwrap();
    lex.consume_depth1_indent(true).unwrap();
    lex.consume_comment_to_eol().unwrap();
    lex.expect_eol_or_eof(ErrorCode::E003).unwrap();

    lex.consume_depth1_indent(false).unwrap();
    assert_eq!(lex.indent, IndentState::LockedSpaces(2));
    lex.advance_ascii(6);
    lex.expect_eol_or_eof(ErrorCode::E003).unwrap();

    let err = lex.consume_depth1_indent(false).unwrap_err();
    assert_eq!(err.code, ErrorCode::E006);

    let mut lex_short = Lexer::new(
        r###"  "a": 1
 "b": 2
"###,
    )
    .unwrap();
    lex_short.consume_depth1_indent(false).unwrap();
    lex_short.advance_ascii(6);
    lex_short.expect_eol_or_eof(ErrorCode::E003).unwrap();
    let err = lex_short.consume_depth1_indent(false).unwrap_err();
    assert_eq!(err.code, ErrorCode::E005);
}

#[test]
fn indentation_locking_tab_and_errors() {
    let mut lex_bad = Lexer::new("\t \"a\": 1\n").unwrap();
    let err = lex_bad.consume_depth1_indent(false).unwrap_err();
    assert_eq!(err.code, ErrorCode::E004);

    let mut lex = Lexer::new("\t\"a\": 1\n\t\t\"b\": 2\n").unwrap();
    lex.consume_depth1_indent(false).unwrap();
    assert_eq!(lex.indent, IndentState::LockedTab);
    lex.advance_ascii(6);
    lex.expect_eol_or_eof(ErrorCode::E003).unwrap();
    assert_eq!(
        lex.consume_depth1_indent(false).unwrap_err().code,
        ErrorCode::E006
    );

    let mut lex_sp = Lexer::new("\t\"a\": 1\n  \"b\": 2\n").unwrap();
    lex_sp.consume_depth1_indent(false).unwrap();
    lex_sp.advance_ascii(6);
    lex_sp.expect_eol_or_eof(ErrorCode::E003).unwrap();
    assert_eq!(
        lex_sp.consume_depth1_indent(false).unwrap_err().code,
        ErrorCode::E005
    );
}

#[test]
fn quoted_strings_and_escapes() {
    let valid = r###""hello \"world\" \\ \n \r \t \0 \u{1F680} \u{0041}""###;
    let mut lex = Lexer::new(valid).unwrap();
    let span = lex.scan_quoted_string().unwrap();
    assert_eq!(span.slice_str(valid), valid);

    let mut unterm = Lexer::new("\"hello\n").unwrap();
    assert_eq!(
        unterm.scan_quoted_string().unwrap_err().code,
        ErrorCode::E007
    );

    let bad_esc = r###""bad\x""###;
    let mut lex_esc = Lexer::new(bad_esc).unwrap();
    assert_eq!(
        lex_esc.scan_quoted_string().unwrap_err().code,
        ErrorCode::E008
    );

    let surrogate = r###""\u{D800}""###;
    let mut lex_sur = Lexer::new(surrogate).unwrap();
    assert_eq!(
        lex_sur.scan_quoted_string().unwrap_err().code,
        ErrorCode::E008
    );
}

#[test]
fn indexed_identifiers_and_e010() {
    let src = r###""Worker" #0#12:"###;
    let mut lex = Lexer::new(src).unwrap();
    let span = lex.scan_indexed_identifier(true).unwrap();
    assert_eq!(span.slice_str(src), r###""Worker" #0#12"###);
    assert_eq!(lex.peek(), Some(b':'));

    let mut bad = Lexer::new(r###""Worker"#foo:"###).unwrap();
    assert_eq!(
        bad.scan_indexed_identifier(true).unwrap_err().code,
        ErrorCode::E010
    );

    let rel_src = r###""Worker"#0 # inline comment"###;
    let mut rel = Lexer::new(rel_src).unwrap();
    let rel_span = rel.scan_indexed_identifier(false).unwrap();
    assert_eq!(rel_span.slice_str(rel_src), r###""Worker"#0"###);
    rel.skip_ows();
    assert_eq!(rel.peek(), Some(b'#'));

    let mut rel_bad = Lexer::new(r###""Worker"#bad"###).unwrap();
    assert_eq!(
        rel_bad.scan_indexed_identifier(false).unwrap_err().code,
        ErrorCode::E010
    );
}

#[test]
fn numbers_and_multiline_headers() {
    for valid_num in ["0", "-0", "42", "-10", "3.14159", "1e-6", "2.5E+10"] {
        let mut lex = Lexer::new(valid_num).unwrap();
        let span = lex.scan_number().expect(valid_num);
        assert_eq!(span.slice_str(valid_num), valid_num);
        assert!(lex.is_eof());
    }

    for invalid_num in ["01", "-01", ".5", "1.", "1e", "1e+", "-", "NaN", "Infinity"] {
        let mut lex = Lexer::new(invalid_num).unwrap();
        assert!(
            lex.scan_number().is_none(),
            "expected None for {}",
            invalid_num
        );
    }

    let ml_src = r###""""rust  
let x = 1;
"""
"###;
    let mut ml = Lexer::new(ml_src).unwrap();
    let tag = ml.scan_multiline_header().unwrap().expect("tag present");
    assert_eq!(tag.slice_str(ml.source()), "rust");

    let bad_ml_src = r###""""rust # comment
"###;
    let mut bad_ml = Lexer::new(bad_ml_src).unwrap();
    assert_eq!(
        bad_ml.scan_multiline_header().unwrap_err().code,
        ErrorCode::E014
    );
}
