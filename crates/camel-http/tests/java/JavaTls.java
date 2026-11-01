// JSSE peer for the live interop suite (tests/live_java.rs), run with the JDK's single-file
// source launcher: `java JavaTls.java server|client ...`.
//
//   server <port> <keystore.p12> <password> <TLSv1.3|TLSv1.2>
//       HTTPS server (com.sun.net.httpserver) echoing POST bodies on /echo with the negotiated
//       TLS protocol and cipher suite in X-Tls / X-Cipher; prints READY, stops when stdin closes.
//   client <url> <truststore.p12> <password> <HTTP_2|HTTP_1_1> [body]
//       java.net.http.HttpClient request; prints "<status> <version> <X-Tls> <body>".
import com.sun.net.httpserver.HttpsConfigurator;
import com.sun.net.httpserver.HttpsExchange;
import com.sun.net.httpserver.HttpsParameters;
import com.sun.net.httpserver.HttpsServer;
import java.io.FileInputStream;
import java.io.OutputStream;
import java.net.InetSocketAddress;
import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.nio.charset.StandardCharsets;
import java.security.KeyStore;
import javax.net.ssl.KeyManagerFactory;
import javax.net.ssl.SSLContext;
import javax.net.ssl.SSLParameters;
import javax.net.ssl.TrustManagerFactory;

public class JavaTls {
    public static void main(String[] a) throws Exception {
        if (a[0].equals("server")) {
            server(Integer.parseInt(a[1]), a[2], a[3], a[4]);
        } else {
            client(a[1], a[2], a[3], a[4], a.length > 5 ? a[5] : null);
        }
    }

    static KeyStore load(String file, String pw) throws Exception {
        KeyStore k = KeyStore.getInstance("PKCS12");
        try (FileInputStream in = new FileInputStream(file)) {
            k.load(in, pw.toCharArray());
        }
        return k;
    }

    static void server(int port, String ks, String pw, String proto) throws Exception {
        KeyManagerFactory kmf = KeyManagerFactory.getInstance(KeyManagerFactory.getDefaultAlgorithm());
        kmf.init(load(ks, pw), pw.toCharArray());
        SSLContext c = SSLContext.getInstance("TLS");
        c.init(kmf.getKeyManagers(), null, null);
        HttpsServer s = HttpsServer.create(new InetSocketAddress("127.0.0.1", port), 0);
        s.setHttpsConfigurator(new HttpsConfigurator(c) {
            @Override
            public void configure(HttpsParameters p) {
                SSLParameters sp = c.getDefaultSSLParameters();
                sp.setProtocols(new String[] {proto});
                p.setSSLParameters(sp);
            }
        });
        s.createContext("/echo", ex -> {
            byte[] body = ex.getRequestBody().readAllBytes();
            var session = ((HttpsExchange) ex).getSSLSession();
            ex.getResponseHeaders().add("X-Tls", session.getProtocol());
            ex.getResponseHeaders().add("X-Cipher", session.getCipherSuite());
            ex.getResponseHeaders().add("X-Method", ex.getRequestMethod());
            ex.getResponseHeaders().add("Content-Type", "text/plain; charset=UTF-8");
            ex.sendResponseHeaders(200, body.length == 0 ? -1 : body.length);
            try (OutputStream out = ex.getResponseBody()) {
                out.write(body);
            }
        });
        s.start();
        System.out.println("READY");
        System.out.flush();
        while (System.in.read() >= 0) {
            // run until the test closes stdin
        }
        s.stop(0);
    }

    static void client(String url, String ts, String pw, String version, String body) throws Exception {
        TrustManagerFactory tmf = TrustManagerFactory.getInstance(TrustManagerFactory.getDefaultAlgorithm());
        tmf.init(load(ts, pw));
        SSLContext c = SSLContext.getInstance("TLS");
        c.init(null, tmf.getTrustManagers(), null);
        HttpClient hc = HttpClient.newBuilder().sslContext(c).version(HttpClient.Version.valueOf(version)).build();
        HttpRequest.Builder b = HttpRequest.newBuilder(URI.create(url));
        if (body != null) {
            b.POST(HttpRequest.BodyPublishers.ofString(body, StandardCharsets.UTF_8)).header("Content-Type", "text/plain; charset=UTF-8");
        }
        HttpResponse<String> r = hc.send(b.build(), HttpResponse.BodyHandlers.ofString(StandardCharsets.UTF_8));
        System.out.println(r.statusCode() + " " + r.version() + " " + r.headers().firstValue("X-Tls").orElse("-") + " " + r.body());
    }
}
