//! Measurements for perf-review finding 16 (idle threads and per-evaluation threads):
//! threads held by timer routes and by the supervising route controller, XQuery and XSLT
//! latency per evaluation, and how often the local status file is rewritten while idle.
//!
//! Run: cargo run --release -p cameleer-main --example idle_threads
use std::any::Any;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use cameleer_core::prelude::*;
use cameleer_core::supervising::SupervisingRouteControllerConfig;

/// Threads of this process.
#[cfg(target_os = "linux")]
fn threads() -> usize {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    s.lines().find_map(|l| l.strip_prefix("Threads:")).and_then(|v| v.trim().parse().ok()).unwrap_or(0)
}

#[cfg(windows)]
fn threads() -> usize {
    #[repr(C)]
    struct ThreadEntry32 {
        size: u32,
        usage: u32,
        thread_id: u32,
        owner_process_id: u32,
        base_pri: i32,
        delta_pri: i32,
        flags: u32,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> isize;
        fn Thread32First(snap: isize, e: *mut ThreadEntry32) -> i32;
        fn Thread32Next(snap: isize, e: *mut ThreadEntry32) -> i32;
        fn CloseHandle(h: isize) -> i32;
    }
    const TH32CS_SNAPTHREAD: u32 = 4;
    let pid = std::process::id();
    let mut n = 0;
    // SAFETY: toolhelp snapshot walk with a correctly sized THREADENTRY32; the handle is closed.
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
        if snap == -1 {
            return 0;
        }
        let mut e = ThreadEntry32 { size: std::mem::size_of::<ThreadEntry32>() as u32, usage: 0, thread_id: 0, owner_process_id: 0, base_pri: 0, delta_pri: 0, flags: 0 };
        let mut ok = Thread32First(snap, &mut e);
        while ok != 0 {
            if e.owner_process_id == pid {
                n += 1;
            }
            ok = Thread32Next(snap, &mut e);
        }
        CloseHandle(snap);
    }
    n
}

#[cfg(not(any(target_os = "linux", windows)))]
fn threads() -> usize {
    0
}

/// Threads above `base` once the count has settled (thread exits are asynchronous).
fn settled_threads(base: usize) -> usize {
    std::thread::sleep(Duration::from_millis(300));
    threads().saturating_sub(base)
}

// A consumer that fails to start once, then starts (for the supervising controller).
struct Flaky(Arc<AtomicUsize>);
struct FlakyEndpoint(String, Arc<AtomicUsize>);
struct FlakyConsumer(Arc<AtomicUsize>);

impl Service for FlakyConsumer {
    fn start(&self) -> Result<()> {
        if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
            Err(CamelError::io("cannot connect"))
        } else {
            Ok(())
        }
    }
}
impl Endpoint for FlakyEndpoint {
    fn uri(&self) -> &str {
        &self.0
    }
    fn create_consumer(&self, _p: Arc<dyn Processor>) -> Result<Arc<dyn Consumer>> {
        Ok(Arc::new(FlakyConsumer(self.1.clone())))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
impl Component for Flaky {
    fn create_endpoint(&self, _c: &CamelContext, uri: &str, _r: &str, _p: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        Ok(Arc::new(FlakyEndpoint(uri.to_string(), self.0.clone())))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn timer_threads(routes: usize, same_name: bool) -> usize {
    let base = threads();
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        for i in 0..routes {
            let name = if same_name { "tick".to_string() } else { format!("tick{}", i) };
            b.from(&format!("timer:{}?period=60000&delay=60000", name)).route_id(&format!("t{}", i)).to("log:x?level=OFF");
        }
    })
    .unwrap();
    ctx.start().unwrap();
    let n = settled_threads(base);
    ctx.stop().unwrap();
    n
}

fn supervising_threads() -> usize {
    let base = threads();
    let ctx = CamelContext::new();
    ctx.add_component("flaky", Flaky(Arc::default()));
    ctx.enable_supervising_route_controller(SupervisingRouteControllerConfig::default().initial_delay(Duration::ZERO).back_off_delay(Duration::from_millis(50)));
    ctx.add_routes(|b| {
        b.from("flaky:x").route_id("flaky").to("log:x?level=OFF");
    })
    .unwrap();
    ctx.start().unwrap();
    let end = Instant::now() + Duration::from_secs(10);
    while ctx.route("flaky").map(|r| r.status()) != Some(ServiceStatus::Started) && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(10));
    }
    // the route has recovered: what is left running besides the context?
    std::thread::sleep(Duration::from_millis(1000));
    let n = threads().saturating_sub(base);
    ctx.stop().unwrap();
    n
}

