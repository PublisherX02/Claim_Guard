import Decimal from 'decimal.js';

type S = string | null | undefined;
type D = Decimal | null | undefined;

export interface Line { service_code: S; service_date: S; modifier: S; quantity: D; unit_price: D; net_amount: D; authorization_id: S }
export interface Auth { authorization_id: S; patient_id: S; service_code: S; status: S; valid_from: S; valid_to: S; max_quantity: D }
export interface Attachment { type: S; patient_id: S; service_code: S; service_date: S; document_status: S }
export interface Claim {
  claim_id: string; invoice_number: S; patient_id: S; member_id: S; provider_id: S; policy_id: S; diagnosis_code: S;
  submission_date: S; currency: S; total_amount: D;
  coverage: { status: S; beneficiary_patient_id: S; member_id: S; start_date: S; end_date: S };
  lines: Line[]; authorizations: Auth[]; attachments: Attachment[];
}
interface Policy {
  currency: string; submission_window_days: number; allowed: Set<string>; authRequired: Set<string>;
  required_documents: Record<string, string>; max_unit_price: Map<string, Decimal>; max_quantity_per_line: Map<string, Decimal>;
}

const NUMERIC = new Set(['quantity', 'unit_price', 'net_amount', 'total_amount', 'max_quantity']);

/** Parse a claim; numeric fields become exact decimals taken from the source text, never from a binary float. */
export function parseClaim(text: string): Claim {
  return JSON.parse(text, function (this: any, key: string, value: any, ctx?: { source?: string }) {
    if (typeof value === 'number' && NUMERIC.has(key)) return new Decimal(ctx!.source!);
    return value;
  });
}

export class Pack {
  policies = new Map<string, Policy>();
  services = new Set<string>();
  constructor(policiesJson: string, servicesJson: string) {
    const raw = JSON.parse(policiesJson, (k, v, ctx: any) => (typeof v === 'number' && k !== 'submission_window_days' ? new Decimal(ctx.source) : v));
    for (const [id, p] of Object.entries<any>(raw)) {
      this.policies.set(id, {
        currency: p.currency, submission_window_days: p.submission_window_days, allowed: new Set(p.allowed_providers),
        authRequired: new Set(p.auth_required_services), required_documents: p.required_documents,
        max_unit_price: new Map(Object.entries<Decimal>(p.max_unit_price)), max_quantity_per_line: new Map(Object.entries<Decimal>(p.max_quantity_per_line)),
      });
    }
    for (const k of Object.keys(JSON.parse(servicesJson))) this.services.add(k);
  }

  evaluate(c: Claim, out: Uint8Array, off: number): void {
    out[off] = r001(c); out[off + 1] = r002(c); out[off + 2] = r003(c); out[off + 3] = r004(c); out[off + 4] = this.r005(c);
    out[off + 5] = r006(c); out[off + 6] = r007(c); out[off + 7] = this.r008(c); out[off + 8] = this.r009(c); out[off + 9] = this.r010(c);
    out[off + 10] = this.r011(c); out[off + 11] = r012(c); out[off + 12] = this.r013(c); out[off + 13] = this.r014(c); out[off + 14] = this.r015(c);
  }

  policy(c: Claim): Policy | undefined { return c.policy_id == null ? undefined : this.policies.get(c.policy_id); }
  known(code: S): boolean { return !empty(code) && this.services.has(code as string); }

