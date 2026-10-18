# 54 | Handling traffic with nginx: what a proxy in front of ClaimGuard has to do, a tested configuration, and what it costs

Written 2026-10-18 after the browser test of the batch work (docs/53). The question: how does nginx handle the traffic in front of the Rust server, and is it the right proxy?

**Read this first.** Everything here was measured on one Windows laptop with Docker Desktop, the demo server, one python client and synthetic claims. Docker Desktop puts a virtual-machine hop between the proxy container and the server that a Linux machine does not have, so the *cost* numbers (section 6) overstate what a proxy costs on the target server. No real hospital traffic, no real certificate, and no Linux machine was involved.

## 1. What the application expects of whatever sits in front of it

| Fact (where it is in the code) | What the proxy must do |
|---|---|
| TLS is not served by the application; it refuses to listen on a non-local address without `--behind-proxy` (`cg-cli/serve.rs`) | End TLS, redirect port 80 |
| With `--behind-proxy` the client address for the sign-in throttle is the **last** hop of `X-Forwarded-For` (`cg-api/server.rs`) | **Overwrite** the header with the address it saw. If it appended to what the client sent, the last hop would still be right; if it passed the header through, a client could pick its own address and escape the throttle |
| Body limits in the application: 64 KB on every route, 4 MB on `/api/v1/queue/submit` (a batch of up to 1,000 claims), 16 MB on the two import routes (`cg-api/lib.rs`) | Allow at least that on those paths, and refuse more before it reaches the application |
| The live-update stream (`/api/v1/work/events`) sends a short message every few seconds and **polls the ordinary authenticated route** on the server side; it holds no state shared between processes | Never buffer or compress it |
| Taking in a batch of 1,000 claims is one request that lasts tens of seconds (17 s on a release build here, 22 s on a debug build) | A read timeout above that on that one path |

## 2. The configuration: `deploy/nginx/nginx.conf`

It does the same jobs as `deploy/Caddyfile.internal`, in nginx's words: TLS 1.2 and 1.3 with HSTS; port 80 redirects; header 10 s, body 30 s, idle 2 min, header lines at most 8 KB; 100 KB by default and 4 MB / 17 MB on the three paths that need more; the stream unbuffered; gzip for JSON and the console files; `X-Forwarded-For` overwritten with `$remote_addr`; keep-alive connections to the application; clean JSON bodies for the 413, 429 and 503 that nginx itself produces; and two things the Caddyfile does not have: a **rate limit** (30 requests a second per address after a burst of 60 on ordinary routes; ten a minute after a burst of five on sign-in) and a TLS session cache.

