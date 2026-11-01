mod common;
#[test]
fn probe() {
    let ctx = common::ctx();
    let ex = common::exchange(&ctx);
    let lang = std::env::var("PL").unwrap_or("java".into());
    for src in std::env::var("PS").unwrap_or_default().split("|||") {
        eprintln!("{} => {:?}", src, camel_script::eval(&lang, src, &ex).map(|v| v.to_json()));
    }
}
