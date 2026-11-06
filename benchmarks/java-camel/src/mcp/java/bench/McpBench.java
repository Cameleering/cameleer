package bench;

import org.apache.camel.builder.RouteBuilder;
import org.apache.camel.main.Main;

/**
 * The mcp-session scenario on Apache Camel: camel-main with camel-mcp-server (Streamable HTTP on
 * the main HTTP server) and one ai-tool route, the same tool as crates/cameleer-bench/src/ai.rs.
 * Kept out of src/main/java so the Quarkus native build, which compiles those sources, needs no
 * MCP dependencies; Bench loads this class by name.
 */
public final class McpBench {
    private McpBench() {
    }

    public static void serve(int port, double secs) throws Exception {
        Main main = new Main();
        main.configure().addRoutesBuilder(new RouteBuilder() {
            @Override
            public void configure() {
                from("ai-tool:lookup_order?tags=bench&description=Look up an order"
                        + "&parameter.orderId=integer&parameter.orderId.required=true&readOnlyHint=true")
                        .routeId("ai-tool")
                        .setBody(simple("{\"orderId\":${header.orderId},\"status\":\"shipped\"}"));
            }
        });
        main.addInitialProperty("camel.server.enabled", "true");
        main.addInitialProperty("camel.server.host", "0.0.0.0");
        main.addInitialProperty("camel.server.port", String.valueOf(port));
        main.addInitialProperty("camel.server.mcp-enabled", "true");
        main.addInitialProperty("camel.server.mcp-tags", "bench");
        main.start();
        Bench.version(main.getCamelContext());
        System.out.println("READY");
        System.out.flush();
        Thread.sleep((long) (secs * 1000));
        main.stop();
    }
}
