package bench;

import java.util.concurrent.atomic.AtomicLong;

import javax.sql.DataSource;

import io.quarkus.runtime.QuarkusApplication;
import io.quarkus.runtime.annotations.QuarkusMain;
import jakarta.enterprise.inject.Instance;
import jakarta.inject.Inject;
import org.apache.camel.CamelContext;

/**
 * Apache Camel on Quarkus (native): the same commands and routes as {@link Bench}, on the
 * CamelContext that Camel Quarkus creates and starts. Startup is measured from process spawn to
 * the READY line, like the other sides, so it includes Quarkus and Camel booting.
 *
 * <pre>
 * run &lt;scenario&gt; &lt;warmup_s&gt; &lt;iter_s&gt; &lt;iters&gt;      (also kafka-stream)
 * serve &lt;scenario&gt; &lt;port&gt; &lt;secs&gt;
 * startup
 * </pre>
 */
@QuarkusMain
public class QuarkusBench implements QuarkusApplication {
    @Inject
    CamelContext ctx;

    @Inject
    Instance<DataSource> dataSource;

    @Override
    public int run(String... args) throws Exception {
        // Camel Quarkus starts its context with the application only when Camel owns the main
        // loop; with our own main it may still be stopped here
        if (!ctx.getStatus().isStarted()) {
            ctx.start();
        }
        String cmd = args.length > 0 ? args[0] : "";
        switch (cmd) {
            case "run" -> {
                AtomicLong sink = new AtomicLong();
                if (args[1].equals("kafka-stream")) {
                    Bench.addStreamRoute(ctx, sink);
                } else {
                    Bench.addRoutes(ctx, sink);
                }
                version();
                System.out.println("READY");
                double warmup = Double.parseDouble(args[2]);
                double iter = Double.parseDouble(args[3]);
                int iters = Integer.parseInt(args[4]);
                Bench.Outcome out = args[1].equals("kafka-stream")
                        ? Bench.Driver.run(warmup, iter, iters, 1, sink, i -> () -> Thread.sleep(20))
                        : Bench.run(ctx, sink, args[1], warmup, iter, iters);
                System.out.println(out.resultLine(args[1]));
            }
            case "serve" -> {
                if (args[1].equals("http-sql")) {
                    ctx.getRegistry().bind("ds", dataSource.get());
                }
                Bench.addServerRoute(ctx, args[1], Integer.parseInt(args[2]));
                version();
                System.out.println("READY");
                Thread.sleep((long) (Double.parseDouble(args[3]) * 1000));
            }
            case "startup" -> {
                Bench.addRoutes(ctx, new AtomicLong());
                Bench.addHttpRoute(ctx, Bench.freePort());
                System.out.println("READY");
                version();
            }
            default -> {
                System.err.println("usage: run|serve|startup");
                return 1;
            }
        }
        System.out.flush();
        return 0;
    }

    void version() {
        System.out.println("VERSION impl=camel-quarkus-native camel=" + ctx.getVersion()
                + " vm=" + System.getProperty("java.vm.name", "native").replace(' ', '_')
                + " max_heap_mb=" + Runtime.getRuntime().maxMemory() / (1024 * 1024));
    }
}
