const pptxgen = require("pptxgenjs");
const fs = require("fs");
const path = require("path");
const SKILL = "C:/Users/moham/.claude/skills/synced/95c611d2-5d12-45fc-beb8-f5e9287d1c25_98fa982a-47e3-466d-a4f6-bb541454b0c2/pptx";
const { applyTheme } = require(SKILL + "/scripts/apply_theme.js");

const ASSETS = path.join(__dirname, "assets");
const has = (f) => fs.existsSync(path.join(ASSETS, f));
const rd = (f) => (fs.existsSync(path.join(__dirname, f)) ? JSON.parse(fs.readFileSync(path.join(__dirname, f), "utf8")) : null);
const BENCH = rd("bench.json"), BENCH8 = rd("bench8.json");

const THEME = {
  name: "ClaimGuard",
  headFontFace: "Cambria",
  bodyFontFace: "Calibri",
  colors: {
    dk1: "17292B", lt1: "FFFFFF", dk2: "0E3B3F", lt2: "F2F6F5",
    accent1: "14746F", accent2: "F2A541", accent3: "C8553D", accent4: "5C7173", accent5: "2E9E8F", accent6: "8FB8B3",
    hlink: "14746F", folHlink: "5C7173",
  },
};

const pres = new pptxgen();
pres.layout = "LAYOUT_WIDE"; // 13.33 x 7.5
pres.title = "ClaimGuard: mentor session";
pres.author = "ClaimGuard team";
pres.theme = { headFontFace: THEME.headFontFace, bodyFontFace: THEME.bodyFontFace };
const C = pres.SchemeColor;
const W = 13.33, H = 7.5, MX = 0.6;

// ---- layouts -----------------------------------------------------------------------------------------------------------------
pres.defineSlideMaster({
  title: "DARK_TITLE",
  background: { color: "0E3B3F" },
  objects: [],
  slideNumber: { x: 12.3, y: 7.0, w: 0.6, h: 0.3, fontSize: 10, color: "8FB8B3" },
});
pres.defineSlideMaster({
  title: "DARK_SECTION",
  background: { color: "0E3B3F" },
  slideNumber: { x: 12.3, y: 7.0, w: 0.6, h: 0.3, fontSize: 10, color: "8FB8B3" },
  objects: [
    { placeholder: { options: { name: "title", type: "title", x: 0.9, y: 2.3, w: 11.4, h: 1.4, fontFace: "Cambria", fontSize: 44, bold: true, color: "FFFFFF", valign: "middle" }, text: "" } },
    { placeholder: { options: { name: "body", type: "body", x: 0.9, y: 3.8, w: 10.5, h: 1.2, fontFace: "Calibri", fontSize: 20, color: "CADCD9", valign: "top" }, text: "" } },
  ],
});
pres.defineSlideMaster({
  title: "CONTENT",
  background: { color: "FFFFFF" },
  slideNumber: { x: 12.3, y: 7.0, w: 0.6, h: 0.3, fontSize: 10, color: "5C7173" },
  objects: [
    { placeholder: { options: { name: "title", type: "title", x: MX, y: 0.4, w: 12.1, h: 0.9, fontFace: "Cambria", fontSize: 32, bold: true, color: "0E3B3F", valign: "middle", margin: 0 }, text: "" } },
  ],
});

const T = { ink: "17292B", dark: "0E3B3F", teal: "14746F", amber: "F2A541", red: "C8553D", muted: "5C7173", soft: "F2F6F5", line: "D5E1DF", mint: "DDEEEB", seafoam: "8FB8B3" };
const NOTES = require("./notes.js");
let SECTION = null;
function slide(title, opts = {}) {
  const s = pres.addSlide({ masterName: "CONTENT", sectionTitle: SECTION });
  s.addText(title, { placeholder: "title" });
  if (!NOTES[opts.n]) throw new Error("no notes for " + title);
  s.addNotes(NOTES[opts.n]);
  return s;
}
function section(name) { SECTION = name; pres.addSection({ title: name }); }

// ---- small helpers ---------------------------------------------------------------------------------------------------------
function card(s, x, y, w, h, fill = "F2F6F5", name = "card") {
  s.addShape(pres.ShapeType.roundRect, { x, y, w, h, fill: { color: fill }, line: { color: fill, width: 0 }, rectRadius: 0.12, objectName: name });
}
function badge(s, x, y, n, fill = "14746F", d = 0.42, color = "FFFFFF") {
  s.addShape(pres.ShapeType.ellipse, { x, y, w: d, h: d, fill: { color: fill }, line: { color: fill, width: 0 }, objectName: "badge" });
  s.addText(String(n), { x, y, w: d, h: d, align: "center", valign: "middle", fontSize: 14, bold: true, color, margin: 0, isTextBox: true, objectName: "badge text" });
}
function tag(s, x, y, text, fill = "F2A541", color = "17292B", w = 1.1) {
  s.addShape(pres.ShapeType.roundRect, { x, y, w, h: 0.3, fill: { color: fill }, line: { color: fill, width: 0 }, rectRadius: 0.15, objectName: "tag" });
  s.addText(text, { x, y, w, h: 0.3, align: "center", valign: "middle", fontSize: 11, bold: true, color, margin: 0, isTextBox: true, objectName: "tag text" });
}
function bullets(s, items, x, y, w, h, size = 16, color = T.ink) {
  const runs = items.map((t, i) => ({ text: t, options: { bullet: { indent: 16 }, breakLine: i < items.length - 1, paraSpaceAfter: 6 } }));
  s.addText(runs, { x, y, w, h, fontSize: size, color, valign: "top", margin: 0, isTextBox: true, objectName: "bullets" });
}
function stat(s, x, y, w, big, label, color = T.teal, bigSize = 44) {
  s.addText(big, { x, y, w, h: 0.9, fontSize: bigSize, bold: true, fontFace: "Cambria", color, margin: 0, valign: "bottom", isTextBox: true, objectName: "stat" });
  s.addText(label, { x, y: y + 0.95, w, h: 0.9, fontSize: 14, color: T.muted, margin: 0, valign: "top", isTextBox: true, objectName: "stat label" });
}
function note(s, text, y = 6.78, x = MX, w = 11.4) {
  s.addText(text, { x, y, w, h: 0.4, fontSize: 11, italic: true, color: T.muted, margin: 0, valign: "middle", isTextBox: true, objectName: "footnote" });
}
function img(s, file, x, y, w, h) {
  if (has(file)) s.addImage({ path: path.join(ASSETS, file), x, y, w, h, objectName: file });
  else { card(s, x, y, w, h, "F2F6F5", "image missing"); s.addText("[" + file + "]", { x, y, w, h, align: "center", valign: "middle", color: T.muted, fontSize: 12, isTextBox: true }); }
}
function table(s, head, rows, x, y, w, colW, size = 13, rowH = 0.42) {
  const hdr = head.map((t) => ({ text: t, options: { bold: true, color: "FFFFFF", fill: { color: T.dark }, fontSize: size, valign: "middle" } }));
  const body = rows.map((r, ri) => r.map((c, ci) => {
    const o = typeof c === "object" ? c : { text: c };
    return { text: o.text, options: { fontSize: size, color: o.color || T.ink, bold: !!o.bold, fill: { color: ri % 2 ? "F2F6F5" : "FFFFFF" }, valign: "middle" } };
  }));
  s.addTable([hdr, ...body], { x, y, w, colW, rowH, border: { type: "solid", pt: 0.5, color: T.line }, margin: [0.04, 0.1, 0.04, 0.1], objectName: "table" });
}
function box(s, x, y, w, h, title, sub, fill, tcolor = "FFFFFF", scolor = "DDEEEB") {
  s.addShape(pres.ShapeType.roundRect, { x, y, w, h, fill: { color: fill }, line: { color: fill, width: 0 }, rectRadius: 0.1, objectName: "box " + title });
  s.addText([{ text: title, options: { bold: true, fontSize: 14, color: tcolor, breakLine: !!sub } }, ...(sub ? [{ text: sub, options: { fontSize: 11, color: scolor } }] : [])],
    { x: x + 0.08, y, w: w - 0.16, h, align: "center", valign: "middle", margin: 0, isTextBox: true, objectName: "box text " + title });
}
function arrow(s, x1, y1, x2, y2, color = "5C7173") {
  s.addShape(pres.ShapeType.line, { x: Math.min(x1, x2), y: Math.min(y1, y2), w: Math.abs(x2 - x1), h: Math.abs(y2 - y1), flipH: x2 < x1, flipV: y2 < y1, line: { color, width: 1.75, endArrowType: "triangle" }, objectName: "arrow" });
}

// =====================================================================================================================================
// 1  Title
section("Opening");
{
  const s = pres.addSlide({ masterName: "DARK_TITLE", sectionTitle: SECTION });
  s.addText("ClaimGuard", { x: 0.9, y: 1.7, w: 11, h: 1.3, fontFace: "Cambria", fontSize: 66, bold: true, color: "FFFFFF", margin: 0, isTextBox: true, objectName: "title" });
  s.addText("A copilot that helps a claims officer understand a claim, decide, correct it and revalidate it, with every step on record", { x: 0.9, y: 3.1, w: 10.2, h: 1.3, fontSize: 24, color: "CADCD9", margin: 0, valign: "top", isTextBox: true, objectName: "subtitle" });
  tag(s, 0.9, 4.9, "Mentor session", "F2A541", "17292B", 1.8);
  s.addText("Sunday 18 October 2026. Everything built since the Phase 1 submission, measured, with what is not done said plainly.", { x: 0.9, y: 5.4, w: 10, h: 0.8, fontSize: 16, color: "8FB8B3", margin: 0, valign: "top", isTextBox: true, objectName: "tagline" });
  s.addNotes(NOTES.title);
}

// 2  Where each mentor point stands
{
  const s = slide("Your email of 16 October, point by point", { n: "email" });
  table(s, ["#", "What you asked for", "Where we stand"], [
    ["1", "Human in the loop: confirm, dismiss with reason, request information, correct and revalidate, prioritised and traceable", { text: "Built. Lanes and a transparent score, four-eyes on high severity, full audit trail", bold: true }],
    ["2", "All 15 rules right; extra rules kept apart from the benchmark", { text: "F1 1.00 on 600 public claims. 8 advisory rules in a separate pack", bold: true }],
    ["3", "AI that helps a claims officer, grounded, with a plan for wrong output", { text: "Explainer, correction assistant, what-if, reviewer summary, feedback analysis", bold: true }],
    ["4", "R006 as documented; three public claims to review together", { text: "Kept as documented. Three claims to discuss (slide 40)", bold: true }],
    ["5", "Authorization aggregated per claim; cross-claim only as an extension", { text: "R009 per claim. Cross-claim is advisory rule E102, separate", bold: true }],
    ["6", "Pre-validation clearance, not payer approval", { text: "Wording fixed in the console. Auto-clear exists, off by default, audited", bold: true }],
    ["7", "Keep the JSONL schema; confidence null, not_probabilistic", { text: "Unchanged and enforced in the audit log", bold: true }],
    ["8", "One convincing end-to-end workflow", { text: "Six steps run in the console (next slide)", bold: true }],
    ["9", "ML routing is optional; if used, show the gain over deterministic", { text: "None in use. Routing is the deterministic score. A trained model was tested and set aside (slide 28)", bold: true }],
  ], MX, 1.4, 12.1, [0.5, 6.4, 5.2], 13, 0.5);
}

