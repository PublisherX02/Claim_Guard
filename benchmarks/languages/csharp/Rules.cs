using System.Text.Json;
using System.Text.Json.Serialization;

public sealed class Coverage
{
    [JsonPropertyName("status")] public string? Status { get; set; }
    [JsonPropertyName("beneficiary_patient_id")] public string? Beneficiary { get; set; }
    [JsonPropertyName("member_id")] public string? MemberId { get; set; }
    [JsonPropertyName("start_date")] public string? StartDate { get; set; }
    [JsonPropertyName("end_date")] public string? EndDate { get; set; }
}

public sealed class Line
{
    [JsonPropertyName("service_code")] public string? ServiceCode { get; set; }
    [JsonPropertyName("service_date")] public string? ServiceDate { get; set; }
    [JsonPropertyName("modifier")] public string? Modifier { get; set; }
    [JsonPropertyName("quantity")] public decimal? Quantity { get; set; }
    [JsonPropertyName("unit_price")] public decimal? UnitPrice { get; set; }
    [JsonPropertyName("net_amount")] public decimal? NetAmount { get; set; }
    [JsonPropertyName("authorization_id")] public string? AuthorizationId { get; set; }
}

public sealed class Auth
{
    [JsonPropertyName("authorization_id")] public string? AuthorizationId { get; set; }
    [JsonPropertyName("patient_id")] public string? PatientId { get; set; }
    [JsonPropertyName("service_code")] public string? ServiceCode { get; set; }
    [JsonPropertyName("status")] public string? Status { get; set; }
    [JsonPropertyName("valid_from")] public string? ValidFrom { get; set; }
    [JsonPropertyName("valid_to")] public string? ValidTo { get; set; }
    [JsonPropertyName("max_quantity")] public decimal? MaxQuantity { get; set; }
}

public sealed class Attachment
{
    [JsonPropertyName("type")] public string? Type { get; set; }
    [JsonPropertyName("patient_id")] public string? PatientId { get; set; }
    [JsonPropertyName("service_code")] public string? ServiceCode { get; set; }
    [JsonPropertyName("service_date")] public string? ServiceDate { get; set; }
    [JsonPropertyName("document_status")] public string? DocumentStatus { get; set; }
}

public sealed class Claim
{
    [JsonPropertyName("claim_id")] public string ClaimId { get; set; } = "";
    [JsonPropertyName("invoice_number")] public string? InvoiceNumber { get; set; }
    [JsonPropertyName("patient_id")] public string? PatientId { get; set; }
    [JsonPropertyName("member_id")] public string? MemberId { get; set; }
    [JsonPropertyName("provider_id")] public string? ProviderId { get; set; }
    [JsonPropertyName("policy_id")] public string? PolicyId { get; set; }
    [JsonPropertyName("diagnosis_code")] public string? DiagnosisCode { get; set; }
    [JsonPropertyName("submission_date")] public string? SubmissionDate { get; set; }
    [JsonPropertyName("currency")] public string? Currency { get; set; }
    [JsonPropertyName("total_amount")] public decimal? TotalAmount { get; set; }
    [JsonPropertyName("coverage")] public Coverage Coverage { get; set; } = new();
    [JsonPropertyName("lines")] public List<Line> Lines { get; set; } = new();
    [JsonPropertyName("authorizations")] public List<Auth> Authorizations { get; set; } = new();
    [JsonPropertyName("attachments")] public List<Attachment> Attachments { get; set; } = new();
}

public sealed class Policy
{
    [JsonPropertyName("currency")] public string Currency { get; set; } = "";
    [JsonPropertyName("submission_window_days")] public int SubmissionWindowDays { get; set; }
    [JsonPropertyName("allowed_providers")] public List<string> AllowedProviders { get; set; } = new();
    [JsonPropertyName("auth_required_services")] public List<string> AuthRequiredServices { get; set; } = new();
    [JsonPropertyName("required_documents")] public Dictionary<string, string> RequiredDocuments { get; set; } = new();
    [JsonPropertyName("max_unit_price")] public Dictionary<string, decimal> MaxUnitPrice { get; set; } = new();
    [JsonPropertyName("max_quantity_per_line")] public Dictionary<string, decimal> MaxQuantityPerLine { get; set; } = new();
    public HashSet<string> Allowed = new();
    public HashSet<string> AuthRequired = new();
}

