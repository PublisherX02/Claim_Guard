using System.Diagnostics;
using System.Text;
using System.Text.Json;

static string Arg(string[] a, string name, string def)
{
    int i = Array.IndexOf(a, name);
    return i >= 0 && i + 1 < a.Length ? a[i + 1] : def;
}
static double Median(List<double> v) { var s = v.OrderBy(x => x).ToList(); return s[s.Count / 2]; }

var corpus = Arg(args, "-corpus", "");
int threads = int.Parse(Arg(args, "-threads", Environment.ProcessorCount.ToString()));
int repeat = int.Parse(Arg(args, "-repeat", "5"));
var serve = Arg(args, "-serve", "");

var pack = new Pack(File.ReadAllText(Path.Combine(corpus, "pack", "policies.json")), File.ReadAllText(Path.Combine(corpus, "pack", "services.json")));

if (serve != "")
{
    var builder = WebApplication.CreateSlimBuilder();
    builder.Logging.ClearProviders();
    builder.WebHost.UseUrls("http://" + serve);
    var app = builder.Build();
    app.MapPost("/evaluate", async (HttpContext ctx) =>
    {
        using var ms = new MemoryStream();
        await ctx.Request.Body.CopyToAsync(ms);
        if (ms.Length > (1 << 20)) return Results.StatusCode(413);
        Claim? c;
        byte[] o = new byte[15];
        try
        {
            c = JsonSerializer.Deserialize<Claim>(ms.GetBuffer().AsSpan(0, (int)ms.Length));
            if (c == null) return Results.BadRequest();
            pack.Evaluate(c, o);
        }
        catch { return Results.BadRequest(); }
        return Results.Text("{\"claim_id\":\"" + c.ClaimId + "\",\"statuses\":\"" + Encoding.ASCII.GetString(o) + "\"}", "application/json");
    });
    app.MapGet("/healthz", () => "ok");
    app.Run();
    return;
}

var sw = Stopwatch.StartNew();
var lines = File.ReadAllLines(Path.Combine(corpus, "claims.jsonl"));
double readMs = sw.Elapsed.TotalMilliseconds;

sw.Restart();
var claims = new Claim[lines.Length];
for (int i = 0; i < lines.Length; i++) claims[i] = JsonSerializer.Deserialize<Claim>(lines[i])!;
double parseMs = sw.Elapsed.TotalMilliseconds;

var outBuf = new byte[claims.Length * 15];
void EvalRange(int lo, int hi)
{
    for (int i = lo; i < hi; i++) pack.Evaluate(claims[i], outBuf.AsSpan(i * 15, 15));
}
void EvalAll(int t)
{
    if (t <= 1) { EvalRange(0, claims.Length); return; }
    int chunk = (claims.Length + t - 1) / t;
    Parallel.For(0, t, new ParallelOptions { MaxDegreeOfParallelism = t }, k =>
        EvalRange(k * chunk, Math.Min(claims.Length, (k + 1) * chunk)));
}

EvalAll(1);
var expected = File.ReadAllLines(Path.Combine(corpus, "expected.txt"));
int mismatches = 0;
for (int i = 0; i < expected.Length && i < claims.Length; i++)
    if (expected[i] != Encoding.ASCII.GetString(outBuf, i * 15, 15)) mismatches++;

var one = new List<double>(); var many = new List<double>();
for (int r = 0; r < repeat; r++)
{
    sw.Restart(); EvalAll(1); one.Add(sw.Elapsed.TotalMilliseconds);
    sw.Restart(); EvalAll(threads); many.Add(sw.Elapsed.TotalMilliseconds);
}
Console.WriteLine(JsonSerializer.Serialize(new Dictionary<string, object>
{
    ["lang"] = "csharp", ["version"] = "dotnet " + Environment.Version, ["claims"] = claims.Length, ["mismatches"] = mismatches, ["threads"] = threads,
    ["read_ms"] = readMs, ["parse_ms"] = parseMs, ["eval_1t_ms"] = Median(one), ["eval_mt_ms"] = Median(many),
}));