// 3  The workflow
{
  const s = slide("The workflow you described, running end to end", { n: "workflow" });
  const steps = [
    ["Claim received and validated", "FHIR, CSV, JSONL, PDF or photo. 15 rules, 0.19 ms per claim", false],
    ["Errors, doubts and evidence", "FAIL, or UNABLE_TO_ASSESS when data is missing. Each finding cites claim paths", false],
    ["Copilot explains and advises", "Summary, what to ask or correct, never an invented value", true],
    ["Officer decides", "Confirm, dismiss with a reason, request information. Seniors for high severity", false],
    ["Correct and revalidate", "What-if first, then a new version. The original stays untouched", true],
    ["Traceable, with limits shown", "Signed audit trail, both versions side by side, measured accuracy", false],
  ];
  const cw = 3.85, ch = 2.35, gx = 0.28, gy = 0.3;
  steps.forEach((st, i) => {
    const col = i % 3, row = Math.floor(i / 3);
    const x = MX + col * (cw + gx), y = 1.65 + row * (ch + gy);
    card(s, x, y, cw, ch, st[2] ? "FFF1DB" : "F2F6F5", "step card " + (i + 1));
    badge(s, x + 0.25, y + 0.25, i + 1, st[2] ? "C47A12" : T.teal);
    if (st[2]) tag(s, x + cw - 1.55, y + 0.3, "built this week", T.amber, T.ink, 1.3);
    s.addText(st[0], { x: x + 0.25, y: y + 0.85, w: cw - 0.5, h: 0.6, fontSize: 18, bold: true, fontFace: "Cambria", color: T.dark, margin: 0, valign: "top", isTextBox: true, objectName: "step title" });
    s.addText(st[1], { x: x + 0.25, y: y + 1.45, w: cw - 0.5, h: 0.8, fontSize: 14, color: T.ink, margin: 0, valign: "top", isTextBox: true, objectName: "step text" });
  });
  note(s, "All numbers in this deck come from commands you can re-run. Anything simulated says so on its slide.");
}

// =====================================================================================================================================
section("Core");
// 4 Architecture
{
  const s = slide("How the system fits together", { n: "arch" });
  // row 1: intake path
  const y1 = 1.75, bh = 1.05, bw = 2.05, gap = 0.42;
  const r1 = [
    ["Inputs", "FHIR, CSV, JSONL, PDF, photo", T.muted],
    ["Intake", "validate, quarantine, reader sandbox", T.teal],
    ["Rule engine", "15 rules, YARA-X, 0.19 ms", T.dark],
    ["MongoDB", "claims and results sealed at rest", T.teal],
    ["Redis Streams", "consumer group, concurrency cap", "8C4A3A"],
  ];
  r1.forEach((b, i) => { const x = MX + i * (bw + gap); box(s, x, y1, bw, bh, b[0], b[1], b[2]); if (i < r1.length - 1) arrow(s, x + bw + 0.04, y1 + bh / 2, x + bw + gap - 0.04, y1 + bh / 2); });
  // row 2
  const y2 = 3.55;
  const r2 = [
    ["AI explanation", "breaker, budget, cache, grounding guard", "6E4B8B"],
    ["Dispatcher", "lanes, leases, personal inboxes", T.teal],
    ["Console", "inbox, copilot, what-if, recheck", T.dark],
    ["Decision", "badge, reason, four-eyes", T.teal],
    ["New version", "corrected claim, validated again", "C47A12"],
  ];
  r2.forEach((b, i) => { const x = MX + (4 - i) * (bw + gap); box(s, x, y2, bw, bh, b[0], b[1], b[2]); if (i < r2.length - 1) arrow(s, x - 0.04, y2 + bh / 2, x - gap + 0.04, y2 + bh / 2); });
  arrow(s, MX + 4 * (bw + gap) + bw / 2, y1 + bh + 0.02, MX + 4 * (bw + gap) + bw / 2, y2 - 0.02);
  // bottom band
  card(s, MX, 5.0, 12.1, 1.5, "F2F6F5", "records band");
  s.addText("Records and people", { x: MX + 0.25, y: 5.08, w: 4, h: 0.35, fontSize: 14, bold: true, color: T.dark, margin: 0, isTextBox: true, objectName: "band title" });
  const rec = [["Review log", "hash-chained, signed"], ["Security log", "every access and decision"], ["Ledger", "Ed25519 checkpoints, Merkle proofs"], ["Access control", "4 clearance levels, pseudonymised identifiers"]];
  rec.forEach((r, i) => {
    const x = MX + 0.25 + i * 2.95;
    s.addText([{ text: r[0], options: { bold: true, fontSize: 14, color: T.ink, breakLine: true } }, { text: r[1], options: { fontSize: 12, color: T.muted } }], { x, y: 5.5, w: 2.8, h: 0.9, margin: 0, valign: "top", isTextBox: true, objectName: "record " + r[0] });
  });
}

// 4b What it is made of
{
  const s = slide("What it is made of", { n: "stack" });
  table(s, ["Layer", "What we used", "What it gives"], [
    ["Language", "Rust, one workspace of 12 crates. The engine knows nothing of the web, the web layer nothing of the database", "One binary, memory safe, each layer tested alone"],
    ["Rules", "YARA-X rule packs over fact lines that one Rust function per rule extracts", "Readable rule files, hashed with the build, 0.19 ms a claim"],
    ["Numbers", "Exact decimals and integers; JSON numbers keep their text", "Money rounds exactly: 2.5 and 2.50 behave the same"],
    ["Storage", "MongoDB: claim versions, configurations, deals, dead letters, cache, counters, users", "Unique indexes and conditional updates make the database enforce the rules"],
    ["Queue", "Redis Streams with a consumer group, plus an outbox in MongoDB", "At-least-once delivery, nothing lost if Redis is"],
    ["AI", "Gemma 3 4B served locally (Ollama; any server with the same protocol)", "No outside call, no claim value in the prompt"],
    ["Documents", "Tesseract and a vision model, in an isolated reader", "Two independent readers cross-checked"],
    ["Identity", "bcrypt, authenticator code (TOTP), signed session tokens, CSRF defence, 4 levels", "Two factors, revocable sessions, server-side permissions"],
    ["Records", "Hash-chained signed logs, Ed25519 checkpoints, Merkle proofs, RFC 3161 timestamp", "An edit to history is detected"],
    ["Console", "Plain JavaScript embedded in the binary, live updates by server-sent events", "No separate web server to run"],
  ], MX, 1.5, 12.1, [1.5, 6.0, 4.6], 11.5, 0.44);
}

// ---- choices (decisions we measured)
{
  const s = slide("Choices we made, and what we measured", { n: "choices" });
  table(s, ["Decision", "Instead of", "What we measured", "Weak spot, said first"], [
    ["Rules decide, the AI explains", "An agent that decides (LangGraph)", "Same model, same claims: 97.5 against 11.5 strict (21.1 lenient). The agent's endpoint failed in 10 of 12 runs", "12 claims. Our correctness is structural, not an independent finding"],
    ["Gemma 3 4B, run locally", "Qwen3 4B, MedGemma 4B", "Gemma: 97.2% usable live (35 of 36), median about 3 s. MedGemma 83.3% and about 4 times slower. no Qwen3 setting worked on the whole set", "Small test sets"],
    ["YARA-X rule packs", "Plain Python functions", "In the Python build, plain Python was about 11 times faster per claim", "We lost the speed argument and say so. The case is structural: rule text apart from code, hashed, no side effects"],
    ["Rust", "Python, Java, Node, C#, Go", "Rust used 7 MB where Java used 283 MB and Node 1,168 MB. The language changes a reviewer's wait by under 0.03%", "Measured on one machine"],
    ["MongoDB and Redis", "SQLite", "The mentor's direction of 27 September: a NoSQL store and a real queue, on premises", "Throughput measured on one laptop only"],
  ], MX, 1.5, 12.1, [2.1, 2.1, 4.9, 3.0], 11.5, 0.5);
}

// 5 Rules
{
  const s = slide("Rule correctness: the 15 reference rules, measured", { n: "rules" });
  stat(s, MX, 1.55, 2.8, "1.00", "F1 on all 600 public claims (400 development, 150 validation, 50 stress)");
  stat(s, MX + 3.0, 1.55, 2.8, "0 of 3,015", "clean results wrongly flagged. 95% bound on false-alarm claims: 1.9%", T.teal, 32);
  stat(s, MX + 6.0, 1.55, 2.8, "0.19 ms", "per claim in the rule engine. The model, not the engine, is the slow step");
  stat(s, MX + 9.0, 1.55, 3.1, "10,600", "claims on which the Rust engine agrees exactly with the Python reference");
  card(s, MX, 3.75, 5.9, 2.85, T.soft, "left card");
  s.addText("Edge cases and uncertainty", { x: MX + 0.25, y: 3.85, w: 5.4, h: 0.4, fontSize: 18, bold: true, fontFace: "Cambria", color: T.dark, margin: 0, isTextBox: true, objectName: "h" });
  bullets(s, ["Missing data gives UNABLE_TO_ASSESS, never PASS", "Boundaries inclusive, ROUND_HALF_UP, 0.01 SAR tolerance", "R006 exactly as documented: code, date, modifier", "R009 aggregates per authorization within one claim"], MX + 0.25, 4.35, 5.4, 2.2, 15);
  card(s, MX + 6.2, 3.75, 5.9, 2.85, T.soft, "right card");
  s.addText("Kept apart from the benchmark", { x: MX + 6.45, y: 3.85, w: 5.4, h: 0.4, fontSize: 18, bold: true, fontFace: "Cambria", color: T.dark, margin: 0, isTextBox: true, objectName: "h" });
  bullets(s, ["8 advisory rules (E001 to E005, E101 to E103) in their own pack, file and schema", "Reference JSONL unchanged: confidence null, not_probabilistic", "A test pins the official results to their recorded values", "Public set is synthetic: a perfect score is not proof for real claims"], MX + 6.45, 4.35, 5.4, 2.2, 15);
}