public sealed class Pack
{
    public Dictionary<string, Policy> Policies;
    public HashSet<string> Services;

    public Pack(string policiesJson, string servicesJson)
    {
        Policies = JsonSerializer.Deserialize<Dictionary<string, Policy>>(policiesJson)!;
        foreach (var p in Policies.Values) { p.Allowed = new(p.AllowedProviders); p.AuthRequired = new(p.AuthRequiredServices); }
        Services = JsonSerializer.Deserialize<Dictionary<string, JsonElement>>(servicesJson)!.Keys.ToHashSet();
    }

    const byte P = (byte)'P', F = (byte)'F', U = (byte)'U', N = (byte)'N';

    static bool Empty(string? s) => s == null || s.Trim().Length == 0;

    static bool Day(string? s, out int days)
    {
        days = 0;
        if (s == null || s.Length != 10) return false;
        for (int i = 0; i < 10; i++)
        {
            char ch = s[i];
            if (i == 4 || i == 7) { if (ch != '-') return false; }
            else if (ch < '0' || ch > '9') return false;
        }
        int y = (s[0] - '0') * 1000 + (s[1] - '0') * 100 + (s[2] - '0') * 10 + (s[3] - '0');
        int m = (s[5] - '0') * 10 + (s[6] - '0');
        int d = (s[8] - '0') * 10 + (s[9] - '0');
        if (y < 1 || m < 1 || m > 12 || d < 1) return false;
        bool leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
        int dim = m == 2 ? (leap ? 29 : 28) : (m == 4 || m == 6 || m == 9 || m == 11) ? 30 : 31;
        if (d > dim) return false;
        int yy = y - 1;
        int[] cum = { 0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334 };
        days = yy * 365 + yy / 4 - yy / 100 + yy / 400 + cum[m - 1] + d + (m > 2 && leap ? 1 : 0);
        return true;
    }

    static decimal Money(decimal d) => Math.Round(d, 2, MidpointRounding.AwayFromZero);
    const decimal Cent = 0.01m;

    static byte Verdict(bool f, bool u, byte ok) => f ? F : u ? U : ok;

    Policy? PolicyOf(Claim c) => c.PolicyId != null && Policies.TryGetValue(c.PolicyId, out var p) ? p : null;
    bool Known(string? code) => !Empty(code) && Services.Contains(code!);
    static Auth? FindAuth(Claim c, string id)
    {
        foreach (var a in c.Authorizations) if (a.AuthorizationId == id) return a;
        return null;
    }

    static byte R001(Claim c)
    {
        bool bad = Empty(c.InvoiceNumber) || Empty(c.MemberId) || Empty(c.DiagnosisCode);
        foreach (var l in c.Lines)
            if (Empty(l.ServiceDate) || Empty(l.ServiceCode) || l.Quantity is null || l.UnitPrice is null || l.NetAmount is null) bad = true;
        return bad ? F : P;
    }

    static byte R002(Claim c)
    {
        bool subOk = Day(c.SubmissionDate, out int sub), f = false, u = false;
        foreach (var l in c.Lines)
        {
            if (!Day(l.ServiceDate, out int d) || !subOk) u = true;
            else if (d > sub) f = true;
        }
        return Verdict(f, u, P);
    }

