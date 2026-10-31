//! README example: a timer route with Simple and a filter.
//! Run: cargo run -p camel-main --example hello_timer
use camel_core::prelude::*;

fn main() -> Result<()> {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("timer:hello?period=200")
            .route_id("hello")
            .set_body(simple("Hello from rust-camel #${exchangeProperty.CamelTimerCounter} at ${date:now:HH:mm:ss}"))
            .filter(simple("${exchangeProperty.CamelTimerCounter} > 2"))
                .to("log:greetings")
            .end();
    })?;
    ctx.start()?;
    std::thread::sleep(std::time::Duration::from_millis(2000));
    ctx.stop()
}