// ---- testing
{
  const s = slide("How it was tested", { n: "testing" });
  stat(s, MX, 1.5, 2.8, "107,635", "generated claims where an independent oracle agrees with the engine: 0 disagreements", T.teal, 36);
  stat(s, MX + 3.1, 1.5, 2.8, "37,000", "boundary mutants of claims, plus 123 hand-derived boundary cases", T.teal, 36);
  stat(s, MX + 6.2, 1.5, 2.8, "374", "Rust workspace tests pass (database tests need MongoDB and Redis)", T.teal, 36);
  stat(s, MX + 9.3, 1.5, 2.8, "1,795", "tests behind the Python reference build", T.teal, 36);
  const cards = [
    ["Against the answer key", "F1 1.00 on the 600 public claims, and on nine labelled example sets in three evidence tiers, each with its file hash and generator recorded"],
    ["Against attack", "116 black-box attack checks, 400,000 fuzzed requests, 93 deliberate breakages of security and queue rules, each caught by a test"],
    ["In a real browser", "Chrome walkthroughs of the correction flow (11 of 11), file import, live updates and the audit alarm"],
  ];
  cards.forEach((c, i) => {
    const x = MX + i * 4.1;
    card(s, x, 3.75, 3.9, 2.9, T.soft, "test card " + (i + 1));
    s.addText(c[0], { x: x + 0.25, y: 3.85, w: 3.4, h: 0.45, fontSize: 17, bold: true, fontFace: "Cambria", color: T.dark, margin: 0, isTextBox: true, objectName: "test title" });
    s.addText(c[1], { x: x + 0.25, y: 4.4, w: 3.45, h: 2.1, fontSize: 13.5, color: T.ink, margin: 0, valign: "top", isTextBox: true, objectName: "test text" });
  });
}

// 6 Intake
{
  const s = slide("Claims arrive in any format, untrusted until proven", { n: "intake" });
  const fm = [["FHIR bundle", "authorization record not carried, so those rules abstain"], ["CSV tables", "same claims as the JSONL"], ["JSONL", "reference format"], ["PDF or photo", "two readers, results shown as a draft"]];
  fm.forEach((f, i) => {
    const y = 1.6 + i * 0.95;
    card(s, MX, y, 4.6, 0.82, T.soft, "format " + f[0]);
    s.addText([{ text: f[0], options: { bold: true, fontSize: 16, color: T.dark, breakLine: true } }, { text: f[1], options: { fontSize: 12, color: T.muted } }], { x: MX + 0.2, y, w: 4.3, h: 0.82, valign: "middle", margin: 0, isTextBox: true, objectName: "format text" });
  });
  stat(s, 5.7, 1.55, 3.2, "600", "public claims give identical claims and reports in all three formats", T.teal, 40);
  stat(s, 9.2, 1.55, 3.4, "12,000", "randomly damaged FHIR bundles reach the same accept or quarantine verdict as the reference", T.teal, 40);
  card(s, 5.7, 3.75, 6.9, 2.9, T.soft, "docs card");
  s.addText("Documents: PDF and photos", { x: 5.95, y: 3.85, w: 6.4, h: 0.4, fontSize: 18, bold: true, fontFace: "Cambria", color: T.dark, margin: 0, isTextBox: true, objectName: "h" });
  bullets(s, ["Reader in an isolated container, polyglot file gate, hidden-Unicode flag", "240 synthetic documents: 97.4% of fields right, 1 silent error", "A reading is only a draft: the clerk compares every field with the page", "Real paper, handwriting and Arabic are not tested yet"], 5.95, 4.35, 6.4, 2.2, 15);
  note(s, "A damaged record is quarantined with a reason and never stops the run.");
}

// ---- OCR
{
  const s = slide("Reading documents: PDFs and photos", { n: "ocr" });
  const st = [["Gate", "size, real type from the first bytes, no hidden second format"], ["Reader", "own container, no network. Text layer first, else Tesseract plus a vision model"], ["Extraction", "labelled fields, tables, dates read only if unambiguous, a check against the catalogue"], ["Draft", "page beside editable fields. A person ticks that every field was compared"]];
  st.forEach((r, i) => {
    const y = 1.5 + i * 0.95;
    card(s, MX, y, 5.7, 0.85, T.soft, "ocr step " + (i + 1));
    badge(s, MX + 0.18, y + 0.22, i + 1);
    s.addText([{ text: r[0], options: { bold: true, fontSize: 14, color: T.dark, breakLine: true } }, { text: r[1], options: { fontSize: 12, color: T.ink } }], { x: MX + 0.75, y, w: 4.85, h: 0.85, valign: "middle", margin: 0, isTextBox: true, objectName: "ocr text" });
  });
  table(s, ["Reader (240 documents)", "Fields right", "Silent errors"], [
    ["PDF text layer", "100%", "0"],
    ["Tesseract alone", "94.6%", "41"],
    ["Two readers, cross-checked", "96.8%", "3"],
    [{ text: "Two readers, small pages upscaled (default)", bold: true }, { text: "97.4%", bold: true, color: T.teal }, { text: "1", bold: true, color: T.teal }],
  ], 6.6, 1.5, 6.1, [3.5, 1.4, 1.2], 12.5, 0.5);
  bullets(s, ["By condition: scan 98.9%, image-only PDF 98.7%, phone-style photo 88.1%", "The vision model is baidu/Unlimited-OCR (3.3B parameters, 0.5B active, MIT licence), run locally as a second, independent reader", "A silent error is a wrong field that nothing flagged. The evaluation found and fixed two bugs"], 6.6, 4.05, 6.1, 2.2, 13);
  note(s, "Not tested: real hospital paper, handwriting, stamps and Arabic. A reading is only a draft; it never becomes a claim on its own.", 6.3, 6.6, 6.1);
}

// =====================================================================================================================================
section("Queue, load balancing and AI");
// Task queue 1
{
  const s = slide("Task queue 1: how a claim moves", { n: "queue1" });
  const st = [["Received", "validated"], ["Triaged", "receipt, lane"], ["Explained", "or skipped"], ["Ready", "waits for a person"], ["Leased", "in one inbox"], ["Decided", "badge, reason"], ["Rechecked", "new version"]];
  st.forEach((b, i) => {
    const x = MX + i * 1.75;
    box(s, x, 1.6, 1.55, 0.85, b[0], b[1], i % 2 ? T.dark : T.teal);
    if (i < st.length - 1) arrow(s, x + 1.57, 2.02, x + 1.73, 2.02);
  });
  s.addText("Side states: awaiting countersign (a high finding signed once) and dead letter (a task that kept failing).", { x: MX, y: 2.55, w: 12.1, h: 0.35, fontSize: 12.5, italic: true, color: T.muted, margin: 0, isTextBox: true, objectName: "side states" });
  const cards = [
    ["Intake is one atomic write", ["The claim, its triage receipt and a publish marker are stored together", "Unique indexes: the same version, or the same input under the same rule pack, is stored once", "A crash between write and publish is the same case: the marker is still set"]],
    ["Every move is one conditional update", ["The database changes the claim only if it is still in the expected state, and appends the event in the same update", "50 callers racing: exactly one wins", "The allowed moves are pinned by a table test"]],
    ["Claims are versions, never edited", ["A correction or a new rule pack creates version N+1; the old one keeps its decisions", "Only the newest version is dealt; a lease on an older one is taken back", "Decisions need the lease and fresh permissions, read again at that moment"]],
  ];
  cards.forEach((c, i) => {
    const x = MX + i * 4.1;
    card(s, x, 3.1, 3.9, 3.55, T.soft, "queue card " + (i + 1));
    badge(s, x + 0.2, 3.25, i + 1);
    s.addText(c[0], { x: x + 0.75, y: 3.2, w: 3.0, h: 0.55, fontSize: 15, bold: true, fontFace: "Cambria", color: T.dark, margin: 0, valign: "middle", isTextBox: true, objectName: "queue card title" });
    bullets(s, c[1], x + 0.25, 3.95, 3.45, 2.6, 12.5);
  });
}

// 7 Task queue
{
  const s = slide("Task queue 2: delivery, retries and failures", { n: "queue2" });
  const fl = [["Intake", "claim and a publish marker, one atomic write", T.teal], ["Outbox relay", "publishes, clears the marker only after Redis accepted", T.dark], ["Redis Stream", "consumer group, pool of consumers", "8C4A3A"], ["Process", "explain and move to ready", T.teal]];
  fl.forEach((b, i) => { const x = MX + i * 3.1; box(s, x, 1.65, 2.6, 1.15, b[0], b[1], b[2]); if (i < 3) arrow(s, x + 2.64, 2.22, x + 3.06, 2.22); });
  const g = [
    ["Redis down", "markers stay in MongoDB, the relay republishes. Consumers back off and keep trying"],
    ["Consumer dies", "its message is taken over by another after an idle time longer than the model deadline"],
    ["Poison claim", "parked as a dead letter after three deliveries, once, never while a person holds it"],
    ["Duplicates and forgeries", "processing is idempotent; a wrong hash or unknown claim changes nothing"],
    ["Burst of claims", "waits in the stream, never in front of the model. Concurrency is capped by a setting"],
    ["Lost messages", "if the stream is empty yet a claim sat 15 minutes, it is published again"],
  ];
  g.forEach((r, i) => {
    const col = i % 2, row = Math.floor(i / 2);
    const x = MX + col * 6.1, y = 3.2 + row * 1.12;
    card(s, x, y, 5.95, 1.0, T.soft, "guarantee " + (i + 1));
    s.addText([{ text: r[0], options: { bold: true, fontSize: 14, color: T.dark, breakLine: true } }, { text: r[1], options: { fontSize: 12.5, color: T.ink } }], { x: x + 0.2, y, w: 5.6, h: 1.0, valign: "middle", margin: 0, isTextBox: true, objectName: "guarantee text" });
  });
  note(s, "Pool size (1 to 64, default 4) caps claims explained at once, so also the model. ai_per_minute (30) and a daily budget (2,000) cap requests. Each guarantee has a test, also on a real Redis 7.4.", 6.85);
}

// Load balancing
{
  const s = slide("Load balancing: how claims are dealt to people", { n: "balancer" });
  const steps = [
    ["Eligible agents only", "active, on shift, room in the inbox, and the permission this claim needs"],
    ["A fair order", "claims shuffled with a logged seed, then sorted by score plus half a point per hour waited (aging)"],
    ["Least work first", "each claim goes to the eligible agent holding the least work so far (greedy balancing)"],
    ["Conflicts of interest", "nobody gets a later version of a claim they decided, a listed patient, or one they signed"],
    ["Only the newest version", "a lease on a superseded version is taken back"],
    ["Replayable", "each deal stores its inputs and seed; verify proves the assignments follow from them"],
  ];
  steps.forEach((r, i) => {
    const y = 1.5 + i * 0.86;
    card(s, MX, y, 7.7, 0.76, T.soft, "balance step " + (i + 1));
    badge(s, MX + 0.18, y + 0.17, i + 1);
    s.addText([{ text: r[0], options: { bold: true, fontSize: 14, color: T.dark, breakLine: true } }, { text: r[1], options: { fontSize: 12, color: T.ink } }], { x: MX + 0.75, y, w: 6.8, h: 0.76, valign: "middle", margin: 0, isTextBox: true, objectName: "balance text" });
  });
  stat(s, 8.9, 1.45, 3.8, "25 and 5", "claims an inbox holds, and the level at which it is topped up: nobody waits", T.teal, 36);
  stat(s, 8.9, 3.15, 3.8, "0.045", "spread of load among seniors (coefficient of variation), simulated", T.teal, 36);
  stat(s, 8.9, 4.85, 3.8, "0", "claims ever in two inboxes, over five simulated seeds", T.teal, 36);
  note(s, "Nobody assigns a particular claim to a particular person. Simulated people and clock; the invariants are what carries over.", 6.8);
}

