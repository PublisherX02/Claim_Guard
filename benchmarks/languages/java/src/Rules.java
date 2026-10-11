import com.fasterxml.jackson.annotation.JsonIgnoreProperties;
import com.fasterxml.jackson.annotation.JsonProperty;
import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;

import java.math.BigDecimal;
import java.math.RoundingMode;
import java.util.*;

public final class Rules {

    @JsonIgnoreProperties(ignoreUnknown = true)
    public static final class Coverage {
        public String status;
        @JsonProperty("beneficiary_patient_id") public String beneficiary;
        @JsonProperty("member_id") public String memberId;
        @JsonProperty("start_date") public String startDate;
        @JsonProperty("end_date") public String endDate;
    }

    @JsonIgnoreProperties(ignoreUnknown = true)
    public static final class Line {
        @JsonProperty("service_code") public String serviceCode;
        @JsonProperty("service_date") public String serviceDate;
        public String modifier;
        public BigDecimal quantity;
        @JsonProperty("unit_price") public BigDecimal unitPrice;
        @JsonProperty("net_amount") public BigDecimal netAmount;
        @JsonProperty("authorization_id") public String authorizationId;
    }

    @JsonIgnoreProperties(ignoreUnknown = true)
    public static final class Auth {
        @JsonProperty("authorization_id") public String authorizationId;
        @JsonProperty("patient_id") public String patientId;
        @JsonProperty("service_code") public String serviceCode;
        public String status;
        @JsonProperty("valid_from") public String validFrom;
        @JsonProperty("valid_to") public String validTo;
        @JsonProperty("max_quantity") public BigDecimal maxQuantity;
    }

    @JsonIgnoreProperties(ignoreUnknown = true)
    public static final class Attachment {
        public String type;
        @JsonProperty("patient_id") public String patientId;
        @JsonProperty("service_code") public String serviceCode;
        @JsonProperty("service_date") public String serviceDate;
        @JsonProperty("document_status") public String documentStatus;
    }

    @JsonIgnoreProperties(ignoreUnknown = true)
    public static final class Claim {
        @JsonProperty("claim_id") public String claimId = "";
        @JsonProperty("invoice_number") public String invoiceNumber;
        @JsonProperty("patient_id") public String patientId;
        @JsonProperty("member_id") public String memberId;
        @JsonProperty("provider_id") public String providerId;
        @JsonProperty("policy_id") public String policyId;
        @JsonProperty("diagnosis_code") public String diagnosisCode;
        @JsonProperty("submission_date") public String submissionDate;
        public String currency;
        @JsonProperty("total_amount") public BigDecimal totalAmount;
        public Coverage coverage = new Coverage();
        public List<Line> lines = new ArrayList<>();
        public List<Auth> authorizations = new ArrayList<>();
        public List<Attachment> attachments = new ArrayList<>();
    }

    @JsonIgnoreProperties(ignoreUnknown = true)
    public static final class Policy {
        public String currency;
        @JsonProperty("submission_window_days") public int window;
        @JsonProperty("allowed_providers") public List<String> allowedProviders = new ArrayList<>();
        @JsonProperty("auth_required_services") public List<String> authRequiredServices = new ArrayList<>();
        @JsonProperty("required_documents") public Map<String, String> requiredDocuments = new HashMap<>();
        @JsonProperty("max_unit_price") public Map<String, BigDecimal> maxUnitPrice = new HashMap<>();
        @JsonProperty("max_quantity_per_line") public Map<String, BigDecimal> maxQuantityPerLine = new HashMap<>();
        Set<String> allowed, authRequired;
    }

    private final Map<String, Policy> policies;
    private final Set<String> services = new HashSet<>();

    public Rules(String policiesJson, String servicesJson) throws Exception {
        ObjectMapper m = new ObjectMapper();
        policies = m.readValue(policiesJson, m.getTypeFactory().constructMapType(HashMap.class, String.class, Policy.class));
        for (Policy p : policies.values()) {
            p.allowed = new HashSet<>(p.allowedProviders);
            p.authRequired = new HashSet<>(p.authRequiredServices);
        }
        JsonNode n = m.readTree(servicesJson);
        n.fieldNames().forEachRemaining(services::add);
    }

    static final byte P = 'P', F = 'F', U = 'U', N = 'N';
    static final BigDecimal CENT = new BigDecimal("0.01");

    static boolean empty(String s) { return s == null || s.strip().isEmpty(); }