Two small differences to Caddy: nginx cannot remove the `Server: nginx` word without a module (the version number is hidden), and nginx does not get a certificate by itself (Caddy's automatic certificate is the one thing it does better for a single machine; docs/40).

## 3. What was tested (`rust/tools/nginx_proxy_test.py`, 22 of 22 passed, `outputs/defense/nginx_proxy_test.json`)

Real `nginx:1.27-alpine` in Docker, the real server on the host, a throwaway certificate, over TLS.

- **Headers (against a tiny echo server in place of the application).** What the client wrote in `X-Forwarded-For` (`6.6.6.6, 7.7.7.7`), `X-Real-IP` and `X-Forwarded-Proto` is thrown away; the application is told one address, the one nginx saw, and `https`. `Host` is passed on.
- **Limits.** 120 KB to an ordinary path: refused at the proxy with `{"error":"payload_too_large"}`. 50 KB: reaches the application (which itself refuses over 64 KB). 5 MB to submit: refused. 18 MB to an import path: refused; 1 MB reaches the application.
- **A batch of 1,000 claims (1.4 MB) goes through nginx in one request** (21 s, debug build), is one batch, and the audit sample is exactly 5%.
- **The stream.** Opens as `text/event-stream`, "hello" within 2 s, not compressed even when the client asks, and an ordinary request is answered while it is open.
- **Slow clients.** A client that stalls in the middle of its header is cut off after 9.6 s (the setting is 10).
- **Rate limits.** 40 ordinary requests in a row are all answered. A flood of 400 (20 at a time) gets 73 answers and 327 `429`s, and service resumes four seconds later. Fourteen wrong sign-ins from one address: the first ones reach the application (401), the rest are `429` with `Retry-After`, as JSON.
- **Failure.** With the application stopped, the answer is a JSON `503`, not nginx's own HTML page.
- **Also:** `nginx -t` accepts the file; HSTS is sent; the console files are gzipped; port 80 redirects.

Static tests (`tests/test_deploy_rust_files.py`, `tests/test_deploy_k8s.py`) now keep every proxy's body limit at or above the application's, so the two cannot drift apart again.

## 4. A real fault this study found

Making a submission one batch (docs/53) raised the application's limit on the submit route from 64 KB to 4 MB. **Every proxy in the repository capped bodies at 100 KB**: a batch of more than about 65 claims would have been refused with a 413 behind Caddy or the Kubernetes ingress, although every test against the server directly passed. Fixed: `@submit` (4 MB) in both Caddyfiles, and in `deploy/k8s/claimguard.yaml` two further Ingress objects (`claimguard-submit`, `claimguard-import`), because ingress-nginx sets the body size per Ingress object, not per path.

The same read of the Kubernetes file showed that **the 17 MB file imports were never allowed through the ingress at all** (100 KB for everything): importing a scanned page or an FHIR export on a cluster would have failed. Fixed by the same change.

## 5. Which proxy

| | Caddy (the Compose stack today) | nginx (this study) | ingress-nginx (the Kubernetes file today) |
|---|---|---|---|
| Certificates | automatic (Let's Encrypt, or its own authority) | you provide them (or cert-manager on Kubernetes) | cert-manager or a Secret |
| Rate limiting | none configured | built in (`limit_req`), tested here | annotations (`limit-rps`, `limit-connections`) |
| Per-path body limit | yes, in one file | yes, in one file | per Ingress object, so one object per size |
| Request lines and timeouts | yes | yes | annotations |
| Load balancing over several API copies | yes | yes (`upstream`, round robin, least connections, `ip_hash`) | yes (service endpoints) |
| `Server` header | removed | word stays, no version | word stays |
| Known to hospital IT | less | very | yes |

**Recommendation.** Keep Caddy for the single-machine Compose install, where its automatic certificate is the point; keep ingress-nginx on Kubernetes (it already is nginx); offer `deploy/nginx/nginx.conf` for a site whose IT already runs nginx and wants rate limiting at the edge. There is no performance reason to move from one to the other (section 6). Adding `limit_req`-style limits to the Caddy side would need a plugin; if the edge must rate limit and the site has no preference, nginx is the simpler way to get it.

## 6. What it costs (`rust/tools/proxy_load.py`, `outputs/defense/proxy_load.json`)

The same release server, 16 keep-alive connections, about 3,000 requests each. The proxies speak TLS to the client; "direct" is plain HTTP, so the proxy rows include TLS. nginx ran with its rate-limit lines removed.

| Request | direct | nginx | Caddy |
|---|---|---|---|
| `/healthz` (no work) | 6,200/s, p50 2.1 ms | 3,100/s, p50 4.7 ms | 990/s, p50 4.9 ms |
| signed-in inbox | 421/s, p50 37 ms | 215/s, p50 71 ms | 180/s, p50 76 ms |
| console file, gzip | 5,850/s, p50 2.4 ms | 2,375/s, p50 6.2 ms | 900/s, p50 6.2 ms |

How to read it, and what not to:
- **Latency, not throughput, is the comparison.** On a request that does no work, nginx and Caddy add about the same (about 2.5 ms to the median). Caddy's lower requests-a-second at the same latency is not explained: with one python client as the ceiling I do not trust the throughput column, and I did not find the cause.
- **The signed-in inbox goes from 37 to 71 ms through either proxy.** That is far more than the 2.5 ms a trivial request pays, and the same for both proxies, so it is not one proxy's fault. I did not establish why; Docker Desktop's network hop is the first suspect and a Linux run is needed to say.
- **The application, not the proxy, is the limit.** 421 signed-in requests a second from one process is about a hundred times the 200 requests a minute the target-server plan asks for. The proxy matters here for protection (limits, slow clients, TLS), not for speed.

## 7. More than one copy of the API behind nginx

`upstream api { server api1:8443; server api2:8443; }` is all nginx needs. What decides whether it is safe is the application:
- The live-update stream holds no state shared between processes, so **no sticky sessions** are needed for it.
- Sessions and the sign-in counters go through the store (`cg-access`, `store.counter`), which is MongoDB in a real deployment, so two API processes share them **by design**. This was read from the code. **It was not tested:** the demo server keeps its state in its own files, two demo copies do not share a session, and a test needs the production mode (MongoDB and the keys it demands), which this study did not run.
- Exactly one worker must run (the relay, deal and expire loops); more API copies do not change that.
- A batch intake is one long request on one copy. Two copies taking in two batches at once is a case no test covers.

## 8. What this does not settle

- No Linux run, no real certificate, no real traffic. Section 6 is a laptop comparison.
- The long request (now removed for the console, 2026-10-18): intake can be asynchronous (`"async": true`, docs/53 section 7): the request is answered in about 0.06 s with a job id and the 17 s of work happens in the background, so a gateway that cuts requests at 15 s no longer matters. The synchronous call still exists for the command line and old clients and still needs the 120 s read timeout.
- Rate limits are per address. Behind a hospital's own gateway or NAT, many officers share one address and one limit; the numbers (30 a second, ten sign-ins a minute) are a starting point, chosen without measuring real use.
- The stream's `limit_req` counts each reconnect; a console that reconnects in a loop could be limited. The stream was tested for being unbuffered, not for a reconnect storm.
- Multiple API copies: untested (section 7).