// 8 Dispatcher and lanes
{
  const s = slide("Who sees which claim: lanes, leases and four-eyes", { n: "lanes" });
  table(s, ["Flagged finding", "High", "Medium"], [["FAIL", "4 points", "2 points"], ["UNABLE_TO_ASSESS", "2 points", "1 point"]], MX, 1.6, 4.9, [2.3, 1.3, 1.3], 14, 0.45);
  table(s, ["Lane", "When"], [["Green", "nothing flagged"], ["A", "1 to 3 flagged, score under 10"], ["B", "4 or more flagged, or score of 10 or more"]], MX, 3.05, 4.9, [1.3, 3.6], 14, 0.45);
  bullets(s, ["High severity: a senior decides, a different senior countersigns blind. A third settles a disagreement", "Leases return abandoned claims to the pool", "No route assigns or moves a claim"], MX, 5.0, 4.9, 1.7, 14);
  s.addChart(pres.charts.BAR, [{ name: "Hours to drain 500 claims", labels: ["4 seniors", "8 seniors", "4 seniors, four-eyes", "8 seniors, four-eyes"], values: [2.97, 1.82, 5.66, 3.07] }], {
    x: 6.0, y: 1.55, w: 6.7, h: 4.6, barDir: "col", chartColors: [T.teal], showTitle: true, title: "Simulated: hours to drain 500 claims with 20 agents",
    titleFontSize: 14, titleColor: T.dark, showValue: true, dataLabelPosition: "outEnd", dataLabelFontSize: 13, dataLabelColor: T.ink, dataLabelFontFace: "+mn-lt",
    catAxisLabelColor: T.ink, catAxisLabelFontSize: 12, catAxisLabelFontFace: "+mn-lt", valAxisLabelColor: T.muted, valAxisLabelFontSize: 11, valAxisLabelFontFace: "+mn-lt",
    valGridLine: { color: "D5E1DF", size: 0.5 }, catGridLine: { style: "none" }, showLegend: false, valAxisMinVal: 0, titleFontFace: "+mn-lt",
  });
  note(s, "Simulated people and clock, seed 1, five seeds checked. 253 of 500 claims need a senior, so seniors are the bottleneck.", 6.25, 6.0, 6.7);
}

// 9 Cache
{
  const s = slide("The cache, and what is never cached", { n: "cache" });
  table(s, ["What", "Cached?", "Why"], [
    ["Explanation template with {value} and {line}, keyed by rule, failure shape, prompt version, model", { text: "Yes, after the guard approved a fill", bold: true, color: T.teal }, "No claim value in the key or the text"],
    ["Filled text, claim values, evidence", { text: "Never", bold: true, color: T.red }, "Would hold identifiers"],
    ["Permissions, sessions, clearance", { text: "Never", bold: true, color: T.red }, "A demotion must apply at once"],
    ["Rule results of a claim", { text: "In the claim document", bold: true }, "The receipt hashes them, so replay can compare"],
  ], MX, 1.55, 12.1, [5.6, 3.0, 3.5], 13.5, 0.6);
  stat(s, MX, 4.95, 3.6, "90.5% to 97.7%", "of 777 findings need no model call: empty cache, then warm (real model, 399 claims)", T.teal, 30);
  stat(s, MX + 4.1, 4.95, 3.6, "5 failures", "in a row open the breaker. One probe after 20 s", T.teal, 32);
  stat(s, MX + 8.2, 4.95, 3.8, "90 s", "hard deadline. A late or rejected answer leaves the engine's text", T.teal, 32);
  note(s, "Budgets are counted in the database, not Redis (defaults 30 a minute, 2,000 a day; the load test raised the minute budget to 600). A template the guard refuses is not cached.", 6.9);
}

// 10 Grounded AI
{
  const s = slide("The AI explains. It cannot decide or hide a finding", { n: "ai" });
  const rows = [
    ["The model sees only the rule text", "never a claim value; placeholders are filled in afterwards"],
    ["Its text is checked mechanically", "invented values, clinical or fraud talk, approval or payment words (also disguised), garbled script, repetition"],
    ["Failure leaves the engine's text", "slow, down, over budget or rejected: the finding and its explanation stay"],
    ["Prompt injection was red-teamed", "a real model found one hole, it was closed and a test added"],
  ];
  rows.forEach((r, i) => {
    const y = 1.6 + i * 1.12;
    card(s, MX, y, 7.2, 1.0, T.soft, "grounding " + (i + 1));
    badge(s, MX + 0.2, y + 0.29, i + 1);
    s.addText([{ text: r[0], options: { bold: true, fontSize: 15, color: T.dark, breakLine: true } }, { text: r[1], options: { fontSize: 12.5, color: T.ink } }], { x: MX + 0.8, y, w: 6.3, h: 1.0, valign: "middle", margin: 0, isTextBox: true, objectName: "grounding text" });
  });
  card(s, 8.1, 1.6, 4.6, 4.35, T.dark, "what-if-wrong card");
  s.addText("When the model is wrong", { x: 8.35, y: 1.75, w: 4.1, h: 0.45, fontSize: 18, bold: true, fontFace: "Cambria", color: "FFFFFF", margin: 0, isTextBox: true, objectName: "h" });
  s.addText([
    { text: "A statement the finding cannot support is rejected before anyone reads it.", options: { breakLine: true, paraSpaceAfter: 8 } },
    { text: "The reviewer sees the engine's own explanation instead, marked by its source.", options: { breakLine: true, paraSpaceAfter: 8 } },
    { text: "Honest note: in one 25-case run, 17 answers fell back to the template (timeouts and server errors). The fallback worked, and the live-answer sample is small.", options: {} },
  ], { x: 8.35, y: 2.3, w: 4.1, h: 3.5, fontSize: 14, color: "DDEEEB", margin: 0, valign: "top", isTextBox: true, objectName: "wrong text" });
  note(s, "The same guard checks every cached template after it is filled. Nothing in this path can change a status.", 6.2);
}

// ---- RAG
{
  const s = slide("Ask the rulebook: answers that quote, never invent", { n: "rag" });
  stat(s, MX, 1.5, 3.6, "81%", "right passage in the top 3 when the question is in the officer's own words (was 5% with keywords)", T.teal, 44);
  stat(s, MX + 4.1, 1.5, 3.6, "6 of 6", "French and Arabic questions answered (keywords: 0 of 6)", T.teal, 44);
  stat(s, MX + 8.2, 1.5, 3.9, "96%", "top 3 on the original 26 questions, unchanged", T.teal, 44);
  card(s, MX, 3.5, 5.9, 3.15, T.soft, "rag how");
  s.addText("How it works", { x: MX + 0.25, y: 3.6, w: 5.4, h: 0.4, fontSize: 17, bold: true, fontFace: "Cambria", color: T.dark, margin: 0, isTextBox: true, objectName: "h" });
  bullets(s, ["Searches the text of the 15 rules and the payer policy table as it is now, each passage naming its rule or table version", "Ranks by meaning: a small multilingual model on the server's own CPU, offline, in its own container with no route out", "If that service is off or down, the keyword search answers and the console says so", "The answer is the quoted passage and its source. Nothing is generated"], MX + 0.25, 4.1, 5.4, 2.5, 13);
  card(s, MX + 6.2, 3.5, 5.9, 3.15, T.soft, "rag limits");
  s.addText("Why it is safe, and where it stops", { x: MX + 6.45, y: 3.6, w: 5.4, h: 0.4, fontSize: 17, bold: true, fontFace: "Cambria", color: T.dark, margin: 0, isTextBox: true, objectName: "h" });
  bullets(s, ["It never reads a claim, so it adds no privacy risk", "Retrieval, not reasoning: it will not combine passages", "A question near the topic but not answered by it gets the nearest passage, so every answer says: it may not answer your question", "Right passage first fell from 88% to 81% on the original 26 (next slide)"], MX + 6.45, 4.1, 5.4, 2.5, 13);
}

// ---- RAG measured
{
  const s = slide("Ask the rulebook: what changed, measured", { n: "rag2" });
  const bad = { color: T.red, bold: true }, good = { color: T.teal, bold: true };
  table(s, ["Questions (number in brackets)", "Keywords (BM25)", "By meaning"], [
    ["Operator wording, English: right passage in top 3 (21)", "5%", { text: "81%", ...good }],
    ["...refused although the documents answer it", "76%", { text: "5%", ...good }],
    ["No word shared with the rule (8)", "0%", { text: "75%", ...good }],
    ["French and Arabic (6)", "0%", { text: "100%", ...good }],
    ["Original 26 questions: right passage first / in top 3", "88% / 96%", { text: "81% / 96%", ...bad }],
    ["Off-topic, plainly (4): refused", "100%", "100%"],
    ["Off-topic, close to the topic (4): refused", "75%", { text: "25%", ...bad }],
  ], MX, 1.45, 7.6, [4.6, 1.5, 1.5], 12.5, 0.5);
  card(s, 8.5, 1.45, 4.3, 5.2, T.dark, "rag proof");
  s.addText("The product gives these numbers", { x: 8.75, y: 1.6, w: 3.8, h: 0.4, fontSize: 16, bold: true, fontFace: "Cambria", color: "FFFFFF", margin: 0, isTextBox: true, objectName: "h" });
  s.addText([
    { text: "The experiment ran in Python on a graphics card. The product is a Rust server with a CPU service. Through the real route, every figure matches to four decimals (rust/tools/rag_parity.py).", options: { breakLine: true, paraSpaceAfter: 8 } },
    { text: "With the service stopped, the keyword search answers and keeps its old numbers.", options: { breakLine: true, paraSpaceAfter: 8 } },
    { text: "A relevance judge (Laya) and a keyword-plus-meaning mix were tried and left out: neither beat this.", options: {} },
  ], { x: 8.75, y: 2.1, w: 3.8, h: 3.4, fontSize: 13, color: "DDEEEB", margin: 0, valign: "top", isTextBox: true, objectName: "proof text" });
  s.addText("Small sets (4 to 21 questions), written by the team, not by officers. French and Arabic not reviewed by a native speaker. Not run on the target server.", { x: 8.75, y: 5.45, w: 3.8, h: 1.0, fontSize: 11.5, italic: true, color: "CADCD9", margin: 0, valign: "top", isTextBox: true, objectName: "rag limits note" });
  note(s, "Dev half used to choose the threshold (0.29); the table shows the held-out half. Counts in brackets are test questions.", 6.8);
}