    static byte R003(Claim c)
    {
        var cv = c.Coverage;
        bool sOk = Day(cv.StartDate, out int start), eOk = Day(cv.EndDate, out int end), f = false, u = false;
        if (Empty(cv.Status)) u = true; else if (cv.Status != "active") f = true;
        if (!sOk || !eOk) u = true;
        foreach (var l in c.Lines)
        {
            if (!Day(l.ServiceDate, out int d)) u = true;
            else if ((sOk && d < start) || (eOk && d > end)) f = true;
        }
        return Verdict(f, u, P);
    }

    static byte R004(Claim c)
    {
        bool f = false, u = false;
        if (Empty(c.PatientId) || Empty(c.Coverage.Beneficiary)) u = true; else if (c.PatientId != c.Coverage.Beneficiary) f = true;
        if (Empty(c.MemberId) || Empty(c.Coverage.MemberId)) u = true; else if (c.MemberId != c.Coverage.MemberId) f = true;
        return Verdict(f, u, P);
    }

    byte R005(Claim c)
    {
        var pol = PolicyOf(c);
        if (Empty(c.ProviderId) || pol == null) return U;
        return pol.Allowed.Contains(c.ProviderId!) ? P : F;
    }

    static byte R006(Claim c)
    {
        var seen = new HashSet<(string, string, string)>(c.Lines.Count);
        bool dup = false, missing = false;
        foreach (var l in c.Lines)
        {
            if (Empty(l.ServiceCode) || !Day(l.ServiceDate, out _)) { missing = true; continue; }
            if (!seen.Add((l.ServiceCode!, l.ServiceDate!, l.Modifier ?? ""))) dup = true;
        }
        return Verdict(dup, missing, P);
    }

    static byte R007(Claim c)
    {
        bool f = false, u = false;
        foreach (var l in c.Lines)
        {
            if (l.Quantity is null || l.UnitPrice is null || l.NetAmount is null) u = true;
            else if (Math.Abs(l.NetAmount.Value - Money(l.Quantity.Value * l.UnitPrice.Value)) > Cent) f = true;
        }
        return Verdict(f, u, P);
    }

    byte R008(Claim c)
    {
        var pol = PolicyOf(c);
        if (pol == null) return U;
        bool f = false, u = false, req = false;
        foreach (var l in c.Lines)
        {
            if (!Known(l.ServiceCode)) u = true;
            else if (pol.AuthRequired.Contains(l.ServiceCode!)) { req = true; if (Empty(l.AuthorizationId)) f = true; }
        }
        return Verdict(f, u, req ? P : N);
    }

    byte R009(Claim c)
    {
        var pol = PolicyOf(c);
        if (pol == null) return U;
        bool f = false, u = false, req = false;
        foreach (var l in c.Lines)
        {
            if (!Known(l.ServiceCode)) { u = true; continue; }
            if (!pol.AuthRequired.Contains(l.ServiceCode!)) continue;
            req = true;
            if (Empty(l.AuthorizationId)) { u = true; continue; }
            var rec = FindAuth(c, l.AuthorizationId!);
            if (rec == null) { f = true; continue; }
            if (Empty(rec.PatientId)) u = true; else if (rec.PatientId != c.PatientId) f = true;
            if (Empty(rec.ServiceCode)) u = true; else if (rec.ServiceCode != l.ServiceCode) f = true;
            if (Empty(rec.Status)) u = true; else if (rec.Status != "approved") f = true;
            if (!Day(l.ServiceDate, out int d) | !Day(rec.ValidFrom, out int lo) | !Day(rec.ValidTo, out int hi)) u = true;
            else if (!(lo <= d && d <= hi)) f = true;
        }
        List<string>? done = null;
        foreach (var l in c.Lines)
        {
            if (Empty(l.AuthorizationId) || !Known(l.ServiceCode) || !pol.AuthRequired.Contains(l.ServiceCode!)) continue;
            string aid = l.AuthorizationId!;
            done ??= new();
            if (done.Contains(aid)) continue;
            done.Add(aid);
            var rec = FindAuth(c, aid);
            if (rec == null) continue;
            decimal sum = 0; bool missing = rec.MaxQuantity is null;
            foreach (var m in c.Lines)
                if (m.AuthorizationId == aid) { if (m.Quantity is null) missing = true; else sum += m.Quantity.Value; }
            if (missing) u = true; else if (sum > rec.MaxQuantity!.Value) f = true;
        }
        return Verdict(f, u, req ? P : N);
    }