    /** Days since 0001-01-01, or Integer.MIN_VALUE when not a well-formed ISO date. */
    static int day(String s) {
        if (s == null || s.length() != 10) return Integer.MIN_VALUE;
        for (int i = 0; i < 10; i++) {
            char ch = s.charAt(i);
            if (i == 4 || i == 7) { if (ch != '-') return Integer.MIN_VALUE; }
            else if (ch < '0' || ch > '9') return Integer.MIN_VALUE;
        }
        int y = (s.charAt(0) - '0') * 1000 + (s.charAt(1) - '0') * 100 + (s.charAt(2) - '0') * 10 + (s.charAt(3) - '0');
        int m = (s.charAt(5) - '0') * 10 + (s.charAt(6) - '0');
        int d = (s.charAt(8) - '0') * 10 + (s.charAt(9) - '0');
        if (y < 1 || m < 1 || m > 12 || d < 1) return Integer.MIN_VALUE;
        boolean leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
        int dim = m == 2 ? (leap ? 29 : 28) : (m == 4 || m == 6 || m == 9 || m == 11) ? 30 : 31;
        if (d > dim) return Integer.MIN_VALUE;
        int yy = y - 1;
        int[] cum = {0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334};
        return yy * 365 + yy / 4 - yy / 100 + yy / 400 + cum[m - 1] + d + (m > 2 && leap ? 1 : 0);
    }
    static final int BAD = Integer.MIN_VALUE;

    static BigDecimal money(BigDecimal d) { return d.setScale(2, RoundingMode.HALF_UP); }
    static byte verdict(boolean f, boolean u, byte ok) { return f ? F : u ? U : ok; }

    Policy policy(Claim c) { return c.policyId == null ? null : policies.get(c.policyId); }
    boolean known(String code) { return !empty(code) && services.contains(code); }
    static Auth findAuth(Claim c, String id) {
        for (Auth a : c.authorizations) if (id.equals(a.authorizationId)) return a;
        return null;
    }

    static byte r001(Claim c) {
        boolean bad = empty(c.invoiceNumber) || empty(c.memberId) || empty(c.diagnosisCode);
        for (Line l : c.lines)
            if (empty(l.serviceDate) || empty(l.serviceCode) || l.quantity == null || l.unitPrice == null || l.netAmount == null) bad = true;
        return bad ? F : P;
    }

    static byte r002(Claim c) {
        int sub = day(c.submissionDate);
        boolean f = false, u = false;
        for (Line l : c.lines) {
            int d = day(l.serviceDate);
            if (d == BAD || sub == BAD) u = true; else if (d > sub) f = true;
        }
        return verdict(f, u, P);
    }

    static byte r003(Claim c) {
        Coverage cv = c.coverage;
        int start = day(cv.startDate), end = day(cv.endDate);
        boolean f = false, u = false;
        if (empty(cv.status)) u = true; else if (!"active".equals(cv.status)) f = true;
        if (start == BAD || end == BAD) u = true;
        for (Line l : c.lines) {
            int d = day(l.serviceDate);
            if (d == BAD) u = true;
            else if ((start != BAD && d < start) || (end != BAD && d > end)) f = true;
        }
        return verdict(f, u, P);
    }

    static byte r004(Claim c) {
        boolean f = false, u = false;
        if (empty(c.patientId) || empty(c.coverage.beneficiary)) u = true; else if (!c.patientId.equals(c.coverage.beneficiary)) f = true;
        if (empty(c.memberId) || empty(c.coverage.memberId)) u = true; else if (!c.memberId.equals(c.coverage.memberId)) f = true;
        return verdict(f, u, P);
    }

    byte r005(Claim c) {
        Policy pol = policy(c);
        if (empty(c.providerId) || pol == null) return U;
        return pol.allowed.contains(c.providerId) ? P : F;
    }

    static byte r006(Claim c) {
        HashSet<String> seen = new HashSet<>(c.lines.size() * 2);
        boolean dup = false, missing = false;
        for (Line l : c.lines) {
            if (empty(l.serviceCode) || day(l.serviceDate) == BAD) { missing = true; continue; }
            String key = l.serviceCode + '\u0000' + l.serviceDate + '\u0000' + (l.modifier == null ? "" : l.modifier);
            if (!seen.add(key)) dup = true;
        }
        return verdict(dup, missing, P);
    }

    static byte r007(Claim c) {
        boolean f = false, u = false;
        for (Line l : c.lines) {
            if (l.quantity == null || l.unitPrice == null || l.netAmount == null) u = true;
            else if (l.netAmount.subtract(money(l.quantity.multiply(l.unitPrice))).abs().compareTo(CENT) > 0) f = true;
        }
        return verdict(f, u, P);
    }

    byte r008(Claim c) {
        Policy pol = policy(c);
        if (pol == null) return U;
        boolean f = false, u = false, req = false;
        for (Line l : c.lines) {
            if (!known(l.serviceCode)) u = true;
            else if (pol.authRequired.contains(l.serviceCode)) { req = true; if (empty(l.authorizationId)) f = true; }
        }
        return verdict(f, u, req ? P : N);
    }