// Storage and encryption
{
  const s = slide("How the data is stored, and how it is encrypted", { n: "storage" });
  table(s, ["Where", "What it holds", "How it is protected"], [
    ["MongoDB: claims", "Each claim version, its receipt, results, decisions", "Claim, results, advisory, explanations, corrections and each decision are encrypted field by field"],
    ["MongoDB: users", "Badge, password, authenticator seed", "bcrypt hash (cost 12); seed and names encrypted"],
    ["MongoDB: queue", "Configuration versions, deals, dead letters, counters, template cache", "No claim values. Cache and counters expire by themselves (7 and 3 days)"],
    ["Redis Streams", "Only 'run claim X, version V'", "No claim data. Losing it loses nothing"],
    ["Audit logs", "Review log and security log, hash-chained", "Encrypted on disk, signed checkpoints"],
  ], MX, 1.5, 7.6, [1.5, 2.6, 3.5], 11.5, 0.5);
  card(s, 8.4, 1.5, 4.3, 5.15, T.dark, "encryption card");
  s.addText("How the encryption works", { x: 8.65, y: 1.6, w: 3.8, h: 0.4, fontSize: 17, bold: true, fontFace: "Cambria", color: "FFFFFF", margin: 0, isTextBox: true, objectName: "h" });
  s.addText([
    { text: "Done in the application, before the database sees the data", options: { bullet: { indent: 14 }, breakLine: true, paraSpaceAfter: 5 } },
    { text: "AES-128-CBC with an HMAC-SHA256 tag (Fernet)", options: { bullet: { indent: 14 }, breakLine: true, paraSpaceAfter: 5 } },
    { text: "Each token names its claim, version and field: moving one under another claim is detected and refused", options: { bullet: { indent: 14 }, breakLine: true, paraSpaceAfter: 5 } },
    { text: "Readable on purpose: ids, state, lane, score, timestamps, hashes, and a keyed hash of the patient id to find earlier claims", options: { bullet: { indent: 14 }, breakLine: true, paraSpaceAfter: 5 } },
    { text: "Reviewers see pseudonyms, not identifiers", options: { bullet: { indent: 14 }, breakLine: true, paraSpaceAfter: 5 } },
    { text: "Limits: whoever holds the key sees everything, and the shape of the data is visible. The key can be rotated by one command", options: { bullet: { indent: 14 } } },
  ], { x: 8.65, y: 2.1, w: 3.85, h: 4.45, fontSize: 12.5, color: "DDEEEB", margin: 0, valign: "top", isTextBox: true, objectName: "encryption text" });
}

// ---- data protection
{
  const s = slide("Personal-data law: what the system does", { n: "pdpl" });
  const ok = (t) => ({ text: t, bold: false });
  table(s, ["Obligation", "What the system does", "Not built"], [
    ["Data stays inside", "No outside calls from the engine, the queue or the AI. The one optional call sends a hash for a timestamp", { text: "-", color: T.muted }],
    ["Minimisation", "Pseudonyms by default. Unmasking needs a permission, a reason, and is limited to 60 claims an hour. The AI never sees a claim value", { text: "-", color: T.muted }],
    ["Access control", "Four levels enforced on the server. The person who signs cannot countersign their own decision", { text: "-", color: T.muted }],
    ["Security", "Encrypted at rest and in transit, two-factor sign-in, revocable sessions, tamper-evident audit", { text: "-", color: T.muted }],
    ["Accuracy", "A claim is never edited: a correction is a new version, checked again, the old one still visible", { text: "-", color: T.muted }],
    ["Rights of the person", "All claims of one person can be found without the identifier being readable", { text: "Export, correction request and erasure", bold: true, color: T.red }],
    ["Retention", "Short-lived data expires by itself (sign-in counters, used codes, revoked tokens)", { text: "A retention period for claims and logs, and the purge", bold: true, color: T.red }],
  ], MX, 1.5, 12.1, [2.2, 6.7, 3.2], 12, 0.58);
  note(s, "No software is compliant by itself: the law binds the organisation that runs it. This is what the software gives that organisation.", 6.55);
}

// =====================================================================================================================================
section("Storage and built this week");
// 11 Copilot
{
  const s = slide("The reviewer copilot: what is wrong, what to do", { n: "copilot" });
  tag(s, 11.2, 0.08, "new", T.amber, T.ink, 0.9);
  img(s, "copilot.png", MX, 1.5, 6.1, 4.92);
  bullets(s, [
    "One summary: how many findings, which severity, in the order that clears the claim fastest",
    "One suggestion per finding: a value to correct, a value to check, or information to ask for",
    "Built by code from the findings. No model, so nothing to hallucinate",
    "A value is proposed only if it follows from other values on the same claim. Nothing missing is invented, no identifier can be edited",
  ], 7.1, 1.6, 5.6, 4.8, 15);
  note(s, "A claim with no finding reads: Pre-validation clearance, not payer approval or a promise of payment.");
}

// 12 What-if and recheck
{
  const s = slide("Try a correction, then save it as a new version", { n: "whatif" });
  tag(s, 11.2, 0.08, "new", T.amber, T.ink, 0.9);
  img(s, "whatif.png", MX, 1.5, 6.0, 3.86);
  const st = [["What-if", "the engine runs on a copy. Nothing is saved"], ["Recheck", "the correction becomes version 2 and goes through triage"], ["Original", "version 1 is kept exactly as received and closed"], ["Cleared findings", "recorded with the reviewer's reason"]];
  st.forEach((r, i) => {
    const y = 1.5 + i * 0.97;
    card(s, 6.9, y, 5.8, 0.85, T.soft, "flow " + (i + 1));
    s.addText([{ text: r[0], options: { bold: true, fontSize: 14, color: T.dark, breakLine: true } }, { text: r[1], options: { fontSize: 12.5, color: T.ink } }], { x: 7.1, y, w: 5.4, h: 0.85, valign: "middle", margin: 0, isTextBox: true, objectName: "flow text" });
  });
  card(s, MX, 5.4, 12.1, 1.3, "FFF1DB", "controls card");
  s.addText([
    { text: "Controls: ", options: { bold: true, color: T.dark } },
    { text: "only the holder of the claim can try or apply a correction. Clearing a high-severity finding needs the same senior authority as deciding it. Patient, member and claim identifiers cannot be edited. A recheck needs a reason that does not contain the identifier.", options: { color: T.ink } },
  ], { x: MX + 0.25, y: 5.4, w: 11.6, h: 1.3, fontSize: 14.5, valign: "middle", margin: 0, isTextBox: true, objectName: "controls text" });
}

// 13 Copilot measurements
{
  const s = slide("Do the proposals work? Measured against the real engine", { n: "copilot_eval" });
  s.addChart(pres.charts.BAR, [
    { name: "Proposed", labels: ["R007 line amount", "R012 claim total", "R015 currency"], values: [30, 34, 29] },
    { name: "Cleared the finding", labels: ["R007 line amount", "R012 claim total", "R015 currency"], values: [30, 34, 29] },
  ], {
    x: MX, y: 1.5, w: 6.4, h: 4.7, barDir: "col", barGrouping: "clustered", chartColors: [T.seafoam, T.teal], showTitle: true, title: "Proposed corrections that clear their finding",
    titleFontSize: 14, titleColor: T.dark, titleFontFace: "+mn-lt", showValue: true, dataLabelPosition: "outEnd", dataLabelFontSize: 13, dataLabelColor: T.ink, dataLabelFontFace: "+mn-lt",
    catAxisLabelColor: T.ink, catAxisLabelFontSize: 12, catAxisLabelFontFace: "+mn-lt", valAxisLabelColor: T.muted, valAxisLabelFontSize: 11, valAxisLabelFontFace: "+mn-lt",
    valGridLine: { color: "D5E1DF", size: 0.5 }, catGridLine: { style: "none" }, showLegend: true, legendPos: "b", legendFontSize: 12, legendColor: T.ink, legendFontFace: "+mn-lt", valAxisMinVal: 0,
  });
  stat(s, 7.4, 1.5, 2.6, "777", "flagged findings on 600 public claims, each with exactly one suggestion", T.teal, 40);
  stat(s, 10.2, 1.5, 2.6, "0", "cited paths missing on the claim. 0 texts with approval or payment words", T.teal, 40);
  stat(s, 7.4, 3.5, 2.6, "0 new", "findings created when all proposals on a claim are applied together", T.teal, 40);
  stat(s, 10.2, 3.5, 2.6, "4,200", "attempts to edit an identifier, all refused", T.teal, 40);
  card(s, 7.4, 5.45, 5.3, 1.2, "FFF1DB", "limit card");
  s.addText("Limit: only three rules have computable proposals. For the other twelve the copilot asks or checks. The reviewer still compares the figure with the bill.", { x: 7.6, y: 5.45, w: 4.95, h: 1.2, fontSize: 13, color: T.ink, valign: "middle", margin: 0, isTextBox: true, objectName: "limit text" });
}

// =====================================================================================================================================
section("Less work for the operator");
// 14 Workload reduction
{
  const s = slide("Fewer decisions for a person, with the risk bounded", { n: "workload" });
  tag(s, 10.9, 0.08, "simulated", T.amber, T.ink, 1.3);
  const items = [
    ["One-click confirm", "Confirming needs no typed reason. Dismissing still does, because it overrides the engine.", "on"],
    ["Bulk confirm", "For rules reviewers almost always uphold. A random share still gets a full review. Bulk decisions never count as evidence.", "off"],
    ["Auto-clear green claims", "Pre-validation clearance only, with a random audit sample and an exact bound on the miss rate.", "off"],
    ["Auto-request information", "For UNABLE_TO_ASSESS findings. The audit sample goes to a person.", "off"],
  ];
  items.forEach((r, i) => {
    const y = 1.55 + i * 1.12;
    card(s, MX, y, 7.0, 1.0, T.soft, "lever " + (i + 1));
    tag(s, MX + 5.75, y + 0.35, r[2] === "on" ? "on" : "off by default", r[2] === "on" ? T.teal : "D5E1DF", r[2] === "on" ? "FFFFFF" : T.ink, 1.1);
    s.addText([{ text: r[0], options: { bold: true, fontSize: 15, color: T.dark, breakLine: true } }, { text: r[1], options: { fontSize: 12, color: T.ink } }], { x: MX + 0.2, y, w: 5.45, h: 1.0, valign: "middle", margin: 0, isTextBox: true, objectName: "lever text" });
  });
  stat(s, 8.1, 1.55, 2.3, "-39%", "findings a person decides per 500 claims: 650 to 399", T.teal, 44);
  stat(s, 10.5, 1.55, 2.2, "0", "wrong against the answer key", T.teal, 44);
  card(s, 8.1, 3.75, 4.6, 2.9, T.dark, "honesty card");
  s.addText("What is not known", { x: 8.35, y: 3.85, w: 4.1, h: 0.4, fontSize: 17, bold: true, fontFace: "Cambria", color: "FFFFFF", margin: 0, isTextBox: true, objectName: "h" });
  s.addText("Time per finding has not been measured. The savings are counted in decisions, on the real pipeline with simulated reviewers. The first real step is a timed session with a few officers.", { x: 8.35, y: 4.3, w: 4.1, h: 2.2, fontSize: 14, color: "DDEEEB", margin: 0, valign: "top", isTextBox: true, objectName: "honesty text" });
}