    byte R010(Claim c)
    {
        var pol = PolicyOf(c);
        if (pol == null) return U;
        bool f = false, u = false, req = false;
        foreach (var l in c.Lines)
        {
            if (!Known(l.ServiceCode)) { u = true; continue; }
            if (!pol.RequiredDocuments.TryGetValue(l.ServiceCode!, out var need)) continue;
            req = true;
            if (!Day(l.ServiceDate, out int d)) { u = true; continue; }
            int hits = 0; bool fin = false;
            foreach (var a in c.Attachments)
            {
                if (a.Type != need || a.PatientId == null || a.PatientId != c.PatientId || a.ServiceCode != l.ServiceCode) continue;
                if (!Day(a.ServiceDate, out int ad) || ad != d) continue;
                hits++;
                if (a.DocumentStatus == "final") fin = true;
            }
            if (hits == 0) f = true; else if (!fin) u = true;
        }
        return Verdict(f, u, req ? P : N);
    }

    byte R011(Claim c)
    {
        bool f = false, u = false;
        foreach (var l in c.Lines)
        {
            if (Empty(l.ServiceCode)) u = true;
            else if (!Services.Contains(l.ServiceCode!)) f = true;
        }
        return Verdict(f, u, P);
    }

    static byte R012(Claim c)
    {
        if (c.TotalAmount is null) return U;
        decimal sum = 0;
        foreach (var l in c.Lines) { if (l.NetAmount is null) return U; sum += l.NetAmount.Value; }
        return Math.Abs(c.TotalAmount.Value - Money(sum)) > Cent ? F : P;
    }

    byte R013(Claim c)
    {
        var pol = PolicyOf(c);
        bool f = false, u = false;
        foreach (var l in c.Lines)
        {
            decimal mp = 0, mq = 0; bool limits = false;
            if (pol != null && !Empty(l.ServiceCode))
                limits = pol.MaxUnitPrice.TryGetValue(l.ServiceCode!, out mp) & pol.MaxQuantityPerLine.TryGetValue(l.ServiceCode!, out mq);
            if (l.Quantity is null || l.UnitPrice is null || !limits) u = true;
            if (l.Quantity is decimal q)
            {
                if (q <= 0 || q != decimal.Truncate(q)) f = true;
                if (limits && q > mq) f = true;
            }
            if (l.UnitPrice is decimal p)
            {
                if (p <= 0) f = true;
                if (limits && p > mp) f = true;
            }
        }
        return Verdict(f, u, P);
    }

    byte R014(Claim c)
    {
        var pol = PolicyOf(c);
        if (pol == null || !Day(c.SubmissionDate, out int sub) || c.Lines.Count == 0) return U;
        int latest = 0;
        foreach (var l in c.Lines) { if (!Day(l.ServiceDate, out int d)) return U; if (d > latest) latest = d; }
        int lag = sub - latest;
        return lag < 0 ? N : lag > pol.SubmissionWindowDays ? F : P;
    }

    byte R015(Claim c)
    {
        var pol = PolicyOf(c);
        if (Empty(c.Currency) || pol == null) return U;
        return c.Currency == pol.Currency ? P : F;
    }

    public void Evaluate(Claim c, Span<byte> o)
    {
        o[0] = R001(c); o[1] = R002(c); o[2] = R003(c); o[3] = R004(c); o[4] = R005(c);
        o[5] = R006(c); o[6] = R007(c); o[7] = R008(c); o[8] = R009(c); o[9] = R010(c);
        o[10] = R011(c); o[11] = R012(c); o[12] = R013(c); o[13] = R014(c); o[14] = R015(c);
    }
}
