import com.fasterxml.jackson.databind.DeserializationFeature;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.sun.net.httpserver.HttpServer;

import java.io.IOException;
import java.net.InetSocketAddress;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.*;
import java.util.concurrent.Executors;

public final class Main {
    static double median(List<Double> v) {
        List<Double> s = new ArrayList<>(v);
        Collections.sort(s);
        return s.get(s.size() / 2);
    }

    static String arg(String[] a, String name, String def) {
        for (int i = 0; i < a.length - 1; i++) if (a[i].equals(name)) return a[i + 1];
        return def;
    }

    static void evalAll(Rules pack, Rules.Claim[] claims, byte[] out, int threads) throws Exception {
        if (threads <= 1) {
            for (int i = 0; i < claims.length; i++) pack.evaluate(claims[i], out, i * 15);
            return;
        }
        int chunk = (claims.length + threads - 1) / threads;
        Thread[] ts = new Thread[threads];
        for (int t = 0; t < threads; t++) {
            final int lo = t * chunk, hi = Math.min(claims.length, (t + 1) * chunk);
            ts[t] = new Thread(() -> { for (int i = lo; i < hi; i++) pack.evaluate(claims[i], out, i * 15); });
            ts[t].start();
        }
        for (Thread t : ts) t.join();
    }

    public static void main(String[] args) throws Exception {
        String corpus = arg(args, "-corpus", "");
        int threads = Integer.parseInt(arg(args, "-threads", String.valueOf(Runtime.getRuntime().availableProcessors())));
        int repeat = Integer.parseInt(arg(args, "-repeat", "5"));
        String serve = arg(args, "-serve", "");

        Rules pack = new Rules(Files.readString(Path.of(corpus, "pack", "policies.json")), Files.readString(Path.of(corpus, "pack", "services.json")));
        ObjectMapper mapper = new ObjectMapper().configure(DeserializationFeature.FAIL_ON_UNKNOWN_PROPERTIES, false);

        if (!serve.isEmpty()) {
            String[] hp = serve.split(":");
            System.setProperty("sun.net.httpserver.maxIdleConnections", "100000");   // the default (200) closes keep-alive connections under load
            HttpServer server = HttpServer.create(new InetSocketAddress(hp[0], Integer.parseInt(hp[1])), 1024);
            server.createContext("/evaluate", ex -> {
                try {
                    byte[] body = ex.getRequestBody().readNBytes((1 << 20) + 1);
                    if (body.length > (1 << 20)) { ex.sendResponseHeaders(413, -1); ex.close(); return; }
                    Rules.Claim c;
                    byte[] o = new byte[15];
                    try {
                        c = mapper.readValue(body, Rules.Claim.class);
                        pack.evaluate(c, o, 0);
                    } catch (Exception e) { ex.sendResponseHeaders(400, -1); ex.close(); return; }
                    byte[] resp = ("{\"claim_id\":\"" + c.claimId + "\",\"statuses\":\"" + new String(o, StandardCharsets.US_ASCII) + "\"}").getBytes(StandardCharsets.UTF_8);
                    ex.getResponseHeaders().set("Content-Type", "application/json");
                    ex.sendResponseHeaders(200, resp.length);
                    ex.getResponseBody().write(resp);
                } finally { ex.close(); }
            });
            server.createContext("/healthz", ex -> { byte[] r = "ok".getBytes(); ex.sendResponseHeaders(200, r.length); ex.getResponseBody().write(r); ex.close(); });
            server.setExecutor(Executors.newVirtualThreadPerTaskExecutor());
            server.start();
            Thread.currentThread().join();
            return;
        }

        long t0 = System.nanoTime();
        List<String> lines = Files.readAllLines(Path.of(corpus, "claims.jsonl"));
        double readMs = (System.nanoTime() - t0) / 1e6;

        t0 = System.nanoTime();
        Rules.Claim[] claims = new Rules.Claim[lines.size()];
        for (int i = 0; i < claims.length; i++) claims[i] = mapper.readValue(lines.get(i), Rules.Claim.class);
        double parseMs = (System.nanoTime() - t0) / 1e6;

        byte[] out = new byte[claims.length * 15];
        evalAll(pack, claims, out, 1);
        List<String> expected = Files.readAllLines(Path.of(corpus, "expected.txt"));
        int mismatches = 0;
        for (int i = 0; i < expected.size() && i < claims.length; i++)
            if (!expected.get(i).equals(new String(out, i * 15, 15, StandardCharsets.US_ASCII))) mismatches++;

        List<Double> one = new ArrayList<>(), many = new ArrayList<>();
        for (int r = 0; r < repeat; r++) {
            long t = System.nanoTime(); evalAll(pack, claims, out, 1); one.add((System.nanoTime() - t) / 1e6);
            t = System.nanoTime(); evalAll(pack, claims, out, threads); many.add((System.nanoTime() - t) / 1e6);
        }
        System.out.println("{\"lang\":\"java\",\"version\":\"" + System.getProperty("java.version") + "\",\"claims\":" + claims.length + ",\"mismatches\":" + mismatches
                + ",\"threads\":" + threads + ",\"read_ms\":" + readMs + ",\"parse_ms\":" + parseMs + ",\"eval_1t_ms\":" + median(one) + ",\"eval_mt_ms\":" + median(many) + "}");
    }
}