// ---- autoclear
{
  const s = slide("Auto-clear green claims, with a random audit", { n: "autoclear" });
  tag(s, 10.9, 0.08, "off by default", T.amber, T.ink, 1.5);
  bullets(s, [
    "A claim with nothing flagged and an intact set of 15 results can be cleared by the system (actor system:auto-clear)",
    "Except a random 5% (audit_percent, 1 to 100) that still go to a person. The draw is written on the claim",
    "A damaged claim, or one with a flagged finding, is never cleared by the system",
    "System-cleared claims never count as verified by a person in the feedback reports",
  ], MX, 1.5, 6.0, 3.0, 13.5);
  stat(s, MX, 4.55, 2.9, "159 of 500", "claims cleared by the system (31.8%), on 20 random batches of the 600 labelled claims", T.teal, 34);
  stat(s, MX + 3.1, 4.55, 2.9, "0", "cleared claims that had a finding in the answer key", T.teal, 34);
  s.addChart(pres.charts.BAR, [{ name: "Upper bound", labels: ["30", "60", "150", "300", "600", "1,000", "3,000"], values: [9.5, 4.9, 2.0, 1.0, 0.5, 0.3, 0.1] }], {
    x: 6.9, y: 1.45, w: 5.8, h: 3.2, barDir: "col", chartColors: [T.teal], showTitle: true, title: "95% upper bound on the miss rate (%) after n audited claims, none missed",
    titleFontSize: 12, titleColor: T.dark, titleFontFace: "+mn-lt", showValue: true, dataLabelPosition: "outEnd", dataLabelFontSize: 12, dataLabelColor: T.ink, dataLabelFontFace: "+mn-lt",
    catAxisLabelColor: T.ink, catAxisLabelFontSize: 11, catAxisLabelFontFace: "+mn-lt", valAxisLabelColor: T.muted, valAxisLabelFontSize: 10, valAxisLabelFontFace: "+mn-lt",
    valGridLine: { color: "D5E1DF", size: 0.5 }, catGridLine: { style: "none" }, showLegend: false, valAxisMinVal: 0,
  });
  card(s, 6.9, 4.8, 5.8, 1.85, T.dark, "audit panel card");
  s.addText([
    { text: "The audit panel (administrator)", options: { bold: true, fontSize: 15, fontFace: "Cambria", color: "FFFFFF", breakLine: true, paraSpaceAfter: 4 } },
    { text: "Claims cleared, verified clear, escalated, and the escalation rate with an exact 95% upper bound. Every escalation counts as a possible miss. Read-only advice: keep 5% until 300 audited claims are resolved; lower it only when the bound is under 1%.", options: { fontSize: 12, color: "DDEEEB" } },
  ], { x: 7.1, y: 4.85, w: 5.4, h: 1.75, margin: 0, valign: "middle", isTextBox: true, objectName: "audit panel text" });
  note(s, "Synthetic mix, engine equal to the key: this shows what the policy does here, not what a hospital would see.", 6.8);
}

// ---- autorequest
{
  const s = slide("Auto-request, and what each step saves", { n: "autorequest" });
  tag(s, 10.9, 0.08, "off by default", T.amber, T.ink, 1.5);
  table(s, ["Per 500 claims", "Decided without a person", "Findings a person decides"], [
    ["Today", "0", "650"],
    ["Auto-clear only", "159 (31.7%)", "650"],
    ["+ auto-request, medium severity", "170 (34.1%)", "541 (-16.9%)"],
    [{ text: "+ auto-request, all severities", bold: true }, { text: "196 (39.1%)", bold: true, color: T.teal }, { text: "399 (-38.6%)", bold: true, color: T.teal }],
  ], MX, 1.5, 7.2, [3.0, 2.2, 2.0], 12.5, 0.5);
  bullets(s, [
    "For a finding that only says data is missing (UNABLE_TO_ASSESS), the system records the request itself (system:auto-request)",
    "The reason is the rulebook's own corrective-action text, never claim data",
    "A claim that also has real findings keeps them for a person",
    "A random share is handled by a person in full, and the audit counts how often they asked too",
    "0 recorded requests were on a finding the answer key does not call missing data",
  ], MX, 4.2, 7.2, 2.5, 12.5);
  s.addChart(pres.charts.BAR, [{ name: "Findings", labels: ["Today", "Auto-clear", "+ medium", "+ all"], values: [650, 650, 541, 399] }], {
    x: 8.0, y: 1.45, w: 4.7, h: 4.2, barDir: "col", chartColors: [T.teal], showTitle: true, title: "Findings a person decides per 500 claims",
    titleFontSize: 13, titleColor: T.dark, titleFontFace: "+mn-lt", showValue: true, dataLabelPosition: "outEnd", dataLabelFontSize: 12, dataLabelColor: T.ink, dataLabelFontFace: "+mn-lt",
    catAxisLabelColor: T.ink, catAxisLabelFontSize: 11, catAxisLabelFontFace: "+mn-lt", valAxisLabelColor: T.muted, valAxisLabelFontSize: 10, valAxisLabelFontFace: "+mn-lt",
    valGridLine: { color: "D5E1DF", size: 0.5 }, catGridLine: { style: "none" }, showLegend: false, valAxisMinVal: 0,
  });
  note(s, "Real engine and triage, 20 random batches, 5% audit. No review time is measured: these are shares of claims and findings, not minutes.", 6.8);
}

// ---- confirm
{
  const s = slide("One-click confirm, and bulk confirm for reliable rules", { n: "confirm" });
  const cards = [
    ["One-click confirm", "on", ["Confirming agrees with the engine, so it needs no typed reason; dismissing still does", "Confirm the issue, or Confirm all N findings in two clicks, or press c twice", "When the last finding is decided the console opens the next claim", "Same record in the review log and the security log as before"]],
    ["Bulk confirm", "off by default", ["Only for rules whose findings reviewers almost always uphold: an exact 95% lower bound of the upheld rate above a setting, from at least 20 decisions by at least 3 reviewers", "A By rule view: open the list, look, confirm all in two clicks", "Only confirm is possible"]],
    ["Guards, all on the server", "", ["The rule must qualify at that moment, checked on every request", "Each claim goes through the normal decision path: lease, clearance, two-senior sign-off, masking, both logs", "A random share is left for a full review", "Bulk decisions never count as evidence, so a rubber stamp cannot keep a rule eligible"]],
  ];
  cards.forEach((c, i) => {
    const x = MX + i * 4.1;
    card(s, x, 1.5, 3.9, 4.45, T.soft, "confirm card " + (i + 1));
    s.addText(c[0], { x: x + 0.25, y: 1.6, w: 3.4, h: 0.5, fontSize: 17, bold: true, fontFace: "Cambria", color: T.dark, margin: 0, isTextBox: true, objectName: "confirm title" });
    if (c[1]) tag(s, x + 0.25, 2.15, c[1], c[1] === "on" ? T.teal : "D5E1DF", c[1] === "on" ? "FFFFFF" : T.ink, 1.4);
    bullets(s, c[2], x + 0.25, c[1] ? 2.65 : 2.2, 3.45, 3.2, 12.5);
  });
  card(s, MX, 6.05, 12.1, 0.65, "FFF1DB", "confirm limit");
  s.addText("Limits: the 'look at the list first' step is enforced by the screen only. The evidence for the reliable rules came from simulated reviewers. One administrator can switch these on, logged but without a second approval, and there is no automatic brake.", { x: MX + 0.2, y: 6.05, w: 11.7, h: 0.65, fontSize: 12, color: T.ink, valign: "middle", margin: 0, isTextBox: true, objectName: "confirm limit text" });
}

// 15 Learning from feedback
{
  const s = slide("Learning from feedback, and a model we set aside", { n: "feedback" });
  card(s, MX, 1.55, 5.9, 5.0, T.soft, "left");
  s.addText("Workload report (live)", { x: MX + 0.25, y: 1.65, w: 5.4, h: 0.45, fontSize: 18, bold: true, fontFace: "Cambria", color: T.dark, margin: 0, isTextBox: true, objectName: "h" });
  bullets(s, ["Reads reviewer confirmations and dismissals per rule", "Lists rules reviewers keep overriding: candidates to retune", "Lists rules almost always upheld: candidates for light review, with a bounded price", "Read-only. It changes nothing by itself"], MX + 0.25, 2.2, 5.4, 2.6, 15);
  s.addText("In simulation both bad rules were found in 20 of 20 histories after 5,000 decisions, with 0 false alarms.", { x: MX + 0.25, y: 5.0, w: 5.4, h: 1.3, fontSize: 13, italic: true, color: T.muted, margin: 0, valign: "top", isTextBox: true, objectName: "note" });
  card(s, 6.8, 1.55, 5.9, 5.0, T.soft, "right");
  s.addText("Laya fine-tune (archived)", { x: 7.05, y: 1.65, w: 5.4, h: 0.45, fontSize: 18, bold: true, fontFace: "Cambria", color: T.dark, margin: 0, isTextBox: true, objectName: "h" });
  table(s, ["Test split, 150 claims", "Accuracy", "Macro-F1"], [["Zero-shot", "0.631", "0.289"], ["Fine-tuned", "0.971", "0.915"], ["Always guess PASS", "0.835", "-"]], 7.05, 2.2, 5.4, [2.6, 1.4, 1.4], 13, 0.4);
  bullets(s, ["It learned the rulebook, well calibrated (ECE 0.027), but finds only 64% of real FAILs", "Its labels are the engine's own, so after the engine it cannot save an operator any time", "Archived, not in the decision path. A confidence is never shown as a probability"], 7.05, 4.0, 5.4, 2.5, 14);
}

// 16 Advisory
{
  const s = slide("Advisory checks that stay out of the official result", { n: "advisory" });
  table(s, ["Rule", "What it looks for", "Severity"], [
    ["E001", "Procedure pair billed without an exception modifier", "medium"],
    ["E002", "Exception modifier on a never-bundle pair", "high"],
    ["E003", "Event date required by the diagnosis", "medium"],
    ["E004", "Route of administration missing", "medium"],
    ["E005", "Diagnosis cannot be the primary one", "medium"],
    ["E101", "A line duplicates one on an earlier claim", "high"],
    ["E102", "Authorization exhausted across claims", "high"],
    ["E103", "Daily quantity across claims", "medium"],
  ], MX, 1.55, 7.4, [0.9, 5.2, 1.3], 13, 0.5);
  card(s, 8.3, 1.55, 4.4, 4.1, T.soft, "rules card");
  s.addText("Why they are separate", { x: 8.55, y: 1.65, w: 3.9, h: 0.4, fontSize: 17, bold: true, fontFace: "Cambria", color: T.dark, margin: 0, isTextBox: true, objectName: "h" });
  bullets(s, ["Own ids, file and schema. They never change an official result", "E102 is the cross-claim authorization usage you described, on explicitly synthetic history", "E001 to E005 cannot fire on public data by design: they use invented codes", "4,800 of 4,800 results agree with an independent oracle"], 8.55, 2.15, 3.95, 3.4, 13.5);
  note(s, "E102 fires only when earlier claims contributed units, so it never double-reports what R009 already fails.", 6.4);
}

