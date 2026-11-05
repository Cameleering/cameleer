package bench;

import java.io.IOException;
import java.lang.management.GarbageCollectorMXBean;
import java.lang.management.ManagementFactory;
import java.net.InetSocketAddress;
import java.net.ServerSocket;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.concurrent.atomic.AtomicLong;
import java.util.function.IntFunction;
import java.util.stream.Collectors;

import org.apache.camel.CamelContext;
import org.apache.camel.Processor;
import org.apache.camel.ProducerTemplate;
import org.apache.camel.builder.RouteBuilder;
import org.apache.camel.impl.DefaultCamelContext;
import org.apache.camel.processor.aggregate.GroupedBodyAggregationStrategy;

/**
 * Apache Camel side of the benchmark suite. Same commands, routes, driver and output records
 * as crates/cameleer-bench (see its src/main.rs, scenarios.rs and driver.rs):
 *
 * <pre>
 * run &lt;scenario&gt; &lt;warmup_s&gt; &lt;iter_s&gt; &lt;iters&gt;
 * http-server &lt;port&gt;
 * startup
 * </pre>
 */
public final class Bench {
    static final int SEDA_PRODUCERS = 4;
    static final int ITEMS = 10;

    public static void main(String[] args) throws Exception {
        String cmd = args.length > 0 ? args[0] : "";
        switch (cmd) {
            case "run" -> {
                AtomicLong sink = new AtomicLong();
                CamelContext ctx = new DefaultCamelContext();
                addRoutes(ctx, sink);
                ctx.start();
                version(ctx);
                System.out.println("READY");
                double warmup = Double.parseDouble(args[2]);
                double iter = Double.parseDouble(args[3]);
                int iters = Integer.parseInt(args[4]);
                Outcome out = run(ctx, sink, args[1], warmup, iter, iters);
                System.out.println(out.resultLine(args[1]));
                ctx.stop();
            }
            case "http-server" -> {
                CamelContext ctx = new DefaultCamelContext();
                addHttpRoute(ctx, Integer.parseInt(args[1]));
                ctx.start();
                version(ctx);
                System.out.println("READY");
                // serve until the parent closes stdin
                while (System.in.read() >= 0) {
                    // discard
                }
                ctx.stop();
            }
            case "startup" -> {
                AtomicLong sink = new AtomicLong();
                CamelContext ctx = new DefaultCamelContext();
                addRoutes(ctx, sink);
                addHttpRoute(ctx, freePort());
                ctx.start();
                System.out.println("READY");
                version(ctx);
                ctx.stop();
            }
            default -> {
                System.err.println("usage: run|http-server|startup");
                System.exit(1);
            }
        }
        System.out.flush();
        System.exit(0);
    }

    static void version(CamelContext ctx) {
        String gcs = ManagementFactory.getGarbageCollectorMXBeans().stream()
                .map(GarbageCollectorMXBean::getName).collect(Collectors.joining("+")).replace(' ', '_');
        System.out.println("VERSION impl=apache-camel camel=" + ctx.getVersion()
                + " java=" + System.getProperty("java.runtime.version")
                + " vm=" + System.getProperty("java.vm.name").replace(' ', '_')
                + " gc=" + gcs
                + " max_heap_mb=" + Runtime.getRuntime().maxMemory() / (1024 * 1024));
    }

    static int freePort() throws IOException {
        try (ServerSocket s = new ServerSocket()) {
            s.bind(new InetSocketAddress("127.0.0.1", 0));
            return s.getLocalPort();
        }
    }

    static Processor counter(AtomicLong n) {
        return ex -> n.incrementAndGet();
    }

    static String orderJson(long id) {
        List<String> items = new ArrayList<>();
        for (int i = 0; i < ITEMS; i++) {
            items.add("{\"sku\":\"SKU-" + i + "\",\"qty\":" + (i + 1) + ",\"price\":" + (i + 3) + ".99}");
        }
        return "{\"orderId\":" + id + ",\"customer\":\"ACME\",\"items\":[" + String.join(",", items) + "]}";
    }

