"""Run the language benchmark end to end and write results/results.json.

    python benchmarks/languages/run_bench.py [--batch-runs 5] [--load-seconds 8] [--only go,rust]

Per language: clean build (time, artifact size), batch runs (parse + evaluate 20,000 claims, parity with the oracle, peak memory),
then the HTTP service (cold start, throughput and latency at several concurrencies with every answer checked, hostile-input probe).
Run it on a quiet machine: the numbers are only comparable inside one run.
"""
import argparse, json, os, platform, shutil, statistics, subprocess, sys, time, urllib.request
from pathlib import Path

import psutil

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
CORPUS = HERE / 'corpus'
PY = str(ROOT / '.venv' / 'Scripts' / 'python.exe')
GO = r'C:\Users\moham\tools\go\bin\go.exe'
CARGO = r'C:\Users\moham\.cargo\bin\cargo.exe'
ENV = dict(os.environ, PATH=r'C:\Users\moham\.cargo\bin;C:\msys64\ucrt64\bin;C:\Users\moham\tools\go\bin;' + os.environ['PATH'])
CPUS = os.cpu_count()

LANGS = {
    'rust': dict(label='Rust', cwd='rust', clean=lambda: shutil.rmtree(HERE / 'rust' / 'target', ignore_errors=True),
                 build=[CARGO, '+stable-x86_64-pc-windows-gnu', 'build', '--release'], artifact=['rust/target/release/bench.exe'],
                 run=[str(HERE / 'rust/target/release/bench.exe')]),
    'go': dict(label='Go', cwd='go', clean=lambda: None, build=[GO, 'build', '-a', '-o', 'bench.exe', '.'], artifact=['go/bench.exe'],
               run=[str(HERE / 'go/bench.exe')]),
    'java': dict(label='Java', cwd='java', clean=lambda: shutil.rmtree(HERE / 'java' / 'out', ignore_errors=True),
                 build=['javac', '-d', 'out', '-cp', 'lib/*', 'src/Rules.java', 'src/Main.java'], artifact=['java/out', 'java/lib'],
                 run=['java', '-cp', f'{HERE / "java/out"};{HERE / "java/lib"}/*', 'Main']),
    'csharp': dict(label='C# (.NET)', cwd='csharp', clean=lambda: [shutil.rmtree(HERE / 'csharp' / d, ignore_errors=True) for d in ('out', 'obj', 'bin')],
                   build=['dotnet', 'build', '-c', 'Release', '-o', 'out', '--nologo', '-v', 'q'], artifact=['csharp/out'],
                   run=[str(HERE / 'csharp/out/bench.exe')]),
    'node': dict(label='TypeScript (Node)', cwd='node', clean=lambda: None, build=None, artifact=['node/main.ts', 'node/rules.ts'],
                 run=['node', str(HERE / 'node/main.ts')]),
    'python': dict(label='Python (current)', cwd='python', clean=lambda: None, build=None, artifact=['python/bench.py'],
                   run=[PY, str(HERE / 'python/bench.py')]),
}
SERVE = {'rust': LANGS['rust']['run'], 'go': LANGS['go']['run'], 'java': LANGS['java']['run'], 'csharp': LANGS['csharp']['run'],
         'node': LANGS['node']['run'], 'python': [PY, str(HERE / 'python/serve_python.py')]}
PORT = {'rust': 9101, 'go': 9102, 'java': 9103, 'csharp': 9104, 'node': 9105, 'python': 9106}


def size_of(paths):
    total = 0
    for p in paths:
        p = HERE / p
        if p.is_dir():
            total += sum(f.stat().st_size for f in p.rglob('*') if f.is_file())
        else:
            total += p.stat().st_size
    return total


def tree_rss(proc):
    try:
        procs = [proc] + proc.children(recursive=True)
        return sum(p.memory_info().rss for p in procs if p.is_running())
    except psutil.Error:
        return 0


def tree_cpu(proc):
    try:
        procs = [proc] + proc.children(recursive=True)
        return sum(sum(p.cpu_times()[:2]) for p in procs)
    except psutil.Error:
        return 0.0


def run_measured(cmd, cwd):
    """Run a process to completion, sampling the process tree's resident set; return (stdout, peak_rss_bytes, wall_s)."""
    t = time.perf_counter()
    p = psutil.Popen(cmd, cwd=cwd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, env=ENV)
    peak = 0
    while p.poll() is None:
        peak = max(peak, tree_rss(p))
        time.sleep(0.01)
    out, err = p.communicate()
    if p.returncode != 0:
        raise RuntimeError(f'{cmd} failed: {err[-800:]}')
    return out.strip().splitlines()[-1], peak, time.perf_counter() - t


def start_server(lang):
    cmd = SERVE[lang] + ['-corpus', str(CORPUS), '-serve', f'127.0.0.1:{PORT[lang]}', '-threads', str(CPUS)]
    t = time.perf_counter()
    p = psutil.Popen(cmd, cwd=HERE / LANGS[lang]['cwd'], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, env=ENV)
    url = f'http://127.0.0.1:{PORT[lang]}'
    for _ in range(600):
        try:
            if urllib.request.urlopen(url + '/healthz', timeout=1).status == 200:
                return p, url, time.perf_counter() - t
        except Exception:
            time.sleep(0.05)
    p.kill()
    raise RuntimeError(f'{lang} server did not start')