  r005(c: Claim): number {
    const p = this.policy(c);
    if (empty(c.provider_id) || !p) return U;
    return p.allowed.has(c.provider_id as string) ? P : F;
  }
  r008(c: Claim): number {
    const p = this.policy(c);
    if (!p) return U;
    let f = false, u = false, req = false;
    for (const l of c.lines) {
      if (!this.known(l.service_code)) u = true;
      else if (p.authRequired.has(l.service_code as string)) { req = true; if (empty(l.authorization_id)) f = true; }
    }
    return verdict(f, u, req ? P : N);
  }
  r009(c: Claim): number {
    const p = this.policy(c);
    if (!p) return U;
    let f = false, u = false, req = false;
    for (const l of c.lines) {
      if (!this.known(l.service_code)) { u = true; continue; }
      if (!p.authRequired.has(l.service_code as string)) continue;
      req = true;
      if (empty(l.authorization_id)) { u = true; continue; }
      const rec = findAuth(c, l.authorization_id as string);
      if (!rec) { f = true; continue; }
      if (empty(rec.patient_id)) u = true; else if (rec.patient_id !== c.patient_id) f = true;
      if (empty(rec.service_code)) u = true; else if (rec.service_code !== l.service_code) f = true;
      if (empty(rec.status)) u = true; else if (rec.status !== 'approved') f = true;
      const d = day(l.service_date), lo = day(rec.valid_from), hi = day(rec.valid_to);
      if (d === BAD || lo === BAD || hi === BAD) u = true; else if (!(lo <= d && d <= hi)) f = true;
    }
    const done: string[] = [];
    for (const l of c.lines) {
      if (empty(l.authorization_id) || !this.known(l.service_code) || !p.authRequired.has(l.service_code as string)) continue;
      const aid = l.authorization_id as string;
      if (done.includes(aid)) continue;
      done.push(aid);
      const rec = findAuth(c, aid);
      if (!rec) continue;
      let sum = new Decimal(0), missing = rec.max_quantity == null;
      for (const m of c.lines) if (m.authorization_id === aid) { if (m.quantity == null) missing = true; else sum = sum.plus(m.quantity); }
      if (missing) u = true; else if (sum.gt(rec.max_quantity as Decimal)) f = true;
    }
    return verdict(f, u, req ? P : N);
  }
  r010(c: Claim): number {
    const p = this.policy(c);
    if (!p) return U;
    let f = false, u = false, req = false;
    for (const l of c.lines) {
      if (!this.known(l.service_code)) { u = true; continue; }
      const need = p.required_documents[l.service_code as string];
      if (need === undefined) continue;
      req = true;
      const d = day(l.service_date);
      if (d === BAD) { u = true; continue; }
      let hits = 0, fin = false;
      for (const a of c.attachments) {
        if (a.type !== need || a.patient_id == null || a.patient_id !== c.patient_id || a.service_code !== l.service_code) continue;
        const ad = day(a.service_date);
        if (ad === BAD || ad !== d) continue;
        hits++;
        if (a.document_status === 'final') fin = true;
      }
      if (hits === 0) f = true; else if (!fin) u = true;
    }
    return verdict(f, u, req ? P : N);
  }
  r011(c: Claim): number {
    let f = false, u = false;
    for (const l of c.lines) {
      if (empty(l.service_code)) u = true; else if (!this.services.has(l.service_code as string)) f = true;
    }
    return verdict(f, u, P);
  }
  r013(c: Claim): number {
    const p = this.policy(c);
    let f = false, u = false;
    for (const l of c.lines) {
      let mp: Decimal | undefined, mq: Decimal | undefined;
      if (p && !empty(l.service_code)) { mp = p.max_unit_price.get(l.service_code as string); mq = p.max_quantity_per_line.get(l.service_code as string); }
      const limits = mp !== undefined && mq !== undefined;
      if (l.quantity == null || l.unit_price == null || !limits) u = true;
      if (l.quantity != null) {
        const q = l.quantity;
        if (q.lte(0) || !q.isInteger()) f = true;
        if (limits && q.gt(mq!)) f = true;
      }
      if (l.unit_price != null) {
        if (l.unit_price.lte(0)) f = true;
        if (limits && l.unit_price.gt(mp!)) f = true;
      }
    }
    return verdict(f, u, P);
  }
  r014(c: Claim): number {
    const p = this.policy(c);
    const sub = day(c.submission_date);
    if (!p || sub === BAD || c.lines.length === 0) return U;
    let latest = 0;
    for (const l of c.lines) { const d = day(l.service_date); if (d === BAD) return U; if (d > latest) latest = d; }
    const lag = sub - latest;
    return lag < 0 ? N : lag > p.submission_window_days ? F : P;
  }
  r015(c: Claim): number {
    const p = this.policy(c);
    if (empty(c.currency) || !p) return U;
    return c.currency === p.currency ? P : F;
  }
}

const P = 80, F = 70, U = 85, N = 78; // 'P' 'F' 'U' 'N'
const BAD = -1;
const CENT = new Decimal('0.01');