    static void addHttpRoute(CamelContext ctx, int port) throws Exception {
        ctx.addRoutes(new RouteBuilder() {
            @Override
            public void configure() {
                from("netty-http:http://127.0.0.1:" + port + "/bench")
                        .routeId("http")
                        .setHeader("Content-Type", constant("application/json"))
                        .setBody(constant("{\"status\":\"ok\"}"));
            }
        });
    }

    static void addRoutes(CamelContext ctx, AtomicLong sink) throws Exception {
        ctx.addRoutes(new RouteBuilder() {
            @Override
            public void configure() {
                // 1. in-process routing overhead
                from("direct:direct")
                        .routeId("direct")
                        .process(ex -> ex.getMessage().setHeader("seen", true))
                        .process(counter(sink));

                // 2. seda hand-off with concurrent consumers
                from("seda:seda?concurrentConsumers=4&size=1000&blockWhenFull=true")
                        .routeId("seda")
                        .process(ex -> ex.getMessage().setHeader("seen", true))
                        .process(counter(sink));
                from("seda:inout?concurrentConsumers=4")
                        .routeId("seda-inout")
                        .process(ex -> ex.getMessage().setHeader("seen", true))
                        .process(counter(sink));

                // 3. content-based router, Simple filter, setHeader and transform
                from("direct:cbr")
                        .routeId("cbr")
                        .choice()
                            .when(simple("${header.type} == 'gold'"))
                                .setHeader("tier", constant("A"))
                            .when(simple("${header.type} == 'silver'"))
                                .setHeader("tier", constant("B"))
                            .otherwise()
                                .setHeader("tier", constant("C"))
                        .end()
                        .filter(simple("${header.amount} > 100"))
                            .transform(simple("${body}-${header.tier}"))
                            .process(counter(sink))
                        .end();

                // 4. splitter + aggregator over JSON
                from("direct:splitagg")
                        .routeId("splitagg")
                        .setHeader("orderId", jsonpath("$.orderId"))
                        .split(jsonpath("$.items"))
                            .aggregate(header("orderId"), new GroupedBodyAggregationStrategy())
                                .completionSize(ITEMS)
                                .process(counter(sink))
                            .end()
                        .end();
            }
        });
    }

    interface Op {
        void call() throws Exception;
    }

    static Outcome run(CamelContext ctx, AtomicLong sink, String scenario, double warmup, double iter, int iters)
            throws Exception {
        ProducerTemplate t = ctx.createProducerTemplate();
        return switch (scenario) {
            case "direct" -> Driver.run(warmup, iter, iters, 1, null, i -> () -> t.sendBody("direct:direct", "hello world"));
            case "seda" -> Driver.run(warmup, iter, iters, SEDA_PRODUCERS, sink,
                    i -> () -> t.sendBody("seda:seda?blockWhenFull=true", "hello world"));
            case "seda-inout" -> Driver.run(warmup, iter, iters, SEDA_PRODUCERS, null,
                    i -> () -> t.requestBody("seda:inout", "hello world"));
            case "cbr" -> Driver.run(warmup, iter, iters, 1, null, i -> new Op() {
                long n;

                @Override
                public void call() {
                    String ty = new String[] {"gold", "silver", "bronze"}[(int) (n % 3)];
                    int amount = new int[] {50, 150, 250}[(int) ((n % 3 + n / 3 % 3) % 3)];
                    n++;
                    Map<String, Object> headers = new HashMap<>();
                    headers.put("type", ty);
                    headers.put("amount", amount);
                    t.sendBodyAndHeaders("direct:cbr", "order", headers);
                }
            });
            case "splitagg" -> Driver.run(warmup, iter, iters, 1, null, i -> new Op() {
                long n;

                @Override
                public void call() {
                    n++;
                    t.sendBody("direct:splitagg", orderJson(n));
                }
            });
            default -> throw new IllegalArgumentException("unknown scenario: " + scenario);
        };
    }

