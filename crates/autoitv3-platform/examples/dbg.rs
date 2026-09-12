fn run(src: &str) {
    let prog = autoitv3_ast::parse(src).unwrap();
    let mut rt = autoitv3_platform::runtime_with_platform(&prog);
    match rt.call_function("F", vec![]) {
        Ok(v) => println!("OK   {v:?}"),
        Err(e) => println!("ERR  {e}"),
    }
}
fn main() {
    run(r#"
Func F()
    Local $m[]
    $m[1] = "a"
    Return $m[1]
EndFunc
"#);
    run(r#"
Func F()
    Local $m[]
    $m[100] = "a"
    $m["100"] = "b"
    Return MapExists(100, $m) & "/" & MapExists("100", $m) & "/" & UBound($m)
EndFunc
"#);
    run(r#"
Func F()
    Local $m[]
    $m[1.5] = "x"
    Return $m[1.5]
EndFunc
"#);
}