// Also built
{
  const s = slide("Also built since Phase 1", { n: "alsobuilt" });
  const cards = [
    ["Payer rules as data", "not editable code", ["The 15 rules are code, hashed with the build", "A payer's table (window, providers, authorization services, limits) is data an administrator can change", "Validated, shown as differences, needs a reason and a second administrator", "Each change is a numbered version in a hash-chained log; every receipt names the version"]],
    ["Specification and UML", "documents", ["20 use cases, each with a main scenario, alternatives and failures", "14 UML diagrams checked against the code: states, collections, package dependencies", "A catalogue of failures and recovery", "A French cahier des charges for the UML teacher"]],
    ["Live console updates", "no claim data", ["The console hears of a change in about 1.7 seconds instead of a 20-second poll", "The stream carries only 'changed'; the page re-reads through the normal checked routes", "A revoked session ends the stream"]],
  ];
  cards.forEach((c, i) => {
    const x = MX + i * 4.1;
    card(s, x, 1.5, 3.9, 5.15, T.soft, "built card " + (i + 1));
    s.addText(c[0], { x: x + 0.25, y: 1.62, w: 3.4, h: 0.5, fontSize: 18, bold: true, fontFace: "Cambria", color: T.dark, margin: 0, isTextBox: true, objectName: "built title" });
    tag(s, x + 0.25, 2.2, c[1], T.amber, T.ink, 1.5);
    bullets(s, c[2], x + 0.25, 2.75, 3.45, 3.8, 13);
  });
}

// ---- console and desktop
{
  const s = slide("The console and the desktop app", { n: "console" });
  table(s, ["Who", "Pages they see"], [
    ["Viewer (level 1)", "All claims, read only"],
    ["Reviewer, senior (2, 3)", "My inbox, claim review with the copilot, what-if and recheck, Ask the rulebook, Import from files"],
    ["Administrator (4)", "Queue overview, Submit claims, Routing settings, Users, Payer rules, System health, Audit and trace. An administrator cannot decide a claim"],
  ], MX, 1.5, 7.4, [2.3, 5.1], 12.5, 0.7);
  bullets(s, ["Pages are chosen by the permissions the server reports (hidden, not greyed out), and the server checks again on every request", "Server text is only ever put on the page as text; a test scans every script for markup sinks", "The lease countdown is on screen and a heartbeat keeps it alive", "The console hears of changes by a live stream, not a 20-second poll"], MX, 4.35, 7.4, 2.3, 13);
  card(s, 8.3, 1.5, 4.4, 5.15, T.dark, "desktop card");
  s.addText("Desktop app", { x: 8.55, y: 1.6, w: 3.9, h: 0.4, fontSize: 17, bold: true, fontFace: "Cambria", color: "FFFFFF", margin: 0, isTextBox: true, objectName: "h" });
  s.addText([
    { text: "ClaimGuard.exe, one file of about 27 MB (v0.2.0, a pre-release)", options: { bullet: { indent: 14 }, breakLine: true, paraSpaceAfter: 6 } },
    { text: "The same console in its own window; nothing to install beside it", options: { bullet: { indent: 14 }, breakLine: true, paraSpaceAfter: 6 } },
    { text: "A hash is published with the release", options: { bullet: { indent: 14 }, breakLine: true, paraSpaceAfter: 6 } },
    { text: "A clickable mobile prototype exists; after your meeting of 27 September the web console is the main target", options: { bullet: { indent: 14 }, breakLine: true, paraSpaceAfter: 6 } },
    { text: "Not done: a manual accessibility audit, and a test on a phone", options: { bullet: { indent: 14 } } },
  ], { x: 8.55, y: 2.1, w: 3.95, h: 4.4, fontSize: 13, color: "DDEEEB", margin: 0, valign: "top", isTextBox: true, objectName: "desktop text" });
}

// =====================================================================================================================================
section("Trust");
// 17 Security
{
  const s = slide("Security: attacked, mutated and fuzzed before you did it", { n: "security" });
  stat(s, MX, 1.55, 2.8, "116", "black-box attack checks, all passing", T.teal, 48);
  stat(s, MX + 3.1, 1.55, 2.8, "93", "deliberate breakages of the security and queue rules, each caught by a test", T.teal, 48);
  stat(s, MX + 6.2, 1.55, 2.8, "400,000", "fuzzed requests through the API", T.teal, 48);
  stat(s, MX + 9.3, 1.55, 2.8, "60", "claims one person may unmask an hour, with a reason", T.teal, 48);
  const items = [
    ["Identity", "badge, password, authenticator code. Four clearance levels, enforced on the server. A hidden action is not offered, not just disabled"],
    ["Privacy", "reviewers see pseudonyms. Unmasking needs its own permission, a reason without the identifier, and an hourly limit"],
    ["Two people", "the most dangerous administrator actions need a second administrator with a fresh code"],
    ["At rest", "claims, results and corrections are encrypted in MongoDB. Audit logs encrypted, key rotation tested"],
  ];
  items.forEach((r, i) => {
    const col = i % 2, row = Math.floor(i / 2);
    const x = MX + col * 6.1, y = 3.6 + row * 1.5;
    card(s, x, y, 5.95, 1.35, T.soft, "sec " + (i + 1));
    s.addText([{ text: r[0], options: { bold: true, fontSize: 15, color: T.dark, breakLine: true } }, { text: r[1], options: { fontSize: 12.5, color: T.ink } }], { x: x + 0.2, y, w: 5.55, h: 1.35, valign: "middle", margin: 0, isTextBox: true, objectName: "sec text" });
  });
  note(s, "Not done: nobody outside the team has attacked it, and a lone administrator or one person holding two accounts is logged but not prevented.", 6.85);
}

// ---- signin
{
  const s = slide("Sign-in: hard to guess, hard to lock out, equal in time", { n: "signin" });
  stat(s, MX, 1.5, 2.8, "5 in 15 min", "failures per badge and client; then that client is refused", T.teal, 32);
  stat(s, MX + 3.1, 1.5, 2.8, "100", "failures per 15 minutes for one address, over all badges", T.teal, 32);
  stat(s, MX + 6.2, 1.5, 2.8, "30 days", "a client the owner signed in from is remembered, as a keyed hash", T.teal, 32);
  stat(s, MX + 9.3, 1.5, 2.8, "within 3%", "time taken by 8 kinds of refused sign-in: 80.3 to 82.6 ms", T.teal, 32);
  const cards = [
    ["A stranger cannot lock you out", ["The account lock only keeps out clients the owner has never used", "A correct sign-in by the owner clears the lock", "Still true: a stranger can keep someone out of a new device for 15 minutes"]],
    ["Counts survive a restart", ["The counters live in the database, updated atomically and expiring by themselves", "A restart, or a second server process, sees the same counts", "Addresses are stored only as keyed hashes"]],
    ["Every refusal looks the same", ["Unknown badge, wrong password, wrong code, replayed code, locked, inactive: the same steps, the same time, the same answer", "Comparison of the anti-forgery token and the session signature is constant-time", "Measured in-process, not across a network"]],
  ];
  cards.forEach((c, i) => {
    const x = MX + i * 4.1;
    card(s, x, 3.65, 3.9, 3.0, T.soft, "signin card " + (i + 1));
    s.addText(c[0], { x: x + 0.25, y: 3.75, w: 3.4, h: 0.45, fontSize: 15, bold: true, fontFace: "Cambria", color: T.dark, margin: 0, isTextBox: true, objectName: "signin title" });
    bullets(s, c[1], x + 0.25, 4.25, 3.45, 2.35, 12);
  });
}

// ---- backup
{
  const s = slide("Backup, restore and key rotation", { n: "backup" });
  card(s, MX, 1.5, 5.9, 5.15, T.soft, "backup card");
  s.addText("Encrypted backup, proven by wiping", { x: MX + 0.25, y: 1.6, w: 5.4, h: 0.5, fontSize: 16, bold: true, fontFace: "Cambria", color: T.dark, margin: 0, isTextBox: true, objectName: "h" });
  bullets(s, [
    "One encrypted file (AES-256, 600,000 key-stretching rounds, key from a file) holding both databases, both audit logs and the audit mirror, plus a checksum. It never overwrites",
    "Restore refuses a wrong key, a damaged file or contents that fail their checksums, and fails loudly if any document does not restore",
    "The test (23 checks): put users and 12 claims in through the real API, back up, destroy every volume, restore. The same administrator signs in with the same password and authenticator; the audit chains verify",
    "Testing found two real problems, both fixed: a restore that replaced the database's own accounts mid-way, and a script that reported success after a failure",
  ], MX + 0.25, 2.2, 5.4, 4.35, 12.5);
  card(s, MX + 6.2, 1.5, 5.9, 5.15, T.soft, "rotation card");
  s.addText("Keys can be rotated", { x: MX + 6.45, y: 1.6, w: 5.4, h: 0.5, fontSize: 16, bold: true, fontFace: "Cambria", color: T.dark, margin: 0, isTextBox: true, objectName: "h" });
  bullets(s, [
    "cg rotate-key re-encrypts every stored claim, result, explanation, decision and name from the current data key to a new one, and rebuilds the patient index",
    "It stops rather than damage a document that opens with neither key, skips and reports a document changed meanwhile, and can be repeated after an interruption",
    "A dry run first. Tested against a real database: the old key is refused afterwards and the stored data holds nothing readable",
    "cg rotate-fernet-key does the same for authenticator seeds, so nobody has to enrol again",
    "The key for claim data and the key for authenticator seeds are separate",
  ], MX + 6.45, 2.2, 5.4, 4.35, 12.5);
}

// ---- stress
{
  const s = slide("Stress tests: floods and hostile input", { n: "stress" });
  stat(s, MX, 1.5, 2.8, "29,700", "requests from 300 clients in 20 seconds, all answered", T.teal, 36);
  stat(s, MX + 3.1, 1.5, 2.8, "400", "parallel sign-ins: ordinary refusals, then 429, no server error", T.teal, 36);
  stat(s, MX + 6.2, 1.5, 2.8, "1 to 12 ms", "answer time for a polite client during every flood", T.teal, 36);
  stat(s, MX + 9.3, 1.5, 2.8, "87 MB", "peak memory under flood (66 MB idle)", T.teal, 36);
  const cards = [
    ["The flood harness", ["16 checks: 3,000 idle connections, 300 busy clients, 200 clients without a session (all refused), a 400-client sign-in flood, 100 clients sending large bodies", "The server was healthy afterwards", "One machine and one address: no distributed attack"]],
    ["Hostile input in a real browser", ["19 checks in Chrome: script-injection payloads planted in notes, invoice numbers, provider ids, currencies, line ids, attachment text, names and failed-sign-in badges", "On every screen no payload ran and the text showed as plain text"]],
    ["Fuzzing and breakage", ["400,000 random requests through the API, with sessions of each role", "93 deliberate breakages of the security rules, each caught by a test", "Not tested: floods below the HTTP layer, attack on the database server"]],
  ];
  cards.forEach((c, i) => {
    const x = MX + i * 4.1;
    card(s, x, 3.65, 3.9, 3.0, T.soft, "stress card " + (i + 1));
    s.addText(c[0], { x: x + 0.25, y: 3.75, w: 3.4, h: 0.45, fontSize: 15, bold: true, fontFace: "Cambria", color: T.dark, margin: 0, isTextBox: true, objectName: "stress title" });
    bullets(s, c[1], x + 0.25, 4.25, 3.45, 2.35, 12);
  });
}