function empty(s: S): boolean { return s == null || s.trim().length === 0; }
function verdict(f: boolean, u: boolean, ok: number): number { return f ? F : u ? U : ok; }
function money(d: Decimal): Decimal { return d.toDecimalPlaces(2, Decimal.ROUND_HALF_UP); }
function findAuth(c: Claim, id: string): Auth | undefined { for (const a of c.authorizations) if (a.authorization_id === id) return a; return undefined; }

const CUM = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
function day(s: S): number {
  if (s == null || s.length !== 10) return BAD;
  for (let i = 0; i < 10; i++) {
    const ch = s.charCodeAt(i);
    if (i === 4 || i === 7) { if (ch !== 45) return BAD; } else if (ch < 48 || ch > 57) return BAD;
  }
  const y = +s.slice(0, 4), m = +s.slice(5, 7), d = +s.slice(8, 10);
  if (y < 1 || m < 1 || m > 12 || d < 1) return BAD;
  const leap = y % 4 === 0 && (y % 100 !== 0 || y % 400 === 0);
  const dim = m === 2 ? (leap ? 29 : 28) : (m === 4 || m === 6 || m === 9 || m === 11) ? 30 : 31;
  if (d > dim) return BAD;
  const yy = y - 1;
  return yy * 365 + Math.floor(yy / 4) - Math.floor(yy / 100) + Math.floor(yy / 400) + CUM[m - 1] + d + (m > 2 && leap ? 1 : 0);
}

function r001(c: Claim): number {
  let bad = empty(c.invoice_number) || empty(c.member_id) || empty(c.diagnosis_code);
  for (const l of c.lines) if (empty(l.service_date) || empty(l.service_code) || l.quantity == null || l.unit_price == null || l.net_amount == null) bad = true;
  return bad ? F : P;
}
function r002(c: Claim): number {
  const sub = day(c.submission_date);
  let f = false, u = false;
  for (const l of c.lines) { const d = day(l.service_date); if (d === BAD || sub === BAD) u = true; else if (d > sub) f = true; }
  return verdict(f, u, P);
}
function r003(c: Claim): number {
  const cv = c.coverage, start = day(cv.start_date), end = day(cv.end_date);
  let f = false, u = false;
  if (empty(cv.status)) u = true; else if (cv.status !== 'active') f = true;
  if (start === BAD || end === BAD) u = true;
  for (const l of c.lines) {
    const d = day(l.service_date);
    if (d === BAD) u = true; else if ((start !== BAD && d < start) || (end !== BAD && d > end)) f = true;
  }
  return verdict(f, u, P);
}
function r004(c: Claim): number {
  let f = false, u = false;
  if (empty(c.patient_id) || empty(c.coverage.beneficiary_patient_id)) u = true; else if (c.patient_id !== c.coverage.beneficiary_patient_id) f = true;
  if (empty(c.member_id) || empty(c.coverage.member_id)) u = true; else if (c.member_id !== c.coverage.member_id) f = true;
  return verdict(f, u, P);
}
function r006(c: Claim): number {
  const seen = new Set<string>();
  let dup = false, missing = false;
  for (const l of c.lines) {
    if (empty(l.service_code) || day(l.service_date) === BAD) { missing = true; continue; }
    const key = l.service_code + '\u0000' + l.service_date + '\u0000' + (l.modifier ?? '');
    if (seen.has(key)) dup = true; else seen.add(key);
  }
  return verdict(dup, missing, P);
}
function r007(c: Claim): number {
  let f = false, u = false;
  for (const l of c.lines) {
    if (l.quantity == null || l.unit_price == null || l.net_amount == null) u = true;
    else if (l.net_amount.minus(money(l.quantity.times(l.unit_price))).abs().gt(CENT)) f = true;
  }
  return verdict(f, u, P);
}
function r012(c: Claim): number {
  if (c.total_amount == null) return U;
  let sum = new Decimal(0);
  for (const l of c.lines) { if (l.net_amount == null) return U; sum = sum.plus(l.net_amount); }
  return c.total_amount.minus(money(sum)).abs().gt(CENT) ? F : P;
}