def stop(p):
    for c in p.children(recursive=True):
        try:
            c.kill()
        except psutil.Error:
            pass
    try:
        p.kill()
    except psutil.Error:
        pass
    p.wait(timeout=10)


def loadgen(args):
    r = subprocess.run([str(HERE / 'loadgen/loadgen.exe')] + args, capture_output=True, text=True, timeout=300)
    if r.returncode != 0:
        raise RuntimeError(r.stderr[-500:])
    return json.loads(r.stdout.strip().splitlines()[-1])


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--batch-runs', type=int, default=5); ap.add_argument('--load-seconds', type=int, default=8)
    ap.add_argument('--only', default=''); ap.add_argument('--conc', default='1,16,64,256'); ap.add_argument('--skip-build', action='store_true'); ap.add_argument('--http-reps', type=int, default=3)
    a = ap.parse_args()
    names = [n for n in LANGS if not a.only or n in a.only.split(',')]
    out_path = HERE / 'results' / 'results.json'
    out_path.parent.mkdir(exist_ok=True)
    results = json.loads(out_path.read_text()) if out_path.exists() and a.only else {}
    results['_machine'] = {'cpus': CPUS, 'os': platform.platform(), 'cpu': platform.processor(), 'ram_gb': round(psutil.virtual_memory().total / 2**30, 1),
                           'claims': 20000, 'note': 'loadgen and server share this machine'}
    for n in names:
        spec = LANGS[n]
        r = {'label': spec['label']}
        print(f'== {n}', flush=True)
        if spec['build'] and not a.skip_build:
            spec['clean']()
            t = time.perf_counter()
            b = subprocess.run(spec['build'], cwd=HERE / spec['cwd'], capture_output=True, text=True, env=ENV)
            if b.returncode != 0:
                raise RuntimeError(b.stdout[-800:] + b.stderr[-800:])
            r['build_s'] = time.perf_counter() - t
        else:
            r['build_s'] = 0.0
        r['artifact_bytes'] = size_of(spec['artifact'])
        runs = []
        for i in range(a.batch_runs):
            line, peak, wall = run_measured(spec['run'] + ['-corpus', str(CORPUS), '-repeat', '7', '-threads', str(CPUS)], HERE / spec['cwd'])
            j = json.loads(line); j['peak_rss_bytes'] = peak; j['process_wall_s'] = wall
            runs.append(j)
            print('   batch', i, {k: round(v, 1) if isinstance(v, float) else v for k, v in j.items() if k in ('parse_ms', 'eval_1t_ms', 'eval_mt_ms', 'mismatches')}, flush=True)
        med = lambda k: statistics.median(x[k] for x in runs)
        r['version'] = runs[0]['version']
        r['mismatches'] = max(x['mismatches'] for x in runs)
        r['batch'] = {k: med(k) for k in ('read_ms', 'parse_ms', 'eval_1t_ms', 'eval_mt_ms', 'peak_rss_bytes', 'process_wall_s')}
        r['batch_runs'] = runs
        results[n] = r
        out_path.write_text(json.dumps(results, indent=1))

    # HTTP phase: several passes over the languages in rotating order, so a slow minute on this machine (updates, virus scan, thermal
    # limits) is spread over all of them instead of landing on one. Each level reports the median pass; the spread is kept.
    levels = [int(x) for x in a.conc.split(',')]
    passes = {n: [] for n in names}
    starts = {n: [] for n in names}
    probes = {}
    for rep in range(a.http_reps):
        order = names[rep % len(names):] + names[:rep % len(names)]
        for n in order:
            p, url, start_s = start_server(n)
            starts[n].append(start_s)
            got = []
            for c in levels:
                cpu0 = tree_cpu(p); t0 = time.perf_counter()
                res = loadgen(['-mode', 'load', '-url', url, '-corpus', str(CORPUS), '-conc', str(c), '-dur', f'{a.load_seconds}s'])
                wall = time.perf_counter() - t0
                res['server_rss_bytes'] = tree_rss(p)
                res['server_cpu_s'] = tree_cpu(p) - cpu0
                res['server_cores_used'] = res['server_cpu_s'] / wall
                got.append(res)
                print('   http pass', rep, n, c, {k: round(v, 2) if isinstance(v, float) else v for k, v in res.items() if k in ('rps', 'p99_ms', 'errors', 'mismatches')}, flush=True)
            passes[n].append(got)
            if rep == 0:
                probes[n] = loadgen(['-mode', 'probe', '-url', url, '-corpus', str(CORPUS)])
            stop(p)
            time.sleep(1.0)
    for n in names:
        merged = []
        for i, c in enumerate(levels):
            runs = sorted((ps[i] for ps in passes[n]), key=lambda x: x['rps'])
            mid = dict(runs[len(runs) // 2])
            mid['rps_min'], mid['rps_max'] = runs[0]['rps'], runs[-1]['rps']
            mid['errors'] = sum(x['errors'] for x in runs)
            mid['mismatches'] = sum(x['mismatches'] for x in runs)
            merged.append(mid)
        results[n]['http'] = {'cold_start_s': statistics.median(starts[n]), 'levels': merged, 'probe': probes[n], 'passes': a.http_reps}
    out_path.write_text(json.dumps(results, indent=1))
    print('wrote', out_path)


if __name__ == '__main__':
    main()
