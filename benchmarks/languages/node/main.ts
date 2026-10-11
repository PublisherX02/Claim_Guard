import { readFileSync } from 'node:fs';
import { performance } from 'node:perf_hooks';
import { availableParallelism } from 'node:os';
import { Worker, isMainThread, parentPort, workerData } from 'node:worker_threads';
import cluster from 'node:cluster';
import http from 'node:http';
import { Pack, parseClaim } from './rules.ts';

function arg(name: string, def: string): string {
  const a = process.argv, i = a.indexOf(name);
  return i >= 0 && i + 1 < a.length ? a[i + 1] : def;
}
const median = (v: number[]) => [...v].sort((a, b) => a - b)[Math.floor(v.length / 2)];
const corpus = arg('-corpus', '');
const threads = parseInt(arg('-threads', String(availableParallelism())));
const repeat = parseInt(arg('-repeat', '5'));
const serve = arg('-serve', '');
const pack = new Pack(readFileSync(`${corpus}/pack/policies.json`, 'utf8'), readFileSync(`${corpus}/pack/services.json`, 'utf8'));

if (!isMainThread) {
  // Worker: parse its slice once, then evaluate on demand and report the time spent.
  const claims = (workerData.lines as string[]).map(parseClaim);
  const out = new Uint8Array(claims.length * 15);
  parentPort!.postMessage({ ready: true });
  parentPort!.on('message', () => {
    const t = performance.now();
    for (let i = 0; i < claims.length; i++) pack.evaluate(claims[i], out, i * 15);
    parentPort!.postMessage({ done: performance.now() - t });
  });
} else if (serve) {
  const [host, port] = serve.split(':');
  if (cluster.isPrimary) {
    for (let i = 0; i < threads; i++) cluster.fork();
  } else {
    http.createServer((req, res) => {
      if (req.method === 'GET' && req.url === '/healthz') { res.end('ok'); return; }
      if (req.method !== 'POST' || req.url !== '/evaluate') { res.statusCode = 404; res.end(); return; }
      const chunks: Buffer[] = []; let size = 0;
      req.on('data', (b: Buffer) => { size += b.length; if (size > (1 << 20)) { res.statusCode = 413; res.end(); req.destroy(); } else chunks.push(b); });
      req.on('end', () => {
        if (res.writableEnded) return;
        try {
          const c = parseClaim(Buffer.concat(chunks).toString('utf8'));
          const o = new Uint8Array(15);
          pack.evaluate(c, o, 0);
          res.setHeader('Content-Type', 'application/json');
          res.end(`{"claim_id":"${c.claim_id}","statuses":"${Buffer.from(o).toString('ascii')}"}`);
        } catch { res.statusCode = 400; res.end(); }
      });
    }).listen(parseInt(port), host);
  }
} else {
  let t = performance.now();
  const lines = readFileSync(`${corpus}/claims.jsonl`, 'utf8').split('\n').filter(Boolean);
  const readMs = performance.now() - t;
  t = performance.now();
  const claims = lines.map(parseClaim);
  const parseMs = performance.now() - t;

  const out = new Uint8Array(claims.length * 15);
  const run1 = () => { for (let i = 0; i < claims.length; i++) pack.evaluate(claims[i], out, i * 15); };
  run1();
  const expected = readFileSync(`${corpus}/expected.txt`, 'utf8').split('\n').filter(Boolean);
  let mismatches = 0;
  for (let i = 0; i < expected.length && i < claims.length; i++) if (expected[i] !== Buffer.from(out.subarray(i * 15, i * 15 + 15)).toString('ascii')) mismatches++;

  const one: number[] = [];
  for (let r = 0; r < repeat; r++) { const s = performance.now(); run1(); one.push(performance.now() - s); }

  // multi-thread: worker_threads, each owns a slice parsed beforehand
  const chunk = Math.ceil(lines.length / threads);
  const workers: Worker[] = [];
  for (let k = 0; k < threads; k++) {
    workers.push(new Worker(new URL(import.meta.url), { argv: process.argv.slice(2), workerData: { lines: lines.slice(k * chunk, (k + 1) * chunk) } }));
  }
  const waitAll = (key: string) => Promise.all(workers.map((w) => new Promise<void>((resolve) => w.once('message', (m: any) => { if (key in m) resolve(); }))));
  await waitAll('ready');
  const many: number[] = [];
  for (let r = 0; r < repeat; r++) {
    const s = performance.now();
    const p = waitAll('done');
    for (const w of workers) w.postMessage(1);
    await p;
    many.push(performance.now() - s);
  }
  for (const w of workers) await w.terminate();
  console.log(JSON.stringify({ lang: 'node', version: process.version, claims: claims.length, mismatches, threads, read_ms: readMs, parse_ms: parseMs, eval_1t_ms: median(one), eval_mt_ms: median(many) }));
}