// 18 Audit
{
  const s = slide("Everything is on record, and an edit is caught", { n: "audit" });
  table(s, ["Moment", "What is written"], [
    ["Claim received", "input hash, rule pack hash, result hash, lane and score"],
    ["A finding is decided", "badge, action, reason, original status"],
    ["A what-if is tried", "who, which claim, how many changes (nothing else is kept)"],
    ["A correction is saved", "both versions linked by input hash, each change from and to, findings cleared"],
    ["An identifier is unmasked", "badge, claim, reason, counted against an hourly limit"],
  ], MX, 1.55, 7.6, [2.6, 5.0], 13.5, 0.62);
  card(s, 8.5, 1.55, 4.2, 3.75, T.dark, "tamper card");
  s.addText("Tamper evidence", { x: 8.75, y: 1.65, w: 3.7, h: 0.4, fontSize: 17, bold: true, fontFace: "Cambria", color: "FFFFFF", margin: 0, isTextBox: true, objectName: "h" });
  s.addText([
    { text: "Hash-chained, signed logs", options: { bullet: { indent: 14 }, breakLine: true, paraSpaceAfter: 6 } },
    { text: "An append-only mirror that refuses to follow an edited log", options: { bullet: { indent: 14 }, breakLine: true, paraSpaceAfter: 6 } },
    { text: "Ed25519 ledger with checkpoints and Merkle proofs", options: { bullet: { indent: 14 }, breakLine: true, paraSpaceAfter: 6 } },
    { text: "Demo: rewrite a row, the health panel goes red within seconds", options: { bullet: { indent: 14 } } },
  ], { x: 8.75, y: 2.15, w: 3.75, h: 3.1, fontSize: 13.5, color: "DDEEEB", margin: 0, valign: "top", isTextBox: true, objectName: "tamper text" });
  note(s, "Trace panel: both versions of a corrected claim, side by side, with their events and decisions.", 6.85);
}

// 20 Benchmark
{
  const s = slide("Serving the model: what this laptop measured", { n: "bench" });
  if (BENCH && BENCH8) {
    const pick = (b, c) => b.levels.find((r) => r.concurrency === c);
    const rows = [];
    [1, 4, 8, 16].forEach((c) => {
      const d = pick(BENCH, c), p = pick(BENCH8, c);
      rows.push([String(c), d ? String(d.requests_per_minute) : "-", d ? d.latency_p50_s + " s" : "-", { text: String(p.requests_per_minute), bold: true, color: T.teal }, p.latency_p50_s + " s", String(p.output_tokens_per_second)]);
    });
    table(s, ["Parallel requests", "Default: per minute", "Default: median", "8 slots: per minute", "8 slots: median", "8 slots: tokens per second"], rows, MX, 1.55, 8.3, [1.3, 1.4, 1.35, 1.4, 1.3, 1.55], 13, 0.55);
    stat(s, 9.2, 1.45, 3.6, String(Math.floor(BENCH8.best_requests_per_minute)), "explanations a minute with 8 slots (gemma3:4b, laptop RTX 4060). Default: 53, one at a time", BENCH8.reaches_target ? T.teal : T.amber, 54);
  } else {
    card(s, MX, 1.55, 12.1, 2.0, T.soft, "pending");
    s.addText("Benchmark not available.", { x: MX, y: 1.55, w: 12.1, h: 2.0, align: "center", valign: "middle", fontSize: 20, color: T.muted, isTextBox: true });
  }
  card(s, MX, 4.7, 12.1, 2.0, "FFF1DB", "vllm card");
  s.addText([
    { text: "Through the real queue step: 399 claims, 777 findings, empty cache, 8 workers.", options: { bold: true, color: T.dark, breakLine: true, paraSpaceAfter: 6 } },
    { text: "74 model calls (90.5% of findings needed none), 49 seconds, about 490 claims a minute; warm cache about 2,050 a minute, 18 calls that retried 3 templates the guard had refused. 0 model errors. A debug build on this laptop: these are measurements of this machine only.", options: { color: T.ink, breakLine: true } },
    { text: "Served with Ollama. vLLM has not been run: this laptop has too little free disk and the gated model needs a token. Nothing here is a vLLM result.", options: { bold: true, color: T.dark } },
  ], { x: MX + 0.25, y: 4.7, w: 11.6, h: 2.0, fontSize: 14, valign: "middle", margin: 0, isTextBox: true, objectName: "vllm text" });
}

// =====================================================================================================================================
section("Close");
// 21 Limits
{
  const s = slide("What we have not done, said before you ask", { n: "limits" });
  const L = [
    ["Public data is synthetic", "Perfect scores on it do not show how real claims behave."],
    ["Savings are simulated", "No timed session with real reviewers yet."],
    ["Documents", "Real paper, handwriting and Arabic are untested."],
    ["Data subject rights", "Access, erasure and retention are designed, not built."],
    ["Automatic modes", "One administrator can switch them on, logged but without a second approval, and nothing switches them off by itself."],
    ["No write-back", "A correction is a new version here; it is not sent to the hospital's own system."],
    ["Copilot proposals", "Three rules have computable fixes. The rest ask or check."],
    ["Nobody outside the team", "has attacked it."],
  ];
  L.forEach((r, i) => {
    const col = i % 2, row = Math.floor(i / 2);
    const x = MX + col * 6.1, y = 1.55 + row * 1.25;
    card(s, x, y, 5.95, 1.1, T.soft, "limit " + (i + 1));
    badge(s, x + 0.2, y + 0.34, i + 1, T.red);
    s.addText([{ text: r[0], options: { bold: true, fontSize: 15, color: T.dark, breakLine: true } }, { text: r[1], options: { fontSize: 12.5, color: T.ink } }], { x: x + 0.8, y, w: 5.0, h: 1.1, valign: "middle", margin: 0, isTextBox: true, objectName: "limit text" });
  });
}

// Where this could go
{
  const s = slide("Where this could go: plans, not claims", { n: "next" });
  tag(s, 10.9, 0.08, "not built", T.amber, T.ink, 1.3);
  const cards = [
    ["Standard codes on every finding", "Each of the 15 + 8 rules carries the matching adjustment reason or NPHIES error code, shown as a badge in the console. We need the official value sets from you, not guesses."],
    ["Payer overlays", "Today a payer's contract is one versioned, approved policy table. Next: payer-specific rule sets chosen by the insurer on the claim, so one claim can pass for one payer and fail for another."],
    ["A credible time-saved study", "Two matched packs of 30 claims. Half the reviewers use the console first, half the plain view first, so learning effects cancel. Log median handling time, seeded errors missed, clean claims wrongly flagged; add a short workload survey."],
  ];
  cards.forEach((c, i) => {
    const x = MX + i * 4.1;
    card(s, x, 1.5, 3.9, 4.2, T.soft, "next card " + (i + 1));
    badge(s, x + 0.25, 1.7, i + 1, "C47A12");
    s.addText(c[0], { x: x + 0.85, y: 1.62, w: 2.9, h: 0.65, fontSize: 16, bold: true, fontFace: "Cambria", color: T.dark, margin: 0, valign: "middle", isTextBox: true, objectName: "next title" });
    s.addText(c[1], { x: x + 0.25, y: 2.5, w: 3.45, h: 3.1, fontSize: 14, color: T.ink, margin: 0, valign: "top", isTextBox: true, objectName: "next text" });
  });
  card(s, MX, 5.9, 12.1, 0.75, "FFF1DB", "scope card");
  s.addText("The benchmark stays at the 15 rules. These would be advisory or presentation changes, kept apart, and only after the mentor says which matter.", { x: MX + 0.25, y: 5.9, w: 11.6, h: 0.75, fontSize: 14, color: T.ink, valign: "middle", margin: 0, isTextBox: true, objectName: "scope text" });
}

// 22 Demo and questions
{
  const s = slide("Twelve minutes of demo, and four questions for you", { n: "demo" });
  card(s, MX, 1.55, 6.3, 5.1, T.soft, "demo card");
  s.addText("The demo", { x: MX + 0.25, y: 1.65, w: 5.8, h: 0.45, fontSize: 18, bold: true, fontFace: "Cambria", color: T.dark, margin: 0, isTextBox: true, objectName: "h" });
  const demo = ["A claim with a wrong amount arrives", "The senior opens it: summary and evidence", "What-if: every check would pass", "Apply with a reason: version 2", "Trace: both versions, who, what, why", "Rewrite an audit row: the alarm goes red"];
  demo.forEach((d, i) => {
    const y = 2.25 + i * 0.7;
    badge(s, MX + 0.25, y, i + 1, T.teal, 0.4);
    s.addText(d, { x: MX + 0.85, y, w: 5.3, h: 0.4, fontSize: 14.5, color: T.ink, valign: "middle", margin: 0, isTextBox: true, objectName: "demo step" });
  });
  card(s, 7.1, 1.55, 5.6, 5.1, T.dark, "questions card");
  s.addText("For the mentor", { x: 7.35, y: 1.65, w: 5.1, h: 0.45, fontSize: 18, bold: true, fontFace: "Cambria", color: "FFFFFF", margin: 0, isTextBox: true, objectName: "h" });
  const q = [
    "R006 and R013: three public claims split one service into a plain line and an EDU-SEPARATE line (CG-B10B19D8A56A, CG-00348A9B1AAD, CG-EC233ACB399D). The rules pass them as documented, but the total quantity exceeds the per-line limit. Intended?",
    "Is 'pre-validation clear' the right label for a claim a person confirmed or the system cleared?",
    "A Redis Streams queue in Rust: acceptable in place of Celery or ARQ?",
    "Would a timed session with a few officers be possible, to measure real time saved?",
  ];
  s.addText(q.map((t, i) => ({ text: t, options: { bullet: { type: "number" }, breakLine: i < q.length - 1, paraSpaceAfter: 9 } })), { x: 7.35, y: 2.25, w: 5.1, h: 4.2, fontSize: 14, color: "DDEEEB", margin: 0, valign: "top", isTextBox: true, objectName: "questions" });
}

// ---- write -----------------------------------------------------------------------------------------------------------------------
(async () => {
  const out = path.join(__dirname, "ClaimGuard_Mentor_Session.pptx");
  await pres.writeFile({ fileName: out });
  await applyTheme(out, THEME);
  console.log("wrote", out);
})();