    byte r009(Claim c) {
        Policy pol = policy(c);
        if (pol == null) return U;
        boolean f = false, u = false, req = false;
        for (Line l : c.lines) {
            if (!known(l.serviceCode)) { u = true; continue; }
            if (!pol.authRequired.contains(l.serviceCode)) continue;
            req = true;
            if (empty(l.authorizationId)) { u = true; continue; }
            Auth rec = findAuth(c, l.authorizationId);
            if (rec == null) { f = true; continue; }
            if (empty(rec.patientId)) u = true; else if (!rec.patientId.equals(c.patientId)) f = true;
            if (empty(rec.serviceCode)) u = true; else if (!rec.serviceCode.equals(l.serviceCode)) f = true;
            if (empty(rec.status)) u = true; else if (!"approved".equals(rec.status)) f = true;
            int d = day(l.serviceDate), lo = day(rec.validFrom), hi = day(rec.validTo);
            if (d == BAD || lo == BAD || hi == BAD) u = true; else if (!(lo <= d && d <= hi)) f = true;
        }
        ArrayList<String> done = null;
        for (Line l : c.lines) {
            if (empty(l.authorizationId) || !known(l.serviceCode) || !pol.authRequired.contains(l.serviceCode)) continue;
            String aid = l.authorizationId;
            if (done == null) done = new ArrayList<>();
            if (done.contains(aid)) continue;
            done.add(aid);
            Auth rec = findAuth(c, aid);
            if (rec == null) continue;
            BigDecimal sum = BigDecimal.ZERO;
            boolean missing = rec.maxQuantity == null;
            for (Line m : c.lines)
                if (aid.equals(m.authorizationId)) { if (m.quantity == null) missing = true; else sum = sum.add(m.quantity); }
            if (missing) u = true; else if (sum.compareTo(rec.maxQuantity) > 0) f = true;
        }
        return verdict(f, u, req ? P : N);
    }

    byte r010(Claim c) {
        Policy pol = policy(c);
        if (pol == null) return U;
        boolean f = false, u = false, req = false;
        for (Line l : c.lines) {
            if (!known(l.serviceCode)) { u = true; continue; }
            String need = pol.requiredDocuments.get(l.serviceCode);
            if (need == null) continue;
            req = true;
            int d = day(l.serviceDate);
            if (d == BAD) { u = true; continue; }
            int hits = 0; boolean fin = false;
            for (Attachment a : c.attachments) {
                if (!need.equals(a.type) || a.patientId == null || !a.patientId.equals(c.patientId) || !l.serviceCode.equals(a.serviceCode)) continue;
                int ad = day(a.serviceDate);
                if (ad == BAD || ad != d) continue;
                hits++;
                if ("final".equals(a.documentStatus)) fin = true;
            }
            if (hits == 0) f = true; else if (!fin) u = true;
        }
        return verdict(f, u, req ? P : N);
    }

    byte r011(Claim c) {
        boolean f = false, u = false;
        for (Line l : c.lines) {
            if (empty(l.serviceCode)) u = true; else if (!services.contains(l.serviceCode)) f = true;
        }
        return verdict(f, u, P);
    }

    static byte r012(Claim c) {
        if (c.totalAmount == null) return U;
        BigDecimal sum = BigDecimal.ZERO;
        for (Line l : c.lines) { if (l.netAmount == null) return U; sum = sum.add(l.netAmount); }
        return c.totalAmount.subtract(money(sum)).abs().compareTo(CENT) > 0 ? F : P;
    }

    byte r013(Claim c) {
        Policy pol = policy(c);
        boolean f = false, u = false;
        for (Line l : c.lines) {
            BigDecimal mp = null, mq = null;
            if (pol != null && !empty(l.serviceCode)) { mp = pol.maxUnitPrice.get(l.serviceCode); mq = pol.maxQuantityPerLine.get(l.serviceCode); }
            boolean limits = mp != null && mq != null;
            if (l.quantity == null || l.unitPrice == null || !limits) u = true;
            if (l.quantity != null) {
                BigDecimal q = l.quantity;
                if (q.signum() <= 0 || q.stripTrailingZeros().scale() > 0) f = true;
                if (limits && q.compareTo(mq) > 0) f = true;
            }
            if (l.unitPrice != null) {
                if (l.unitPrice.signum() <= 0) f = true;
                if (limits && l.unitPrice.compareTo(mp) > 0) f = true;
            }
        }
        return verdict(f, u, P);
    }

    byte r014(Claim c) {
        Policy pol = policy(c);
        int sub = day(c.submissionDate);
        if (pol == null || sub == BAD || c.lines.isEmpty()) return U;
        int latest = 0;
        for (Line l : c.lines) { int d = day(l.serviceDate); if (d == BAD) return U; if (d > latest) latest = d; }
        int lag = sub - latest;
        return lag < 0 ? N : lag > pol.window ? F : P;
    }

    byte r015(Claim c) {
        Policy pol = policy(c);
        if (empty(c.currency) || pol == null) return U;
        return c.currency.equals(pol.currency) ? P : F;
    }

    public void evaluate(Claim c, byte[] o, int off) {
        o[off] = r001(c); o[off + 1] = r002(c); o[off + 2] = r003(c); o[off + 3] = r004(c); o[off + 4] = r005(c);
        o[off + 5] = r006(c); o[off + 6] = r007(c); o[off + 7] = r008(c); o[off + 8] = r009(c); o[off + 9] = r010(c);
        o[off + 10] = r011(c); o[off + 11] = r012(c); o[off + 12] = r013(c); o[off + 13] = r014(c); o[off + 14] = r015(c);
    }
}