/// Median and p99 of `f` in microseconds.
fn latency(n: usize, mut f: impl FnMut()) -> (f64, f64) {
    for _ in 0..n / 10 {
        f();
    }
    let mut v: Vec<f64> = (0..n)
        .map(|_| {
            let t = Instant::now();
            f();
            t.elapsed().as_secs_f64() * 1e6
        })
        .collect();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    (v[n / 2], v[n * 99 / 100])
}

/// Run `f` on a thread with the default (2 MiB) stack, like a route thread.
fn on_worker<T: Send>(f: impl FnOnce() -> T + Send) -> T {
    std::thread::scope(|s| std::thread::Builder::new().stack_size(2 << 20).spawn_scoped(s, f).unwrap().join().unwrap())
}

fn xml_latency() {
    use cameleer_xml::xquery::XQuery;
    use cameleer_xml::xslt::Stylesheet;
    const DOC: &str = "<orders><order id='1'><item qty='2' price='10.5'/><item qty='1' price='4'/></order><order id='2'><item qty='10' price='1'/></order></orders>";
    let q = XQuery::compile("sum(for $i in //item return $i/@qty * $i/@price)", &[]).unwrap();
    let ctxv = Value::from(DOC);
    let deep = XQuery::compile("declare function local:f($n) { if ($n = 0) then 0 else 1 + local:f($n - 1) }; local:f(2000)", &[]).unwrap();
    let xsl = Stylesheet::compile(
        r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform"><xsl:output omit-xml-declaration="yes"/><xsl:template match="/"><total><xsl:value-of select="count(//item)"/></total></xsl:template></xsl:stylesheet>"#,
        None,
    )
    .unwrap();
    let xsl_deep = Stylesheet::compile(
        r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform"><xsl:output omit-xml-declaration="yes"/><xsl:template match="/"><xsl:call-template name="r"><xsl:with-param name="n" select="2000"/></xsl:call-template></xsl:template><xsl:template name="r"><xsl:param name="n"/><xsl:if test="$n &gt; 0"><xsl:call-template name="r"><xsl:with-param name="n" select="$n - 1"/></xsl:call-template></xsl:if></xsl:template></xsl:stylesheet>"#,
        None,
    )
    .unwrap();
    let none = |_: &str, _: &str| -> Option<Value> { None };
    on_worker(|| {
        let xq = latency(5000, || {
            q.evaluate(Some(&ctxv), &none, |_, items| items.len()).unwrap();
        });
        let xs = latency(5000, || {
            xsl.transform_to_string(DOC, &[]).unwrap();
        });
        let xqd = latency(200, || {
            deep.evaluate(None, &none, |_, items| items.len()).unwrap();
        });
        let xsd = latency(200, || {
            xsl_deep.transform_to_string("<a/>", &[]).unwrap();
        });
        println!("xquery small query (route thread, 2 MiB): median {:.1} us, p99 {:.1} us", xq.0, xq.1);
        println!("xslt small transform (route thread, 2 MiB): median {:.1} us, p99 {:.1} us", xs.0, xs.1);
        println!("xquery recursion depth 2000: median {:.1} us, p99 {:.1} us", xqd.0, xqd.1);
        println!("xslt template recursion depth 2000: median {:.1} us, p99 {:.1} us", xsd.0, xsd.1);
    });
    let xq_main = latency(5000, || {
        q.evaluate(Some(&ctxv), &none, |_, items| items.len()).unwrap();
    });
    println!("xquery small query (main thread): median {:.1} us, p99 {:.1} us", xq_main.0, xq_main.1);
}

fn status_writes() -> usize {
    let dir = std::env::temp_dir().join(format!("cameleer-idle-threads-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:idle").route_id("idle").to("log:x?level=OFF");
    })
    .unwrap();
    ctx.start().unwrap();
    let ls = cameleer_main::local::LocalStatus::start(&ctx, "idle", &dir).unwrap();
    let file = cameleer_main::local::status_file(&dir, ls.pid());
    // count rewrites over 5 s by watching the file content's `updated` stamp
    let mut seen = std::collections::HashSet::new();
    let end = Instant::now() + Duration::from_secs(5);
    while Instant::now() < end {
        if let Ok(t) = std::fs::read_to_string(&file) {
            if let Some(i) = t.find("\"updated\"") {
                seen.insert(t[i..].chars().take(30).collect::<String>());
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    ls.stop();
    ctx.stop().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    seen.len()
}

fn main() {
    println!("os: {}", std::env::consts::OS);
    println!("threads at start: {}", threads());
    println!("20 timer routes, one timer name: {} threads", timer_threads(20, true));
    println!("20 timer routes, 20 timer names: {} threads", timer_threads(20, false));
    println!("1 timer route: {} threads", timer_threads(1, true));
    println!("supervising controller, 1 s after the route recovered: {} threads", supervising_threads());
    xml_latency();
    println!("status file writes in 5 s, idle context: {}", status_writes());
}