    static final class Outcome {
        long msgs;
        double secs;
        final List<Double> perIter = new ArrayList<>();
        final Histogram latency = new Histogram();

        double throughput() {
            return secs > 0 ? msgs / secs : 0;
        }

        String resultLine(String scenario) {
            double min = perIter.stream().mapToDouble(d -> d).min().orElse(0);
            double max = perIter.stream().mapToDouble(d -> d).max().orElse(0);
            boolean hasLat = latency.count() > 0;
            java.util.function.DoubleFunction<String> lat =
                    ns -> hasLat ? String.format(Locale.ROOT, "%.2f", ns / 1000.0) : "-";
            return String.format(Locale.ROOT,
                    "RESULT scenario=%s msgs=%d secs=%.3f tput=%.0f tput_min=%.0f tput_max=%.0f mean_us=%s p50_us=%s p99_us=%s max_us=%s",
                    scenario, msgs, secs, throughput(), min, max,
                    lat.apply(latency.mean()), lat.apply(latency.percentile(50)),
                    lat.apply(latency.percentile(99)), lat.apply(latency.max()));
        }
    }

    /** The same epoch-based closed-loop driver as crates/cameleer-bench/src/driver.rs. */
    static final class Driver {
        static final int STOP = Integer.MAX_VALUE;

        static Outcome run(double warmup, double iter, int iters, int workers, AtomicLong completions,
                IntFunction<Op> makeWorker) throws Exception {
            int epochs = iters + 1;
            java.util.concurrent.atomic.AtomicInteger epoch = new java.util.concurrent.atomic.AtomicInteger(0);
            Histogram[][] hists = new Histogram[workers][epochs];
            Thread[] threads = new Thread[workers];
            Throwable[] failure = new Throwable[1];
            for (int w = 0; w < workers; w++) {
                Op op = makeWorker.apply(w);
                Histogram[] mine = hists[w];
                for (int e = 0; e < epochs; e++) {
                    mine[e] = new Histogram();
                }
                threads[w] = new Thread(() -> {
                    try {
                        while (true) {
                            int e = epoch.getOpaque();
                            if (e == STOP) {
                                break;
                            }
                            long t0 = System.nanoTime();
                            op.call();
                            mine[e].record(System.nanoTime() - t0);
                        }
                    } catch (Throwable t) {
                        failure[0] = t;
                        t.printStackTrace();
                    }
                }, "bench-worker-" + w);
                threads[w].start();
            }
            long[] boundaries = new long[epochs + 1];
            long[] marks = new long[epochs + 1];
            Thread.sleep((long) (warmup * 1000));
            for (int e = 1; e < epochs; e++) {
                boundaries[e - 1] = System.nanoTime();
                marks[e - 1] = completions != null ? completions.get() : 0;
                epoch.setOpaque(e);
                Thread.sleep((long) (iter * 1000));
            }
            boundaries[epochs - 1] = System.nanoTime();
            marks[epochs - 1] = completions != null ? completions.get() : 0;
            epoch.setOpaque(STOP);
            for (Thread th : threads) {
                th.join();
            }
            if (failure[0] != null) {
                throw new IllegalStateException("bench worker failed", failure[0]);
            }
            Outcome out = new Outcome();
            for (int e = 1; e < epochs; e++) {
                double secs = (boundaries[e] - boundaries[e - 1]) / 1e9;
                Histogram h = new Histogram();
                for (int w = 0; w < workers; w++) {
                    h.merge(hists[w][e]);
                }
                long n = completions != null ? marks[e] - marks[e - 1] : h.count();
                out.msgs += n;
                out.secs += secs;
                out.perIter.add(n / secs);
                if (completions == null) {
                    out.latency.merge(h);
                }
            }
            return out;
        }
    }
}
